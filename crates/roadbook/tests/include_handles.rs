//! Handles and what host code does with them: a run is subscribed to its peers when `spawn`
//! returns, a child run that host code drops does not hang its parent, an event that does not
//! encode is the sender's defect, and a run checks the handles it is given.

#![deny(warnings)]

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Duration;

use roadbook::dynamic::{self, Host, Invocation, Outcome};
use roadbook::{BoxFuture, Config, Defect, Program, RunEnd, RunHandle};
use roadbook_core::{Object, SrcKind};
use serde_json::{Value, json};

mod charts {
    roadbook::include!("tests/charts/hub.road");
}

use charts::{burst, holder, hub, relay};

#[derive(Default)]
struct Host_;

impl hub::Handlers for Host_ {}
impl burst::Handlers for Host_ {}

impl relay::Handlers for Host_ {
    async fn relay(&self, hub: &hub::Ref) {
        hub.send(hub::Event::Reading { value: f64::NAN }).await;
    }
}

impl holder::Handlers for Host_ {
    /// Starts the child run, then drops it without letting it finish, and never ends.
    async fn burst(&self, hub: &hub::Ref) {
        let mut child = Box::pin(burst::run(self, hub.clone()));
        std::future::poll_fn(|cx| {
            assert!(child.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        drop(child);
        std::future::pending().await
    }
}

const PATIENCE: Duration = Duration::from_secs(10);

fn runtimes() -> [tokio::runtime::Runtime; 2] {
    [
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap(),
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .unwrap(),
    ]
}

#[test]
fn a_spawned_run_hears_what_its_peer_emits_right_after_spawn() {
    for rt in runtimes() {
        rt.block_on(async {
            let host = Arc::new(Host_);
            for _ in 0..300 {
                let hub = hub::spawn(host.clone());
                let burst = burst::spawn(host.clone(), hub.handle());
                hub.handle().send(hub::Event::Ping).await;
                tokio::time::timeout(PATIENCE, burst.result())
                    .await
                    .expect("Burst never heard the Pong")
                    .unwrap();
                hub.cancel().await;
            }
        });
    }
}

#[tokio::test]
async fn a_parent_does_not_wait_for_a_child_run_that_host_code_dropped() {
    let host = Arc::new(Host_);
    let hub = hub::spawn(host.clone());
    let children: Arc<Mutex<Vec<RunHandle>>> = Arc::default();
    let seen = children.clone();
    let config = Config::new().on_run(move |id, h| {
        if id.contains('.') {
            seen.lock().unwrap().push(h);
        }
    });
    let holder = holder::spawn_with(config, host.clone(), hub.handle());
    while children.lock().unwrap().is_empty() {
        tokio::task::yield_now().await;
    }
    let child = children.lock().unwrap()[0].clone();
    holder.send(holder::Event::Leave).await;
    tokio::time::timeout(PATIENCE, holder.result())
        .await
        .expect("the parent hung cancelling the dropped child")
        .unwrap();
    // The host code dropped the child run's future: a hard stop, so the run has ended.
    assert!(child.is_finished());
    hub.cancel().await;
}

#[tokio::test]
async fn a_dropped_actor_or_run_future_is_finished() {
    let host = Arc::new(Host_);
    let actor = hub::spawn(host.clone());
    let handle = actor.handle();
    tokio::task::yield_now().await;
    drop(actor);
    tokio::time::timeout(PATIENCE, async {
        while !handle.run_handle().is_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("a dropped actor's run never finished");
    assert!(handle.is_closed());

    let program = compile(CHART);
    let host = Dyn(&program);
    let (fut, handle) = dynamic::start(&Config::new(), &program, "Hub", Object::new(), &[], &host);
    let mut fut = Box::pin(fut);
    std::future::poll_fn(|cx| {
        assert!(fut.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    assert!(!handle.is_finished());
    drop(fut);
    assert!(handle.is_finished());
}

fn defect_of(e: tokio::task::JoinError) -> Defect {
    *e.into_panic()
        .downcast::<Defect>()
        .expect("a Defect payload")
}

#[tokio::test(flavor = "multi_thread")]
async fn an_event_that_does_not_encode_is_the_senders_defect() {
    let host = Arc::new(Host_);
    let hub = hub::spawn(host.clone());
    let relay = relay::spawn(host.clone(), hub.handle());
    relay.send(relay::Event::Go).await;
    // The action's `send` panicked: the relay's own host defect.
    let e = tokio::spawn(relay.result()).await.unwrap_err();
    assert!(
        matches!(defect_of(e), Defect::Host { message } if message.contains("not a finite float"))
    );
    // The hub never got it, and is still up.
    assert!(!hub.handle().is_closed());
    assert_eq!(hub.state(), hub::State::Up);
    hub.cancel().await;
    assert_eq!(hub.result().await, Err(roadbook::Stopped::Cancelled));
}

// ---- the erased interface ------------------------------------------------------------------

const CHART: &str = include_str!("charts/hub.road");

fn compile(source: &str) -> Program {
    let c = roadbook_compiler::compile_source("hub.road", source, &Default::default());
    assert!(!c.has_errors(), "{}", c.render());
    Program::new(c.ir.unwrap()).unwrap()
}

/// Runs invoked machines; actions do nothing; tasks never end.
struct Dyn<'p>(&'p Program);

impl Host for Dyn<'_> {
    fn function(&self, name: &str, _: Vec<Value>) -> Result<Value, Defect> {
        Err(Defect::host(name))
    }

    fn action<'a>(&'a self, _: &str, _: Object) -> BoxFuture<'a, Result<(), Defect>> {
        Box::pin(async { Ok(()) })
    }

    fn invoke<'a>(&'a self, call: Invocation) -> BoxFuture<'a, Outcome> {
        match call.kind {
            SrcKind::Machine => Box::pin(async move {
                match dynamic::run(self.0, &call.src, call.args, &call.refs, self).await {
                    Ok(v) => Outcome::Done(v),
                    Err(e) => Outcome::Fail(e),
                }
            }),
            SrcKind::Task => Box::pin(std::future::pending()),
        }
    }
}

fn hub_input(id: &str) -> Object {
    json!({ "hub": id }).as_object().unwrap().clone()
}

/// Starts `Burst` with `hub` as its `ref Hub` and gives how it ended, which must be at once.
async fn burst_with(program: &Program, hub: &RunHandle) -> RunEnd {
    let host = Dyn(program);
    let (fut, _) = dynamic::start(
        &Config::new(),
        program,
        "Burst",
        hub_input(hub.id()),
        std::slice::from_ref(hub),
        &host,
    );
    tokio::time::timeout(PATIENCE, fut)
        .await
        .expect("the run started")
}

fn host_defect(end: &RunEnd, says: &str) -> bool {
    matches!(end, RunEnd::Defect(Defect::Host { message }) if message.contains(says))
}

#[tokio::test]
async fn a_handle_to_another_machine_is_a_host_defect() {
    let program = compile(CHART);
    let host = Dyn(&program);
    let (hub_fut, hub) = dynamic::start(&Config::new(), &program, "Hub", Object::new(), &[], &host);
    let (_relay_fut, relay) = dynamic::start(
        &Config::new(),
        &program,
        "Relay",
        hub_input(hub.id()),
        std::slice::from_ref(&hub),
        &host,
    );
    let end = burst_with(&program, &relay).await;
    assert!(host_defect(&end, "is a `Relay`"), "{end:?}");

    // A Hub of a chart where Hub is different has another machine hash.
    let changed = compile(&CHART.replace("event Ping", "event Ping\n  event Pang"));
    let (_other_fut, other) =
        dynamic::start(&Config::new(), &changed, "Hub", Object::new(), &[], &host);
    let end = burst_with(&program, &other).await;
    assert!(host_defect(&end, "another version of `Hub`"), "{end:?}");

    // The right one is taken: Burst hears the Hub's Pong.
    let (burst_fut, _) = dynamic::start(
        &Config::new(),
        &program,
        "Burst",
        hub_input(hub.id()),
        std::slice::from_ref(&hub),
        &host,
    );
    hub.send(json!({ "_tag": "Ping" }));
    let mut hub_fut = hub_fut;
    let mut burst_fut = burst_fut;
    let end = std::future::poll_fn(|cx| {
        let _ = hub_fut.as_mut().poll(cx);
        burst_fut.as_mut().poll(cx)
    });
    let end = tokio::time::timeout(PATIENCE, end).await.unwrap();
    assert_eq!(end, RunEnd::Done(Value::Null));
}

#[tokio::test]
async fn a_child_runs_handle_cannot_be_passed_as_a_ref() {
    let program = compile(CHART);
    let host = Dyn(&program);
    let children: Arc<Mutex<Vec<RunHandle>>> = Arc::default();
    let seen = children.clone();
    let config = Config::new().on_run(move |id, h| {
        if id.contains('.') {
            seen.lock().unwrap().push(h);
        }
    });
    let (keeper, _) = dynamic::start(&config, &program, "Keeper", Object::new(), &[], &host);
    let mut keeper = Box::pin(keeper);
    std::future::poll_fn(|cx| {
        let _ = keeper.as_mut().poll(cx);
        Poll::Ready(())
    })
    .await;
    let child = children.lock().unwrap()[0].clone();
    assert_eq!(child.machine(), "Hub");
    let end = burst_with(&program, &child).await;
    assert!(host_defect(&end, "child run"), "{end:?}");
}
