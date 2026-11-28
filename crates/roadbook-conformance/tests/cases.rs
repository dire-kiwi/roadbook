//! Checks every committed conformance case: schemas, canonical form,
//! structure, and (once the compiler has generated it) the IR.
//!
//! Run with `ROADBOOK_BLESS=1` to rewrite the `ir` member of every `started`
//! record from the committed IR files; `regen-ir.sh` does this after
//! regenerating them. Nothing else in the traces is ever rewritten.

use std::collections::BTreeSet;

use roadbook_conformance::{
    FEATURES, LoadedCase, ROOT_RUN, SNAPSHOT_SUFFIX, Status, Step, case_validator, conformance_dir,
    lint::{lint, lint_runs},
    load_all, restamp_hashes, restamp_snapshot, schema_errors, snapshot_text, snapshot_validator,
    to_lines, trace_validator,
};
use serde_json::{Value, json};

fn cases() -> Vec<LoadedCase> {
    let cases = load_all(&conformance_dir()).expect("every case loads");
    assert!(cases.len() >= 10, "found only {} cases", cases.len());
    cases
}

#[test]
fn schemas_are_valid_draft_2020_12() {
    for text in [
        roadbook_conformance::CASE_SCHEMA,
        roadbook_conformance::TRACE_SCHEMA,
        roadbook_conformance::SNAPSHOT_SCHEMA,
    ] {
        let schema: Value = serde_json::from_str(text).unwrap();
        jsonschema::meta::validate(&schema).expect("a valid draft 2020-12 schema");
    }
}

#[test]
fn cases_match_the_case_schema() {
    let v = case_validator();
    for c in cases() {
        let errors = schema_errors(&v, &c.case_json);
        assert!(errors.is_empty(), "{}: {errors:#?}", c.name);
    }
}

#[test]
fn trace_records_match_the_trace_schema() {
    let v = trace_validator();
    for c in cases() {
        for (i, r) in c.records.iter().enumerate() {
            let errors = schema_errors(&v, r);
            assert!(errors.is_empty(), "{} line {}: {errors:#?}", c.name, i + 1);
        }
    }
}

#[test]
fn traces_are_canonical() {
    for c in cases() {
        assert!(!c.expected.is_empty(), "{}: empty trace", c.name);
        assert_eq!(
            to_lines(&c.records),
            c.expected,
            "{}: trace.jsonl is not one canonical record per line",
            c.name
        );
    }
}

fn statuses(c: &LoadedCase) -> Vec<(String, Status)> {
    c.case.top_level_statuses()
}

/// Every snapshot file is one saved snapshot and a line feed, and matches the
/// snapshot schema; so does the snapshot of every `restored` record. A file a
/// runtime writes or a migration gives is in canonical JSON; one that is only
/// restored stands for a snapshot from elsewhere, in any JSON form. The script
/// names exactly the files there are.
#[test]
fn snapshots_are_canonical_and_valid() {
    let v = snapshot_validator();
    for c in cases() {
        let names = c.case.snapshot_names();
        for (name, text) in &c.snapshots {
            let value: Value = serde_json::from_str(text)
                .unwrap_or_else(|e| panic!("{}: {name}{SNAPSHOT_SUFFIX}: {e}", c.name));
            let written = names.iter().any(|(n, current)| n == name && *current);
            assert!(
                text.ends_with('\n') && text.lines().count() == 1,
                "{}: {name}{SNAPSHOT_SUFFIX} is not one line and a line feed",
                c.name
            );
            if written {
                assert_eq!(
                    &snapshot_text(&value),
                    text,
                    "{}: {name}{SNAPSHOT_SUFFIX} is not canonical JSON and a line feed",
                    c.name
                );
            }
            // A file only restored to be refused as `invalid` may break the schema on purpose.
            let refused_only = !written
                && c.case.script.iter().all(|s| match s {
                    Step::Restore(r) if &r.name == name => r.refused.as_deref() == Some("invalid"),
                    _ => true,
                });
            let errors = schema_errors(&v, &value);
            assert!(
                errors.is_empty() || refused_only,
                "{}: {name}: {errors:#?}",
                c.name
            );
        }
        for r in c.records.iter().filter(|r| r["kind"] == "restored") {
            let errors = schema_errors(&v, &r["snapshot"]);
            assert!(
                errors.is_empty(),
                "{}: restored snapshot: {errors:#?}",
                c.name
            );
        }
        let named: BTreeSet<String> = c
            .case
            .snapshot_names()
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        let files: BTreeSet<String> = c.snapshots.keys().cloned().collect();
        assert_eq!(
            named, files,
            "{}: the snapshot files are not those the script names",
            c.name
        );
    }
}

