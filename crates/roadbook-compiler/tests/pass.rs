//! Charts that compile: `tests/pass/`, the shop example, and every failing chart that has
//! only warnings. Each yields IR that the schema accepts, and every `check` line of the
//! passing charts holds.

mod common;

use std::path::Path;

use roadbook_compiler::{FsLoader, Options, Verdict, compile};

#[test]
fn passing_charts_compile_without_diagnostics() {
    for case in common::cases("pass") {
        let out = case.compile();
        assert!(
            out.diagnostics.is_empty(),
            "{}:\n{}",
            case.name,
            out.render()
        );
    }
}

#[test]
fn the_checks_of_passing_charts_hold() {
    for case in common::cases("pass") {
        let out = case.check();
        assert!(
            out.diagnostics.is_empty(),
            "{}:\n{}",
            case.name,
            out.render()
        );
        for c in &out.checks {
            assert!(
                matches!(c.verdict, Verdict::Proved | Verdict::Conditional),
                "{}: check {} is {:?}",
                case.name,
                c.check,
                c.verdict
            );
        }
    }
    // The chart that has every form, with a parallel state and history.
    let case = common::cases("pass")
        .into_iter()
        .find(|c| c.name == "properties")
        .unwrap();
    let out = case.check();
    let got: Vec<(&str, Verdict)> = out
        .checks
        .iter()
        .map(|c| (c.check.as_str(), c.verdict))
        .collect();
    use Verdict::{Conditional, Proved};
    assert_eq!(
        got,
        [
            ("reachable Shipped", Conditional),
            ("reachable Cancelled", Proved),
            ("reachable Fulfilling.Work.Billing.Charged", Conditional),
            ("never Shipped and Cancelled", Proved),
            ("never Paused and Fulfilling.Work.Packing.Packed", Proved),
            ("Shipped only after Fulfilling.Work.Billing.Charged", Proved),
            ("Fulfilling.Work.Packing.Waiting only after Cart", Proved),
            (
                "Fulfilling.Work.Billing.Open only after Fulfilling.Work.Packing.Waiting",
                Proved
            ),
            ("can finish", Proved),
        ]
    );
    let charged = &out.checks[2];
    assert_eq!(charged.doc, "");
    assert_eq!(out.checks[0].doc, "Shipping needs the payment approved.");
    assert_eq!(
        charged.path_text,
        "Cart --Pay [approved()]--> Fulfilling.Work.Packing.Waiting + Fulfilling.Work.Billing.Open --Charge--> Fulfilling.Work.Packing.Waiting + Fulfilling.Work.Billing.Charged"
    );
}

