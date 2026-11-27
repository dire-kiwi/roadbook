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
fn examples_are_fixed_points() {
    for (name, src) in EXAMPLES {
        assert_eq!(format(src).unwrap(), src, "{name}");
    }
}

const DOCS: [(&str, &str); 4] = [
    ("README.md", include_str!("../../../docs/design/README.md")),
    (
        "language.md",
        include_str!("../../../docs/design/language.md"),
    ),
    (
        "alternatives.md",
        include_str!("../../../docs/design/alternatives.md"),
    ),
    (
        "semantics.md",
        include_str!("../../../docs/design/semantics.md"),
    ),
];

/// The unlabelled code blocks of a Markdown page.
fn code_blocks(md: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current: Option<String> = None;
    for line in md.lines() {
        match (&mut current, line.trim_end()) {
            (None, "```") => current = Some(String::new()),
            (None, l) if l.starts_with("```") => current = Some("\u{0}".into()),
            (Some(_), "```") => blocks.extend(current.take().filter(|b| !b.starts_with('\u{0}'))),
            (Some(b), l) => {
                b.push_str(l);
                b.push('\n');
            }
            (None, _) => {}
        }
    }
    blocks
}

/// Every chart snippet in the design pages that is valid on its own (or, for a lone state, inside
/// a machine) is formatted exactly as written. Snippets using syntax from later milestones and
/// annotated fragments do not parse and are skipped.
#[test]
fn doc_snippets_are_fixed_points() {
    let mut checked = Vec::new();
    for (page, md) in DOCS {
        for block in code_blocks(md) {
            let src = if block.starts_with("state ") {
                let body: String = block.lines().map(|l| format!("  {l}\n")).collect();
                format!("machine M() {{\n{body}}}\n")
            } else {
                block
            };
            if parse_file(FileId(0), &src).1.is_empty() {
                assert_eq!(format(&src).unwrap(), src, "{page}");
                checked.push(page);
            }
        }
    }
    assert_eq!(
        checked,
        [
            "README.md",
            "language.md",
            "language.md",
            "language.md",
            "language.md",
            "language.md",
            "alternatives.md"
        ]
    );
}
