//! The examples in `docs/design/language.md` are what the compiler prints.

use std::path::Path;

use roadbook_compiler::{MemoryLoader, Options, compile};

const LANGUAGE: &str = include_str!("../../../docs/design/language.md");

fn shop(file: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/shop")
        .join(file);
    std::fs::read_to_string(p).unwrap()
}

/// The first fenced block of the page that starts with `start`.
fn block(start: &str) -> String {
    let i = LANGUAGE
        .find(&format!("```\n{start}"))
        .expect("the example is on the page");
    let body = &LANGUAGE[i + 4..];
    body[..body.find("```").unwrap()].to_string()
}

#[test]
fn the_rb0204_example() {
    // `checkout.road` without the `fail GatewayUnavailable` outcome.
    let checkout = shop("checkout.road");
    let start = checkout.find("      fail GatewayUnavailable {").unwrap();
    let end = start + checkout[start..].find("      }\n").unwrap() + "      }\n".len();
    let edited = format!("{}{}", &checkout[..start], &checkout[end..]);
    let mut loader = MemoryLoader::new()
        .with("examples/shop/checkout.road", edited)
        .with("examples/shop/types.road", shop("types.road"));
    let out = compile(
        &mut loader,
        "examples/shop/checkout.road",
        &Options::default(),
    )
    .unwrap();
    assert_eq!(out.render(), block("error[RB0204]"));
}

#[test]
fn the_checkout_example_is_the_shop_file() {
    // The page shows `Checkout` without its import line.
    let page = block("/// Charge the customer's card");
    let file = shop("checkout.road");
    assert!(
        file.ends_with(&page),
        "the example on the page differs from checkout.road"
    );
}

#[test]
fn the_definite_assignment_example() {
    // `place_order.road` is accepted as it is; without `set order = result` on `Validating`'s
    // `done`, the compiler points at the read in `Pricing`.
    let compile_with = |place_order: String| {
        let mut loader = MemoryLoader::new()
            .with("examples/shop/place_order.road", place_order)
            .with("examples/shop/checkout.road", shop("checkout.road"))
            .with("examples/shop/types.road", shop("types.road"));
        compile(
            &mut loader,
            "examples/shop/place_order.road",
            &Options::default(),
        )
        .unwrap()
    };
    let file = shop("place_order.road");
    let out = compile_with(file.clone());
    assert!(out.diagnostics.is_empty(), "{}", out.render());

    let line = "        done                  -> Pricing  set order = result\n";
    assert!(file.contains(line));
    let edited = file.replace(line, "        done                  -> Pricing\n");
    let out = compile_with(edited);
    assert_eq!(out.render(), block("error[RB0403]"));
}
