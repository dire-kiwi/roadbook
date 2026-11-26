//! Rules of `spec/execution.md` that the conformance cases do not reach.

use roadbook_core::{Command, Defect, Input, Program, ProgramError, Record, Status};
use serde_json::{Map, Value, json};

fn program(src: &str) -> Program {
    let c = roadbook_compiler::compile_source("t.road", src, &Default::default());
    assert!(!c.has_errors(), "{}", c.render());
    Program::new(c.ir.unwrap()).unwrap()
}

fn no_fns(name: &str, _: Vec<Value>) -> Result<Value, Defect> {
    panic!("unexpected call of {name}")
}

fn records(commands: &[Command]) -> Vec<&Record> {
    commands
        .iter()
        .filter_map(|c| match c {
            Command::Trace { record } => Some(record),
            _ => None,
        })
        .collect()
}

fn go() -> Input {
    Input::Event {
        value: json!({"_tag": "Go"}),
    }
}

#[test]
fn integer_arithmetic_overflows_at_i64() {
    let p = program(
        "machine M(n: int) -> int {\n  var x: int = 0\n  event Go\n  initial A\n  state A { on Go -> B  set x = n * 2 }\n  final state B { output x }\n}\n",
    );
    let input = |n: i64| json!({ "n": n }).as_object().unwrap().clone();
    let (s, _) = p.start("M", input(1 << 62), &mut no_fns).unwrap();
    let (s, cmds) = p.step(&s, go(), &mut no_fns);
    assert_eq!(s.status, Status::Defect);
    assert_eq!(
        records(&cmds),
        [&Record::Defect {
            defect: Defect::Overflow {
                expr: "n * 2".into()
            }
        }]
    );
    // The step is atomic: no `event` record, and the snapshot is the one given, failed.
    assert_eq!(s.active, ["M", "M.A"]);
    assert_eq!(s.vars["x"], json!(0));
    // Beyond 2^53 but within i64 is fine in Rust.
    let (s, _) = p.start("M", input(1 << 61), &mut no_fns).unwrap();
    let (s, _) = p.step(&s, go(), &mut no_fns);
    assert_eq!(s.status, Status::Done);
    assert_eq!(
        s.result,
        Some(roadbook_core::RunResult::Output(json!(1_i64 << 62)))
    );
}

#[test]
fn a_guarded_always_loop_hits_the_limit() {
    let p = program(
        "machine M() {\n  var n: int = 0\n  event Go\n  initial A\n  state A { on Go -> B }\n  state B { always { if n >= 0 -> C  set n = n + 1 } }\n  state C { always { if n >= 0 -> B } }\n  final state D {}\n}\n",
    );
    let (s, _) = p.start("M", Map::new(), &mut no_fns).unwrap();
    let (s, cmds) = p.step(&s, go(), &mut no_fns);
    assert_eq!(s.status, Status::Defect);
    assert_eq!(
        records(&cmds),
        [&Record::Defect {
            defect: Defect::Limit
        }]
    );
}

#[test]
fn a_defect_in_start_keeps_the_started_record() {
    let p = program(
        "fn f() -> int\nmachine M() {\n  var x: int = f()\n  initial A\n  final state A {}\n}\n",
    );
    let mut fns = |_: &str, _: Vec<Value>| Err(Defect::host("nope"));
    let (s, cmds) = p.start("M", Map::new(), &mut fns).unwrap();
    assert_eq!(s.status, Status::Defect);
    assert_eq!(s.active, Vec::<String>::new());
    let r = records(&cmds);
    assert_eq!(r.len(), 2);
    assert!(matches!(r[0], Record::Started { .. }));
    assert_eq!(
        r[1],
        &Record::Defect {
            defect: Defect::host("nope")
        }
    );
}

#[test]
fn a_defect_input_is_taken_after_the_run_ended_but_only_once() {
    let p = program("machine M() {\n  initial A\n  final state A {}\n}\n");
    let (s, _) = p.start("M", Map::new(), &mut no_fns).unwrap();
    assert_eq!(s.status, Status::Done);
    let d = Defect::host("late");
    let (s, cmds) = p.step(&s, Input::Defect { defect: d.clone() }, &mut no_fns);
    assert_eq!(s.status, Status::Defect);
    assert_eq!(records(&cmds), [&Record::Defect { defect: d.clone() }]);
    let (s2, cmds) = p.step(&s, Input::Defect { defect: d }, &mut no_fns);
    assert!(cmds.is_empty());
    assert_eq!(s2, s);
    // Any other input to an ended run does nothing.
    let (_, cmds) = p.step(&s, go(), &mut no_fns);
    assert!(cmds.is_empty());
}

#[test]
fn snapshots_have_the_spec_shape() {
    let p = program(
        "task t() -> int\nmachine M() {\n  initial A\n  state A { invoke t() { done -> B }  after 1s -> B }\n  final state B {}\n}\n",
    );
    let (s, _) = p.start("M", Map::new(), &mut no_fns).unwrap();
    let v = serde_json::to_value(&s).unwrap();
    assert_eq!(v["invokes"], json!([{"id": 1, "state": "M.A", "src": "t"}]));
    assert_eq!(
        v["timers"],
        json!([{"id": 1, "state": "M.A", "index": 1, "delay": 1000}])
    );
    assert_eq!(v["nextInvoke"], 2);
    assert_eq!(v["result"], Value::Null);
    let (s, _) = p.step(
        &s,
        Input::InvokeDone {
            invoke: 1,
            value: json!(5),
        },
        &mut no_fns,
    );
    assert_eq!(
        serde_json::to_value(&s).unwrap()["result"],
        json!({"output": null})
    );
}

#[test]
fn parallel_states_are_refused() {
    let c = roadbook_compiler::compile_source(
        "t.road",
        "machine M() {\n  initial P\n  parallel state P {\n    state X { initial A  state A {} }\n    state Y { initial B  state B {} }\n  }\n}\n",
        &Default::default(),
    );
    let ir = c.ir.expect("the compiler accepts parallel states");
    assert!(matches!(
        Program::new(ir),
        Err(ProgramError::Parallel { state, .. }) if state == "M.P"
    ));
}
