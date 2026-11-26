//! Checking several files with `check`: each is compiled as an entry with what it imports,
//! and the results are merged.

use roadbook_compiler::{Compilation, MemoryLoader, Options, check, compile};

fn loader(files: &[(&str, &str)]) -> MemoryLoader {
    let mut l = MemoryLoader::new();
    for (p, t) in files {
        l.insert(p, *t);
    }
    l
}

fn run(files: &[(&str, &str)], roots: &[&str]) -> Compilation {
    let roots: Vec<String> = roots.iter().map(|r| r.to_string()).collect();
    check(&mut loader(files), &roots, &Options::default()).unwrap()
}

/// `(file, code, line)` of every diagnostic, in order.
fn found(c: &Compilation) -> Vec<(String, String, u32)> {
    c.diagnostics
        .iter()
        .map(|d| {
            (
                c.files[d.span.file.0 as usize].0.clone(),
                d.code.to_string(),
                d.span.start.line,
            )
        })
        .collect()
}

fn f(file: &str, code: &str, line: u32) -> (String, String, u32) {
    (file.to_string(), code.to_string(), line)
}

const LIB: &str = "action log(s: string)\naction spare(s: string)\n";

fn chart(name: &str, import: &str) -> String {
    format!(
        "import {{ log }} from \"{import}\"\n\nmachine {name}() {{\n  initial A\n  state A {{ entry do log(\"a\") }}\n}}\n"
    )
}

#[test]
fn charts_that_do_not_import_one_another_may_share_names() {
    let (a, b) = (chart("M", "./lib.road"), chart("M", "./lib.road"));
    let files = [
        ("a.road", a.as_str()),
        ("b.road", b.as_str()),
        ("lib.road", LIB),
    ];
    let out = run(&files, &["a.road", "b.road", "lib.road"]);
    assert!(!out.has_errors(), "{}", out.render());
    // Each chart's `M.A` has no way out; `spare` is unused, reported once; `log` is used.
    assert_eq!(
        found(&out),
        [
            f("a.road", "RB0902", 5),
            f("lib.road", "RB0903", 2),
            f("b.road", "RB0902", 5)
        ]
    );
    let paths: Vec<&str> = out.files.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(paths, ["a.road", "lib.road", "b.road"]);
    // Names are still unique within one compilation.
    let c = "import { M } from \"./a.road\"\n\nmachine M() {\n  initial Done\n  final state Done {}\n}\n";
    let out = run(
        &[("a.road", a.as_str()), ("c.road", c), ("lib.road", LIB)],
        &["c.road"],
    );
    assert_eq!(found(&out), [f("a.road", "RB0107", 3)]);
}

#[test]
fn a_use_from_any_checked_file_counts() {
    let a = chart("M", "./lib.road");
    let files = [("a.road", a.as_str()), ("lib.road", LIB)];
    let unused = |out: &Compilation| -> Vec<String> {
        out.diagnostics
            .iter()
            .filter(|d| d.code.to_string() == "RB0903")
            .map(|d| d.message.clone())
            .collect()
    };
    assert_eq!(
        unused(&run(&files, &["lib.road"])),
        ["action `log` is never used", "action `spare` is never used"]
    );
    assert_eq!(
        unused(&run(&files, &["lib.road", "a.road"])),
        ["action `spare` is never used"]
    );
    // Declarations of files that are only imported are not reported.
    assert!(unused(&run(&files, &["a.road"])).is_empty());
}

#[test]
fn a_shared_import_is_reported_on_once() {
    let lib = "type User { id: string }\naction log(s: Usr)\n";
    let (a, b) = (chart("A", "./lib.road"), chart("B", "./lib.road"));
    let out = run(
        &[
            ("a.road", a.as_str()),
            ("b.road", b.as_str()),
            ("lib.road", lib),
        ],
        &["a.road", "b.road"],
    );
    assert_eq!(found(&out), [f("lib.road", "RB0101", 2)]);
    // The fix's edit points into the right file.
    let edit = &out.diagnostics[0].fixes[0].edits[0];
    assert_eq!(out.files[edit.span.file.0 as usize].0, "lib.road");
}

#[test]
fn errors_in_one_chart_leave_the_analyses_of_others_on() {
    let bad = "machine Bad() {\n  initial Nowhere\n  state A {}\n}\n";
    let good = chart("Good", "./lib.road");
    let out = run(
        &[
            ("bad.road", bad),
            ("good.road", good.as_str()),
            ("lib.road", LIB),
        ],
        &["bad.road", "good.road"],
    );
    assert_eq!(
        found(&out),
        [f("bad.road", "RB0203", 2), f("good.road", "RB0902", 5)]
    );
}

#[test]
fn a_syntax_error_does_not_stop_the_other_files() {
    let broken = "action log(s: string)\ntype T { x: int\n";
    let a = chart("A", "./broken.road");
    let b = chart("B", "./lib.road");
    let mut c = b.replace("log(\"a\")", "log(1)");
    c = c.replace("machine B", "machine C");
    let out = run(
        &[
            ("a.road", a.as_str()),
            ("b.road", b.as_str()),
            ("c.road", c.as_str()),
            ("broken.road", broken),
            ("lib.road", LIB),
        ],
        &["a.road", "b.road", "c.road"],
    );
    // `a.road` imports the broken file: its names are poisoned, so `do log` is not reported.
    // `b.road` is checked fully, warnings included, and so is `c.road`, with its type error.
    assert_eq!(
        found(&out),
        [
            f("broken.road", "RB0011", 3),
            f("b.road", "RB0902", 5),
            f("c.road", "RB0301", 5)
        ]
    );
    // Within one entry's compilation too, the files that parse are checked.
    let d = a
        .replace("./broken.road", "./x.road")
        .replace("initial A", "initial Nowhere");
    let single = compile(
        &mut loader(&[("a.road", d.as_str()), ("x.road", "type X {\n")]),
        "a.road",
        &Options::default(),
    )
    .unwrap();
    assert_eq!(
        found(&single),
        [f("a.road", "RB0203", 4), f("x.road", "RB0011", 2)]
    );
}

#[test]
fn a_missing_import_is_offered_once_per_name() {
    let m = "import { Order } from \"./types.road\"\n\nmachine M(o: Order, a: User, b: User) {\n  initial Done\n  final state Done {}\n}\n";
    let types = "type Order { id: string }\ntype User { id: string }\n";
    let out = run(&[("m.road", m), ("types.road", types)], &["m.road"]);
    let fixes: Vec<_> = out.diagnostics.iter().flat_map(|d| &d.fixes).collect();
    assert_eq!(out.error_count(), 2);
    assert_eq!(fixes.len(), 1, "{}", out.render());
}
