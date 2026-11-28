//! Checking and restoring saved snapshots: the refusal reasons, in the spec's order, and what
//! `restore` asks the driver to do (`spec/execution.md#restoring-a-snapshot`).

use roadbook_core::{Command, Defect, Program, Record, Saved, Status};
use serde_json::{Value, json};

const CHART: &str = r#"
type Item { sku: string, qty: int }
fn price(n: int) -> int
task fetch(n: int) -> int

machine M(item: Item) {
  var n: int = 1
  var note: string?

  event Go

  initial A

  state A {
    after 1s -> B
    after 2s -> B
    on Go -> W
  }

  state W { invoke fetch(price(n)) { done -> B } }

  state B {}
}

machine Peer(other: ref M) {
  initial X
  state X {}
}
"#;

fn program() -> Program {
    let c = roadbook_compiler::compile_source("t.road", CHART, &Default::default());
    Program::new(c.ir.expect("the chart compiles")).unwrap()
}

fn no_fns(name: &str, _: Vec<Value>) -> Result<Value, Defect> {
    Err(Defect::host(format!("unexpected call {name}")))
}

/// A saved snapshot of a fresh run of `M`, its timers due at 1000 and 2000.
fn saved(p: &Program) -> Value {
    let input = json!({"item": {"sku": "s1", "qty": 2}});
    let (snap, _) = p
        .start("M", "r1", input.as_object().unwrap().clone(), &mut no_fns)
        .unwrap();
    Saved::new(&snap, |_, delay| delay).to_value()
}

/// A way to break a snapshot.
type Break = Box<dyn Fn(&mut Value)>;

fn reason(p: &Program, v: Value) -> &'static str {
    match p.check_saved(v, None, None) {
        Ok(_) => "ok",
        Err(e) => e.reason(),
    }
}

#[test]
fn a_fresh_snapshot_passes() {
    let p = program();
    let (s, migrated) = p.check_saved(saved(&p), Some("M"), None).unwrap();
    assert_eq!(s.active, ["M", "M.A"]);
    assert_eq!(s.vars, *json!({"n": 1, "note": null}).as_object().unwrap());
    assert_eq!(migrated, None);
}

#[test]
fn refusals_come_in_the_spec_order() {
    let p = program();
    let base = saved(&p);
    let with = |f: &dyn Fn(&mut Value)| {
        let mut v = base.clone();
        f(&mut v);
        v
    };
    // The schema and the version first, even for a snapshot of no machine.
    assert_eq!(reason(&p, with(&|v| v["extra"] = json!(1))), "invalid");
    assert_eq!(
        reason(&p, with(&|v| v["snapshotVersion"] = json!(5))),
        "invalid"
    );
    assert_eq!(
        reason(&p, with(&|v| v["machine"] = json!("Nope"))),
        "machine"
    );
    assert_eq!(
        p.check_saved(base.clone(), Some("Peer"), None)
            .unwrap_err()
            .reason(),
        "machine"
    );
    let other = format!("sha256:{}", "0".repeat(64));
    // Another hash before an ended status.
    assert_eq!(
        reason(
            &p,
            with(&|v| {
                v["ir"] = json!(other);
                v["status"] = json!("done");
            })
        ),
        "ir"
    );
    assert_eq!(
        reason(&p, with(&|v| v["status"] = json!("cancelled"))),
        "ended"
    );
    // An ended snapshot is `ended` even when it would not fit.
    assert_eq!(
        reason(
            &p,
            with(&|v| {
                v["status"] = json!("done");
                v["active"] = json!([]);
            })
        ),
        "ended"
    );
}

#[test]
fn a_machine_with_a_ref_parameter_is_refused() {
    let p = program();
    let (snap, _) = p
        .start(
            "Peer",
            "r2",
            json!({"other": "r1"}).as_object().unwrap().clone(),
            &mut no_fns,
        )
        .unwrap();
    let v = Saved::new(&snap, |_, delay| delay).to_value();
    assert_eq!(reason(&p, v), "ref");
}