#[test]
fn traces_are_well_formed() {
    for c in cases() {
        let problems = lint_runs(&c.records, &statuses(&c), c.ir.as_ref());
        assert!(problems.is_empty(), "{}: {problems:#?}", c.name);
    }
}

/// Start steps name `r2`, `r3`, … in order; every `run` of an event or
/// cancel step is a top-level run started before it; a case requires
/// `messaging` exactly when a machine of its chart uses it, and so does a
/// case with more than one top-level run.
#[test]
fn top_level_runs_are_started_in_order() {
    for c in cases() {
        let mut started = vec![ROOT_RUN.to_owned()];
        for f in &c.case.requires {
            assert!(
                FEATURES.contains(&f.as_str()),
                "{}: unknown feature {f}",
                c.name
            );
        }
        let mut steps: Vec<&Step> = c.case.script.iter().collect();
        steps.reverse();
        while let Some(s) = steps.pop() {
            match s {
                Step::Start(st) => {
                    let want = format!("r{}", started.len() + 1);
                    assert_eq!(st.run, want, "{}: start steps name r2, r3, …", c.name);
                    started.push(st.run.clone());
                }
                Step::Restore(r) => {
                    if let Some(run) = &r.run {
                        let want = format!("r{}", started.len() + 1);
                        assert_eq!(*run, want, "{}: restore steps name r2, r3, …", c.name);
                        started.push(run.clone());
                    }
                }
                Step::Snapshot(st) | Step::Suspend(st) => assert!(
                    started.iter().any(|r| r == st.run()),
                    "{}: snapshot of {}, which has not been started",
                    c.name,
                    st.run()
                ),
                Step::Event(e) => assert!(
                    started.iter().any(|r| r == e.run()),
                    "{}: event for {}, which has not been started",
                    c.name,
                    e.run()
                ),
                Step::Cancel(Some(r)) => assert!(
                    started.contains(r),
                    "{}: cancel of {r}, which has not been started",
                    c.name
                ),
                Step::Together(inner) => steps.extend(inner.iter().rev()),
                _ => {}
            }
        }
        let requires = |f: &str| c.case.requires.iter().any(|r| r == f);
        let messaging = requires("messaging");
        let snapshots = !c.case.snapshot_names().is_empty();
        assert_eq!(
            snapshots,
            requires("snapshots"),
            "{}: `requires` must list `snapshots` exactly when the script saves or restores",
            c.name
        );
        let starts = c.case.script.iter().any(|s| matches!(s, Step::Start(_)));
        if starts {
            assert!(
                messaging,
                "{}: several top-level runs started from the machine need `messaging`",
                c.name
            );
        }
        if let Some(ir) = &c.ir {
            let uses = ir.machines.values().any(|m| m.messaging().is_some());
            assert_eq!(
                uses || starts,
                messaging,
                "{}: `requires` must list `messaging` exactly when a machine of its chart uses it, or the script starts runs",
                c.name
            );
            fn has_parallel(s: &roadbook_ir::State) -> bool {
                s.kind == roadbook_ir::StateKind::Parallel || s.states.iter().any(has_parallel)
            }
            let parallel = ir.machines.values().any(|m| has_parallel(&m.root));
            assert_eq!(
                parallel,
                requires("parallel"),
                "{}: `requires` must list `parallel` exactly when its chart has a parallel state",
                c.name
            );
            fn has_call(s: &roadbook_ir::State) -> bool {
                s.invoke
                    .as_ref()
                    .is_some_and(|i| i.kind == roadbook_ir::InvokeKind::Call)
                    || s.states.iter().any(has_call)
            }
            let calls = ir
                .machines
                .values()
                .any(|m| !m.calls.is_empty() || has_call(&m.root));
            assert_eq!(
                calls,
                requires("calls"),
                "{}: `requires` must list `calls` exactly when a machine of its chart declares or makes a call",
                c.name
            );
            if calls {
                assert!(messaging, "{}: `calls` needs `messaging` too", c.name);
            }
            let spawn = ir.machines.values().any(|m| !m.children.is_empty());
            assert_eq!(
                spawn,
                requires("spawn"),
                "{}: `requires` must list `spawn` exactly when its chart has a `children` collection",
                c.name
            );
            fn has_history(s: &roadbook_ir::State) -> bool {
                s.kind.is_history() || s.states.iter().any(has_history)
            }
            let history = ir.machines.values().any(|m| has_history(&m.root));
            assert_eq!(
                history,
                requires("history"),
                "{}: `requires` must list `history` exactly when its chart has a history state",
                c.name
            );
            fn has_ended(s: &roadbook_ir::State) -> bool {
                s.transitions
                    .iter()
                    .any(|t| matches!(t.trigger, roadbook_ir::Trigger::Ended { .. }))
                    || s.states.iter().any(has_ended)
            }
            let ended = ir.machines.values().any(|m| has_ended(&m.root));
            assert_eq!(
                ended,
                requires("ended"),
                "{}: `requires` must list `ended` exactly when a machine of its chart has an `on name.ended`",
                c.name
            );
            if ended {
                assert!(messaging, "{}: `ended` needs `messaging` too", c.name);
            }
            fn has_raise(s: &roadbook_ir::State) -> bool {
                s.entry
                    .iter()
                    .chain(&s.exit)
                    .chain(s.transitions.iter().flat_map(|t| &t.effects))
                    .any(|e| matches!(e, roadbook_ir::Effect::Raise { .. }))
                    || s.states.iter().any(has_raise)
            }
            let raise = ir
                .machines
                .values()
                .any(|m| has_raise(&m.root) || !m.internal_events.is_empty());
            assert_eq!(
                raise,
                requires("raise"),
                "{}: `requires` must list `raise` exactly when a machine of its chart raises an event or declares an internal one",
                c.name
            );
        }
    }
}

