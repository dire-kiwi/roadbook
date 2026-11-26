//! The `place_order.road` fixture: schema, round trip and hashes.
//!
//! Run with `ROADBOOK_BLESS=1` to rewrite the fixture's `hash` fields after
//! editing it by hand.

use std::path::Path;

use roadbook_ir::{
    Effect, Expr, ExprKind, FunctionDecl, Ir, Literal, Span, State, Trigger, fill_hashes,
    machine_hash,
};
use serde_json::Value;

const FIXTURE: &str = include_str!("data/place_order.ir.json");
const SCHEMA: &str = include_str!("../../../spec/ir.schema.json");

fn fixture() -> Ir {
    Ir::from_json(FIXTURE).unwrap()
}

fn validator() -> jsonschema::Validator {
    let schema: Value = serde_json::from_str(SCHEMA).unwrap();
    jsonschema::meta::validate(&schema).expect("the schema is a valid draft 2020-12 schema");
    jsonschema::draft202012::new(&schema).unwrap()
}

fn schema_errors(v: &jsonschema::Validator, doc: &Value) -> Vec<String> {
    v.iter_errors(doc)
        .map(|e| format!("{} at {}", e, e.instance_path))
        .collect()
}

#[test]
fn bless_or_check_hashes() {
    let mut ir = fixture();
    fill_hashes(&mut ir).unwrap();
    if std::env::var_os("ROADBOOK_BLESS").is_some() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/place_order.ir.json");
        std::fs::write(path, ir.to_json()).unwrap();
        return;
    }
    assert_eq!(
        ir,
        fixture(),
        "stored hashes are stale: rerun with ROADBOOK_BLESS=1"
    );
}

#[test]
fn round_trip_is_byte_identical() {
    assert_eq!(fixture().to_json(), FIXTURE);
}

#[test]
fn fixture_matches_schema() {
    let doc: Value = serde_json::from_str(FIXTURE).unwrap();
    let errors = schema_errors(&validator(), &doc);
    assert!(errors.is_empty(), "{errors:#?}");
}

#[test]
fn schema_rejects_malformed_documents() {
    let v = validator();
    let doc: Value = serde_json::from_str(FIXTURE).unwrap();
    let charging = "/machines/Checkout/root/states/0";
    let cases: Vec<(String, Value)> = vec![
        ("/irVersion".into(), "0.1".into()),
        (
            "/types/LineItem/fields/0/type".into(),
            "map<string,int>".into(),
        ),
        ("/types/LineItem/fields/0/type".into(), "string[".into()),
        ("/machines/Checkout/hash".into(), "sha256:abc".into()),
        (
            format!("{charging}/transitions/0/trigger"),
            serde_json::json!({"kind": "invoke.done", "error": "X"}),
        ),
        (
            format!("{charging}/transitions/0/trigger"),
            serde_json::json!({"kind": "on"}),
        ),
        (format!("{charging}/kind"), "compound".into()),
        (format!("{charging}/target"), "Checkout.Charged".into()),
        (format!("{charging}/transitions/2/guard/op"), "not".into()),
        (
            format!("{charging}/transitions/2/guard/args/1/value"),
            1.5.into(),
        ),
        (
            format!("{charging}/transitions/0/target"),
            "Checkout..Charged".into(),
        ),
        ("/machines/Checkout/root/kind".into(), "parallel".into()),
        ("/machines/Checkout/span/col".into(), 0.into()),
    ];
    for (pointer, value) in cases {
        let mut bad = doc.clone();
        let (parent, key) = pointer.rsplit_once('/').unwrap();
        match bad.pointer_mut(parent).unwrap() {
            Value::Object(m) => {
                m.insert(key.to_owned(), value.clone());
            }
            Value::Array(a) => a[key.parse::<usize>().unwrap()] = value.clone(),
            _ => panic!("{pointer}"),
        }
        assert!(
            !v.is_valid(&bad),
            "setting {pointer} to {value} should be rejected"
        );
    }
    // And a member that is not in the reference.
    let mut bad = doc.clone();
    bad["machines"]["Checkout"]["root"]["states"][0]["transitions"][0]["note"] = "x".into();
    assert!(!v.is_valid(&bad));
    assert!(Ir::from_json(&bad.to_string()).is_err());
}

fn states_mut(s: &mut State, f: &mut impl FnMut(&mut State)) {
    f(s);
    for c in &mut s.states {
        states_mut(c, f);
    }
}

fn exprs_mut(e: &mut Expr, f: &mut impl FnMut(&mut Expr)) {
    f(e);
    match &mut e.kind {
        ExprKind::Field { of, .. } => exprs_mut(of, f),
        ExprKind::Call { args, .. } | ExprKind::Op { args, .. } => {
            args.iter_mut().for_each(|a| exprs_mut(a, f))
        }
        ExprKind::Record { fields } => fields.iter_mut().for_each(|fv| exprs_mut(&mut fv.value, f)),
        ExprKind::Lit(_) | ExprKind::Ref { .. } | ExprKind::Enum { .. } => {}
    }
}

