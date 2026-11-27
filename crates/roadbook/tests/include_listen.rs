//! Host listeners of a run's emitted events (`spec/execution.md#host-listeners`):
//! `Actor::subscribe` and `RunHandle::subscribe`, on the Effect driver tests' `Beacon`, which
//! emits as it starts, on each `Ping`, after `log` on `Boom`, and as it leaves `Up`.

#![deny(warnings)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use roadbook::{
    BoxFuture, Config, MemorySink, RunHandle, Stopped, Subscription, Timer, TraceRecord,
};
use serde_json::{Value, json};

mod rt {
    roadbook::include!("../../packages/effect/test/charts/beacon.road");
}

use rt::beacon::{Emitted, Event};

/// Virtual milliseconds on Tokio's paused clock, so that two runs write the same `t`s.
struct VirtualTimer(tokio::time::Instant);

impl Timer for VirtualTimer {
    fn now(&self) -> u64 {
        self.0.elapsed().as_millis() as u64
    }

    fn start(&self, _run: &str, _timer: u64, delay: u64) -> BoxFuture<'static, ()> {
        Box::pin(tokio::time::sleep(Duration::from_millis(delay)))
    }
}

fn config(sink: &Arc<MemorySink>) -> Config {
    Config::new()
        .sink(sink.clone())
        .timer(Arc::new(VirtualTimer(tokio::time::Instant::now())))
        .run_id("b")
}

/// `log` panics, a host defect, when `boom` is set.
struct Host {
    boom: bool,
}

impl rt::beacon::Handlers for Host {
    async fn log(&self, what: &str) {
        if self.boom {
            panic!("{what}");
        }
    }
}

fn host(boom: bool) -> Arc<Host> {
    Arc::new(Host { boom })
}

fn emitted_values(records: &[TraceRecord]) -> Vec<Value> {
    records
        .iter()
        .map(TraceRecord::to_json)
        .filter(|j| j["kind"] == "emitted")
        .map(|j| j["value"].clone())
        .collect()
}

/// Every event left in `sub` until the run ends.
async fn rest<T>(sub: &mut Subscription<T>) -> Vec<T> {
    let mut all = Vec::new();
    while let Some(e) = sub.next().await {
        all.push(e);
    }
    all
}

fn beat(n: i64) -> Emitted {
    Emitted::Beat { n }
}

/// Spawn, optionally subscribe, send Ping, Ping, Stop: what was heard and the trace.
async fn pings(listening: bool) -> (Vec<Emitted>, Vec<Value>) {
    let sink = Arc::new(MemorySink::new());
    let actor = rt::beacon::spawn_with(config(&sink), host(false));
    let mut sub = listening.then(|| actor.subscribe());
    actor.send(Event::Ping).await;
    actor.send(Event::Ping).await;
    actor.send(Event::Stop).await;
    actor.result().await.unwrap();
    let heard = match sub.as_mut() {
        Some(s) => rest(s).await,
        None => Vec::new(),
    };
    let records = sink.records().iter().map(TraceRecord::to_json).collect();
    (heard, records)
}

#[tokio::test(start_paused = true)]
async fn subscribe_hears_what_the_run_emits_after_it_typed_and_writes_nothing() {
    let (heard, records) = pings(true).await;
    // The start step's Beat came before the actor, so the subscription does not have it.
    assert_eq!(heard, vec![beat(1), beat(2), Emitted::Bye]);
    let values: Vec<Value> = records
        .iter()
        .filter(|j| j["kind"] == "emitted")
        .map(|j| j["value"].clone())
        .collect();
    assert_eq!(
        values,
        vec![
            json!({"_tag": "Beat", "n": 0}),
            json!({"_tag": "Beat", "n": 1}),
            json!({"_tag": "Beat", "n": 2}),
            json!({"_tag": "Bye"}),
        ]
    );
    assert_eq!(records, pings(false).await.1);
}

#[tokio::test(start_paused = true)]
async fn each_subscription_hears_from_when_it_was_made() {
    let sink = Arc::new(MemorySink::new());
    let actor = rt::beacon::spawn_with(config(&sink), host(false));
    let mut early = actor.subscribe();
    let dropped = actor.subscribe();
    drop(dropped);
    actor.send(Event::Ping).await;
    assert_eq!(early.next().await, Some(beat(1)));
    let mut late = actor.subscribe();
    assert_eq!(late.try_next(), None);
    actor.send(Event::Ping).await;
    actor.send(Event::Stop).await;
    actor.result().await.unwrap();
    assert_eq!(rest(&mut early).await, vec![beat(2), Emitted::Bye]);
    assert_eq!(rest(&mut late).await, vec![beat(2), Emitted::Bye]);
    assert_eq!(early.next().await, None);
    assert_eq!(early.try_next(), None);
}

#[tokio::test(start_paused = true)]
async fn a_cancelled_runs_exit_effects_reach_the_subscription_then_it_ends() {
    let sink = Arc::new(MemorySink::new());
    let actor = rt::beacon::spawn_with(config(&sink), host(false));
    let mut sub = actor.subscribe();
    actor.cancel().await;
    assert_eq!(rest(&mut sub).await, vec![Emitted::Bye]);
    assert_eq!(actor.result().await, Err(Stopped::Cancelled));
}

#[tokio::test(start_paused = true)]
async fn suspending_ends_the_subscription_without_exit_effects() {
    let sink = Arc::new(MemorySink::new());
    let actor = rt::beacon::spawn_with(config(&sink), host(false));
    let mut sub = actor.subscribe();
    actor.send(Event::Ping).await;
    assert_eq!(sub.next().await, Some(beat(1)));
    actor.suspend().await;
    assert_eq!(rest(&mut sub).await, vec![]);
}

#[tokio::test(start_paused = true)]
async fn an_emit_after_an_actions_host_defect_is_heard_by_no_listener() {
    let sink = Arc::new(MemorySink::new());
    let actor = rt::beacon::spawn_with(config(&sink), host(true));
    let mut sub = actor.subscribe();
    actor.send(Event::Boom).await;
    let ended = tokio::spawn(actor.result()).await;
    assert!(ended.unwrap_err().is_panic());
    assert_eq!(rest(&mut sub).await, vec![]);
    assert_eq!(
        emitted_values(&sink.records()),
        vec![json!({"_tag": "Beat", "n": 0})]
    );
}

#[tokio::test(start_paused = true)]
async fn a_tools_subscription_from_on_run_hears_the_start_step_too() {
    let sink = Arc::new(MemorySink::new());
    let subs: Arc<Mutex<Vec<Subscription<Value>>>> = Arc::default();
    let handles: Arc<Mutex<Vec<RunHandle>>> = Arc::default();
    let (keep, keep_handles) = (subs.clone(), handles.clone());
    let actor = rt::beacon::spawn_with(
        config(&sink).on_run(move |_, run| {
            keep.lock().unwrap().push(run.subscribe());
            keep_handles.lock().unwrap().push(run);
        }),
        host(false),
    );
    actor.send(Event::Ping).await;
    actor.send(Event::Stop).await;
    actor.result().await.unwrap();
    let mut sub = subs.lock().unwrap().pop().unwrap();
    let mut heard = Vec::new();
    while let Some(v) = sub.try_next() {
        heard.push(v);
    }
    assert_eq!(heard, emitted_values(&sink.records()));
    assert_eq!(heard.len(), 3);
    // One made after the run ended has ended at once.
    let handle = handles.lock().unwrap().pop().unwrap();
    assert_eq!(handle.subscribe().next().await, None);
}