#[test]
fn charts_exist_and_scripts_name_known_things() {
    for c in cases() {
        assert!(
            c.chart_path().is_file(),
            "{}: no chart {}",
            c.name,
            c.chart_path().display()
        );
        let started: BTreeSet<&str> = c
            .records
            .iter()
            .filter(|r| r["kind"] == "invoke.started" || r["kind"] == "spawn.started")
            .filter_map(|r| r["src"].as_str())
            .collect();
        let mut steps: Vec<&Step> = c.case.script.iter().collect();
        while let Some(s) = steps.pop() {
            match s {
                Step::Task(t) => assert!(
                    started.contains(t.task.as_str()),
                    "{}: the script ends {}, which is never started",
                    c.name,
                    t.task
                ),
                Step::Together(inner) => steps.extend(inner),
                _ => {}
            }
        }
    }
}

#[test]
fn ir_when_present() {
    let mut missing = Vec::new();
    for c in cases() {
        let Some(ir) = &c.ir else {
            missing.push(c.ir_path.display().to_string());
            continue;
        };
        for (_, m, _, _) in c.case.top_level_runs() {
            assert!(
                ir.machines.contains_key(m),
                "{}: the IR has no machine {m}",
                c.name
            );
        }
        for m in &c.case.stub {
            assert!(
                ir.machines.contains_key(m),
                "{}: stubbed {m} is not a machine",
                c.name
            );
        }
        if std::env::var_os("ROADBOOK_BLESS").is_some() {
            let text = to_lines(&restamp_hashes(&c.records, ir));
            std::fs::write(c.dir.join("trace.jsonl"), text).unwrap();
            for (name, current) in c.case.snapshot_names() {
                let Some(text) = c.snapshots.get(&name) else {
                    continue;
                };
                if !current {
                    continue;
                }
                let value: Value = serde_json::from_str(text).unwrap();
                let file = c.dir.join(format!("{name}{SNAPSHOT_SUFFIX}"));
                std::fs::write(file, snapshot_text(&restamp_snapshot(&value, ir))).unwrap();
            }
        }
    }
    if !missing.is_empty() {
        // Tolerated until the compiler exists; spec/conformance/regen-ir.sh
        // generates them.
        eprintln!("IR not generated yet: {missing:?}");
    }
}

