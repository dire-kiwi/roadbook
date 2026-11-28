//! The calls conformance chart: schema, round trip, and what the hashes see of a call.

use roadbook_ir::{InvokeKind, Ir, Trigger, TypeRef, machine_hash, semantic_view};
use serde_json::Value;

const CALLS: &str = include_str!("../../../spec/conformance/charts/calls.ir.json");
const SCHEMA: &str = include_str!("../../../spec/ir.schema.json");

fn calls() -> Ir {
    Ir::from_json(CALLS).unwrap()
}

#[test]
fn the_calls_chart_matches_the_schema_and_round_trips() {
    let schema: Value = serde_json::from_str(SCHEMA).unwrap();
    let v = jsonschema::draft202012::new(&schema).unwrap();
    let doc: Value = serde_json::from_str(CALLS).unwrap();
    let errors: Vec<String> = v.iter_errors(&doc).map(|e| e.to_string()).collect();
    assert!(errors.is_empty(), "{errors:#?}");
    assert_eq!(calls().to_json(), CALLS);

    // The schema refuses what the compiler never writes.
    let mut bad = doc.clone();
    bad["machines"]["Inventory"]["calls"]["reserve"]["errors"] = serde_json::json!(["Unanswered"]);
    assert!(!v.is_valid(&bad));
    let mut bad = doc.clone();
    bad["machines"]["Order"]["root"]["states"][0]["invokes"][0]["to"] = Value::Null;
    assert!(!v.is_valid(&bad));
    let mut bad = doc;
    bad["machines"]["Inventory"]
        .as_object_mut()
        .unwrap()
        .remove("calls");
    assert!(!v.is_valid(&bad));
}

#[test]
fn the_ir_holds_calls_invokes_and_replies() {
    let ir = calls();
    let inv = &ir.machines["Inventory"];
    assert_eq!(inv.calls.keys().collect::<Vec<_>>(), ["reserve", "count"]);
    assert_eq!(inv.vars[0].ty, TypeRef::Call("reserve".into()));
    assert!(matches!(
        &inv.root.transitions[0].trigger,
        Trigger::Call { call } if call == "count"
    ));
    let order = &ir.machines["Order"].root.states[0];
    let invoke = &order.invokes[0];
    assert_eq!(invoke.kind, InvokeKind::Call);
    assert_eq!(invoke.callee(), "inventory.reserve");
    assert_eq!(
        invoke.to.as_ref().unwrap().machine.as_deref(),
        Some("Inventory")
    );
}

#[test]
fn a_caller_hash_sees_the_callee_calls_not_its_states() {
    let ir = calls();
    let order = machine_hash(&ir, "Order").unwrap();
    let view = semantic_view(&ir, "Order").unwrap();
    assert!(view["interfaces"]["Inventory"]["calls"]["reserve"].is_object());

    // Inside the callee: the caller's hash stays.
    let mut inside = calls();
    inside.machines["Inventory"].root.states[0]
        .transitions
        .pop();
    assert_eq!(machine_hash(&inside, "Order").unwrap(), order);
    assert_ne!(
        machine_hash(&inside, "Inventory").unwrap(),
        machine_hash(&ir, "Inventory").unwrap()
    );

    // What the callee answers: the caller's hash changes.
    let mut answers = calls();
    answers.machines["Inventory"].calls["count"].returns = None;
    assert_ne!(machine_hash(&answers, "Order").unwrap(), order);
}

#[test]
fn a_call_named_like_a_stripped_member_is_kept() {
    let mut ir = calls();
    let count = ir.machines["Inventory"]
        .calls
        .shift_remove("count")
        .unwrap();
    ir.machines["Inventory"].calls.insert("text".into(), count);
    let view = semantic_view(&ir, "Inventory").unwrap();
    let calls = &view["machines"]["Inventory"]["calls"];
    assert!(calls["text"].is_object(), "{calls}");
    assert!(calls["text"].get("doc").is_none());
}
