//! The driver with real panics and a real executor: what the conformance cases, whose host
//! code is scripted, do not exercise.

use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex};

use roadbook::dynamic::{self, Host, Invocation, Outcome};
use roadbook::{BoxFuture, Config, Defect, JsonLines, MemorySink, Program, RunEnd};
use roadbook_core::{Object, SrcKind};
use serde_json::{Value, json};

const CHART: &str = r#"
action log(what: string)
task work(n: int) -> int
fn double(n: int) -> int

machine Main(n: int) -> int {
  event Stop
  exit do log("exit Main")
  initial Calling
  state Calling {
    entry do log("enter Calling")
    exit do log("exit Calling")
    invoke Child(double(n)) {
      done -> Over
    }
    on Stop -> Over
  }
  final state Over { output n }
}

machine Child(n: int) -> int {
  var unset: int
  exit do log("exit Child")
  initial Working
  state Working {
    invoke work(n) {
      done {
        if result > unset -> Finished
        else              -> Finished
      }
    }
  }
  final state Finished { output n }
}
"#;

fn program() -> Program {
    // `Child` reads `unset` before anything assigns it, to test the `unassigned` defect.
    let options = roadbook_compiler::Options {
        allow_unassigned: vec!["main.road".into()],
        ..Default::default()
    };
    let c = roadbook_compiler::compile_source("main.road", CHART, &options);
    assert!(!c.has_errors(), "{}", c.render());
    Program::new(c.ir.unwrap()).unwrap()
}

/// What host code does in a test.
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Fine,
    PanicInFunction,
    PanicInAction,
    PanicInTask,
    TaskNeverEnds,
    /// The task never ends, and its future panics when it is dropped.
    TaskPanicsWhenDropped,
    /// The task ends, and its future panics when it is dropped afterwards.
    TaskPanicsWhenDroppedAfterDone,
}

/// A future that panics when it is dropped.
struct PanicOnDrop(BoxFuture<'static, Outcome>);

impl std::future::Future for PanicOnDrop {
    type Output = Outcome;
    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Outcome> {
        self.0.as_mut().poll(cx)
    }
}

impl Drop for PanicOnDrop {
    fn drop(&mut self) {
        panic!("dropped a bomb");
    }
}

struct TestHost<'p> {
    program: &'p Program,
    mode: Mode,
    actions: Mutex<Vec<String>>,
}

impl Host for TestHost<'_> {
    fn function(&self, _name: &str, args: Vec<Value>) -> Result<Value, Defect> {
        if self.mode == Mode::PanicInFunction {
            panic!("function blew up: {}", args[0]);
        }
        Ok(json!(args[0].as_i64().unwrap() * 2))
    }

    fn action<'a>(&'a self, _name: &str, args: Object) -> BoxFuture<'a, Result<(), Defect>> {
        Box::pin(async move {
            let what = args["what"].as_str().unwrap().to_owned();
            if self.mode == Mode::PanicInAction && what == "enter Calling" {
                panic!("action blew up");
            }
            self.actions.lock().unwrap().push(what);
            Ok(())
        })
    }

    fn invoke<'a>(&'a self, call: Invocation) -> BoxFuture<'a, Outcome> {
        match call.kind {
            SrcKind::Machine => Box::pin(async move {
                match dynamic::run(self.program, &call.src, call.args, &call.refs, self).await {
                    Ok(v) => Outcome::Done(v),
                    Err(e) => Outcome::Fail(e),
                }
            }),
            SrcKind::Task if self.mode == Mode::TaskPanicsWhenDropped => {
                Box::pin(PanicOnDrop(Box::pin(std::future::pending())))
            }
            SrcKind::Task if self.mode == Mode::TaskPanicsWhenDroppedAfterDone => {
                let n = call.args["n"].clone();
                Box::pin(PanicOnDrop(Box::pin(async move { Outcome::Done(n) })))
            }
            SrcKind::Task => Box::pin(async move {
                tokio::task::yield_now().await;
                match self.mode {
                    Mode::PanicInTask => std::panic::panic_any(42_u8),
                    Mode::TaskNeverEnds => std::future::pending().await,
                    _ => Outcome::Done(call.args["n"].clone()),
                }
            }),
        }
    }
}

fn host(program: &Program, mode: Mode) -> TestHost<'_> {
    TestHost {
        program,
        mode,
        actions: Mutex::new(Vec::new()),
    }
}

fn input(n: i64) -> Object {
    json!({ "n": n }).as_object().unwrap().clone()
}

async fn run_to_end(mode: Mode) -> (RunEnd, Vec<Value>) {
    let program = program();
    let host = host(&program, mode);
    let sink = Arc::new(MemorySink::new());
    let config = Config::new().sink(sink.clone()).run_id("r1");
    let (fut, _) = dynamic::start(&config, &program, "Main", input(3), &[], &host);
    let end = fut.await;
    let records = sink.records().iter().map(|r| r.to_json()).collect();
    (end, records)
}

