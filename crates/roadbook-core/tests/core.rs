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

fn event(tag: &str) -> Input {
    Input::Event {
        value: json!({"_tag": tag}),
    }
}

/// The states entered, in order.
fn entered(commands: &[Command]) -> Vec<&str> {
    records(commands)
        .into_iter()
        .filter_map(|r| match r {
            Record::Entered { state } => Some(state.as_str()),
            _ => None,
        })
        .collect()
}

const HISTORY: &str = r#"
machine M() {
  event Go
  event Out
  event Back
  event Deep
  event Jump
  event Stop

  initial Q

  state P {
    initial A
    history H
    deep history D

    state A {
      on Go   -> B
      on Jump -> H
    }
    state B {
      initial B1
      state B1 {
        on Go   -> B2
        on Jump -> P.H
      }
      state B2 {}
    }

    on Out  -> Q
    on Stop -> Z
  }

  state Q {
    on Back -> P.H
    on Deep -> P.D
  }

  final state Z {}
}
"#;

#[test]
fn history_is_recorded_when_a_microstep_exits_the_parent_only() {
    let p = program(HISTORY);
    let mut s = p.start("M", "r1", Map::new(), &mut no_fns).unwrap().0;
    assert!(s.history.is_empty());
    for e in ["Back", "Go", "Go"] {
        s = p.step(&s, event(e), &mut no_fns).0;
    }
    // Moving inside `P` exits states below it, not `P`: nothing is recorded.
    assert_eq!(s.active, ["M", "M.P", "M.P.B", "M.P.B.B2"]);
    assert!(s.history.is_empty());

    // Cancelling exits `P` without recording.
    let (cancelled, _) = p.step(&s, Input::Cancel, &mut no_fns);
    assert_eq!(cancelled.status, Status::Cancelled);
    assert!(cancelled.history.is_empty());

    // Leaving `P` records where it was, the shallow and the deep way.
    let out = p.step(&s, event("Out"), &mut no_fns).0;
    let recorded = json!({"M.P.D": ["M.P.B.B2"], "M.P.H": ["M.P.B"]});
    assert_eq!(serde_json::to_value(&out.history).unwrap(), recorded);

    // Returning enters `B` by its default entry, `B1`. The microstep that leaves `P` for
    // `Z` records again, each record replacing the one before; finishing then exits every
    // state but records nothing more.
    let back = p.step(&out, event("Back"), &mut no_fns).0;
    assert_eq!(back.active, ["M", "M.P", "M.P.B", "M.P.B.B1"]);
    let (done, _) = p.step(&back, event("Stop"), &mut no_fns);
    assert_eq!(done.status, Status::Done);
    assert_eq!(
        serde_json::to_value(&done.history).unwrap(),
        json!({"M.P.D": ["M.P.B.B1"], "M.P.H": ["M.P.B"]})
    );
}

#[test]
fn a_history_state_with_no_record_enters_its_parents_default_entry() {
    let p = program(HISTORY);
    let s = p.start("M", "r1", Map::new(), &mut no_fns).unwrap().0;
    // From outside the parent: the parent, then its initial chain.
    for e in ["Back", "Deep"] {
        let (s, commands) = p.step(&s, event(e), &mut no_fns);
        assert_eq!(entered(&commands), ["M.P", "M.P.A"]);
        assert_eq!(s.active, ["M", "M.P", "M.P.A"]);
    }
    // From inside it: the domain is the parent, which is neither exited nor entered.
    let s = p.step(&s, event("Back"), &mut no_fns).0;
    let (s, commands) = p.step(&s, event("Jump"), &mut no_fns);
    let kinds: Vec<Value> = records(&commands).iter().map(|r| r.to_value()).collect();
    assert_eq!(
        kinds,
        [
            json!({"kind": "event", "event": {"kind": "event", "value": {"_tag": "Jump"}}}),
            json!({"kind": "transition", "from": "M.P.A", "index": 1, "to": "M.P.H"}),
            json!({"kind": "exited", "state": "M.P.A"}),
            json!({"kind": "entered", "state": "M.P.A"}),
        ]
    );
    assert_eq!(s.active, ["M", "M.P", "M.P.A"]);
    assert!(s.history.is_empty());

    // With a record, from inside: what the parent recorded when it was last exited (`B`,
    // by its default entry), not where it is now.
    let mut s = s;
    for e in ["Go", "Out", "Back"] {
        s = p.step(&s, event(e), &mut no_fns).0;
    }
    assert_eq!(s.active, ["M", "M.P", "M.P.B", "M.P.B.B1"]);
    let (s, commands) = p.step(&s, event("Jump"), &mut no_fns);
    assert_eq!(entered(&commands), ["M.P.B", "M.P.B.B1"]);
    assert_eq!(s.active, ["M", "M.P", "M.P.B", "M.P.B.B1"]);

    // A deep record, from outside: the leaf itself, with nothing entered by default.
    let mut s = p.step(&s, event("Go"), &mut no_fns).0;
    s = p.step(&s, event("Out"), &mut no_fns).0;
    let (s, commands) = p.step(&s, event("Deep"), &mut no_fns);
    assert_eq!(entered(&commands), ["M.P", "M.P.B", "M.P.B.B2"]);
    assert_eq!(s.active, ["M", "M.P", "M.P.B", "M.P.B.B2"]);
}

