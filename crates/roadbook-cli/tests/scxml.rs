//! `roadbook export scxml`, run as a user would.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use roadbook_ir::{Ir, State};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn roadbook(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_roadbook"))
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

const SCXML: &str = "http://www.w3.org/2005/07/scxml";

#[test]
fn export_scxml_snapshots_the_shop() {
    let shop = "examples/shop/place_order.road";
    for (machine, snapshot) in [
        ("PlaceOrder", "shop_place_order_scxml"),
        ("Checkout", "shop_checkout_scxml"),
    ] {
        let out = roadbook(&repo(), &["export", "scxml", shop, "--machine", machine]);
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        insta::assert_snapshot!(snapshot, text(&out.stdout));
    }
    let out = roadbook(&repo(), &["export", "scxml", shop, "--machine", "Nope"]);
    assert_eq!(out.status.code(), Some(2));
    // Two machines and nowhere to put them but one document.
    let out = roadbook(&repo(), &["export", "scxml", shop]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        text(&out.stderr).contains("--machine"),
        "{}",
        text(&out.stderr)
    );
    // The one machine of a compilation needs no `--machine`.
    let out = roadbook(
        &repo(),
        &["export", "scxml", "spec/conformance/charts/root.road"],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(text(&out.stdout).contains("name=\"Top\""));
}

/// Calls: the callee takes `on c` as the event `call.c` and answers with a `<send>` of type
/// `urn:roadbook:reply`; the caller's invoke has type `urn:roadbook:call`, and its `fail *`
/// covers `Unanswered`.
#[test]
fn export_scxml_snapshots_calls() {
    let chart = "spec/conformance/charts/calls.road";
    for (machine, snapshot) in [
        ("Inventory", "calls_inventory_scxml"),
        ("Order", "calls_order_scxml"),
        ("Counter", "calls_counter_scxml"),
    ] {
        let out = roadbook(&repo(), &["export", "scxml", chart, "--machine", machine]);
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        insta::assert_snapshot!(snapshot, text(&out.stdout));
    }
}

/// A peer's end, `on x.ended`, is the event `x.ended`, its guards `cond`s as for any event.
#[test]
fn export_scxml_snapshots_a_peer_s_end() {
    let chart = "spec/conformance/charts/ended.road";
    for (machine, snapshot) in [
        ("Order", "ended_order_scxml"),
        ("Switcher", "ended_switcher_scxml"),
    ] {
        let out = roadbook(&repo(), &["export", "scxml", chart, "--machine", machine]);
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        insta::assert_snapshot!(snapshot, text(&out.stdout));
    }
}

/// What a task emits, through an `as` name or a collection of tasks, is the event `x.E`; an
/// invoke with no outcomes has no `done.invoke` transition; a collection of tasks is a `<data>`
/// with `rb:kind="task"`, and its `<rb:spawn>` has the type of a task invoke.
#[test]
fn export_scxml_snapshots_tasks_that_emit() {
    for (chart, machine, snapshot) in [
        (
            "spec/conformance/charts/task-emits.road",
            "Nested",
            "task_emits_nested_scxml",
        ),
        (
            "spec/conformance/charts/streams.road",
            "Clock",
            "streams_clock_scxml",
        ),
        (
            "spec/conformance/charts/spawn-tasks.road",
            "Batch",
            "spawn_tasks_batch_scxml",
        ),
    ] {
        let out = roadbook(&repo(), &["export", "scxml", chart, "--machine", machine]);
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        insta::assert_snapshot!(snapshot, text(&out.stdout));
    }
}

/// An internal transition (`-> .X`) is `type="internal"`, with a comment where the source is a
/// parallel state, and several targets are one `target` attribute.
#[test]
fn export_scxml_snapshots_internal_transitions_and_several_targets() {
    for (chart, machine, snapshot) in [
        (
            "spec/conformance/charts/internal.road",
            "Kept",
            "internal_kept_scxml",
        ),
        (
            "spec/conformance/charts/internal-parallel.road",
            "Sides",
            "internal_sides_scxml",
        ),
        (
            "spec/conformance/charts/targets.road",
            "Player",
            "targets_player_scxml",
        ),
    ] {
        let out = roadbook(&repo(), &["export", "scxml", chart, "--machine", machine]);
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        let xml = text(&out.stdout);
        roxmltree::Document::parse(&xml).unwrap();
        insta::assert_snapshot!(snapshot, xml);
    }
}

/// History states are `<history type="shallow|deep">` in their parent, with the default
/// transition SCXML requires: the parent's default entry, its `initial` or every region.
#[test]
fn export_scxml_writes_history_states() {
    for (chart, machine, snapshot) in [
        (
            "spec/conformance/charts/history.road",
            "Editor",
            "history_editor_scxml",
        ),
        (
            "spec/conformance/charts/history-regions.road",
            "Panels",
            "history_panels_scxml",
        ),
    ] {
        let out = roadbook(&repo(), &["export", "scxml", chart, "--machine", machine]);
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        let xml = text(&out.stdout);
        let doc = roxmltree::Document::parse(&xml).unwrap_or_else(|e| panic!("{e}\n{xml}"));
        let histories: Vec<(&str, &str, &str, String)> = doc
            .descendants()
            .filter(|n| n.has_tag_name((SCXML, "history")))
            .map(|n| {
                let parent = n.parent_element().unwrap();
                let default = n
                    .children()
                    .find(|c| c.has_tag_name((SCXML, "transition")))
                    .and_then(|t| t.attribute("target"))
                    .unwrap_or_default()
                    .to_string();
                (
                    parent.attribute("id").unwrap(),
                    n.attribute("id").unwrap(),
                    n.attribute("type").unwrap(),
                    default,
                )
            })
            .collect();
        match machine {
            "Editor" => assert_eq!(
                histories,
                [
                    (
                        "Editor_Work",
                        "Editor_Work_Shallow",
                        "shallow",
                        "Editor_Work_Draft".to_string()
                    ),
                    (
                        "Editor_Work",
                        "Editor_Work_Deep",
                        "deep",
                        "Editor_Work_Draft".to_string()
                    )
                ]
            ),
            _ => assert_eq!(
                histories,
                [
                    (
                        "Panels_Both",
                        "Panels_Both_Deep",
                        "deep",
                        "Panels_Both_L Panels_Both_R".to_string()
                    ),
                    (
                        "Panels_Both",
                        "Panels_Both_Shallow",
                        "shallow",
                        "Panels_Both_L Panels_Both_R".to_string()
                    )
                ]
            ),
        }
        insta::assert_snapshot!(snapshot, xml);
    }
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let e = e.unwrap();
        let p = e.path();
        if e.file_type().unwrap().is_symlink() || p.ends_with("node_modules") {
            continue;
        }
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().is_some_and(|x| x == "road") {
            out.push(p);
        }
    }
}