#[test]
fn a_snapshot_that_does_not_fit_is_invalid() {
    let p = program();
    let base = saved(&p);
    let bad: Vec<(&str, Break)> = vec![
        ("no root", Box::new(|v| v["active"] = json!(["M.A"]))),
        (
            "two children",
            Box::new(|v| v["active"] = json!(["M", "M.A", "M.B"])),
        ),
        (
            "out of order",
            Box::new(|v| v["active"] = json!(["M.A", "M"])),
        ),
        (
            "unknown state",
            Box::new(|v| v["active"] = json!(["M", "M.Z"])),
        ),
        (
            "timer of an inactive state",
            Box::new(|v| v["timers"][0]["state"] = json!("M.B")),
        ),
        (
            "timer index not an after",
            Box::new(|v| v["timers"][0]["index"] = json!(2)),
        ),
        (
            "two timers of one block",
            Box::new(|v| v["timers"][1]["index"] = json!(0)),
        ),
        (
            "timer id not below nextTimer",
            Box::new(|v| v["nextTimer"] = json!(2)),
        ),
        (
            "invoke of an inactive state",
            Box::new(|v| v["invokes"] = json!([{"id": 1, "state": "M.W", "src": "fetch"}])),
        ),
        ("a missing parameter", Box::new(|v| v["input"] = json!({}))),
        (
            "an unknown parameter",
            Box::new(|v| v["input"]["other"] = json!(1)),
        ),
        (
            "an unknown variable",
            Box::new(|v| v["vars"]["count"] = json!(1)),
        ),
        (
            "no optional variable",
            Box::new(|v| {
                v["vars"].as_object_mut().unwrap().remove("note");
            }),
        ),
        (
            "an int that is a string",
            Box::new(|v| v["vars"]["n"] = json!("1")),
        ),
        (
            "an int that is not whole",
            Box::new(|v| v["vars"]["n"] = json!(1.5)),
        ),
        (
            "a record with a member its type does not declare",
            Box::new(|v| v["input"]["item"]["colour"] = json!("red")),
        ),
        (
            "a record without a field",
            Box::new(|v| {
                v["input"]["item"].as_object_mut().unwrap().remove("qty");
            }),
        ),
    ];
    for (what, f) in bad {
        let mut v = base.clone();
        f(&mut v);
        assert_eq!(reason(&p, v), "invalid", "{what}");
    }
    // A variable with no value has no member, and that fits.
    let mut v = base.clone();
    v["vars"].as_object_mut().unwrap().remove("n");
    assert_eq!(reason(&p, v), "ok");
}

#[test]
fn a_migration_is_called_once_and_only_when_the_hashes_differ() {
    let p = program();
    let base = saved(&p);
    let mut calls = 0;
    let mut same = |v: Value| {
        calls += 1;
        Ok(v)
    };
    assert!(p.check_saved(base.clone(), None, Some(&mut same)).is_ok());
    assert_eq!(calls, 0);

    let hash = base["ir"].clone();
    let mut old = base.clone();
    old["ir"] = json!(format!("sha256:{}", "a".repeat(64)));
    old["vars"] = json!({"count": 1, "note": null});
    let mut seen = Vec::new();
    let mut rename = |mut v: Value| {
        seen.push(v.clone());
        let count = v["vars"].as_object_mut().unwrap().remove("count").unwrap();
        v["vars"]["n"] = count;
        v["ir"] = hash.clone();
        Ok(v)
    };
    let (s, migrated) = p.check_saved(old.clone(), None, Some(&mut rename)).unwrap();
    assert_eq!(seen, [old.clone()]);
    assert_eq!(s.vars["n"], 1);
    assert_eq!(migrated.as_deref(), old["ir"].as_str());

    let mut fails = |_: Value| Err("no".to_owned());
    let e = p
        .check_saved(old.clone(), None, Some(&mut fails))
        .unwrap_err();
    assert_eq!(e.reason(), "migration");
    // Its result is checked from the top, without a second migration.
    let mut keeps = |v: Value| Ok(v);
    assert_eq!(
        p.check_saved(old.clone(), None, Some(&mut keeps))
            .unwrap_err()
            .reason(),
        "ir"
    );
    let mut junk = |_: Value| Ok(json!([]));
    assert_eq!(
        p.check_saved(old, None, Some(&mut junk))
            .unwrap_err()
            .reason(),
        "invalid"
    );
}

