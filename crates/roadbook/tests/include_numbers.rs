//! Numbers across the generated code: a non-finite `float` from host code is a host defect,
//! not `null` (in an event sent from host code, the sender's); `int` and `duration` read any integral JSON number; an error with fields checks
//! its `_tag`; an output that does not decode is a `Defect`; and dropping `result()`'s future
//! stops the run.

#![deny(warnings)]

use std::any::Any;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use roadbook::{Defect, MemorySink, Record};
use serde_json::json;

mod numbers {
    /// The chart's `extern type Opaque`: it encodes, but refuses to be read back.
    #[derive(Debug, Clone, PartialEq)]
    pub struct Opaque;

    impl serde::Serialize for Opaque {
        fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            s.serialize_str("opaque")
        }
    }

    impl<'de> serde::Deserialize<'de> for Opaque {
        fn deserialize<D: serde::Deserializer<'de>>(_: D) -> Result<Self, D::Error> {
            Err(serde::de::Error::custom("an Opaque is never read back"))
        }
    }

    roadbook::include!("tests/charts/numbers.road");
}

use numbers::*;

#[derive(Default)]
struct Host {
    /// What `ratio` and `measure` give.
    value: f64,
    /// Set when `forever`'s future is dropped.
    dropped: Arc<AtomicBool>,
}

struct SetOnDrop(Arc<AtomicBool>);

impl Drop for SetOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

impl ratio::Handlers for Host {
    fn ratio(&self, _n: i64) -> f64 {
        self.value
    }
}

impl measure::Handlers for Host {
    async fn measure(&self, n: i64) -> Result<Reading, MeasureTaskError> {
        Ok(Reading {
            value: self.value,
            count: n,
            wait: Duration::from_millis(1),
        })
    }
}

impl scale::Handlers for Host {}

impl hidden::Handlers for Host {
    fn opaque(&self) -> Opaque {
        Opaque
    }
}

impl forever::Handlers for Host {
    async fn forever(&self) {
        let _guard = SetOnDrop(self.dropped.clone());
        std::future::pending::<()>().await
    }
}

fn host(value: f64) -> Arc<Host> {
    Arc::new(Host {
        value,
        ..Host::default()
    })
}

/// The `Defect` a task panicked with.
fn defect_of(e: tokio::task::JoinError) -> Defect {
    let p: Box<dyn Any + Send> = e.into_panic();
    match p.downcast::<Defect>() {
        Ok(d) => *d,
        Err(p) => panic!(
            "the payload is not a Defect: {:?}",
            p.downcast_ref::<String>()
        ),
    }
}

