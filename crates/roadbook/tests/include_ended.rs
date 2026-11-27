//! The built-in enum `Ended` through `roadbook::include!`: the chart names it, so the module
//! has it as an ordinary enum, which an action, a function, a parameter and an output take,
//! and which a holder's `on peer.ended` gives the host through `event.status`.

#![deny(warnings)]

use std::sync::{Arc, Mutex};

mod ended {
    roadbook::include!("tests/charts/ended.road");
}

use ended::*;

#[derive(Default)]
struct Host {
    notes: Mutex<Vec<Ended>>,
}

impl echo::Handlers for Host {
    async fn note(&self, status: Ended) {
        self.notes.lock().unwrap().push(status);
    }
}

impl watcher::Handlers for Host {
    async fn note(&self, status: Ended) {
        self.notes.lock().unwrap().push(status);
    }

    fn clean(&self, status: Ended) -> bool {
        matches!(status, Ended::Done | Ended::Suspended)
    }
}

impl peer::Handlers for Host {}

#[test]
fn the_variants_are_their_names_in_json() {
    let all = [
        Ended::Done,
        Ended::Failed,
        Ended::Cancelled,
        Ended::Defect,
        Ended::Suspended,
    ];
    let json: Vec<serde_json::Value> = all
        .iter()
        .map(|v| serde_json::to_value(v).unwrap())
        .collect();
    assert_eq!(
        json,
        ["Done", "Failed", "Cancelled", "Defect", "Suspended"].map(serde_json::Value::from)
    );
    for (v, j) in all.iter().zip(json) {
        assert_eq!(serde_json::from_value::<Ended>(j).unwrap(), *v);
    }
    assert!(serde_json::from_value::<Ended>(serde_json::json!("Stopped")).is_err());
}

#[tokio::test]
async fn a_parameter_and_an_output() {
    let host = Host::default();
    assert_eq!(echo::run(&host, Ended::Defect).await, Ended::Defect);
    assert_eq!(*host.notes.lock().unwrap(), [Ended::Defect]);
}

#[tokio::test]
async fn event_status_reaches_the_host() {
    for (event, status) in [
        (peer::Event::Stop, Ended::Done),
        (peer::Event::Fail, Ended::Failed),
    ] {
        let host = Arc::new(Host::default());
        let peer = peer::spawn(host.clone());
        let watcher = watcher::spawn(host.clone(), peer.handle());
        peer.handle().send(event).await;
        let _ = peer.result().await;
        assert_eq!(watcher.result().await, Ok(status));
        assert_eq!(*host.notes.lock().unwrap(), [status]);
    }
}
