//! The `place_order.road` and `orders.road` fixtures: schema, round trip and
//! hashes.
//!
//! Run with `ROADBOOK_BLESS=1` to rewrite the fixtures' `hash` fields after
//! editing them by hand.

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
    for (file, json) in [("place_order.ir.json", FIXTURE), ("orders.ir.json", ORDERS)] {
        let stored = Ir::from_json(json).unwrap();
        let mut ir = stored.clone();
        fill_hashes(&mut ir).unwrap();
        if std::env::var_os("ROADBOOK_BLESS").is_some() {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/data")
                .join(file);
            std::fs::write(path, ir.to_json()).unwrap();
            continue;
        }
        assert_eq!(
            ir, stored,
            "{file}: stored hashes are stale: rerun with ROADBOOK_BLESS=1"
        );
    }
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
            format!("{charging}/transitions/0/targets/0"),
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
                Effect::Send { key, value, .. } => {
                    if let Some(k) = key {
                        exprs_mut(k, &mut |x| f(x));
                    }
                    exprs_mut(value, &mut |x| f(x))
                }
                Effect::Reply { call, value, .. } => {
                    exprs_mut(call, &mut |x| f(x));
                    if let Some(v) = value {
                        exprs_mut(v, &mut |x| f(x));
                    }
                }
                Effect::Emit { value, .. } => exprs_mut(value, &mut |x| f(x)),
                Effect::Spawn { key, args, .. } => {
                    exprs_mut(key, &mut |x| f(x));
                    args.iter_mut().for_each(|a| exprs_mut(a, &mut |x| f(x)))
                }
                Effect::Stop { key, .. } => exprs_mut(key, &mut |x| f(x)),
                Effect::Raise { value, delay, .. } => {
                    exprs_mut(value, &mut |x| f(x));
                    if let Some(d) = delay {
                        exprs_mut(d, &mut |x| f(x));
                    }
                }
                Effect::Cancel { .. } => {}
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
    changed.machines["Checkout"].root.states[0].transitions[3].targets =
        vec!["Checkout.Declined".into()];
    assert_ne!(machine_hash(&changed, "Checkout").unwrap(), checkout);

    // An internal transition, and a second target, are seen.
    let mut changed = fixture();
    changed.machines["Checkout"].root.states[0].transitions[3].internal = true;
    assert_ne!(machine_hash(&changed, "Checkout").unwrap(), checkout);
    let mut changed = fixture();
    changed.machines["Checkout"].root.states[0].transitions[3]
        .targets
        .push("Checkout.Declined".into());
    assert_ne!(machine_hash(&changed, "Checkout").unwrap(), checkout);
}

/// IR 0.9's `targets` and `internal` leave the hash of a machine that uses neither as it was in
/// 0.8: its semantic view writes them as 0.8's `target`.
#[test]
fn hash_writes_one_target_as_before() {
    let ir = fixture();
    let view = roadbook_ir::semantic_view(&ir, "Checkout").unwrap();
    let t = &view["machines"]["Checkout"]["root"]["states"][0]["transitions"][3];
    assert!(
        t.get("targets").is_none() && t.get("internal").is_none(),
        "{t}"
    );
    assert!(t["target"].is_string(), "{t}");
}

/// IR 0.11's `internalEvents` is left out of the view when it is empty, so a machine with no
/// internal event keeps its 0.10 hash; one with an internal event, or a `raise`, hashes
/// differently.
#[test]
fn hash_leaves_out_empty_internal_events() {
    let ir = fixture();
    let checkout = machine_hash(&ir, "Checkout").unwrap();
    let view = roadbook_ir::semantic_view(&ir, "Checkout").unwrap();
    assert!(view["machines"]["Checkout"].get("internalEvents").is_none());
    let mut changed = ir.clone();
    let span = changed.machines["Checkout"].span;
    changed.machines["Checkout"].internal_events.insert(
        "Ping".into(),
        roadbook_ir::EventDecl {
            fields: vec![],
            doc: String::new(),
            span,
        },
    );
    assert_ne!(machine_hash(&changed, "Checkout").unwrap(), checkout);
    let ping = Expr {
        ty: "Ping".parse().unwrap(),
        kind: ExprKind::Record { fields: vec![] },
        text: "Ping".into(),
    };
    let with_ping = machine_hash(&changed, "Checkout").unwrap();
    changed.machines["Checkout"].root.entry.push(Effect::Raise {
        event: "Ping".into(),
        value: ping,
        delay: None,
        name: None,
    });
    assert_ne!(machine_hash(&changed, "Checkout").unwrap(), with_ping);
}