fn states<'a>(s: &'a State, out: &mut Vec<&'a State>) {
    out.push(s);
    for c in &s.states {
        states(c, out);
    }
}

/// Every chart of the repository's examples, conformance suite, viewer tests and compiler
/// tests exports to well-formed SCXML, one document per machine, holding every state of the
/// IR under its id and every transition's target.
#[test]
fn export_scxml_is_well_formed_and_keeps_every_state_and_target() {
    let root = repo();
    let mut charts = Vec::new();
    for dir in [
        "examples",
        "spec/conformance/charts",
        "packages/viewer/test/charts",
        "crates/roadbook-compiler/tests/pass",
    ] {
        walk(&root.join(dir), &mut charts);
    }
    let tmp = std::env::temp_dir().join(format!("roadbook-scxml-{}", std::process::id()));
    let mut documents = 0;
    for (i, chart) in charts.iter().enumerate() {
        let rel = chart.strip_prefix(&root).unwrap().to_string_lossy();
        let out = roadbook(&root, &["export", "ir", &rel]);
        assert_eq!(out.status.code(), Some(0), "{rel}: {}", text(&out.stderr));
        let ir = Ir::from_json(&text(&out.stdout)).unwrap();
        let dir = tmp.join(i.to_string());
        let out = roadbook(
            &root,
            &["export", "scxml", &rel, "-o", dir.to_str().unwrap()],
        );
        assert_eq!(out.status.code(), Some(0), "{rel}: {}", text(&out.stderr));
        for (name, m) in &ir.machines {
            let xml = std::fs::read_to_string(dir.join(format!("{name}.scxml"))).unwrap();
            let doc = roxmltree::Document::parse(&xml)
                .unwrap_or_else(|e| panic!("{rel} {name}: {e}\n{xml}"));
            let top = doc.root_element();
            assert_eq!(top.tag_name().name(), "scxml", "{rel} {name}");
            assert_eq!(top.tag_name().namespace(), Some(SCXML), "{rel} {name}");
            assert_eq!(top.attribute("name"), Some(name.as_str()));
            let mut ids = HashSet::new();
            let mut targets = HashSet::new();
            for n in doc.descendants().filter(|n| n.is_element()) {
                if n.tag_name().namespace() != Some(SCXML) {
                    continue;
                }
                match n.tag_name().name() {
                    "state" | "parallel" | "final" | "history" => {
                        assert!(
                            ids.insert(n.attribute("id").unwrap().to_string()),
                            "{rel}: duplicate id"
                        );
                    }
                    "transition" => {
                        // A history state's default transition targets every region of a
                        // parallel parent.
                        if let Some(t) = n.attribute("target") {
                            targets.extend(t.split_whitespace().map(str::to_string));
                        }
                    }
                    _ => {}
                }
            }
            let mut all = Vec::new();
            states(&m.root, &mut all);
            for s in all {
                // `.` is `_` in an SCXML id, and back. The machine is the `<scxml>` element,
                // and a `<state>` too only when its body has transitions or effects.
                let id = s.id.replace('.', "_");
                assert!(
                    ids.contains(&id) || s.id == m.root.id,
                    "{rel}: no state {id}\n{xml}"
                );
                for t in s.transitions.iter().flat_map(|t| &t.targets) {
                    let target = t.replace('.', "_");
                    assert!(targets.contains(&target), "{rel}: no target {target}");
                }
            }
            // And nothing else: every id and target is one of the IR's states.
            assert!(targets.is_subset(&ids), "{rel}: {targets:?} not in {ids:?}");
            documents += 1;
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(documents > 40, "only {documents} documents");
}