#[test]
fn every_status_and_record_kind_is_covered() {
    let all = cases();
    let statuses: BTreeSet<String> = all.iter().map(|c| format!("{:?}", c.case.status)).collect();
    for s in ["Done", "Failed", "Cancelled", "Defect"] {
        assert!(statuses.contains(s), "no case ends with status {s}");
    }
    let kinds: BTreeSet<&str> = all
        .iter()
        .flat_map(|c| c.records.iter())
        .filter_map(|r| r["kind"].as_str())
        .collect();
    for k in [
        "started",
        "entered",
        "exited",
        "event",
        "dropped",
        "stale",
        "transition",
        "set",
        "action",
        "invoke.started",
        "invoke.cancelled",
        "timer.started",
        "timer.cancelled",
        "done",
        "failed",
        "cancelled",
        "defect",
        "sent",
        "emitted",
        "undelivered",
        "restored",
        "suspended",
        "replied",
        "raised",
        "raise.scheduled",
        "raise.cancelled",
    ] {
        assert!(kinds.contains(k), "no expected trace has a `{k}` record");
    }
    let inputs: BTreeSet<&str> = all
        .iter()
        .flat_map(|c| c.records.iter())
        .filter_map(|r| r["event"]["kind"].as_str())
        .collect();
    for k in ["message", "emitted", "call", "raised"] {
        assert!(inputs.contains(k), "no expected trace takes a `{k}` input");
    }
}

#[test]
fn trace_schema_rejects_malformed_records() {
    let v = trace_validator();
    let good = json!({"t": 0, "run": "r1", "kind": "entered", "state": "Checkout"});
    assert!(v.is_valid(&good));
    for good in [
        json!({"t": 0, "run": "r2", "kind": "sent", "message": 1, "to": "r1.3", "value": {"_tag": "A"}}),
        json!({"t": 0, "run": "r1", "kind": "undelivered", "message": 2, "to": null, "value": {"_tag": "A"}}),
        json!({"t": 0, "run": "r1", "kind": "event", "event": {"kind": "emitted", "from": "r1.1", "invoke": 1, "message": 1, "value": {"_tag": "A"}}}),
        json!({"t": 0, "run": "r1", "kind": "stale", "event": {"kind": "message", "from": "r2", "message": 1, "value": {"_tag": "A"}}}),
    ] {
        assert!(v.is_valid(&good), "should be accepted: {good}");
    }
    let child = json!({"t": 0, "run": "r1.2", "parent": "r1", "kind": "cancelled"});
    assert!(v.is_valid(&child));
    for bad in [
        json!({"t": 0, "run": "r1", "kind": "entered"}),
        json!({"t": 0, "run": "r1", "kind": "entered", "state": "Checkout", "extra": 1}),
        json!({"t": -1, "run": "r1", "kind": "entered", "state": "Checkout"}),
        json!({"t": 0, "run": "r1", "kind": "entered", "state": "checkout"}),
        json!({"t": 0, "run": "r1", "kind": "nonsense"}),
        json!({"t": 0, "run": "r1.2", "kind": "cancelled"}),
        json!({"t": 0, "run": "r1", "parent": "r0", "kind": "cancelled"}),
        json!({"t": 0, "run": "r1", "kind": "transition", "from": "A", "index": 0}),
        json!({"t": 0, "run": "r1", "kind": "event", "event": {"kind": "after"}}),
        json!({"t": 0, "run": "r1", "kind": "event", "event": {"kind": "cancel"}}),
        json!({"t": 0, "run": "r1", "kind": "failed", "state": "A", "error": {"reason": "x"}}),
        json!({"t": 0, "run": "r1", "kind": "defect", "defect": {"kind": "host"}}),
        json!({"t": 0, "run": "r1", "kind": "started", "machine": "A", "ir": "sha256:abc", "input": {}}),
        json!({"t": 0, "run": "r1", "kind": "sent", "message": 1, "value": {"_tag": "A"}}),
        json!({"t": 0, "run": "r1", "kind": "sent", "message": 0, "to": "r2", "value": {"_tag": "A"}}),
        json!({"t": 0, "run": "r1", "kind": "emitted", "message": 1, "to": "r2", "value": {"_tag": "A"}}),
        json!({"t": 0, "run": "r1", "kind": "undelivered", "message": 1, "value": {"_tag": "A"}}),
        json!({"t": 0, "run": "r1", "kind": "event", "event": {"kind": "message", "from": "r2", "message": 1}}),
        json!({"t": 0, "run": "r1", "kind": "event", "event": {"kind": "emitted", "from": "r2", "message": 1, "value": {"_tag": "A"}}}),
    ] {
        assert!(!v.is_valid(&bad), "should be rejected: {bad}");
    }
}

