//! Saving and restoring through `roadbook::include!`.
//!
//! The request handler of `docs/design/semantics.md` ("Saving and restoring"), with the
//! generated, typed code: each "request" loads an order's saved snapshot from a store,
//! `resume`s it with one event, and stores the snapshot it gives. Between requests the chart
//! changes (a variable is renamed), and the stored snapshots go on through a migration. Also
//! an actor saved, restored and suspended, parallel states in the typed `State`, and history
//! states, which are never one.

#![deny(warnings)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use roadbook::{BoxFuture, Config, MemorySink, Record, RunResult, Saved, Status, Timer};
use serde_json::{Value, json};

/// The chart as it was first deployed.
mod v1 {
    roadbook::include!("tests/charts/order_v1.road");
}

/// The chart as it is now: `Order` renamed `trackingNo` to `tracking`.
mod v2 {
    roadbook::include!("../../spec/conformance/charts/saved.road");
}

mod par {
    roadbook::include!("../../spec/conformance/charts/parallel.road");
}

mod hist {
    roadbook::include!("../../spec/conformance/charts/history.road");
}

const HOUR: u64 = 3_600_000;

/// The time, which the test sets, and timers that never fire by themselves: a request
/// handler never waits for a timer, it saves it with its deadline.
#[derive(Clone, Default)]
struct Clock(Arc<AtomicU64>);

impl Clock {
    fn set(&self, ms: u64) {
        self.0.store(ms, Ordering::SeqCst);
    }
}

impl Timer for Clock {
    fn now(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }

    fn start(&self, _run: &str, _timer: u64, _delay: u64) -> BoxFuture<'static, ()> {
        Box::pin(std::future::pending())
    }
}

#[derive(Default)]
struct Shop {
    shipped: Mutex<Vec<String>>,
    refunds: Mutex<Vec<(String, i64)>>,
    counted: Mutex<Vec<i64>>,
}

impl Shop {
    async fn ship_order(&self, order: &str) -> String {
        // Shipping takes a while: `resume` waits for it.
        tokio::time::sleep(Duration::from_millis(5)).await;
        let mut shipped = self.shipped.lock().unwrap();
        shipped.push(order.to_owned());
        format!("t-{}", shipped.len())
    }
}

impl v1::order::Handlers for Shop {
    async fn refund(&self, order: &str, amount: i64) {
        self.refunds
            .lock()
            .unwrap()
            .push((order.to_owned(), amount));
    }

    async fn ship(&self, order: &str) -> String {
        self.ship_order(order).await
    }
}

impl v2::order::Handlers for Shop {
    async fn refund(&self, order: &str, amount: i64) {
        self.refunds
            .lock()
            .unwrap()
            .push((order.to_owned(), amount));
    }

    async fn ship(&self, order: &str) -> String {
        self.ship_order(order).await
    }
}

impl v2::reminder::Handlers for Shop {
    async fn log(&self, _what: &str) {}
}

impl v2::counter::Handlers for Shop {
    async fn count(&self, _what: &str, n: i64) {
        self.counted.lock().unwrap().push(n);
    }
}

/// Where the service keeps its orders between requests: saved snapshots as JSON text.
#[derive(Default)]
struct Store(Mutex<HashMap<String, String>>);

impl Store {
    fn save(&self, id: &str, saved: &Saved) {
        let text = saved.to_canonical();
        self.0.lock().unwrap().insert(id.to_owned(), text);
    }

    fn load(&self, id: &str) -> Saved {
        Saved::from_json(&self.0.lock().unwrap()[id]).expect("the store holds saved snapshots")
    }
}

/// The deployment's migration of snapshots saved by the first version.
fn migrate(mut old: Value) -> Result<Value, String> {
    let vars = old["vars"].as_object_mut().ok_or("no vars")?;
    let tracking = vars
        .remove("trackingNo")
        .ok_or("not a snapshot of the first version")?;
    vars.insert("tracking".into(), tracking);
    old["ir"] = Value::from(v2::order::HASH);
    Ok(old)
}