fn state_exprs_mut(s: &mut State, f: &mut impl FnMut(&mut Expr)) {
    let effects = |effects: &mut Vec<Effect>, f: &mut dyn FnMut(&mut Expr)| {
        for e in effects {
            match e {
                Effect::Set { value, .. } => exprs_mut(value, &mut |x| f(x)),
                Effect::Do { args, .. } => {
                    args.iter_mut().for_each(|a| exprs_mut(a, &mut |x| f(x)))
                }
                Effect::Send { value, .. } | Effect::Emit { value, .. } => {
                    exprs_mut(value, &mut |x| f(x))
                }
            }
        }
    };
    effects(&mut s.entry, f);
    effects(&mut s.exit, f);
    if let Some(inv) = &mut s.invoke {
        inv.args.iter_mut().for_each(|a| exprs_mut(a, f));
    }
    for t in &mut s.transitions {
        if let Trigger::After { delay } = &mut t.trigger {
            exprs_mut(delay, f);
        }
        if let Some(g) = &mut t.guard {
            exprs_mut(g, f);
        }
        effects(&mut t.effects, f);
    }
    for e in s.output.iter_mut().chain(s.fail.iter_mut()) {
        exprs_mut(e, f);
    }
}

fn hashes(ir: &Ir) -> Vec<String> {
    ir.machines
        .keys()
        .map(|n| machine_hash(ir, n).unwrap())
        .collect()
}

#[test]
fn hash_ignores_docs_spans_and_text() {
    let before = hashes(&fixture());
    let mut ir = fixture();
    let moved = Span {
        file: 2,
        line: 99,
        col: 7,
        end_line: 100,
        end_col: 3,
    };
    for t in ir.types.values_mut() {
        t.doc = "changed".into();
        t.span = moved;
    }
    for f in ir.functions.values_mut() {
        f.doc.push_str(" more");
        f.span = moved;
    }
    for m in ir.machines.values_mut() {
        m.doc.clear();
        m.span = moved;
        for v in &mut m.vars {
            v.doc = "a var".into();
            v.span = moved;
        }
        for e in m.events.values_mut() {
            e.span = moved;
        }
        states_mut(&mut m.root, &mut |s| {
            s.doc = format!("{} doc", s.name);
            s.span = moved;
            if let Some(inv) = &mut s.invoke {
                inv.span = moved;
            }
            for t in &mut s.transitions {
                t.span = moved;
            }
            state_exprs_mut(s, &mut |e| e.text = format!("( {} )", e.text));
        });
    }
    // Sources are not part of a machine's meaning either.
    ir.sources[0].path = "elsewhere/place_order.road".into();
    // Nor is declaration order, or a declaration no machine uses.
    ir.types.reverse();
    ir.functions.insert(
        "unused".into(),
        FunctionDecl {
            params: vec![],
            returns: "int".parse().unwrap(),
            doc: String::new(),
            span: moved,
        },
    );
    assert_eq!(hashes(&ir), before);
}

#[test]
fn hash_changes_with_behaviour() {
    let ir = fixture();
    let checkout = machine_hash(&ir, "Checkout").unwrap();
    let place_order = machine_hash(&ir, "PlaceOrder").unwrap();
    assert_ne!(checkout, place_order);

    // `if attempts < 3` becomes `if attempts < 4`.
    let mut changed = fixture();
    let charging = &mut changed.machines["Checkout"].root.states[0];
    let guard = charging.transitions[2].guard.as_mut().unwrap();
    let ExprKind::Op { args, .. } = &mut guard.kind else {
        panic!("guard is an op")
    };
    args[1].kind = ExprKind::Lit(Literal::Int(4));
    assert_ne!(machine_hash(&changed, "Checkout").unwrap(), checkout);
    // PlaceOrder invokes Checkout, so its hash changes too.
    assert_ne!(machine_hash(&changed, "PlaceOrder").unwrap(), place_order);

    // A change to a type a machine uses changes its hash.
    let mut changed = fixture();
    let roadbook_ir::TypeDef::Enum { variants } = &mut changed.types["Role"].def else {
        panic!("Role is an enum")
    };
    variants.push("Guest".into());
    assert_ne!(machine_hash(&changed, "Checkout").unwrap(), checkout);

    // A change to a function signature only PlaceOrder uses leaves Checkout alone.
    let mut changed = fixture();
    changed.functions["problem"].params[1].ty = "int".parse().unwrap();
    assert_eq!(machine_hash(&changed, "Checkout").unwrap(), checkout);
    assert_ne!(machine_hash(&changed, "PlaceOrder").unwrap(), place_order);

    // A different target.
    let mut changed = fixture();
    changed.machines["Checkout"].root.states[0].transitions[3].target =
        Some("Checkout.Declined".into());
    assert_ne!(machine_hash(&changed, "Checkout").unwrap(), checkout);
}