#[test]
fn case_schema_rejects_malformed_cases() {
    let v = case_validator();
    let c = &cases()[0];
    assert!(v.is_valid(&c.case_json));
    let mut missing = c.case_json.clone();
    missing.as_object_mut().unwrap().remove("stub");
    let mut extra = c.case_json.clone();
    extra["expect"] = json!("trace.jsonl");
    let mut status = c.case_json.clone();
    status["status"] = json!("finished");
    let mut two_outcomes = c.case_json.clone();
    two_outcomes["script"] = json!([{"task": "t", "done": 1, "fail": {"_tag": "E"}}]);
    let mut lonely = c.case_json.clone();
    lonely["script"] = json!([{"together": [{"event": {"_tag": "A"}}]}]);
    let mut feature = c.case_json.clone();
    feature["requires"] = json!(["telepathy"]);
    let mut start = c.case_json.clone();
    start["script"] = json!([{"start": "A", "run": "r2.1", "input": {}, "status": "done"}]);
    let mut event_to_child = c.case_json.clone();
    event_to_child["script"] = json!([{"event": {"_tag": "A"}, "run": "r1.2"}]);
    for bad in [
        missing,
        extra,
        status,
        two_outcomes,
        lonely,
        feature,
        start,
        event_to_child,
    ] {
        assert!(!v.is_valid(&bad), "should be rejected: {bad}");
    }
}

#[test]
fn lint_catches_mistakes() {
    let c = cases()
        .into_iter()
        .find(|c| c.name == "checkout-retry")
        .unwrap();
    assert!(lint(&c.records, Status::Done, None).is_empty());
    // Wrong status, a dropped exit, and an invoke cancelled after its outcome.
    assert!(!lint(&c.records, Status::Failed, None).is_empty());
    let mut no_exit = c.records.clone();
    let i = no_exit.iter().position(|r| r["kind"] == "exited").unwrap();
    no_exit.remove(i);
    assert!(!lint(&no_exit, Status::Done, None).is_empty());
    let mut twice = c.records.clone();
    let i = twice
        .iter()
        .position(|r| r["kind"] == "transition")
        .unwrap();
    twice.insert(
        i,
        json!({"t": 0, "run": "r1", "kind": "invoke.cancelled", "invoke": 1,
               "state": "Checkout.Charging", "src": "chargeCard"}),
    );
    assert!(!lint(&twice, Status::Done, None).is_empty());
}

#[test]
fn lint_catches_peer_end_mistakes() {
    let c = cases()
        .into_iter()
        .find(|c| c.name == "peer-ended-order")
        .unwrap();
    let st = statuses(&c);
    assert!(lint_runs(&c.records, &st, None).is_empty());
    let finds = |recs: &[Value], what: &str| {
        let problems = lint_runs(recs, &st, None);
        assert!(
            problems.iter().any(|p| p.contains(what)),
            "no problem with {what:?} in {problems:#?}"
        );
    };
    let i = c
        .records
        .iter()
        .position(|r| r["event"]["kind"] == "ended")
        .unwrap();
    // A status that is not how the peer ended, or not one at all.
    let mut wrong = c.records.clone();
    wrong[i]["event"]["status"] = json!("Failed");
    finds(&wrong, "but r2 was told Failed");
    wrong[i]["event"]["status"] = json!("Gone");
    finds(&wrong, "not a variant of Ended");
    // An end that is stale, and one from a run that is not top-level.
    let mut stale = c.records.clone();
    stale[i]["kind"] = json!("stale");
    finds(&stale, "never stale");
    let mut child = c.records.clone();
    child[i]["event"]["from"] = json!("r1.1");
    finds(&child, "not a top-level run");
    // Told twice: the same end dropped again.
    let mut twice = c.records.clone();
    let mut again = c.records[i].clone();
    again["kind"] = json!("dropped");
    twice.insert(i, again);
    finds(&twice, "told the end of r1 twice");
    // The emitted event taken after the end.
    let mut late = c.records.clone();
    let e = late
        .iter()
        .position(|r| r["event"]["kind"] == "emitted")
        .unwrap();
    let mut emitted = late[e].clone();
    emitted["kind"] = json!("dropped");
    late.insert(i, emitted);
    late.swap(i, i + 1);
    finds(&late, "after its end");
}

