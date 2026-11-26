//! The example charts and the chart snippets in the design documents.

use roadbook_syntax::diagnostic::SingleFile;
use roadbook_syntax::{FileId, format, parse_file};

const EXAMPLES: [(&str, &str); 3] = [
    (
        "examples/shop/types.road",
        include_str!("../../../examples/shop/types.road"),
    ),
    (
        "examples/shop/checkout.road",
        include_str!("../../../examples/shop/checkout.road"),
    ),
    (
        "examples/shop/place_order.road",
        include_str!("../../../examples/shop/place_order.road"),
    ),
];

fn render_all(name: &str, src: &str) -> String {
    let (_, diags) = parse_file(FileId(0), src);
    let files = SingleFile { name, source: src };
    diags.iter().map(|d| d.render(&files)).collect()
}

#[test]
fn examples_parse_without_diagnostics() {
    for (name, src) in EXAMPLES {
        assert_eq!(render_all(name, src), "", "{name}");
    }
}

#[test]
fn scratch() {
    let src = "machine M() {\n  var x: int\n  state A {\n    on Go\n    invoke foo(x) {\n      done -> B\n      on X -> C\n      fail Bad -> D set = 1\n    }\n    var y: int\n  }\n  done -> X\n  after Foo { a: 1 } -> X\n}\nstate Z {}\ntype a = order\n";
    println!("{}", render_all("t.road", src));
}

#[test]
fn examples_are_fixed_points() {
    for (name, src) in EXAMPLES {
        let out = format(src).unwrap();
        if out != src {
            std::fs::write(format!("/tmp/fmt-{}", name.replace('/', "_")), &out).unwrap();
        }
        assert_eq!(out, src, "{name}");
    }
}