#[test]
fn every_successful_compilation_yields_schema_valid_ir() {
    let v = common::validator();
    let mut checked = Vec::new();
    for case in common::cases("pass")
        .into_iter()
        .chain(common::cases("fail"))
    {
        let out = case.compile();
        if out.has_errors() {
            continue;
        }
        let errors = common::schema_errors(&v, &out);
        assert!(errors.is_empty(), "{}: {errors:#?}", case.name);
        checked.push(case.name);
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for f in ["place_order", "checkout", "types"] {
        let mut loader = FsLoader::new(&root);
        let path = format!("examples/shop/{f}.road");
        let out = compile(&mut loader, &path, &Options::default()).unwrap();
        assert!(!out.has_errors(), "{}", out.render());
        let errors = common::schema_errors(&v, &out);
        assert!(errors.is_empty(), "{f}: {errors:#?}");
        checked.push(f.into());
    }
    // The pass charts, the shop's three files and the warning-only failing charts.
    assert!(checked.len() >= 9, "{checked:?}");
}

#[test]
fn ir_is_deterministic() {
    for case in common::cases("pass") {
        let a = case.compile().ir.unwrap().to_json();
        let b = case.compile().ir.unwrap().to_json();
        assert_eq!(a, b, "{}", case.name);
    }
}

/// A machine's hash covers its `children` collections whatever they are named, also `text`,
/// `doc` or `span`, the names the hash strips elsewhere.
#[test]
fn the_hash_covers_collections_of_any_name() {
    let hash = |key: &str, child: &str| {
        let k = if key == "int" { "1" } else { "\"1\"" };
        let src = format!(
            "machine C() {{\n  initial A\n  final state A {{}}\n}}\n\nmachine D() {{\n  initial A\n  final state A {{}}\n}}\n\nmachine M() {{\n  event Go\n  children text: {child} by {key}\n  initial A\n  state A {{\n    on Go spawn text[{k}] = {child}()\n  }}\n}}\n"
        );
        let mut loader = roadbook_compiler::MemoryLoader::new().with("m.road", src);
        let out = compile(&mut loader, "m.road", &Options::default()).unwrap();
        assert!(!out.has_errors(), "{}", out.render());
        out.ir.unwrap().machines["M"].hash.clone()
    };
    let base = hash("int", "C");
    assert_ne!(base, hash("string", "C"));
    assert_ne!(base, hash("int", "D"));
}

/// The interface of a peer, its `calls` included, names the types inside a call's parameters
/// and result: a map's value type, a generic type's arguments. So a machine that only holds a
/// `ref` to the peer, and one that calls it, change hash when such a type does, and neither
/// changes when the peer's own states do.
#[test]
fn the_hash_covers_the_types_inside_a_peers_calls() {
    let hash = |item: &str, count: &str, reply: &str, machine: &str| {
        let src = format!(
            "type Item {item}\ntype Count = {count}\ntype Page<T> {{ items: T[], next: string? }}\n\n\
             machine Catalog() {{\n  call search(stock: map<string, Count>, page: Page<Item>) -> Page<Item>\n  event Ping\n  initial Open\n  state Open {{\n    on search reply call done {reply}\n    on Ping -> Open\n  }}\n}}\n\n\
             machine Watcher(catalog: ref Catalog) {{\n  event Go\n  initial A\n  state A {{ on Go send catalog.Ping }}\n}}\n\n\
             machine Shopper(catalog: ref Catalog, stock: map<string, Count>, page: Page<Item>) {{\n  initial A\n  state A {{\n    invoke catalog.search(stock, page) {{\n      done            -> B\n      fail Unanswered -> B\n    }}\n  }}\n  final state B {{}}\n}}\n"
        );
        let mut loader = roadbook_compiler::MemoryLoader::new().with("m.road", src);
        let out = compile(&mut loader, "m.road", &Options::default()).unwrap();
        assert!(!out.has_errors(), "{}", out.render());
        out.ir.unwrap().machines[machine].hash.clone()
    };
    let item = "{ sku: string }";
    let page = "event.page";
    for m in ["Watcher", "Shopper"] {
        let base = hash(item, "int", page, m);
        assert_ne!(
            base,
            hash("{ sku: string, qty: int }", "int", page, m),
            "{m}: Item"
        );
        assert_ne!(base, hash(item, "float", page, m), "{m}: Count");
        let empty = "Page { items: event.page.items, next: \"\" }";
        assert_eq!(base, hash(item, "int", empty, m), "{m}: the peer's states");
    }
}

/// `on name.ended` puts the built-in enum `Ended` in `types` only when something names it,
/// after the declared types and with an empty span at the start of the entry file, and adds
/// nothing of the peer to its holder's hash but the interface the `ref` already brings.
#[test]
fn ended_brings_the_builtin_enum_and_only_the_peers_interface() {
    let ir = |peer_state: &str, on: &str| {
        let src = format!(
            "type T {{ x: int }}\n\nmachine Peer() {{\n  event Stop\n  initial A\n  state A {{\n    on Stop -> {peer_state}\n  }}\n  final state B {{}}\n  final state C {{}}\n}}\n\nmachine M(peer: ref Peer) {{\n  initial A\n  state A {{\n    {on}\n  }}\n  final state Z {{}}\n}}\n"
        );
        let mut loader = roadbook_compiler::MemoryLoader::new().with("m.road", src);
        let out = compile(&mut loader, "m.road", &Options::default()).unwrap();
        // The peer's other final state is unreachable: a warning.
        assert!(!out.has_errors(), "{}", out.render());
        out.ir.unwrap()
    };
    let bare = ir("B", "on peer.ended -> Z");
    assert!(!bare.types.contains_key(roadbook_ir::ENDED));
    let read = ir("B", "on peer.ended if event.status == Ended.Done -> Z");
    let types: Vec<&str> = read.types.keys().map(String::as_str).collect();
    assert_eq!(types, ["T", roadbook_ir::ENDED]);
    let ended = &read.types[roadbook_ir::ENDED];
    let roadbook_ir::TypeDef::Enum { variants } = &ended.def else {
        panic!("not an enum: {ended:?}")
    };
    assert_eq!(variants, &roadbook_ir::ENDED_VARIANTS);
    assert_eq!(
        (ended.span.file, ended.span.line, ended.span.col),
        (0, 1, 1)
    );
    assert_eq!((ended.span.end_line, ended.span.end_col), (1, 1));
    // How the peer works inside is not part of the holder's hash; what it reads is.
    assert_eq!(
        bare.machines["M"].hash,
        ir("C", "on peer.ended -> Z").machines["M"].hash
    );
    assert_ne!(bare.machines["M"].hash, read.machines["M"].hash);
    let view = roadbook_ir::semantic_view(&read, "M").unwrap();
    assert_eq!(
        view["interfaces"]["Peer"],
        serde_json::json!({ "events": { "Stop": { "fields": [] } }, "emits": {}, "calls": {} })
    );
    assert_eq!(
        view["types"][roadbook_ir::ENDED],
        serde_json::json!({ "kind": "enum", "variants": roadbook_ir::ENDED_VARIANTS })
    );
}