fn last_of(records: &[Value], run: &str) -> Value {
    records
        .iter()
        .rev()
        .find(|r| r["run"] == run)
        .cloned()
        .unwrap()
}

#[tokio::test]
async fn a_panic_in_a_function_is_a_host_defect_with_its_message() {
    let (end, records) = run_to_end(Mode::PanicInFunction).await;
    assert_eq!(end, RunEnd::Defect(Defect::host("function blew up: 3")));
    assert_eq!(
        last_of(&records, "r1")["defect"],
        json!({"kind": "host", "message": "function blew up: 3"})
    );
}

#[tokio::test]
async fn a_panic_in_an_action_stops_the_step() {
    let (end, records) = run_to_end(Mode::PanicInAction).await;
    assert_eq!(end, RunEnd::Defect(Defect::host("action blew up")));
    // The defect record comes right after the action's, and no invoke was started.
    let kinds: Vec<&str> = records
        .iter()
        .map(|r| r["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["started", "entered", "entered", "action", "defect"]);
}

#[tokio::test]
async fn a_task_panic_with_another_payload_is_box_dyn_any() {
    let (end, records) = run_to_end(Mode::PanicInTask).await;
    // The child's task panicked: the child ends with the defect, then the parent.
    let d = Defect::host("Box<dyn Any>");
    assert_eq!(end, RunEnd::Defect(d.clone()));
    assert_eq!(last_of(&records, "r1.1")["kind"], "defect");
    assert_eq!(last_of(&records, "r1")["defect"], d.to_value());
}

#[tokio::test]
async fn a_childs_defect_reaches_the_root_unchanged() {
    // The child's guard reads `unset`: an unassigned defect inside the child's core, raised
    // as a `Defect` payload and recognised by the parent's driver.
    let (end, records) = run_to_end(Mode::Fine).await;
    let d = Defect::Unassigned {
        var: "unset".into(),
    };
    assert_eq!(end, RunEnd::Defect(d.clone()));
    assert_eq!(last_of(&records, "r1.1")["defect"], d.to_value());
    let event = records
        .iter()
        .find(|r| r["run"] == "r1" && r["kind"] == "event")
        .unwrap();
    assert_eq!(
        event["event"],
        json!({"kind": "invoke.defect", "invoke": 1, "defect": d.to_value()})
    );
}

#[test]
fn a_root_run_that_ends_with_a_defect_resumes_unwinding_with_it() {
    let program = program();
    let host = host(&program, Mode::PanicInFunction);
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let caught = std::panic::catch_unwind(AssertUnwindSafe(|| {
        rt.block_on(dynamic::run(&program, "Main", input(3), &[], &host))
    }));
    let payload = caught.unwrap_err();
    assert_eq!(
        payload.downcast_ref::<Defect>(),
        Some(&Defect::host("function blew up: 3"))
    );
}

#[tokio::test]
async fn cancelling_from_outside_waits_for_the_childs_exit_effects() {
    let program = program();
    let host = host(&program, Mode::TaskNeverEnds);
    let sink = Arc::new(MemorySink::new());
    let config = Config::new().sink(sink.clone()).run_id("r1");
    let (fut, handle) = dynamic::start(&config, &program, "Main", input(3), &[], &host);
    let canceller = async {
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
        handle.cancel();
    };
    let (end, ()) = tokio::join!(fut, canceller);
    assert_eq!(end, RunEnd::Cancelled);
    assert_eq!(
        *host.actions.lock().unwrap(),
        ["enter Calling", "exit Child", "exit Calling", "exit Main"]
    );
    let records: Vec<Value> = sink.records().iter().map(|r| r.to_json()).collect();
    assert_eq!(last_of(&records, "r1.1")["kind"], "cancelled");
    assert_eq!(last_of(&records, "r1")["kind"], "cancelled");
}

#[tokio::test]
async fn dropping_a_root_run_is_a_hard_stop() {
    let program = program();
    let host = host(&program, Mode::TaskNeverEnds);
    let sink = Arc::new(MemorySink::new());
    let config = Config::new().sink(sink.clone()).run_id("r1");
    let (fut, _) = dynamic::start(&config, &program, "Main", input(3), &[], &host);
    let timed_out = tokio::time::timeout(std::time::Duration::from_millis(20), fut).await;
    assert!(timed_out.is_err());
    // No exit effects ran and no run has a final record.
    assert_eq!(*host.actions.lock().unwrap(), ["enter Calling"]);
    let kinds: Vec<String> = sink
        .records()
        .iter()
        .map(|r| r.to_json()["kind"].as_str().unwrap().to_owned())
        .collect();
    for k in ["done", "failed", "cancelled", "defect", "exited"] {
        assert!(!kinds.iter().any(|x| x == k), "{kinds:?}");
    }
}

#[test]
fn a_root_run_id_must_be_a_run_id() {
    // Ids the snapshot schema accepts.
    for id in ["order-42", "o_1", "r1.4"] {
        let _ = Config::new().run_id(id);
    }
    // `order:42` would save a snapshot that `restore` refuses: the setter refuses it.
    for id in ["order:42", "", "a b", "r1.0", "r1."] {
        let refused = std::panic::catch_unwind(|| Config::new().run_id(id)).unwrap_err();
        let message = refused.downcast_ref::<String>().unwrap();
        assert!(message.contains("is not a run id"), "{message}");
        assert!(!roadbook::is_run_id(id), "{id}");
    }
}

#[tokio::test]
async fn json_lines_writes_one_canonical_record_per_line() {
    let program = program();
    let host = host(&program, Mode::PanicInAction);
    let sink = Arc::new(JsonLines::new(Vec::new()));
    let config = Config::new().sink(sink.clone()).run_id("r9");
    let (fut, _) = dynamic::start(&config, &program, "Main", input(3), &[], &host);
    fut.await;
    drop(config);
    let text = String::from_utf8(Arc::into_inner(sink).unwrap().into_inner()).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 5);
    assert!(text.ends_with('\n'));
    assert!(
        lines[0].starts_with(r#"{"input":{"n":3},"ir":"sha256:"#),
        "{}",
        lines[0]
    );
    assert!(lines[1].starts_with(r#"{"kind":"entered","run":"r9","state":"Main","t":"#));
}

#[tokio::test]
async fn a_panic_while_dropping_a_cancelled_task_is_ignored() {
    let program = program();
    let host = host(&program, Mode::TaskPanicsWhenDropped);
    let sink = Arc::new(MemorySink::new());
    let config = Config::new().sink(sink.clone()).run_id("r1");
    let (fut, handle) = dynamic::start(&config, &program, "Main", input(3), &[], &host);
    let stopper = async {
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
        handle.send(json!({"_tag": "Stop"}));
    };
    // Stop cancels the child, whose state is left, which drops the task's future.
    let (end, ()) = tokio::join!(fut, stopper);
    assert_eq!(end, RunEnd::Done(json!(3)));
    let records: Vec<Value> = sink.records().iter().map(|r| r.to_json()).collect();
    assert_eq!(last_of(&records, "r1.1")["kind"], "cancelled");
    assert_eq!(last_of(&records, "r1")["kind"], "done");

    // So is one while dropping it in a hard stop, and in the cancel from outside.
    let (fut, _) = dynamic::start(&config, &program, "Main", input(3), &[], &host);
    let timed_out = tokio::time::timeout(std::time::Duration::from_millis(20), fut).await;
    assert!(timed_out.is_err());
    let (fut, handle) = dynamic::start(&config, &program, "Main", input(3), &[], &host);
    let canceller = async {
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
        handle.cancel();
    };
    let (end, ()) = tokio::join!(fut, canceller);
    assert_eq!(end, RunEnd::Cancelled);
}

#[tokio::test]
async fn a_panic_while_dropping_a_finished_task_is_its_outcome() {
    let (end, records) = run_to_end(Mode::TaskPanicsWhenDroppedAfterDone).await;
    let d = Defect::host("dropped a bomb");
    assert_eq!(end, RunEnd::Defect(d.clone()));
    let event = records
        .iter()
        .find(|r| r["run"] == "r1.1" && r["kind"] == "event")
        .unwrap();
    assert_eq!(
        event["event"],
        json!({"kind": "invoke.defect", "invoke": 1, "defect": d.to_value()})
    );
}

#[tokio::test]
async fn a_panic_while_dropping_a_task_whose_outcome_came_from_outside_is_ignored() {
    let program = program();
    let host = host(&program, Mode::TaskPanicsWhenDropped);
    let sink = Arc::new(MemorySink::new());
    let child = Arc::new(Mutex::new(None));
    let seen = child.clone();
    let config = Config::new()
        .sink(sink.clone())
        .run_id("r1")
        .on_run(move |run, h| {
            if run == "r1.1" {
                *seen.lock().unwrap() = Some(h);
            }
        });
    let (fut, _) = dynamic::start(&config, &program, "Main", input(3), &[], &host);
    let feeder = async {
        let h = loop {
            tokio::task::yield_now().await;
            if let Some(h) = child.lock().unwrap().clone() {
                break h;
            }
        };
        tokio::task::yield_now().await;
        // The child's `work` is invoke 1; its outcome arrives from outside, and the core
        // taking it drops the task's future.
        h.push(vec![roadbook::Input::InvokeDone {
            invoke: 1,
            value: json!(3),
        }]);
    };
    let (end, ()) = tokio::join!(fut, feeder);
    // The child's guard reads `unset`, as in `a_childs_defect_reaches_the_root_unchanged`.
    assert_eq!(
        end,
        RunEnd::Defect(Defect::Unassigned {
            var: "unset".into()
        })
    );
}