#[test]
fn a_deep_history_state_restores_every_region_and_regions_off_the_record_are_filled() {
    let p = program(
        r#"
machine M() {
  event Go
  event Out
  event Back
  event Deep

  initial P

  parallel state P {
    deep history D

    state X {
      initial A
      deep history H
      state A { on Go -> A2 }
      state A2 {}
    }
    state Y {
      initial B
      state B { on Go -> B2 }
      state B2 {}
    }

    on Out -> Q
  }

  state Q {
    on Back -> P.X.H
    on Deep -> P.D
  }
}
"#,
    );
    let mut s = p.start("M", "r1", Map::new(), &mut no_fns).unwrap().0;
    s = p.step(&s, event("Go"), &mut no_fns).0;
    s = p.step(&s, event("Out"), &mut no_fns).0;
    assert_eq!(
        serde_json::to_value(&s.history).unwrap(),
        json!({"M.P.D": ["M.P.X.A2", "M.P.Y.B2"], "M.P.X.H": ["M.P.X.A2"]})
    );
    // The parallel state's deep history: a leaf in every region, none entered by default.
    let (back, commands) = p.step(&s, event("Deep"), &mut no_fns);
    assert_eq!(
        entered(&commands),
        ["M.P", "M.P.X", "M.P.X.A2", "M.P.Y", "M.P.Y.B2"]
    );
    assert_eq!(
        back.active,
        ["M", "M.P", "M.P.X", "M.P.X.A2", "M.P.Y", "M.P.Y.B2"]
    );
    // A region's history: the other region gets its default entry.
    let (_, commands) = p.step(&s, event("Back"), &mut no_fns);
    assert_eq!(
        entered(&commands),
        ["M.P", "M.P.X", "M.P.X.A2", "M.P.Y", "M.P.Y.B"]
    );
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

#[test]
fn a_machine_hears_the_emitted_events_it_has_an_on_for_anywhere() {
    let p = program(concat!(
        "machine P() {\n  emits A\n  emits B\n  emits C\n  initial S\n  state S {}\n}\n",
        "machine Q(p: ref P, other: ref P) {\n  initial Outer\n  on p.A -> Outer\n  state Outer {\n    initial Inner\n    state Inner { on other.B -> Inner }\n  }\n}\n",
        "machine R() {\n  initial S\n  state S {\n    invoke P() as kid { done -> S }\n    on kid.C -> S\n  }\n}\n",
    ));
    let heard = |m: &str, sender: &str| {
        let mut events: Vec<_> = p.heard(m, sender).into_iter().collect();
        events.sort();
        events
    };
    // By machine, not by name: an `on` under either parameter is enough, at any depth.
    assert_eq!(heard("Q", "P"), ["A", "B"]);
    assert_eq!(heard("R", "P"), ["C"]);
    assert!(heard("P", "Q").is_empty());
    assert!(heard("Q", "R").is_empty());
    assert!(heard("Missing", "P").is_empty());
}

/// A callee that keeps a pending call in a variable, and a caller.
const CALLS: &str = "error Nope {}\n\
machine S() {\n  call ask(n: int) -> int ! Nope\n  var p: call ask\n  var m: int = 0\n  event Go\n  event Again\n  \
initial A\n  state A { on ask -> B  set p = call, m = event.n }\n  \
state B {\n    on Go     reply p done m\n    on Again  reply p done 2\n  }\n}\n\
machine C(s: ref S) -> int {\n  var r: int = 0\n  initial W\n  \
state W { invoke s.ask(4) { done -> D  set r = result\n  fail * -> E } }\n  \
final state D { output r }\n  final state E { output 0 }\n}\n";

fn ask(from: &str, invoke: u64, n: i64) -> Input {
    Input::Call {
        from: from.into(),
        invoke,
        call: "ask".into(),
        args: json!({ "n": n }).as_object().unwrap().clone(),
    }
}

#[test]
fn a_call_is_pending_until_a_reply_answers_it_once() {
    let p = program(CALLS);
    let (s, _) = p.start("S", "r1", Map::new(), &mut no_fns).unwrap();
    assert_eq!(serde_json::to_value(&s).unwrap()["calls"], json!([]));
    let (s, cmds) = p.step(&s, ask("r2", 3, 7), &mut no_fns);
    assert!(matches!(records(&cmds)[0], Record::Event { .. }));
    // `call` is the pending-call value, kept in `p`; the call is pending.
    assert_eq!(s.vars["p"], json!({"from": "r2", "invoke": 3}));
    assert_eq!(
        serde_json::to_value(&s).unwrap()["calls"],
        json!([{"from": "r2", "invoke": 3, "call": "ask"}])
    );
    let (s, cmds) = p.step(&s, go(), &mut no_fns);
    let replied = Record::Replied {
        to: "r2".into(),
        invoke: 3,
        call: "ask".into(),
        outcome: roadbook_core::ReplyOutcome::Done,
        value: json!(7),
    };
    assert_eq!(records(&cmds).last(), Some(&&replied));
    assert_eq!(
        serde_json::to_value(cmds.last().unwrap()).unwrap(),
        json!({"kind": "reply", "to": "r2", "invoke": 3, "call": "ask", "outcome": "done", "value": 7})
    );
    assert!(s.calls.is_empty());
    // A second reply through the same value: the call is no longer pending.
    let again = Input::Event {
        value: json!({"_tag": "Again"}),
    };
    let (s2, cmds) = p.step(&s, again, &mut no_fns);
    assert_eq!(s2.status, Status::Defect);
    assert_eq!(
        records(&cmds),
        [&Record::Defect {
            defect: Defect::Answered { call: "ask".into() }
        }]
    );
    assert_eq!(
        Defect::Answered { call: "ask".into() }.to_value(),
        json!({"kind": "answered", "call": "ask"})
    );
}

#[test]
fn a_call_that_enables_nothing_is_dropped_and_answered_unanswered() {
    let p = program(CALLS);
    let (s, _) = p.start("S", "r1", Map::new(), &mut no_fns).unwrap();
    let (s, _) = p.step(&s, ask("r2", 1, 7), &mut no_fns);
    // `B` has no `on ask`, and the machine body none either.
    let (s, cmds) = p.step(&s, ask("r3", 1, 8), &mut no_fns);
    let cmds: Vec<Value> = cmds
        .iter()
        .map(|c| serde_json::to_value(c).unwrap())
        .collect();
    assert_eq!(cmds.len(), 3);
    assert_eq!(cmds[0]["record"]["kind"], "dropped");
    assert_eq!(
        cmds[1]["record"],
        json!({"kind": "replied", "to": "r3", "invoke": 1, "call": "ask", "outcome": "fail",
               "value": {"_tag": "Unanswered"}})
    );
    assert_eq!(cmds[2]["kind"], "reply");
    // Only the first call is pending; `event` was the call's arguments.
    assert_eq!(s.calls.len(), 1);
    let (_, cmds) = p.step(&s, go(), &mut no_fns);
    assert_eq!(
        serde_json::to_value(cmds.last().unwrap()).unwrap()["value"],
        7
    );
}

#[test]
fn a_call_invoke_names_its_peer_and_takes_unanswered() {
    let p = program(CALLS);
    let input = json!({"s": "r1"}).as_object().unwrap().clone();
    let (s, cmds) = p.start("C", "r2", input.clone(), &mut no_fns).unwrap();
    let start = cmds
        .iter()
        .find(|c| matches!(c, Command::InvokeStart { .. }))
        .unwrap();
    assert_eq!(
        serde_json::to_value(start).unwrap(),
        json!({"kind": "invoke.start", "invoke": 1, "state": "C.W", "src": "ask",
               "srcKind": "call", "to": "r1", "as": null, "args": {"n": 4}})
    );
    assert!(records(&cmds).iter().any(|r| matches!(
        r, Record::InvokeStarted { src, .. } if src == "s.ask"
    )));
    assert_eq!(s.invokes[0].src, "s.ask");
    let fail = |tag: &str| Input::InvokeFail {
        invoke: 1,
        error: json!({ "_tag": tag }),
    };
    // `Unanswered` and the declared errors are taken by `fail *`; any other is undeclared.
    for tag in ["Unanswered", "Nope"] {
        let (s, _) = p.step(&s, fail(tag), &mut no_fns);
        assert_eq!(s.result, Some(roadbook_core::RunResult::Output(json!(0))));
    }
    let (s2, _) = p.step(&s, fail("Other"), &mut no_fns);
    assert_eq!(
        s2.result,
        Some(roadbook_core::RunResult::Defect(Defect::Undeclared {
            error: "Other".into()
        }))
    );
    let (s, _) = p.step(
        &s,
        Input::InvokeDone {
            invoke: 1,
            value: json!(9),
        },
        &mut no_fns,
    );
    assert_eq!(s.result, Some(roadbook_core::RunResult::Output(json!(9))));
}
