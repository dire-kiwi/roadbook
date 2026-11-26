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
    Saved::new(&snap, |t| t.delay).to_value()
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
        reason(&p, with(&|v| v["snapshotVersion"] = json!(2))),
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
    let v = Saved::new(&snap, |t| t.delay).to_value();
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
    let s = Saved::new(&snap, |t| t.delay);
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
        "snapshotVersion": 1, "machine": "Peer", "run": "r9", "ir": peer_hash,
        "status": "running", "active": ["Peer", "Peer.X"], "input": {"other": "r1"},
        "vars": {}, "invokes": [], "timers": [], "nextInvoke": 1, "nextTimer": 1,
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
    v["snapshotVersion"] = json!(1.0);
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
    let text = serde_json::to_string(&Saved::new(&snap, |t| t.delay)).unwrap();
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
