//! `roadbook::include!` on a chart of awkward names and shapes: Rust keywords, aliases, an
//! extern type, nested state enums that need other names, and colliding error enum names.

#![deny(warnings)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

mod edge {
    /// The chart's `extern type Blob`, defined next to the call.
    #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
    pub struct Blob(pub Vec<u8>);

    roadbook::include!("tests/charts/edge.road");
}

use edge::*;

#[derive(Default)]
struct Host {
    broken: bool,
    probe_delay: Duration,
    calls: Mutex<Vec<String>>,
}

impl Host {
    fn log(&self, s: String) {
        self.calls.lock().unwrap().push(s);
    }
}

impl nested::Handlers for Host {
    fn pick(
        &self,
        kind: Kind,
        count: Count,
        id: &Id,
        maybe: Option<&Id>,
        snapshot: &Snapshot,
        many: &[Snapshot],
    ) -> Pair {
        self.log(format!(
            "pick {kind:?} {count} {id} {maybe:?} {} {}",
            snapshot.r#move,
            many.len()
        ));
        Pair {
            kind,
            self_: count,
            ..snapshot.clone()
        }
    }
    fn empty_delays(&self) -> Delays {
        vec![Duration::from_millis(3)]
    }
    fn none(&self) -> Vec<Snapshot> {
        Vec::new()
    }
    fn no_kind(&self) -> Option<Kind> {
        None
    }
    async fn tick(&self) {
        self.log("tick".into());
    }
    async fn note(&self, self_: &str, r#async: Option<Kind>) {
        self.log(format!("note {self_} {async:?}", async = r#async));
    }
    async fn probe(&self, blob: &Blob) -> Result<(), ProbeTaskError> {
        tokio::time::sleep(self.probe_delay).await;
        self.log(format!("probe {:?}", blob.0));
        if self.broken {
            Err(Broken {
                r#match: "fuse".into(),
            })?;
        }
        Ok(())
    }
    async fn fire(&self) {
        self.log("fire".into());
    }
}

impl checkout::Handlers for Host {
    async fn checkout(&self, id: &Id) -> Result<Count, CheckoutTaskError> {
        if self.broken {
            Err(Gone)?;
        }
        Ok(id.len() as Count)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn keywords_aliases_and_extern_types() {
    let host = Host::default();
    nested::run(&host, Blob(vec![1, 2])).await;
    assert_eq!(
        *host.calls.lock().unwrap(),
        [
            "tick",
            "probe [1, 2]",
            "fire",
            "pick Large 2 id Some(\"maybe\") m 0"
        ]
    );

    let host = Host {
        broken: true,
        ..Host::default()
    };
    nested::run(&host, Blob(vec![])).await;
    assert_eq!(
        *host.calls.lock().unwrap(),
        ["tick", "probe []", "note fuse None"]
    );
}

#[test]
fn json_of_keyword_fields() {
    let pair = Pair {
        r#move: "m".into(),
        self_: 1,
        blob: Blob(vec![7]),
        delays: vec![Duration::from_millis(5)],
        kind: Kind::Small,
    };
    let json = serde_json::json!({
        "move": "m", "self": 1, "blob": [7], "delays": [5], "kind": "Small"
    });
    assert_eq!(serde_json::to_value(&pair).unwrap(), json);
    assert_eq!(serde_json::from_value::<Pair>(json).unwrap(), pair);
    let snapshot: Snapshot = pair;
    let _ = snapshot;
    assert_eq!(
        serde_json::to_value(Broken {
            r#match: "x".into()
        })
        .unwrap(),
        serde_json::json!({ "_tag": "Broken", "match": "x" })
    );
    let _: ProbeError = ProbeError {
        why: "a type".into(),
    };
}

#[tokio::test(flavor = "multi_thread")]
async fn colliding_error_enums() {
    let host = Host::default();
    assert_eq!(checkout::run(&host).await, Ok(1));
    let host = Host {
        broken: true,
        ..Host::default()
    };
    assert_eq!(
        checkout::run(&host).await,
        Err(CheckoutMachineError::Gone(Gone))
    );
    let _ = CheckoutTaskError::from(Gone);
}

#[tokio::test(flavor = "multi_thread")]
async fn nested_state_enums() {
    let host = Arc::new(Host {
        probe_delay: Duration::from_secs(60),
        ..Host::default()
    });
    let actor = nested::spawn(host, Blob(vec![]));
    assert_eq!(
        actor.state(),
        nested::State::Outer(nested::Outer::State(nested::OuterState::Event(
            nested::StateEvent::Kind
        )))
    );
    actor.cancel().await;
    assert_eq!(actor.result().await, Err(roadbook::Stopped::Cancelled));
}

impl probing::Handlers for Host {
    async fn probe(&self, blob: &Blob) -> Result<(), ProbeTaskError> {
        <Host as nested::Handlers>::probe(self, blob).await
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_machine_that_returns_nothing_and_fails() {
    let host = Host::default();
    assert_eq!(probing::run(&host, Blob(vec![])).await, Ok(()));
    let host = Host {
        broken: true,
        ..Host::default()
    };
    let r = probing::run(&host, Blob(vec![])).await;
    assert_eq!(
        r,
        Err(ProbingError::Broken(Broken {
            r#match: "fuse".into()
        }))
    );
    assert_eq!(probing::State::Ok, probing::State::Ok);
}