/// The hash of a machine that uses none of IR 0.10's additions (task emits, task addresses,
/// collections of tasks) is its 0.9 hash. The stored hashes of both fixtures were written by
/// the 0.9 compiler, and `bless_or_check_hashes` checks them; this checks the view's shape.
#[test]
fn hash_writes_a_machine_collection_and_its_spawns_as_0_9_did() {
    let mut ir = fixture();
    let span = ir.machines["Checkout"].span;
    let key: roadbook_ir::TypeRef = "string".parse().unwrap();
    let place_order = &mut ir.machines["PlaceOrder"];
    place_order.children.insert(
        "checkouts".into(),
        roadbook_ir::ChildrenDecl {
            kind: roadbook_ir::ChildrenKind::Machine,
            src: "Checkout".into(),
            key: key.clone(),
            doc: String::new(),
            span,
        },
    );
    let lit = |s: &str| Expr {
        ty: key.clone(),
        kind: ExprKind::Lit(Literal::String(s.into())),
        text: format!("{s:?}"),
    };
    place_order.root.entry.push(Effect::Spawn {
        children: "checkouts".into(),
        src: "Checkout".into(),
        key: lit("a"),
        args: vec![],
    });
    let view = roadbook_ir::semantic_view(&ir, "PlaceOrder").unwrap();
    let m = &view["machines"]["PlaceOrder"];
    assert_eq!(
        m["children"]["checkouts"],
        serde_json::json!({"machine": "Checkout", "key": "string"})
    );
    let spawn = &m["root"]["entry"][0];
    assert_eq!(spawn["machine"], "Checkout", "{spawn}");
    assert!(spawn.get("src").is_none(), "{spawn}");
    // A task that emits nothing is written without `emits`.
    assert!(view["tasks"]["authenticate"].get("emits").is_none());
}

