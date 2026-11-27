//! A peer's end (`on name.ended`) with a real executor: what the conformance cases, which run
//! one step at a time, do not reach. A holder that subscribes while its peer closes, a run
//! held under several names, and a hard stop.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use roadbook::dynamic::{self, Host, Invocation, Outcome};
use roadbook::{BoxFuture, Config, Defect, MemorySink, Program, RunEnd, RunHandle};
use roadbook_core::Object;
use serde_json::{Value, json};

const CHART: &str = r#"
machine Peer() {
  event Close
  initial Open
  state Open { on Close -> Over }
  final state Over {}
}

/// Counts the ends it takes, under any name, and never ends by itself.
machine Holder(a: ref Peer, b: ref Peer, maybe: ref Peer?, all: ref Peer[]) {
  var n: int = 0
  initial Watching
  state Watching {
    on a.ended     set n = n + 1
    on b.ended     set n = n + 1
    on maybe.ended set n = n + 1
    on all.ended   set n = n + 1
  }
}

/// Ends once its peer has, by how it ended.
machine Waiter(p: ref Peer) -> Ended {
  var how: Ended = Ended.Done
  initial Waiting
  state Waiting { on p.ended -> Over  set how = event.status }
  final state Over { output how }
}
"#;

fn program() -> Program {
    let c = roadbook_compiler::compile_source("ended.road", CHART, &Default::default());
    assert!(!c.has_errors(), "{}", c.render());
    Program::new(c.ir.unwrap()).unwrap()
}

/// No host code at all.
struct Nothing;

impl Host for Nothing {
    fn function(&self, name: &str, _: Vec<Value>) -> Result<Value, Defect> {
        Err(Defect::host(format!("no function {name}")))
    }

    fn action<'a>(&'a self, name: &str, _: Object) -> BoxFuture<'a, Result<(), Defect>> {
        let name = name.to_owned();
        Box::pin(async move { Err(Defect::host(format!("no action {name}"))) })
    }

    fn invoke<'a>(&'a self, _: Invocation) -> BoxFuture<'a, Outcome> {
        Box::pin(std::future::pending())
    }
}

fn object(v: Value) -> Object {
    v.as_object().unwrap().clone()
}

fn config(sink: &Arc<MemorySink>, id: &str) -> Config {
    Config::new().sink(sink.clone()).run_id(id)
}

/// The `ended` inputs run `run` took or dropped, as the trace shows them.
fn ends_of(sink: &MemorySink, run: &str) -> Vec<Value> {
    sink.records()
        .iter()
        .map(|r| r.to_json())
        .filter(|r| r["run"] == run && r["event"]["kind"] == "ended")
        .map(|r| r["event"].clone())
        .collect()
}

/// A run held under every name a holder has, twice in a list among them, is one subscription:
/// its end is one input.
#[tokio::test]
async fn a_run_held_under_several_names_is_told_once() {
    let program = program();
    let sink = Arc::new(MemorySink::new());
    let (peer_fut, peer) = dynamic::start(
        &config(&sink, "r1"),
        &program,
        "Peer",
        Object::new(),
        &[],
        &Nothing,
    );
    let input = object(json!({"a": "r1", "b": "r1", "maybe": "r1", "all": ["r1", "r1"]}));
    let (holder_fut, holder) = dynamic::start(
        &config(&sink, "r2"),
        &program,
        "Holder",
        input,
        std::slice::from_ref(&peer),
        &Nothing,
    );
    let script = async {
        tokio::task::yield_now().await;
        peer.send(json!({"_tag": "Close"}));
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        holder.suspend();
    };
    let (peer_end, holder_end, ()) = tokio::join!(peer_fut, holder_fut, script);
    assert_eq!(peer_end, RunEnd::Done(Value::Null));
    let RunEnd::Suspended(saved) = holder_end else {
        panic!("{holder_end:?}");
    };
    assert_eq!(saved.vars["n"], json!(1));
    assert_eq!(
        ends_of(&sink, "r2"),
        [json!({"kind": "ended", "from": "r1", "status": "Done"})]
    );
}

/// Dropping a root run's future is a hard stop, which tells its holders `Cancelled`.
#[tokio::test]
async fn a_hard_stop_tells_the_holders_cancelled() {
    let program = program();
    let sink = Arc::new(MemorySink::new());
    let (mut peer_fut, peer) = dynamic::start(
        &config(&sink, "r1"),
        &program,
        "Peer",
        Object::new(),
        &[],
        &Nothing,
    );
    let (waiter_fut, _) = dynamic::start(
        &config(&sink, "r2"),
        &program,
        "Waiter",
        object(json!({"p": "r1"})),
        std::slice::from_ref(&peer),
        &Nothing,
    );
    let stopped = tokio::time::timeout(std::time::Duration::from_millis(10), &mut peer_fut).await;
    assert!(stopped.is_err(), "the peer ended by itself");
    drop(peer_fut);
    assert_eq!(waiter_fut.await, RunEnd::Done(json!("Cancelled")));
    // A holder that starts after the hard stop is told too, by its own driver.
    let (late_fut, _) = dynamic::start(
        &config(&sink, "r3"),
        &program,
        "Waiter",
        object(json!({"p": "r1"})),
        std::slice::from_ref(&peer),
        &Nothing,
    );
    assert_eq!(late_fut.await, RunEnd::Done(json!("Cancelled")));
}

/// A holder subscribes while its peer closes, on another thread. Checking whether the peer has
/// closed and subscribing are one operation with respect to the peer telling its end, so the
/// holder is told exactly once: by the peer's driver, or by its own right after its start
/// step. Never neither, never both.
#[test]
fn a_holder_that_subscribes_while_its_peer_closes_is_told_once() {
    let program = program();
    let (mut before, mut after) = (0, 0);
    for round in 0..2000_u64 {
        let sink = Arc::new(MemorySink::new());
        let (peer_fut, peer) = dynamic::start(
            &config(&sink, "r1"),
            &program,
            "Peer",
            Object::new(),
            &[],
            &Nothing,
        );
        peer.send(json!({"_tag": "Close"}));
        let closed_first = AtomicU64::new(0);
        let input = object(json!({"a": "r1", "b": "r1", "maybe": null, "all": []}));
        let (holder_fut, holder): (_, RunHandle) = std::thread::scope(|s| {
            let peer_thread = s.spawn(|| {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .build()
                    .unwrap();
                rt.block_on(peer_fut)
            });
            for _ in 0..(round * 97) % 40_000 {
                std::hint::spin_loop();
            }
            if peer.is_finished() {
                closed_first.fetch_add(1, Ordering::SeqCst);
            }
            let started = dynamic::start(
                &config(&sink, "r2"),
                &program,
                "Holder",
                input,
                std::slice::from_ref(&peer),
                &Nothing,
            );
            assert_eq!(peer_thread.join().unwrap(), RunEnd::Done(Value::Null));
            started
        });
        if closed_first.load(Ordering::SeqCst) == 1 {
            before += 1;
        } else {
            after += 1;
        }
        // The peer has ended and told whoever it was going to tell: the holder takes what it
        // was given and then suspends, idle.
        holder.suspend();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let RunEnd::Suspended(saved) = rt.block_on(holder_fut) else {
            panic!("the holder was not suspended");
        };
        assert_eq!(saved.vars["n"], json!(1), "round {round}");
        assert_eq!(ends_of(&sink, "r2").len(), 1, "round {round}");
    }
    // Both ways happened.
    assert!(before > 0 && after > 0, "{before} before, {after} after");
}
