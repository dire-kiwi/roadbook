//! Checks every committed conformance case: schemas, canonical form,
//! structure, and (once the compiler has generated it) the IR.
//!
//! Run with `ROADBOOK_BLESS=1` to rewrite the `ir` member of every `started`
//! record from the committed IR files; `regen-ir.sh` does this after
//! regenerating them. Nothing else in the traces is ever rewritten.

use std::collections::BTreeSet;

use roadbook_conformance::{
    LoadedCase, Status, Step, case_validator, conformance_dir, lint::lint, load_all,
    restamp_hashes, schema_errors, to_lines, trace_validator,
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

#[test]
fn traces_are_well_formed() {
    for c in cases() {
        let problems = lint(&c.records, c.case.status, c.ir.as_ref());
        assert!(problems.is_empty(), "{}: {problems:#?}", c.name);
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
            .filter(|r| r["kind"] == "invoke.started")
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
        assert!(
            ir.machines.contains_key(&c.case.machine),
            "{}: the IR has no machine {}",
            c.name,
            c.case.machine
        );
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
    ] {
        assert!(kinds.contains(k), "no expected trace has a `{k}` record");
    }
}

#[test]
fn trace_schema_rejects_malformed_records() {
    let v = trace_validator();
    let good = json!({"t": 0, "run": "r1", "kind": "entered", "state": "Checkout"});
    assert!(v.is_valid(&good));
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
    for bad in [missing, extra, status, two_outcomes, lonely] {
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
        let problems = lint(&c.records, c.case.status, Some(&ir));
        assert!(problems.is_empty(), "{}: {problems:#?}", c.name);
        checked += 1;
    }
    assert!(checked >= 4);
}