/// A task's emits, a task's address and a collection of tasks are in the view, and change
/// the hash.
#[test]
fn hash_sees_task_emits_task_addresses_and_task_collections() {
    let ir = fixture();
    let checkout = machine_hash(&ir, "Checkout").unwrap();
    let span = ir.tasks["chargeCard"].span;
    let progress = roadbook_ir::EventDecl {
        fields: vec![roadbook_ir::Field {
            name: "percent".into(),
            ty: "Role".parse().unwrap(),
            sensitive: false,
            doc: String::new(),
        }],
        doc: String::new(),
        span,
    };

    // An emitted event of a task the machine invokes, with the types of its fields.
    let mut emitting = ir.clone();
    emitting.tasks["chargeCard"]
        .emits
        .insert("Progress".into(), progress.clone());
    assert_ne!(machine_hash(&emitting, "Checkout").unwrap(), checkout);
    let view = roadbook_ir::semantic_view(&emitting, "Checkout").unwrap();
    assert!(view["tasks"]["chargeCard"]["emits"]["Progress"].is_object());
    assert!(view["types"]["Role"].is_object());
    // The same emits on a task the machine does not use change nothing.
    let mut unused = ir.clone();
    unused.tasks["authenticate"]
        .emits
        .insert("Progress".into(), progress.clone());
    assert_eq!(machine_hash(&unused, "Checkout").unwrap(), checkout);

    // `on pay.Progress` through the task's `as`: an address of kind `task`, which names no
    // machine, with `event` of type `chargeCard.Progress`.
    let mut on = emitting.clone();
    let charging = &mut on.machines["Checkout"].root.states[0];
    charging.invoke.as_mut().unwrap().alias = Some("pay".into());
    let mut t = charging.transitions[0].clone();
    t.trigger = Trigger::Emitted {
        from: roadbook_ir::Address {
            kind: roadbook_ir::AddressKind::Task,
            name: "pay".into(),
            machine: None,
            task: Some("chargeCard".into()),
            state: Some("Checkout.Charging".into()),
        },
        event: "Progress".into(),
    };
    t.targets = vec![];
    t.effects = vec![];
    let event = Expr {
        ty: "chargeCard.Progress".parse().unwrap(),
        kind: ExprKind::Ref {
            path: vec!["event".into()],
        },
        text: "event".into(),
    };
    t.guard = Some(Expr {
        ty: "bool".parse().unwrap(),
        kind: ExprKind::Op {
            op: roadbook_ir::Op::Eq,
            args: vec![event.clone(), event],
        },
        text: "event == event".into(),
    });
    charging.transitions.push(t);
    let hash = machine_hash(&on, "Checkout").unwrap();
    assert_ne!(hash, machine_hash(&emitting, "Checkout").unwrap());
    let view = roadbook_ir::semantic_view(&on, "Checkout").unwrap();
    assert_eq!(view["interfaces"], serde_json::json!({}));
    let from =
        &view["machines"]["Checkout"]["root"]["states"][0]["transitions"][4]["trigger"]["from"];
    assert_eq!(from["task"], "chargeCard", "{from}");
    assert!(from.get("machine").is_none(), "{from}");
    // An event type of an event the task does not emit is reported.
    let mut missing = on.clone();
    missing.tasks["chargeCard"].emits.clear();
    assert!(machine_hash(&missing, "Checkout").is_err());

    // A collection of tasks adds the task to K, and is written with `kind` and `src`.
    let mut spawning = ir.clone();
    spawning.machines["PlaceOrder"].children.insert(
        "charges".into(),
        roadbook_ir::ChildrenDecl {
            kind: roadbook_ir::ChildrenKind::Task,
            src: "chargeCard".into(),
            key: "string".parse().unwrap(),
            doc: String::new(),
            span,
        },
    );
    let view = roadbook_ir::semantic_view(&spawning, "PlaceOrder").unwrap();
    assert_eq!(
        view["machines"]["PlaceOrder"]["children"]["charges"],
        serde_json::json!({"kind": "task", "src": "chargeCard", "key": "string"})
    );
    assert!(view["tasks"]["chargeCard"].is_object());
    assert_eq!(
        view["machines"].as_object().unwrap().len(),
        2,
        "M is unchanged"
    );
    let mut changed = spawning.clone();
    changed.tasks["chargeCard"].params.pop();
    assert_ne!(
        machine_hash(&changed, "PlaceOrder").unwrap(),
        machine_hash(&spawning, "PlaceOrder").unwrap()
    );
}