fn kinds(sink: &MemorySink, run: &str) -> Vec<String> {
    sink.records()
        .iter()
        .filter(|r| r.run == run)
        .map(|r| r.to_json()["kind"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn a_request_handler_keeps_orders_as_snapshots_across_a_migration() {
    let app = Arc::new(Shop::default());
    let store = Store::default();
    let clock = Clock::default();
    let sink = Arc::new(MemorySink::new());
    let config = Config::new()
        .sink(sink.clone())
        .timer(Arc::new(clock.clone()));

    // Request 1, version 1, at 0: create two orders, suspending each once idle.
    for id in ["o1", "o2"] {
        let actor = v1::order::spawn_with(config.clone().run_id(id), app.clone(), id.into());
        let saved = actor.suspend().await;
        assert_eq!(saved.status, Status::Running);
        assert_eq!(saved.active, ["Order", "Order.AwaitingPayment"]);
        assert_eq!(saved.next_due(), Some(72 * HOUR));
        store.save(id, &saved);
    }
    assert_eq!(kinds(&sink, "o1").last().unwrap(), "suspended");

    // Request 2, version 1, an hour later: pay o1. Resume waits for the shipping task the
    // payment starts, so the snapshot has no running invoke and no timer.
    clock.set(HOUR);
    let event = v1::order::Event::PaymentReceived { amount: 42 };
    let paid = v1::order::resume_with(
        &config.clone().run_id("pay-o1"),
        &*app,
        store.load("o1"),
        Some(event),
    )
    .await
    .unwrap();
    assert_eq!(paid.active, ["Order", "Order.AwaitingDelivery"]);
    assert_eq!(paid.vars["trackingNo"], "t-1");
    assert!(paid.invokes.is_empty() && paid.timers.is_empty());
    assert_eq!(paid.run, "pay-o1");
    store.save("o1", &paid);
    // The restored run restarted the payment timer for what was left of the 72 hours.
    let restarted = sink
        .records()
        .into_iter()
        .find(|r| r.run == "pay-o1" && matches!(r.record, Record::TimerStarted { .. }))
        .unwrap();
    assert_eq!(restarted.to_json()["delay"], 71 * HOUR);

    // Version 2 is deployed. Without the migration its old snapshots are refused, and
    // nothing is written.
    clock.set(24 * HOUR);
    let before = sink.records().len();
    let refused = v2::order::resume_with(
        &config,
        &*app,
        store.load("o1"),
        Some(v2::order::Event::Delivered),
    )
    .await
    .unwrap_err();
    assert_eq!(refused.reason(), "ir");
    assert_eq!(sink.records().len(), before);

    // Request 3, version 2, a day after creation: deliver o1, migrating its snapshot.
    let config = config.migrate(migrate);
    let delivered = v2::order::resume_with(
        &config.clone().run_id("deliver-o1"),
        &*app,
        store.load("o1"),
        Some(v2::order::Event::Delivered),
    )
    .await
    .unwrap();
    assert_eq!(delivered.status, Status::Done);
    assert_eq!(delivered.result, Some(RunResult::Output(Value::Null)));
    assert_eq!(delivered.vars["tracking"], "t-1");
    assert_eq!(delivered.ir, v2::order::HASH);
    store.save("o1", &delivered);
    let restored = sink
        .records()
        .into_iter()
        .find(|r| r.run == "deliver-o1")
        .unwrap();
    match restored.record {
        Record::Restored {
            migrated, snapshot, ..
        } => {
            assert_eq!(migrated.as_deref(), Some(v1::order::HASH));
            assert_eq!(snapshot.vars["tracking"], "t-1");
        }
        other => panic!("the first record is {other:?}"),
    }
    // The run ended, so it wrote `done` and no `suspended`.
    assert_eq!(kinds(&sink, "deliver-o1").last().unwrap(), "done");

    // A finished order is not restored again.
    let ended = v2::order::resume_with(&config, &*app, store.load("o1"), None)
        .await
        .unwrap_err();
    assert_eq!(ended.reason(), "ended");

    // Request 4, version 2, four days after creation: a late payment for o2. Its deadline
    // passed while it was stored, so it expires first and the payment is refunded.
    clock.set(96 * HOUR);
    let late = v2::order::resume_with(
        &config.clone().run_id("pay-o2"),
        &*app,
        store.load("o2"),
        Some(v2::order::Event::PaymentReceived { amount: 42 }),
    )
    .await
    .unwrap();
    assert_eq!(late.active, ["Order", "Order.Expired"]);
    assert_eq!(
        late.vars,
        json!({"paid": 0, "tracking": ""})
            .as_object()
            .unwrap()
            .clone()
    );
    store.save("o2", &late);
    assert_eq!(*app.refunds.lock().unwrap(), [("o2".to_owned(), 42)]);
    assert_eq!(*app.shipped.lock().unwrap(), ["o1"]);
    let late_kinds = kinds(&sink, "pay-o2");
    assert_eq!(
        late_kinds[..3],
        ["restored", "event", "transition"],
        "the expiry is taken first"
    );
    assert_eq!(late_kinds.last().unwrap(), "suspended");

    // A snapshot saved by version 2 is restored by version 2 without the migration.
    let again = v2::order::resume_with(
        &Config::new().timer(Arc::new(clock.clone())),
        &*app,
        store.load("o2"),
        None,
    )
    .await
    .unwrap();
    assert_eq!(again.active, late.active);
}

#[tokio::test]
async fn an_actor_is_saved_restored_and_suspended() {
    let app = Arc::new(Shop::default());
    let config = Config::new().timer(Arc::new(Clock::default()));
    let first = v2::counter::spawn_with(config.clone(), app.clone(), 1);
    first.send(v2::counter::Event::Add { by: 2 }).await;
    let mut saved = first.save().await;
    for _ in 0..1000 {
        if saved.vars["n"] == 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
        saved = first.save().await;
    }
    assert_eq!(saved.vars["n"], 3);

    // A copy goes on from the snapshot, separately.
    let copy = v2::counter::restore_with(config.clone(), app.clone(), saved.clone()).unwrap();
    assert_ne!(copy.id(), first.id());
    assert_eq!(copy.state(), v2::counter::State::Counting);
    copy.send(v2::counter::Event::Add { by: 10 }).await;
    copy.send(v2::counter::Event::Finish).await;
    assert_eq!(copy.result().await, Ok(13));

    // The original is suspended once idle and keeps its own count.
    let last = first.suspend().await;
    assert_eq!(last.status, Status::Running);
    assert_eq!(last.vars["n"], 3);

    // A snapshot of another machine is refused.
    let wrong = v2::reminder::restore_with(config, app, last).unwrap_err();
    assert_eq!(wrong.reason(), "machine");
}

struct Jobs;

impl par::jobs::Handlers for Jobs {
    async fn upload(&self) -> i64 {
        std::future::pending().await
    }

    async fn download(&self) -> i64 {
        std::future::pending().await
    }
}

#[tokio::test]
async fn a_parallel_run_is_in_its_parallel_state_and_restores_with_every_region() {
    let sink = Arc::new(MemorySink::new());
    let config = Config::new()
        .sink(sink.clone())
        .timer(Arc::new(Clock::default()));
    let jobs = par::jobs::spawn_with(config.clone().run_id("j1"), Arc::new(Jobs));
    assert_eq!(jobs.state(), par::jobs::State::Busy);
    let saved = jobs.save().await;
    assert_eq!(
        saved.active,
        [
            "Jobs",
            "Jobs.Busy",
            "Jobs.Busy.Up",
            "Jobs.Busy.Up.Uploading",
            "Jobs.Busy.Down",
            "Jobs.Busy.Down.Downloading"
        ]
    );
    // Work starts in document order: the parallel state's timer, then each region's.
    let timers: Vec<(u64, &str)> = saved
        .timers
        .iter()
        .map(|t| (t.id, t.state.as_str()))
        .collect();
    assert_eq!(
        timers,
        [
            (1, "Jobs.Busy"),
            (2, "Jobs.Busy.Up.Uploading"),
            (3, "Jobs.Busy.Down.Downloading")
        ]
    );
    assert_eq!(saved.invokes.len(), 2);

    let copy = par::jobs::restore_with(config.run_id("j2"), Arc::new(Jobs), saved).unwrap();
    assert_eq!(copy.state(), par::jobs::State::Busy);
    copy.send(par::jobs::Event::Stop).await;
    let mut copy = copy;
    assert_eq!(copy.changed().await, par::jobs::State::Stopped);
    assert_eq!(
        kinds(&sink, "j2")[..6],
        [
            "restored",
            "timer.started",
            "invoke.started",
            "timer.started",
            "invoke.started",
            "timer.started"
        ]
    );
    jobs.cancel().await;
}

/// Host code holding a run's handle from `Config::on_run` can ask it to suspend. A spawned
/// run stops once idle, and its actor's `result` says so with the saved snapshot; a run of
/// `run_with` goes on to its end, ignoring the request, since its caller gets an output.
#[tokio::test]
async fn suspending_through_the_handle_from_on_run() {
    let app = Arc::new(Shop::default());
    let handles: Arc<Mutex<Vec<roadbook::RunHandle>>> = Arc::default();
    let seen = handles.clone();
    let sink = Arc::new(MemorySink::new());
    let config = Config::new()
        .sink(sink.clone())
        .timer(Arc::new(Clock::default()))
        .on_run(move |_, handle| seen.lock().unwrap().push(handle));

    let actor = v2::counter::spawn_with(config.clone().run_id("a1"), app.clone(), 1);
    actor.send(v2::counter::Event::Add { by: 2 }).await;
    let handle = loop {
        if let Some(h) = handles.lock().unwrap().pop() {
            break h;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    };
    handle.suspend();
    match actor.result().await {
        Err(roadbook::Stopped::Suspended(saved)) => {
            assert_eq!(saved.status, Status::Running);
            assert_eq!(saved.vars["n"], 3);
        }
        other => panic!("not suspended: {other:?}"),
    }
    assert_eq!(
        kinds(&sink, "a1").last().map(String::as_str),
        Some("suspended")
    );

    let finish = async {
        let handle = loop {
            if let Some(h) = handles.lock().unwrap().pop() {
                break h;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        };
        handle.suspend();
        tokio::time::sleep(Duration::from_millis(5)).await;
        handle.send(json!({"_tag": "Finish"}));
    };
    let config = config.run_id("w1");
    let (out, ()) = tokio::join!(v2::counter::run_with(&config, &*app, 5), finish);
    assert_eq!(out, 5);
    assert!(!kinds(&sink, "w1").iter().any(|k| k == "suspended"));
}

struct Writer;

impl hist::editor::Handlers for Writer {
    async fn log(&self, _what: &str) {}

    async fn fetch(&self) {
        std::future::pending().await
    }
}

#[tokio::test]
async fn a_run_returns_through_its_history_states_and_saves_what_they_recorded() {
    use hist::editor::{Event, Review, State, Work};
    let config = Config::new().timer(Arc::new(Clock::default()));
    async fn until<O, E>(run: &mut roadbook::Actor<Event, State, O, E>, want: State) {
        while run.state() != want {
            run.changed().await;
        }
    }
    let review = State::Work(Work::Review(Review::Read));
    let mut first = hist::editor::spawn_with(config.clone(), Arc::new(Writer));
    first.send(Event::Next).await;
    until(&mut first, review).await;
    first.send(Event::Help).await;
    until(&mut first, State::Help).await;
    let saved = first.save().await;
    assert_eq!(saved.snapshot_version, 3);
    assert_eq!(
        saved.history,
        Some(
            [
                (
                    "Editor.Work.Deep".into(),
                    vec!["Editor.Work.Review.Read".into()]
                ),
                (
                    "Editor.Work.Shallow".into(),
                    vec!["Editor.Work.Review".into()]
                ),
            ]
            .into()
        )
    );
    first.send(Event::Back).await;
    until(&mut first, review).await;
    first.cancel().await;

    // Restored, it goes back to where the history states say.
    let mut copy =
        hist::editor::restore_with(config.clone(), Arc::new(Writer), saved.clone()).unwrap();
    assert_eq!(copy.state(), State::Help);
    copy.send(Event::BackDeep).await;
    until(&mut copy, review).await;
    copy.cancel().await;

    // A version 1 snapshot, written before history states (and keyed spawn), restores with no
    // records and is saved again as version 3.
    let mut old = saved.to_value();
    old["snapshotVersion"] = json!(1);
    old.as_object_mut().unwrap().remove("history");
    old.as_object_mut().unwrap().remove("spawned");
    let old = Saved::from_value(old).unwrap();
    let mut copy = hist::editor::restore_with(config, Arc::new(Writer), old).unwrap();
    let again = copy.save().await;
    assert_eq!(again.snapshot_version, 3);
    assert_eq!(again.history, Some(Default::default()));
    copy.send(Event::Back).await;
    until(&mut copy, State::Work(Work::Draft)).await;
    copy.cancel().await;
}