#[test]
fn restore_restarts_work_and_fires_what_is_due() {
    let p = program();
    let (s, _) = p.check_saved(saved(&p), None, None).unwrap();
    // At 1500 the first timer (due 1000) is due, the second (due 2000) is not.
    let (snap, cmds) = p.restore(&s, "r9", 1500, None, &mut no_fns).unwrap();
    assert_eq!(snap.run, "r9");
    assert_eq!(snap.timers, s.to_snapshot("r9").timers);
    assert!(matches!(
        &cmds[0],
        Command::Trace {
            record: Record::Restored { migrated: None, .. }
        }
    ));
    assert_eq!(
        cmds[1..],
        [
            Command::Trace {
                record: Record::TimerStarted {
                    timer: 2,
                    state: "M.A".into(),
                    index: 1,
                    delay: 500
                }
            },
            Command::TimerStart {
                timer: 2,
                delay: 500
            },
            Command::TimerFire { timer: 1 },
        ]
    );
    // Both due: the earliest first.
    let mut late = s.clone();
    late.timers[0].due = 3000;
    let (_, cmds) = p.restore(&late, "r9", 5000, None, &mut no_fns).unwrap();
    assert_eq!(
        cmds[1..],
        [
            Command::TimerFire { timer: 2 },
            Command::TimerFire { timer: 1 }
        ]
    );
}

#[test]
fn a_defect_restarting_an_invoke_makes_restore_atomic() {
    let p = program();
    let (snap, _) = p
        .start(
            "M",
            "r1",
            json!({"item": {"sku": "s1", "qty": 2}})
                .as_object()
                .unwrap()
                .clone(),
            &mut no_fns,
        )
        .unwrap();
    let mut price = |_: &str, _: Vec<Value>| Ok(json!(7));
    let (snap, _) = p.step(
        &snap,
        roadbook_core::Input::Event {
            value: json!({"_tag": "Go"}),
        },
        &mut price,
    );
    assert_eq!(snap.invokes.len(), 1);
    let s = Saved::new(&snap, |_, delay| delay);
    let (s, _) = p.check_saved(s.to_value(), None, None).unwrap();
    let (after, cmds) = p.restore(&s, "r2", 0, None, &mut no_fns).unwrap();
    assert_eq!(after.status, Status::Defect);
    let records: Vec<_> = cmds
        .iter()
        .filter_map(|c| match c {
            Command::Trace { record } => Some(record),
            _ => None,
        })
        .collect();
    assert!(matches!(
        records[..],
        [Record::Restored { .. }, Record::Defect { .. }]
    ));
}

#[test]
fn a_migration_cannot_turn_the_snapshot_into_another_machines() {
    let p = program();
    let mut old = saved(&p);
    old["ir"] = json!(format!("sha256:{}", "0".repeat(64)));
    // A valid snapshot of `Peer` (whose `ref` would otherwise be refused later, as `ref`).
    let peer_hash = p.ir().machines["Peer"].hash.clone();
    let peer = json!({
        "snapshotVersion": 3, "machine": "Peer", "run": "r9", "ir": peer_hash,
        "status": "running", "active": ["Peer", "Peer.X"], "input": {"other": "r1"},
        "vars": {}, "invokes": [], "history": {}, "spawned": [], "timers": [], "nextInvoke": 1, "nextTimer": 1,
        "nextMessage": 1, "result": null
    });
    for machine in [None, Some("M")] {
        let mut other = |_: Value| Ok(peer.clone());
        let e = p
            .check_saved(old.clone(), machine, Some(&mut other))
            .unwrap_err();
        assert_eq!(e.reason(), "machine", "asked for {machine:?}: {e}");
    }
}

