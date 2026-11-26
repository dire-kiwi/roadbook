//! Failing charts: `tests/fail/RBxxxx-*.road` (or a directory with `main.road`). Each file
//! lists the diagnostics it must produce, exactly, as `// expect: CODE at LINE:COL` lines,
//! and the rendered diagnostics are snapshot in `tests/snapshots/`.

mod common;

use std::collections::BTreeSet;

use roadbook_compiler::codes;

#[test]
fn every_failing_chart_gives_exactly_its_diagnostics() {
    let mut failures = Vec::new();
    for case in common::cases("fail") {
        let out = case.compile();
        let found = common::found(&out);
        if found != case.expected {
            failures.push(format!(
                "{}:\n  expected {:?}\n  found    {:?}\n{}",
                case.name,
                case.expected,
                found,
                out.render()
            ));
        }
        let prefix = &case.name[..6];
        if !found.iter().any(|(_, code, ..)| code == prefix) {
            failures.push(format!("{} does not produce {prefix}", case.name));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn every_code_has_a_failing_chart() {
    let names: BTreeSet<String> = common::cases("fail").into_iter().map(|c| c.name).collect();
    let missing: Vec<String> = codes::ALL
        .iter()
        .map(|c| c.to_string())
        .filter(|c| !names.iter().any(|n| n.starts_with(c.as_str())))
        .collect();
    assert!(missing.is_empty(), "no failing chart for {missing:?}");
}

#[test]
fn rendered_diagnostics() {
    for case in common::cases("fail") {
        let out = case.compile();
        insta::with_settings!({ prepend_module_to_snapshot => false, omit_expression => true }, {
            insta::assert_snapshot!(case.name.clone(), out.render());
        });
    }
}

#[test]
fn applying_a_fix_removes_its_diagnostic() {
    let mut checked = 0;
    for case in common::cases("fail") {
        let out = case.compile();
        for d in &out.diagnostics {
            let Some(fix) = d.fixes.first() else {
                continue;
            };
            let mut loader = case.loader.clone();
            let mut edits = fix.edits.clone();
            edits.sort_by_key(|e| std::cmp::Reverse(e.span.start.offset));
            for e in &edits {
                let (path, text) = &out.files[e.span.file.0 as usize];
                assert!(e.span.start.offset <= e.span.end.offset);
                assert!(e.span.end.offset as usize <= text.len(), "{}", case.name);
                let mut new = loader.files[path].clone();
                new.replace_range(e.span.range(), &e.text);
                loader.insert(path, new);
            }
            let fixed =
                roadbook_compiler::compile(&mut loader, &case.entry, &case.options).unwrap();
            let count = |c: &roadbook_compiler::Compilation| {
                c.diagnostics.iter().filter(|x| x.code == d.code).count()
            };
            assert!(
                count(&fixed) < count(&out),
                "{}: applying `{}` does not remove {}:\n{}",
                case.name,
                fix.message,
                d.code,
                fixed.render()
            );
            checked += 1;
        }
    }
    assert!(checked >= 15, "only {checked} fixes were exercised");
}
