//! Charts that compile: `tests/pass/`, the shop example, and every failing chart that has
//! only warnings. Each yields IR that the schema accepts.

mod common;

use std::path::Path;

use roadbook_compiler::{FsLoader, Options, compile};

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
