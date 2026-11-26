//! Rules of `spec/execution.md` that the conformance cases do not reach.

use roadbook_core::{Command, Defect, Input, Program, Record, Status};
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
    let (s, _) = p.start("M", "r1", input(1 << 62), &mut no_fns).unwrap();
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
    let (s, _) = p.start("M", "r1", input(1 << 61), &mut no_fns).unwrap();
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
    let (s, _) = p.start("M", "r1", Map::new(), &mut no_fns).unwrap();
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
    let (s, cmds) = p.start("M", "r1", Map::new(), &mut fns).unwrap();
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
    let (s, _) = p.start("M", "r1", Map::new(), &mut no_fns).unwrap();
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
    let (s, _) = p.start("M", "r1", Map::new(), &mut no_fns).unwrap();
    let v = serde_json::to_value(&s).unwrap();
    assert_eq!(v["invokes"], json!([{"id": 1, "state": "M.A", "src": "t"}]));
    assert_eq!(
        v["timers"],
        json!([{"id": 1, "state": "M.A", "index": 1, "delay": 1000}])
    );
    assert_eq!(v["nextInvoke"], 2);
    assert_eq!(v["nextMessage"], 1);
    assert_eq!(v["run"], "r1");
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
fn parallel_states_run() {
    let c = roadbook_compiler::compile_source(
        "t.road",
        "machine M() {\n  initial P\n  parallel state P {\n    state X { initial A  state A {} }\n    state Y { initial B  state B {} }\n  }\n}\n",
        &Default::default(),
    );
    let program = Program::new(c.ir.expect("the compiler accepts parallel states")).unwrap();
    let (snap, _) = program
        .start("M", "r1", Default::default(), &mut no_fns)
        .unwrap();
    assert_eq!(
        snap.active,
        ["M", "M.P", "M.P.X", "M.P.X.A", "M.P.Y", "M.P.Y.B"]
    );
}

#[test]
fn emit_and_send_take_message_ids() {
    const SRC: &str = "machine P(peer: ref M) {\n  initial A\n  state A { entry send peer.Go }\n}\n\n\
        machine M() {\n  event Go\n  emits Ping { n: int? }\n  initial A\n  state A {\n    entry emit Ping { n: 1 }\n    on Go emit Ping { n: 2 }\n  }\n}\n";
    let p = program(SRC);
    let (s, cmds) = p.start("M", "r1", Map::new(), &mut no_fns).unwrap();
    assert!(cmds.contains(&Command::Emit {
        message: 1,
        value: json!({"_tag": "Ping", "n": 1}),
    }));
    let go = Input::Message {
        from: "r2".into(),
        message: 1,
        value: json!({"_tag": "Go"}),
    };
    let (s, cmds) = p.step(&s, go, &mut no_fns);
    assert_eq!(s.next_message, 3);
    assert!(cmds.contains(&Command::Emit {
        message: 2,
        value: json!({"_tag": "Ping", "n": 2}),
    }));
    let mut input = Map::new();
    input.insert("peer".into(), json!("r1"));
    let (_, cmds) = p.start("P", "r2", input, &mut no_fns).unwrap();
    assert!(cmds.contains(&Command::Send {
        to: "r1".into(),
        invoke: None,
        message: 1,
        value: json!({"_tag": "Go"}),
    }));
}

#[test]
fn operators_see_through_aliases() {
    // Each guard holds only if its operator works on the aliased type: a `false` guard
    // drops `Go`.
    let guards = [
        "m < 10",
        "m <= 5",
        "m > 4",
        "m >= 5",
        "m == 5",
        "m != 6",
        "m + 1 == 6",
        "f < 2.5",
        "f == 1.5",
        "-f < 0.0",
        "d < 2s",
        "d >= 1s",
        "d == 1000ms",
        "k == Kind.B",
        "k != Kind.A",
        "w == 5",
        "w < 6",
    ];
    for g in guards {
        let src = format!(
            "type Money = int\ntype Cash = Money\ntype Ratio = float\ntype Wait = duration\n\
             enum Kind {{ A, B }}\ntype K = Kind\n\
             machine M() {{\n  var m: Money = 5\n  var w: Cash = 5\n  var f: Ratio = 1.5\n  \
             var d: Wait = 1s\n  var k: K = Kind.B\n  event Go\n  initial A\n  \
             state A {{ on Go {{ if {g} -> B }} }}\n  final state B {{}}\n}}\n"
        );
        let p = program(&src);
        let (s, _) = p.start("M", "r1", Map::new(), &mut no_fns).unwrap();
        let (s, _) = p.step(&s, go(), &mut no_fns);
        assert_eq!(s.status, Status::Done, "`{g}` should hold");
    }
}

#[test]
fn an_optional_alias_var_starts_as_null() {
    let p = program(
        "type S = string?\nmachine M() {\n  var s: S\n  event Go\n  initial A\n  state A { on Go -> B }\n  final state B {}\n}\n",
    );
    let (s, _) = p.start("M", "r1", Map::new(), &mut no_fns).unwrap();
    assert_eq!(s.vars.get("s"), Some(&Value::Null));
}

/// A hand-written IR whose transition or initial state names a state the machine does not
/// have is refused when it is indexed: running it would have nothing to enter.
#[test]
fn an_ir_naming_a_missing_state_is_refused() {
    let src =
        "machine M() {\n  event Go\n  initial A\n  state A { on Go -> B }\n  final state B {}\n}\n";
    let ir = || {
        roadbook_compiler::compile_source("t.road", src, &Default::default())
            .ir
            .unwrap()
    };
    let mut bad_target = ir();
    let m = bad_target.machines.get_mut("M").unwrap();
    m.root.states[0].transitions[0].target = Some("M.Nope".into());
    let e = Program::new(bad_target).unwrap_err().to_string();
    assert!(e.contains("the target `M.Nope` of state `M.A`"), "{e}");

    let mut bad_initial = ir();
    bad_initial.machines.get_mut("M").unwrap().root.initial = Some("M.Nope".into());
    let e = Program::new(bad_initial).unwrap_err().to_string();
    assert!(e.contains("the initial state `M.Nope` of state `M`"), "{e}");
}