fn not_finite(d: &Defect) -> bool {
    matches!(d, Defect::Host { message } if message.contains("is not a finite float"))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_non_finite_function_result_is_a_host_defect() {
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let h = host(value);
        let sink = Arc::new(MemorySink::new());
        let config = roadbook::Config::new().sink(sink.clone());
        let e = tokio::spawn(async move { ratio::run_with(&config, &*h, 1).await })
            .await
            .unwrap_err();
        let d = defect_of(e);
        assert!(not_finite(&d), "{d:?}");
        let last = sink.records().last().unwrap().record.clone();
        assert_eq!(last, Record::Defect { defect: d });
    }
    // A finite one is fine.
    assert_eq!(ratio::run(&*host(0.5), 1).await, 0.5);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_non_finite_float_in_a_task_result_is_a_host_defect() {
    let h = host(f64::NAN);
    let e = tokio::spawn(async move { measure::run(&*h, 1).await })
        .await
        .unwrap_err();
    assert!(not_finite(&defect_of(e)));
    assert_eq!(measure::run(&*host(2.5), 1).await, Ok(2.5));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_non_finite_machine_argument_is_a_host_defect() {
    let h = host(0.0);
    let e = tokio::spawn(async move { scale::run(&*h, f64::INFINITY).await })
        .await
        .unwrap_err();
    assert!(not_finite(&defect_of(e)));
    // Through spawn, the actor's run ended with it before it started.
    let actor = scale::spawn(host(0.0), f64::NAN);
    let e = tokio::spawn(actor.result()).await.unwrap_err();
    assert!(not_finite(&defect_of(e)));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_non_finite_float_in_an_event_is_the_senders_host_defect() {
    let sink = Arc::new(MemorySink::new());
    let config = roadbook::Config::new().sink(sink.clone());
    let actor = Arc::new(scale::spawn_with(config, host(0.0), 1.0));
    // `send` panics in the sender with the defect; nothing reaches the run.
    let sender = actor.clone();
    let e = tokio::spawn(async move { sender.send(scale::Event::Set { value: f64::NAN }).await })
        .await
        .unwrap_err();
    assert!(not_finite(&defect_of(e)));
    let handle = actor.handle();
    let e = tokio::spawn(async move {
        handle
            .send(scale::Event::Set {
                value: f64::INFINITY,
            })
            .await
    })
    .await
    .unwrap_err();
    assert!(not_finite(&defect_of(e)));
    // The run goes on and takes a finite one.
    actor.send(scale::Event::Set { value: 4.0 }).await;
    let actor = Arc::into_inner(actor).unwrap();
    assert_eq!(actor.result().await, Ok(4.0));
    assert!(
        !sink
            .records()
            .iter()
            .any(|r| matches!(r.record, Record::Defect { .. }))
    );
}

#[test]
fn a_non_finite_float_does_not_encode() {
    let r = Reading {
        value: f64::NAN,
        count: 1,
        wait: Duration::ZERO,
    };
    assert!(serde_json::to_value(&r).is_err());
}

#[test]
fn ints_and_durations_read_any_integral_number() {
    let r: Reading = serde_json::from_str(r#"{"value": 2, "count": 2.0, "wait": 3e0}"#).unwrap();
    assert_eq!(
        r,
        Reading {
            value: 2.0,
            count: 2,
            wait: Duration::from_millis(3)
        }
    );
    assert_eq!(
        serde_json::to_value(&r).unwrap(),
        json!({"value": 2.0, "count": 2, "wait": 3})
    );
    for bad in [
        r#"{"value": 2, "count": 2.5, "wait": 3}"#,
        r#"{"value": 2, "count": 2, "wait": 3.5}"#,
        r#"{"value": 2, "count": 2, "wait": -1}"#,
        r#"{"value": 2, "count": 1e300, "wait": 3}"#,
        r#"{"value": 2, "count": "2", "wait": 3}"#,
    ] {
        assert!(serde_json::from_str::<Reading>(bad).is_err(), "{bad}");
    }
}

#[test]
fn an_error_with_fields_checks_its_tag() {
    let bad: Bad = serde_json::from_value(json!({"_tag": "Bad", "why": "x"})).unwrap();
    assert_eq!(bad, Bad { why: "x".into() });
    assert_eq!(
        serde_json::to_value(&bad).unwrap(),
        json!({"_tag": "Bad", "why": "x"})
    );
    for v in [json!({"_tag": "Other", "why": "x"}), json!({"why": "x"})] {
        assert!(serde_json::from_value::<Bad>(v.clone()).is_err(), "{v}");
    }
    let e: MeasureTaskError = serde_json::from_value(json!({"_tag": "Bad", "why": "y"})).unwrap();
    assert_eq!(e, MeasureTaskError::Bad(Bad { why: "y".into() }));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_output_that_does_not_decode_is_a_host_defect() {
    let e = tokio::spawn(async { hidden::run(&*host(0.0)).await })
        .await
        .unwrap_err();
    let d = defect_of(e);
    assert!(
        matches!(&d, Defect::Host { message } if message.contains("never read back")),
        "{d:?}"
    );
    let actor = hidden::spawn(host(0.0));
    let e = tokio::spawn(actor.result()).await.unwrap_err();
    assert_eq!(defect_of(e), d);
}

#[tokio::test(flavor = "multi_thread")]
async fn dropping_the_result_future_stops_the_run() {
    let h = host(0.0);
    let dropped = h.dropped.clone();
    let actor = forever::spawn(h);
    let waited = tokio::time::timeout(Duration::from_millis(20), actor.result()).await;
    assert!(waited.is_err());
    // The run's task was aborted, which drops the task's future.
    for _ in 0..100 {
        if dropped.load(Ordering::SeqCst) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the run is still going after its result future was dropped");
}