#[test]
fn hash_handles_recursion_and_reports_missing_declarations() {
    // A machine that invokes itself (legal with an event or `after` between).
    let mut ir = fixture();
    let backoff = &mut ir.machines["Checkout"].root.states[1];
    backoff.invoke = Some(roadbook_ir::Invoke {
        kind: roadbook_ir::InvokeKind::Machine,
        src: "Checkout".into(),
        alias: None,
        args: vec![],
        span: backoff.span,
    });
    assert!(machine_hash(&ir, "Checkout").is_ok());

    let mut ir = fixture();
    ir.tasks.shift_remove("chargeCard");
    let err = machine_hash(&ir, "PlaceOrder").unwrap_err();
    assert_eq!(
        err.to_string(),
        "machine `Checkout` refers to task `chargeCard`, which the IR does not declare"
    );
    assert!(machine_hash(&ir, "Nope").is_err());
}

const ORDERS: &str = include_str!("data/orders.ir.json");

#[test]
fn messaging_fixture_round_trips_and_matches_the_schema() {
    let ir = Ir::from_json(ORDERS).unwrap();
    assert_eq!(ir.to_json(), ORDERS);
    let doc: Value = serde_json::from_str(ORDERS).unwrap();
    let v = validator();
    let errors = schema_errors(&v, &doc);
    assert!(errors.is_empty(), "{errors:#?}");
    let mut with_hashes = ir.clone();
    fill_hashes(&mut with_hashes).unwrap();
    assert_eq!(with_hashes, ir, "stored hashes are stale");

    let reserving = "/machines/Order/root/states/0";
    let cases: Vec<(String, Value)> = vec![
        (
            format!("{reserving}/entry/0/to/state"),
            "Order.Reserving".into(),
        ),
        (format!("{reserving}/entry/0/to/kind"), "child".into()),
        (
            format!("{reserving}/entry/0/value/type"),
            "Reserve[]".into(),
        ),
        (format!("{reserving}/entry/0/value/kind"), "ref".into()),
        (
            format!("{reserving}/transitions/0/trigger/from/name"),
            "Inventory".into(),
        ),
        (
            "/machines/Order/params/2/type".into(),
            "ref inventory".into(),
        ),
        ("/sources/1/package".into(), "".into()),
        (
            "/machines/Inventory/root/states/1/invoke/as".into(),
            "reserving".into(),
        ),
    ];
    for (pointer, value) in cases {
        let mut bad = doc.clone();
        let (parent, key) = pointer.rsplit_once('/').unwrap();
        bad.pointer_mut(parent).unwrap()[key] = value.clone();
        assert!(
            !v.is_valid(&bad),
            "setting {pointer} to {value} should be rejected"
        );
    }
}

#[test]
fn a_peer_contributes_only_its_interface_to_the_hash() {
    let ir = Ir::from_json(ORDERS).unwrap();
    let order = machine_hash(&ir, "Order").unwrap();
    let view = roadbook_ir::semantic_view(&ir, "Order").unwrap();
    assert_eq!(
        view["machines"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        ["Order"]
    );
    assert!(view["interfaces"]["Inventory"]["emits"]["Reserved"].is_object());
    assert!(view["types"]["Item"].is_object());

    // How the warehouse works inside does not change the order's hash.
    let mut changed = ir.clone();
    changed.machines["Inventory"].root.states[1].transitions[0].target = None;
    assert_ne!(
        machine_hash(&changed, "Inventory").unwrap(),
        machine_hash(&ir, "Inventory").unwrap()
    );
    assert_eq!(machine_hash(&changed, "Order").unwrap(), order);

    // What it accepts and emits does, and so do the types of their fields.
    let mut changed = ir.clone();
    changed.machines["Inventory"].emits["Rejected"].fields.pop();
    assert_ne!(machine_hash(&changed, "Order").unwrap(), order);
    let mut changed = ir.clone();
    let roadbook_ir::TypeDef::Record { fields } = &mut changed.types["Item"].def else {
        panic!("Item is a record")
    };
    fields[1].ty = "float".parse().unwrap();
    assert_ne!(machine_hash(&changed, "Order").unwrap(), order);

    // A peer the IR does not declare is reported.
    let mut missing = ir.clone();
    missing.machines.shift_remove("Inventory");
    assert!(machine_hash(&missing, "Order").is_err());
}
