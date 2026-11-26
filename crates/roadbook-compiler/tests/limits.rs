//! Nesting at the parser's depth limit compiles, formats and round-trips through the IR on a
//! test thread's stack in a debug build; one level more is a single RB0025.

use roadbook_compiler::{Ir, Options, compile_source};
use roadbook_syntax::{MAX_DEPTH, format};

fn machine(var: &str, states: &str) -> String {
    format!("machine M() {{\n  var x: {var}\n  initial S\n{states}}}\n")
}

/// `n` states nested in one another, the innermost final.
fn nested_states(n: usize) -> String {
    let s = format!(
        "{}final state S {{}}\n{}",
        "state S {\ninitial S\n".repeat(n - 1),
        "}\n".repeat(n - 1)
    );
    machine("int", &s)
}

/// The charts nested `n` levels deep in each way the parser counts.
fn charts(n: usize) -> Vec<(&'static str, String)> {
    let leaf = "  final state S {}\n";
    let init = |e: String| machine(&format!("int = {e}"), leaf);
    vec![
        (
            "parentheses",
            init(format!("{}1{}", "(".repeat(n - 1), ")".repeat(n - 1))),
        ),
        ("operators", init(vec!["1"; n].join(" + "))),
        ("states", nested_states(n)),
        (
            "type suffixes",
            machine(&format!("int{}", "[]".repeat(n)), leaf),
        ),
        (
            "type arguments",
            machine(
                &format!("{}int{}", "map<string, ".repeat(n), ">".repeat(n)),
                leaf,
            ),
        ),
    ]
}

fn codes(src: &str) -> Vec<String> {
    let out = compile_source("deep.road", src, &Options::default());
    out.diagnostics.iter().map(|d| d.code.to_string()).collect()
}

#[test]
fn nesting_at_the_limit_is_accepted_everywhere() {
    for (what, src) in charts(MAX_DEPTH) {
        let out = compile_source("deep.road", &src, &Options::default());
        assert!(!out.has_errors(), "{what}:\n{}", out.render());
        let ir = out.ir.expect("no errors");
        let back = Ir::from_json(&ir.to_json()).unwrap_or_else(|e| panic!("{what}: {e}"));
        assert_eq!(back, ir, "{what}");
        let once = format(&src).unwrap_or_else(|_| panic!("{what}: does not format"));
        assert_eq!(format(&once).ok().as_ref(), Some(&once), "{what}");
    }
}

#[test]
fn nesting_past_the_limit_is_one_error() {
    for (what, src) in charts(MAX_DEPTH + 1) {
        assert_eq!(codes(&src), ["RB0025"], "{what}");
    }
}