#[test]
fn a_number_s_form_does_not_matter() {
    let p = program();
    let mut v = saved(&p);
    v["snapshotVersion"] = json!(3.0);
    v["nextTimer"] = json!(3.0);
    v["timers"][0]["due"] = json!(1000.0);
    v["timers"][0]["index"] = json!(0.0);
    v["vars"]["n"] = json!(-0.0);
    v["input"]["item"]["qty"] = json!(2.0);
    let read = Saved::from_value(v.clone()).expect("integral floats are integers");
    let (checked, _) = p.check_saved(v, Some("M"), None).unwrap();
    for s in [&read, &checked] {
        let text = serde_json::to_string(s).unwrap();
        assert!(!text.contains(".0"), "an integer saved as a float: {text}");
        assert_eq!(s.vars["n"], json!(0));
        assert_eq!(s.timers[0].due, 1000);
    }
    // A float that holds an integer is saved as the integer, as the other runtime writes it.
    let (mut snap, _) = p
        .start(
            "M",
            "r1",
            json!({"item": {"sku": "s", "qty": 1}})
                .as_object()
                .unwrap()
                .clone(),
            &mut no_fns,
        )
        .unwrap();
    snap.vars.insert("n".into(), json!(2.0));
    snap.vars.insert("note".into(), json!(-0.0));
    let text = serde_json::to_string(&Saved::new(&snap, |_, delay| delay)).unwrap();
    assert!(text.contains(r#""n":2,"note":0"#), "{text}");
    // A fraction is not an integer.
    let mut v = saved(&p);
    v["nextTimer"] = json!(3.5);
    assert_eq!(reason(&p, v), "invalid");
}

#[test]
fn a_result_decides_with_the_status() {
    let p = program();
    // A running run has no result yet.
    let mut v = saved(&p);
    v["result"] = json!({"output": null});
    assert_eq!(reason(&p, v.clone()), "invalid");
    assert!(Saved::from_value(v).is_ok(), "it matches the schema");
    // A defect of a kind this runtime does not know matches the schema: the run has ended.
    let mut v = saved(&p);
    v["status"] = json!("defect");
    v["active"] = json!([]);
    v["result"] = json!({"defect": {"kind": "cosmic", "ray": 1}});
    assert_eq!(reason(&p, v.clone()), "ended");
    assert_eq!(Saved::from_value(v).unwrap_err().reason(), "ended");
}

#[test]
fn an_event_is_checked_against_the_events_the_machine_accepts() {
    let p = program();
    assert_eq!(
        p.check_event("M", json!({"_tag": "Go"})),
        Ok(json!({"_tag": "Go"}))
    );
    for bad in [
        json!({"_tag": "Stop"}),
        json!({"_tag": "Go", "extra": 1}),
        json!({"by": 1}),
        json!(7),
    ] {
        assert!(p.check_event("M", bad.clone()).is_err(), "{bad}");
    }
}

/// A callee that keeps its pending call in a variable, and a caller of it.
const CALLS: &str = r#"
machine S() {
  call ask() -> int
  var p: call ask
  event Go
  initial A
  state A { on ask -> B  set p = call }
  state B { on Go -> A  reply p done 1 }
}

machine C(s: ref S) {
  initial W
  state W { invoke s.ask() { done -> W  fail * -> W } }
}
"#;

#[test]
fn a_snapshot_with_pending_calls_is_refused_after_ref() {
    let c = roadbook_compiler::compile_source("t.road", CALLS, &Default::default());
    let p = Program::new(c.ir.expect("the chart compiles")).unwrap();
    let (s, _) = p
        .start("S", "r1", serde_json::Map::new(), &mut no_fns)
        .unwrap();
    // No pending call: no `calls` member.
    assert!(
        Saved::new(&s, |_, delay| delay)
            .to_value()
            .get("calls")
            .is_none()
    );
    let call = roadbook_core::Input::Call {
        from: "r2".into(),
        invoke: 1,
        call: "ask".into(),
        args: serde_json::Map::new(),
    };
    let (s, _) = p.step(&s, call, &mut no_fns);
    let pending = Saved::new(&s, |_, delay| delay).to_value();
    assert_eq!(
        pending["calls"],
        json!([{"from": "r2", "invoke": 1, "call": "ask"}])
    );
    assert_eq!(Saved::from_value(pending.clone()).unwrap().calls.len(), 1);
    assert_eq!(reason(&p, pending.clone()), "calls");
    // Answered (as a later step would), the same snapshot is restored, the pending-call
    // value in `p` and all: a later `reply` through it is `answered`.
    let mut answered = pending.clone();
    answered.as_object_mut().unwrap().remove("calls");
    assert_eq!(reason(&p, answered.clone()), "ok");
    let mut bad = answered.clone();
    bad["vars"]["p"] = json!({"from": "r2"});
    assert_eq!(reason(&p, bad), "invalid");
    let mut bad = answered.clone();
    bad["vars"]["p"] = json!({"from": "r2", "invoke": 0});
    assert_eq!(reason(&p, bad), "invalid");
    let (saved, _) = p.check_saved(answered, None, None).unwrap();
    let (s, _) = p.restore(&saved, "r9", 0, None, &mut no_fns).unwrap();
    assert!(s.calls.is_empty());
    let (s, cmds) = p.step(
        &s,
        roadbook_core::Input::Event {
            value: json!({"_tag": "Go"}),
        },
        &mut no_fns,
    );
    assert_eq!(s.status, Status::Defect);
    assert!(matches!(
        cmds.last(),
        Some(Command::Trace {
            record: Record::Defect {
                defect: Defect::Answered { .. }
            }
        })
    ));
    // `ref` is checked first: a caller holds one.
    let (s, _) = p
        .start(
            "C",
            "r2",
            json!({"s": "r1"}).as_object().unwrap().clone(),
            &mut no_fns,
        )
        .unwrap();
    let mut caller = Saved::new(&s, |_, delay| delay).to_value();
    assert_eq!(caller["invokes"][0]["src"], "s.ask");
    caller["calls"] = pending["calls"].clone();
    assert_eq!(reason(&p, caller), "ref");
}

const HISTORY: &str = r#"
machine H() {
  event Go
  event Out

  initial P

  state P {
    initial A
    history S
    deep history D

    state A { on Go -> B }
    state B {
      initial B1
      state B1 {}
      state B2 {}
    }

    on Out -> Q
  }

  parallel state Q {
    history S
    deep history D

    state X {
      initial X1
      state X1 {}
      state X2 {}
    }
    state Y {
      initial Y1
      state Y1 {}
    }
  }
}
"#;

/// A saved snapshot of a fresh run of `H`, in `P.A`, with nothing recorded.
fn history_saved(p: &Program) -> Value {
    let (snap, _) = p.start("H", "r1", Default::default(), &mut no_fns).unwrap();
    Saved::new(&snap, |_, delay| delay).to_value()
}

#[test]
fn history_is_checked_against_the_history_states() {
    let c = roadbook_compiler::compile_source("h.road", HISTORY, &Default::default());
    let p = Program::new(c.ir.expect("the chart compiles")).unwrap();
    let base = history_saved(&p);
    assert_eq!(base["snapshotVersion"], 4);
    assert_eq!(base["history"], json!({}));
    let with = |history: Value| {
        let mut v = base.clone();
        v["history"] = history;
        reason(&p, v)
    };
    let cases = [
        // Keys: only history states.
        (json!({"H.P.A": ["H.P.B"]}), "invalid"),
        (json!({"H.P.Nope": ["H.P.B"]}), "invalid"),
        // Shallow, of a compound parent: one child.
        (json!({"H.P.S": ["H.P.B"]}), "ok"),
        (json!({"H.P.S": ["H.P.A", "H.P.B"]}), "invalid"),
        (json!({"H.P.S": ["H.P.B.B1"]}), "invalid"),
        (json!({"H.P.S": ["H.Q"]}), "invalid"),
        (json!({"H.P.S": ["H.P.Nope"]}), "invalid"),
        (json!({"H.P.S": []}), "invalid"),
        // Shallow, of a parallel parent: every region, in document order.
        (json!({"H.Q.S": ["H.Q.X", "H.Q.Y"]}), "ok"),
        (json!({"H.Q.S": ["H.Q.Y", "H.Q.X"]}), "invalid"),
        (json!({"H.Q.S": ["H.Q.Y"]}), "invalid"),
        // Deep: the leaves of a configuration below the parent.
        (json!({"H.P.D": ["H.P.A"]}), "ok"),
        (json!({"H.P.D": ["H.P.B.B2"]}), "ok"),
        (json!({"H.P.D": ["H.P.B"]}), "invalid"),
        (json!({"H.P.D": ["H.P.A", "H.P.B.B1"]}), "invalid"),
        (json!({"H.P.D": ["H.P.B.B1", "H.P.B.B2"]}), "invalid"),
        (json!({"H.P.D": ["H.Q.X.X1"]}), "invalid"),
        (json!({"H.Q.D": ["H.Q.X.X2", "H.Q.Y.Y1"]}), "ok"),
        (json!({"H.Q.D": ["H.Q.Y.Y1", "H.Q.X.X2"]}), "invalid"),
        (json!({"H.Q.D": ["H.Q.X.X2"]}), "invalid"),
        (
            json!({"H.Q.D": ["H.Q.X.X1", "H.Q.X.X2", "H.Q.Y.Y1"]}),
            "invalid",
        ),
        (
            json!({"H.P.S": ["H.P.B"], "H.P.D": ["H.P.B.B1"], "H.Q.S": ["H.Q.X", "H.Q.Y"]}),
            "ok",
        ),
    ];
    for (history, want) in cases {
        assert_eq!(with(history.clone()), want, "{history}");
    }

    // Version 1 has no `history` and no `spawned`, version 2 has `history`, versions 3 and 4
    // both.
    let mut v1 = base.clone();
    v1["snapshotVersion"] = json!(1);
    assert_eq!(reason(&p, v1.clone()), "invalid");
    v1.as_object_mut().unwrap().remove("history");
    assert_eq!(reason(&p, v1.clone()), "invalid");
    v1.as_object_mut().unwrap().remove("spawned");
    assert_eq!(reason(&p, v1), "ok");
    let mut v2 = base.clone();
    v2["snapshotVersion"] = json!(2);
    assert_eq!(reason(&p, v2.clone()), "invalid");
    v2.as_object_mut().unwrap().remove("spawned");
    assert_eq!(reason(&p, v2), "ok");
    let mut v3 = base.clone();
    v3["snapshotVersion"] = json!(3);
    assert_eq!(reason(&p, v3.clone()), "ok");
    v3.as_object_mut().unwrap().remove("history");
    assert_eq!(reason(&p, v3), "invalid");
}

#[test]
fn a_version_1_snapshot_restores_and_is_saved_again_as_version_4() {
    let c = roadbook_compiler::compile_source("h.road", HISTORY, &Default::default());
    let p = Program::new(c.ir.expect("the chart compiles")).unwrap();
    let mut v1 = history_saved(&p);
    v1["snapshotVersion"] = json!(1);
    v1.as_object_mut().unwrap().remove("history");
    v1.as_object_mut().unwrap().remove("spawned");
    let (saved, _) = p.check_saved(v1.clone(), None, None).unwrap();
    assert_eq!((saved.snapshot_version, &saved.history), (1, &None));
    let (snap, commands) = p.restore(&saved, "r2", 0, None, &mut no_fns).unwrap();
    // The `restored` record holds the snapshot as it was given.
    let Some(Command::Trace {
        record: Record::Restored { snapshot, .. },
    }) = commands.first()
    else {
        panic!("no restored record first: {commands:?}");
    };
    assert_eq!(snapshot.to_value(), v1);
    assert!(snap.history.is_empty());
    let again = Saved::new(&snap, |_, delay| delay).to_value();
    assert_eq!(again["snapshotVersion"], 4);
    assert_eq!(again["history"], json!({}));
    assert_eq!(again["spawned"], json!([]));
}

const KEPT: &str = r#"
task fetch(n: int) -> int

machine Tally(start: int) {
  event Add { by: int }
  var n: int = start
  initial Counting
  state Counting { after 5s -> Counting  on Add set n = n + event.by }
}

machine Desk(start: int) {
  initial Open
  state Open {
    invoke Tally(start) as tally { done -> Closed }
  }
  final state Closed {}
}

machine Fetcher() {
  initial Fetching
  state Fetching { invoke fetch(1) { done -> Done } }
  final state Done {}
}

machine Pool() {
  children slots: Tally by int
  initial Open
  state Open {}
}

machine Peer(other: ref Tally?) {
  initial X
  state X {}
}

machine Lobby() {
  var t: ref Tally?
  initial S
  state S { invoke Peer(t) { done -> E } }
  final state E {}
}
"#;

/// A saved `Desk` whose running `Tally` is nested in its invoke, and the program.
fn desk() -> (Program, Value) {
    let c = roadbook_compiler::compile_source("k.road", KEPT, &Default::default());
    assert!(!c.has_errors(), "{}", c.render());
    let p = Program::new(c.ir.unwrap()).unwrap();
    let input = json!({"start": 1}).as_object().unwrap().clone();
    let (desk, _) = p.start("Desk", "r1", input.clone(), &mut no_fns).unwrap();
    let (tally, _) = p.start("Tally", "r1.1", input, &mut no_fns).unwrap();
    let mut v = Saved::new(&desk, |_, delay| delay).to_value();
    v["invokes"][0]["child"] = Saved::new(&tally, |_, delay| delay + 100).to_value();
    (p, v)
}

#[test]
fn nested_snapshots_are_checked_with_the_tree() {
    let (p, base) = desk();
    assert_eq!(reason(&p, base.clone()), "ok");
    let with = |f: &dyn Fn(&mut Value)| {
        let mut v = base.clone();
        f(&mut v);
        reason(&p, v)
    };
    let other = format!("sha256:{}", "0".repeat(64));
    // By the schema: a nested snapshot is of version 4, and only version 4 nests one.
    assert_eq!(
        with(&|v| v["invokes"][0]["child"]["snapshotVersion"] = json!(3)),
        "invalid"
    );
    assert_eq!(with(&|v| v["snapshotVersion"] = json!(3)), "invalid");
    assert_eq!(
        with(&|v| v["invokes"][0]["child"]["extra"] = json!(1)),
        "invalid"
    );
    // A stale nested hash is `ir`, before an ended status or calls of the top-level one.
    assert_eq!(
        with(&|v| v["invokes"][0]["child"]["ir"] = json!(other)),
        "ir"
    );
    assert_eq!(
        with(&|v| {
            v["invokes"][0]["child"]["ir"] = json!(other);
            v["status"] = json!("cancelled");
        }),
        "ir"
    );
    // Its status, calls, machine and run are `invalid`, after the top-level reasons.
    assert_eq!(
        with(&|v| v["invokes"][0]["child"]["status"] = json!("cancelled")),
        "invalid"
    );
    let calls = json!([{"from": "r9", "invoke": 1, "call": "c"}]);
    assert_eq!(
        with(&|v| v["invokes"][0]["child"]["calls"] = calls.clone()),
        "invalid"
    );
    assert_eq!(
        with(&|v| {
            v["invokes"][0]["child"]["calls"] = calls.clone();
            v["calls"] = calls.clone();
        }),
        "calls"
    );
    assert_eq!(
        with(&|v| v["invokes"][0]["child"]["run"] = json!("r1.2")),
        "invalid"
    );
    assert_eq!(
        with(&|v| v["invokes"][0]["child"]["run"] = json!("r2.1")),
        "invalid"
    );
    let desk = base.clone();
    assert_eq!(
        with(&|v| {
            let mut d = desk.clone();
            d["run"] = json!("r1.1");
            d.as_object_mut().unwrap().remove("invokes");
            d["invokes"] = json!([]);
            d["ir"] = desk["ir"].clone();
            v["invokes"][0]["child"] = d;
        }),
        // Of `Desk`, but its hash is checked against `Tally`'s first.
        "ir"
    );
    // It must fit its own machine.
    assert_eq!(
        with(&|v| v["invokes"][0]["child"]["vars"] = json!({"n": "x"})),
        "invalid"
    );
    assert_eq!(
        with(&|v| v["invokes"][0]["child"]["active"] = json!(["Tally"])),
        "invalid"
    );
    // A migration is called once for the whole tree when a nested hash is stale.
    let mut stale = base.clone();
    stale["invokes"][0]["child"]["ir"] = json!(other);
    let mut calls = 0;
    let mut migrate = |mut v: Value| {
        calls += 1;
        v["invokes"][0]["child"]["ir"] = base["invokes"][0]["child"]["ir"].clone();
        Ok(v)
    };
    let (saved, migrated) = p.check_saved(stale, None, Some(&mut migrate)).unwrap();
    assert_eq!(calls, 1);
    assert_eq!(migrated.as_deref(), base["ir"].as_str());
    assert!(saved.invokes[0].child.is_some());
}

#[test]
fn a_child_on_a_task_or_holding_a_ref_is_refused() {
    let (p, base) = desk();
    let input = json!({}).as_object().unwrap().clone();
    let (fetcher, _) = p.start("Fetcher", "r1", input, &mut no_fns).unwrap();
    let mut v = Saved::new(&fetcher, |_, delay| delay).to_value();
    v["invokes"][0]["child"] = base["invokes"][0]["child"].clone();
    assert_eq!(reason(&p, v), "invalid");
    // An entry that starts a machine with a `ref` parameter, with a snapshot nested in it, is
    // `ref`: by the machine it starts, not the nested snapshot's `machine`.
    let none = json!({}).as_object().unwrap().clone();
    let (lobby, _) = p.start("Lobby", "r1", none, &mut no_fns).unwrap();
    let other = json!({"other": null}).as_object().unwrap().clone();
    let (peer, _) = p.start("Peer", "r1.1", other, &mut no_fns).unwrap();
    let mut v = Saved::new(&lobby, |_, delay| delay).to_value();
    v["invokes"][0]["child"] = Saved::new(&peer, |_, delay| delay).to_value();
    assert_eq!(reason(&p, v.clone()), "ref");
    v["invokes"][0]["child"] = base["invokes"][0]["child"].clone();
    v["invokes"][0]["child"]["ir"] = Saved::new(&peer, |_, delay| delay).to_value()["ir"].clone();
    assert_eq!(reason(&p, v), "ref");
    // A `Peer` nested in the entry of a `Tally` is of another machine: `invalid`, not `ref`.
    let mut v = base.clone();
    let mut nested = Saved::new(&peer, |_, delay| delay).to_value();
    nested["ir"] = base["invokes"][0]["child"]["ir"].clone();
    v["invokes"][0]["child"] = nested;
    assert_eq!(reason(&p, v), "invalid");
}

#[test]
fn restore_passes_each_nested_snapshot_to_its_start_command() {
    let (p, base) = desk();
    let (saved, _) = p.check_saved(base.clone(), None, None).unwrap();
    let (_, commands) = p.restore(&saved, "r2", 0, None, &mut no_fns).unwrap();
    let Some(Command::Trace {
        record: Record::Restored { snapshot, .. },
    }) = commands.first()
    else {
        panic!("no restored record first: {commands:?}");
    };
    // The record shows the snapshot without the nested one.
    assert!(!snapshot.has_children());
    assert_eq!(
        snapshot.to_value()["invokes"],
        json!([{"id": 1, "state": "Desk.Open", "src": "Tally"}])
    );
    let start = commands
        .iter()
        .find_map(|c| match c {
            Command::InvokeStart { saved, .. } => Some(saved.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(start.unwrap().to_value(), base["invokes"][0]["child"]);
    // A spawned child's too.
    let input = json!({}).as_object().unwrap().clone();
    let (pool, _) = p.start("Pool", "r1", input, &mut no_fns).unwrap();
    let mut v = Saved::new(&pool, |_, delay| delay).to_value();
    let mut child = base["invokes"][0]["child"].clone();
    child["run"] = json!("r1.3");
    v["nextInvoke"] = json!(4);
    v["spawned"] =
        json!([{"id": 3, "children": "slots", "key": 7, "args": {"start": 1}, "child": child}]);
    let (saved, _) = p.check_saved(v, None, None).unwrap();
    let (_, commands) = p.restore(&saved, "r2", 0, None, &mut no_fns).unwrap();
    let spawn = commands.iter().find_map(|c| match c {
        Command::SpawnStart { saved, .. } => Some(saved.clone()),
        _ => None,
    });
    assert_eq!(spawn.unwrap().unwrap().to_value(), child);
    // `start` and `step` never carry one.
    let input = json!({"start": 1}).as_object().unwrap().clone();
    let (_, commands) = p.start("Desk", "r3", input, &mut no_fns).unwrap();
    assert!(
        commands
            .iter()
            .all(|c| !matches!(c, Command::InvokeStart { saved: Some(_), .. }))
    );
}
