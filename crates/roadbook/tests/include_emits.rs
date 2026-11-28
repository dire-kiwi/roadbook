//! Tasks that emit through `roadbook::include!` (`spec/execution.md#tasks-that-emit`), with
//! real host implementations: an upload that emits as it goes, a stream task that ticks on a
//! timer until its state is left, and an emit that cannot be encoded.

#![deny(warnings)]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use roadbook::{BoxFuture, Config, Defect, Emitter, MemorySink, Timer, TraceRecord};
use serde_json::{Value, json};

mod rt {
    roadbook::include!("tests/charts/emits.road");
}

use rt::{MeasureEmitted, TicksTaskEmitted, UploadEmitted};

/// Virtual milliseconds on Tokio's paused clock.
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
        .run_id("u")
}

/// Sets its flag when it is dropped: when the stream task's future is.
struct Dropped(Arc<AtomicBool>);

impl Drop for Dropped {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[derive(Default)]
struct Host {
    shown: Mutex<Vec<i64>>,
    ticked: AtomicUsize,
    dropped: Arc<AtomicBool>,
    /// The upload's emitter, kept after the upload ended.
    kept: Mutex<Option<Emitter<UploadEmitted>>>,
}

impl rt::uploader::Handlers for Host {
    async fn shown(&self, percent: i64) {
        self.shown.lock().unwrap().push(percent);
    }

    async fn upload(&self, size: i64, emit: Emitter<UploadEmitted>) -> i64 {
        emit.emit(UploadEmitted::Stage {
            name: "hashing".into(),
        });
        emit.emit(UploadEmitted::Progress { percent: 50 });
        tokio::time::sleep(Duration::from_millis(5)).await;
        // From another thread: an emitter is `Send + 'static`.
        let other = emit.clone();
        std::thread::spawn(move || other.emit(UploadEmitted::Progress { percent: 100 }))
            .join()
            .unwrap();
        *self.kept.lock().unwrap() = Some(emit);
        size
    }

    async fn ticks(&self, every: Duration, emit: Emitter<TicksTaskEmitted>) {
        let _dropped = Dropped(self.dropped.clone());
        let mut n = 0;
        loop {
            tokio::time::sleep(every).await;
            n += 1;
            self.ticked.fetch_add(1, Ordering::SeqCst);
            emit.emit(TicksTaskEmitted::Tick { n });
        }
    }
}

/// The values of the `task.emitted` inputs the run took, as their records show them.
fn taken(records: &[TraceRecord]) -> Vec<Value> {
    records
        .iter()
        .map(TraceRecord::to_json)
        .filter(|j| j["event"]["kind"] == "task.emitted")
        .map(|j| json!([j["kind"], j["event"]["invoke"], j["event"]["value"]]))
        .collect()
}

#[tokio::test(start_paused = true)]
async fn a_tasks_emits_reach_the_machine_and_a_stream_task_is_dropped_when_its_state_is_left() {
    let sink = Arc::new(MemorySink::new());
    let host = Arc::new(Host::default());
    let actor = rt::uploader::spawn_with(config(&sink), host.clone(), 10);
    // The upload ends after 5ms; three ticks come 10ms apart after that.
    tokio::time::sleep(Duration::from_millis(37)).await;
    assert_eq!(actor.state(), rt::uploader::State::Waiting);
    assert!(!host.dropped.load(Ordering::SeqCst));
    // A stream task is work in flight: the run is not idle, and keeps ticking.
    actor.send(rt::uploader::Event::Stop).await;
    let count = actor.result().await.unwrap();
    assert_eq!(count, 3);
    assert!(
        host.dropped.load(Ordering::SeqCst),
        "leaving the state dropped the stream task's future"
    );
    let ticked = host.ticked.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(host.ticked.load(Ordering::SeqCst), ticked);
    assert_eq!(*host.shown.lock().unwrap(), [50, 100]);
    // `Stage`, which nothing hears, gave no input; the Progress made from another thread
    // came before the outcome.
    let records = sink.records();
    assert_eq!(
        taken(&records),
        [
            json!(["event", 1, {"_tag": "Progress", "percent": 50}]),
            json!(["event", 1, {"_tag": "Progress", "percent": 100}]),
            json!(["event", 2, {"_tag": "Tick", "n": 1}]),
            json!(["event", 2, {"_tag": "Tick", "n": 2}]),
            json!(["event", 2, {"_tag": "Tick", "n": 3}]),
        ]
    );
    // The upload's emitter, kept after its outcome was queued, is closed.
    let kept = host.kept.lock().unwrap().take().unwrap();
    assert!(!kept.is_open());
    kept.emit(UploadEmitted::Progress { percent: 1 });
    assert_eq!(sink.records().len(), records.len());
}

struct Meter;

impl rt::meter::Handlers for Meter {
    async fn measure(&self, emit: Emitter<MeasureEmitted>) -> f64 {
        emit.emit(MeasureEmitted::Reading { value: 1.5 });
        tokio::task::yield_now().await;
        emit.emit(MeasureEmitted::Reading { value: f64::NAN });
        2.5
    }
}

#[tokio::test(start_paused = true)]
async fn an_emit_that_does_not_encode_is_the_tasks_defect() {
    let sink = Arc::new(MemorySink::new());
    let actor = rt::meter::spawn_with(config(&sink), Arc::new(Meter));
    // A run that ends with a defect resumes unwinding with it.
    let panic = tokio::spawn(actor.result())
        .await
        .expect_err("the run ended with a defect")
        .into_panic();
    let Ok(defect) = panic.downcast::<Defect>() else {
        panic!("not a defect payload");
    };
    let Defect::Host { message } = *defect else {
        panic!("not a host defect: {defect:?}");
    };
    assert!(message.contains("not a finite float"), "{message}");
    let records: Vec<Value> = sink.records().iter().map(TraceRecord::to_json).collect();
    // The first reading was taken; the second made the task's outcome a defect.
    assert_eq!(
        taken(&sink.records()),
        [json!(["event", 1, {"_tag": "Reading", "value": 1.5}])]
    );
    let defect = records
        .iter()
        .find(|j| j["event"]["kind"] == "invoke.defect")
        .expect("the invoke's defect was taken");
    assert_eq!(defect["event"]["invoke"], 1);
}

#[test]
fn the_emitted_enums_are_tagged_and_named_like_error_enums() {
    // `TicksEmitted` is a type of the chart, so the task's enum is `TicksTaskEmitted`.
    let _: rt::TicksEmitted = rt::TicksEmitted { n: 1 };
    assert_eq!(
        serde_json::to_value(UploadEmitted::Progress { percent: 5 }).unwrap(),
        json!({"_tag": "Progress", "percent": 5})
    );
    assert_eq!(
        serde_json::to_value(TicksTaskEmitted::Tick { n: 2 }).unwrap(),
        json!({"_tag": "Tick", "n": 2})
    );
}