#[test]
fn lint_catches_message_mistakes() {
    let c = cases()
        .into_iter()
        .find(|c| c.name == "pair-order")
        .unwrap();
    let st = statuses(&c);
    assert!(lint_runs(&c.records, &st, None).is_empty());
    let find = |recs: &[Value], kind: &str, n: u64| {
        recs.iter()
            .position(|r| r["event"]["kind"] == kind && r["event"]["message"] == n)
            .unwrap()
    };
    // Second taken before First.
    let mut swapped = c.records.clone();
    let (a, b) = (find(&swapped, "message", 1), find(&swapped, "message", 2));
    let (va, vb) = (swapped[a]["event"].clone(), swapped[b]["event"].clone());
    swapped[a]["event"] = vb;
    swapped[b]["event"] = va;
    assert!(!lint_runs(&swapped, &st, None).is_empty());
    // A message nobody sent, and one with another value.
    let mut unsent = c.records.clone();
    let i = find(&unsent, "message", 1);
    unsent[i]["event"]["message"] = json!(7);
    assert!(!lint_runs(&unsent, &st, None).is_empty());
    let mut changed = c.records.clone();
    changed[i]["event"]["value"] = json!({"_tag": "Second"});
    assert!(!lint_runs(&changed, &st, None).is_empty());
    // A message id skipped by the sender.
    let mut skipped = c.records.clone();
    let j = skipped.iter().position(|r| r["kind"] == "sent").unwrap();
    skipped[j]["message"] = json!(2);
    assert!(!lint_runs(&skipped, &st, None).is_empty());
    // The wrong status of a top-level run that is not the root.
    let wrong = [st[0].clone(), ("r2".to_owned(), Status::Failed)];
    assert!(!lint_runs(&c.records, &wrong, None).is_empty());

    let c = cases()
        .into_iter()
        .find(|c| c.name == "peer-stopped")
        .unwrap();
    let st = statuses(&c);
    assert!(lint_runs(&c.records, &st, None).is_empty());
    // An `undelivered` that does not follow its `sent`.
    let mut moved = c.records.clone();
    let k = moved
        .iter()
        .position(|r| r["kind"] == "undelivered")
        .unwrap();
    let u = moved.remove(k);
    moved.insert(k + 1, u);
    assert!(!lint_runs(&moved, &st, None).is_empty());

    let c = cases()
        .into_iter()
        .find(|c| c.name == "child-stale-emit")
        .unwrap();
    let st = statuses(&c);
    assert!(lint_runs(&c.records, &st, None).is_empty());
    // A stale emitted event written as taken.
    let mut live = c.records.clone();
    let k = live.iter().position(|r| r["kind"] == "stale").unwrap();
    live[k]["kind"] = json!("dropped");
    assert!(!lint_runs(&live, &st, None).is_empty());
}

/// Until the compiler generates the shop charts' IR, check their cases
/// against the hand-written IR fixture of `examples/shop`, of which
/// `charts/shop` is a copy: state ids, transition indices and targets,
/// invoked names and machine hashes.
#[test]
fn shop_cases_agree_with_the_ir_fixture() {
    let fixture = include_str!("../../roadbook-ir/tests/data/place_order.ir.json");
    let ir = roadbook_ir::Ir::from_json(fixture).unwrap();
    let mut checked = 0;
    for c in cases() {
        if !c.case.chart.starts_with("charts/shop/") {
            continue;
        }
        let problems = lint_runs(&c.records, &statuses(&c), Some(&ir));
        assert!(problems.is_empty(), "{}: {problems:#?}", c.name);
        checked += 1;
    }
    assert!(checked >= 4);
}