/// An address names its other end by exactly one of `machine` and `task`, by its kind.
#[test]
fn an_address_names_a_machine_or_a_task() {
    let read = |json: &str| serde_json::from_str::<roadbook_ir::Address>(json);
    let task = read(r#"{"kind": "task", "name": "up", "task": "upload", "state": "A.B"}"#).unwrap();
    assert_eq!(task.peer(), "upload");
    assert_eq!(
        serde_json::to_string(&task).unwrap(),
        r#"{"kind":"task","name":"up","task":"upload","state":"A.B"}"#
    );
    assert!(
        read(r#"{"kind": "children", "name": "ups", "task": "upload", "state": null}"#).is_ok()
    );
    assert!(read(r#"{"kind": "child", "name": "c", "machine": "C", "state": "A"}"#).is_ok());
    for bad in [
        r#"{"kind": "task", "name": "up", "machine": "Up", "state": "A"}"#,
        r#"{"kind": "child", "name": "c", "task": "upload", "state": "A"}"#,
        r#"{"kind": "ref", "name": "r", "machine": "R", "task": "upload", "state": null}"#,
        r#"{"kind": "children", "name": "cs", "state": null}"#,
        r#"{"kind": "ref", "name": "r", "machine": "R", "state": null, "other": 1}"#,
    ] {
        assert!(read(bad).is_err(), "{bad}");
    }
}

#[test]
fn hash_handles_recursion_and_reports_missing_declarations() {
    // A machine that invokes itself (legal with an event or `after` between).
    let mut ir = fixture();
    let backoff = &mut ir.machines["Checkout"].root.states[1];
    backoff.invoke = Some(roadbook_ir::Invoke {
        kind: roadbook_ir::InvokeKind::Machine,
        src: "Checkout".into(),
        to: None,
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

    // Order's call to the warehouse, the Release it sends when the payment is declined, and
    // Inventory's taking of the call and its reply.
    let call = "/machines/Order/root/states/0/invoke";
    let release = "/machines/Order/root/states/1/transitions/1/effects/0";
    let cases: Vec<(String, Value)> = vec![
        (format!("{call}/to/kind"), "child".into()),
        (format!("{call}/to"), Value::Null),
        (format!("{call}/src"), "Reserve".into()),
        (format!("{release}/to/state"), "Order.Paying".into()),
        (format!("{release}/to/kind"), "child".into()),
        (format!("{release}/value/type"), "Release[]".into()),
        (format!("{release}/value/kind"), "ref".into()),
        (
            "/machines/Inventory/root/states/0/transitions/0/trigger/call".into(),
            "Reserve".into(),
        ),
        (
            "/machines/Inventory/root/states/1/transitions/1/effects/0/value".into(),
            Value::Null,
        ),
        (
            "/machines/Order/params/2/type".into(),
            "ref inventory".into(),
        ),
        ("/sources/1/package".into(), "".into()),
        // `as` on a call; on a task it is allowed from IR 0.10 (for a task that emits).
        (format!("{call}/as"), "reserving".into()),
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
    assert!(view["interfaces"]["Inventory"]["calls"]["reserve"].is_object());
    assert!(view["interfaces"]["Inventory"]["events"]["Release"].is_object());
    assert!(view["types"]["Item"].is_object());

    // How the warehouse works inside does not change the order's hash.
    let mut changed = ir.clone();
    changed.machines["Inventory"].root.states[1].transitions[0].targets = Vec::new();
    assert_ne!(
        machine_hash(&changed, "Inventory").unwrap(),
        machine_hash(&ir, "Inventory").unwrap()
    );
    assert_eq!(machine_hash(&changed, "Order").unwrap(), order);

    // What it accepts and answers does, and so do the types of their fields.
    let mut changed = ir.clone();
    changed.machines["Inventory"].events["Release"].fields.pop();
    assert_ne!(machine_hash(&changed, "Order").unwrap(), order);
    let mut changed = ir.clone();
    changed.machines["Inventory"].calls["reserve"].params.pop();
    assert_ne!(machine_hash(&changed, "Order").unwrap(), order);
    let mut changed = ir.clone();
    let roadbook_ir::TypeDef::Record { fields, .. } = &mut changed.types["Item"].def else {
        panic!("Item is a record")
    };
    fields[1].ty = "float".parse().unwrap();
    assert_ne!(machine_hash(&changed, "Order").unwrap(), order);

    // A peer the IR does not declare is reported.
    let mut missing = ir.clone();
    missing.machines.shift_remove("Inventory");
    assert!(machine_hash(&missing, "Order").is_err());
}

/// An alias that refers to itself, which only a hand-written IR can have, is found, through
/// type arguments and maps too; the compiled IR has none.
#[test]
fn alias_cycles_are_found() {
    let mut ir = fixture();
    assert_eq!(ir.alias_cycle(), None);
    let span = ir.types["Order"].span;
    let alias = |params: &[&str], ty: &str| roadbook_ir::TypeDecl {
        def: roadbook_ir::TypeDef::Alias {
            params: params.iter().map(|p| p.to_string()).collect(),
            ty: ty.parse().unwrap(),
        },
        doc: String::new(),
        span,
    };
    let (a, b) = (
        alias(&["T"], "Boom<Pair<T, T>>"),
        alias(&[], "map<string, Loop>[]"),
    );
    ir.types.insert("Boom".into(), a);
    assert_eq!(ir.alias_cycle(), Some("Boom"));
    ir.types.shift_remove("Boom");
    let c = alias(&[], "Twice");
    ir.types.insert("Loop".into(), b);
    ir.types.insert("Twice".into(), c);
    assert!(matches!(ir.alias_cycle(), Some("Loop" | "Twice")));
}
