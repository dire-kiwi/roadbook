//! The shop example: compiles cleanly, and its IR is the `roadbook-ir` fixture.

use std::path::Path;

use roadbook_compiler::{FsLoader, Options, check, compile};

fn repo() -> FsLoader {
    FsLoader::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

#[test]
fn place_order_matches_the_fixture() {
    let out = compile(
        &mut repo(),
        "examples/shop/place_order.road",
        &Options::default(),
    )
    .unwrap();
    assert!(out.diagnostics.is_empty(), "{}", out.render());
    let ir = out.ir.unwrap().to_json();
    let fixture = include_str!("../../roadbook-ir/tests/data/place_order.ir.json");
    if std::env::var_os("ROADBOOK_BLESS").is_some() {
        std::fs::write("/tmp/place_order.compiled.ir.json", &ir).unwrap();
    }
    assert!(ir == fixture, "the compiled IR differs from the fixture");
}

#[test]
fn the_shop_checks_clean() {
    let roots: Vec<String> = ["checkout.road", "place_order.road", "types.road"]
        .iter()
        .map(|f| format!("examples/shop/{f}"))
        .collect();
    let out = check(&mut repo(), &roots, &Options::default()).unwrap();
    assert!(out.diagnostics.is_empty(), "{}", out.render());
}

#[test]
fn orders_matches_the_messaging_fixture() {
    let mut loader = repo().with_package("inventory", "examples/inventory");
    let out = compile(
        &mut loader,
        "examples/orders/orders.road",
        &Options::default(),
    )
    .unwrap();
    assert!(out.diagnostics.is_empty(), "{}", out.render());
    let ir = out.ir.unwrap().to_json();
    let fixture = include_str!("../../roadbook-ir/tests/data/orders.ir.json");
    assert!(
        ir == fixture,
        "the compiled IR differs from the fixture: run `roadbook export ir examples/orders/orders.road`"
    );
}