#[test]
fn lint_catches_parallel_and_snapshot_mistakes() {
    let find = |name: &str| cases().into_iter().find(|c| c.name == name).unwrap();
    let c = find("parallel-two-regions");
    let (st, ir) = (statuses(&c), c.ir.as_ref());
    assert!(lint_runs(&c.records, &st, ir).is_empty());
    let at = |recs: &[Value], kind: &str, state: &str| {
        recs.iter()
            .position(|r| r["kind"] == kind && r["state"] == state)
            .unwrap()
    };
    // A second active child of a compound region.
    let mut two = c.records.clone();
    let i = at(&two, "entered", "Fork.P.A.A2");
    let mut extra = two[i].clone();
    extra["state"] = json!("Fork.P.A.A3");
    two.insert(i + 1, extra);
    assert!(!lint_runs(&two, &st, ir).is_empty());
    // A state that is never exited, so its region enters a second child.
    let mut stuck = c.records.clone();
    let i = at(&stuck, "exited", "Fork.P.B.B1");
    stuck.remove(i);
    assert!(!lint_runs(&stuck, &st, ir).is_empty());
    // A parallel state exited before its regions.
    let c = find("parallel-entry-exit");
    let (st, ir) = (statuses(&c), c.ir.as_ref());
    assert!(lint_runs(&c.records, &st, ir).is_empty());
    let mut early = c.records.clone();
    let i = at(&early, "exited", "Regions.Both");
    let r = early.remove(i);
    early.insert(at(&early, "exited", "Regions.Both.Lone"), r);
    assert!(!lint_runs(&early, &st, ir).is_empty());

    let c = find("snapshot-timer");
    let (st, ir) = (statuses(&c), c.ir.as_ref());
    assert!(lint_runs(&c.records, &st, ir).is_empty());
    // A restarted timer with an id the snapshot does not have.
    let mut renamed = c.records.clone();
    let i = renamed
        .iter()
        .position(|r| r["run"] == "r2" && r["kind"] == "timer.started")
        .unwrap();
    renamed[i]["timer"] = json!(5);
    assert!(!lint_runs(&renamed, &st, ir).is_empty());
    // A record after `suspended`.
    let mut after = c.records.clone();
    let i = after.iter().position(|r| r["kind"] == "suspended").unwrap();
    let mut extra = after[i].clone();
    extra["kind"] = json!("cancelled");
    after.insert(i + 1, extra);
    assert!(!lint_runs(&after, &st, ir).is_empty());
}

#[test]
fn lint_catches_task_emit_mistakes() {
    let c = cases()
        .into_iter()
        .find(|c| c.name == "task-emit-stale")
        .unwrap();
    let st = statuses(&c);
    let ir = c.ir.as_ref();
    assert!(lint_runs(&c.records, &st, ir).is_empty());
    let emits: Vec<usize> = c
        .records
        .iter()
        .enumerate()
        .filter(|(_, r)| r["event"]["kind"] == "task.emitted")
        .map(|(i, _)| i)
        .collect();
    let (stale, taken) = (emits[0], emits[1]);
    assert_eq!(c.records[stale]["kind"], "stale");
    // A stale emit written as taken, and a taken one written as stale.
    let mut live = c.records.clone();
    live[stale]["kind"] = json!("dropped");
    assert!(!lint_runs(&live, &st, ir).is_empty());
    let mut gone = c.records.clone();
    gone[taken]["kind"] = json!("stale");
    assert!(!lint_runs(&gone, &st, ir).is_empty());
    // An emit of an invoke never started, and one of an event the task does not declare.
    let mut unknown = c.records.clone();
    unknown[taken]["event"]["invoke"] = json!(9);
    assert!(!lint_runs(&unknown, &st, ir).is_empty());
    let mut undeclared = c.records.clone();
    undeclared[taken]["event"]["value"] = json!({"_tag": "Tick"});
    assert!(!lint_runs(&undeclared, &st, ir).is_empty());

    // A spawned task has no run, so no group of records.
    let c = cases()
        .into_iter()
        .find(|c| c.name == "spawn-task")
        .unwrap();
    let st = statuses(&c);
    assert!(lint_runs(&c.records, &st, c.ir.as_ref()).is_empty());
    let mut grouped = c.records.clone();
    grouped.push(
        json!({"input": {}, "ir": "x", "kind": "started", "machine": "Batch",
                        "parent": "r1", "run": "r1.1", "t": 0}),
    );
    let problems = lint_runs(&grouped, &st, None);
    assert!(
        problems.iter().any(|p| p.contains("has no run")),
        "{problems:#?}"
    );
}
