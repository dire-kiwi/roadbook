//! The `roadbook` binary, run as a user would.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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

/// A fresh directory with a `roadbook.toml` and the given files.
fn project(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("roadbook-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("roadbook.toml"), "").unwrap();
    for (path, src) in files {
        let p = dir.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, src).unwrap();
    }
    dir
}

const OK: &str = "machine M() {\n  initial Done\n  final state Done {}\n}\n";

#[test]
fn the_messaging_examples_check_and_format_clean() {
    let dirs = ["examples/upload", "examples/inventory", "examples/orders"];
    let mut args = vec!["check"];
    args.extend(dirs);
    let out = roadbook(&repo(), &args);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(text(&out.stderr), "");
    let mut args = vec!["fmt", "--check"];
    args.extend(dirs);
    let out = roadbook(&repo(), &args);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stdout));
    // The order imports `Inventory` from the package `inventory`, which the root
    // `roadbook.toml` maps to `examples/inventory`.
    let out = roadbook(&repo(), &["export", "ir", "examples/orders/orders.road"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let ir: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        ir["sources"],
        serde_json::json!([
            { "path": "examples/orders/orders.road", "package": null },
            { "path": "inventory.road", "package": "inventory" }
        ])
    );
}

#[test]
fn packages_are_found_through_roadbook_toml_and_node_modules() {
    let chart = "import { Stock } from \"stock/stock.road\"\nimport { Item } from \"@acme/items/items.road\"\n\nmachine M(stock: ref Stock, item: Item) {\n  initial A\n  state A {\n    entry send stock.Take { item: item }\n    on stock.Taken -> Done\n  }\n  final state Done {}\n}\n";
    let stock = "import { Item } from \"@acme/items/items.road\"\n\nmachine Stock() {\n  event Take { item: Item }\n  emits Taken\n\n  initial A\n  state A {\n    on Take -> A  emit Taken\n  }\n}\n";
    let dir = project(
        "packages",
        &[
            ("app/m.road", chart),
            ("libs/stock/stock.road", stock),
            (
                "node_modules/@acme/items/package.json",
                "{ \"name\": \"@acme/items\" }",
            ),
            (
                "node_modules/@acme/items/items.road",
                "type Item { sku: string }\n",
            ),
        ],
    );
    std::fs::write(
        dir.join("roadbook.toml"),
        "exclude = [\"libs\"]\n\n[packages]\nstock = \"libs/stock\"\n",
    )
    .unwrap();
    let out = roadbook(&dir, &["check", "app"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(text(&out.stderr), "");
    // A package's own warnings are not reported, and its file is named by where it is.
    std::fs::write(
        dir.join("libs/stock/stock.road"),
        stock.replace("Take { item: Item }", "Take { item: Itm }"),
    )
    .unwrap();
    let out = roadbook(&dir, &["check", "--json", "app"]);
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["diagnostics"][0]["code"], "RB0101");
    assert_eq!(json["diagnostics"][0]["file"], "libs/stock/stock.road");
    std::fs::write(dir.join("roadbook.toml"), "[packages]\n").unwrap();
    let out = roadbook(&dir, &["check", "app"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("package `stock` is not known"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn the_shop_checks_and_formats_clean() {
    let out = roadbook(&repo(), &["check", "examples/shop"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    // No diagnostics: only the verdicts of `place_order.road`'s checks.
    assert_eq!(text(&out.stderr), "checks: 2 proved, 1 conditional\n");
    assert_eq!(text(&out.stdout), "");
    let out = roadbook(&repo(), &["fmt", "--check", "examples/shop"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stdout));
}

#[test]
fn export_ir_writes_the_fixture() {
    let out = roadbook(&repo(), &["export", "ir", "examples/shop/place_order.road"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let fixture = include_str!("../../roadbook-ir/tests/data/place_order.ir.json");
    assert!(text(&out.stdout) == fixture);
    let ir = roadbook_ir::Ir::from_json(&text(&out.stdout)).unwrap();
    assert_eq!(ir.sources[0].path, "examples/shop/place_order.road");

    let dir = project("export", &[("m.road", OK)]);
    let out = roadbook(&dir, &["export", "ir", "m.road", "-o", "m.ir.json"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let written = std::fs::read_to_string(dir.join("m.ir.json")).unwrap();
    assert!(written.contains("\"irVersion\": \"0.14\""));
}

#[test]
fn errors_exit_1_and_warnings_exit_0() {
    let bad =
        "machine M() {\n  initial Done\n  state Busy {}\n  final state Done { output 1 }\n}\n";
    let warn = "machine M() {\n  event Go\n  initial Done\n  final state Done {}\n}\n";
    let dir = project("exit", &[("bad.road", bad), ("sub/warn.road", warn)]);
    let out = roadbook(&dir, &["check", "bad.road"]);
    assert_eq!(out.status.code(), Some(1));
    let err = text(&out.stderr);
    assert!(err.contains("error[RB0310]"), "{err}");
    assert!(err.contains("--> bad.road:4:29"), "{err}");
    assert!(err.ends_with("1 error\n"), "{err}");

    let out = roadbook(&dir.join("sub"), &["check", "warn.road"]);
    assert_eq!(out.status.code(), Some(0));
    let err = text(&out.stderr);
    // Paths are relative to the project root, found upward from the file.
    assert!(
        err.contains("warning[RB0903]") && err.contains("--> sub/warn.road:2:9"),
        "{err}"
    );

    let out = roadbook(&dir, &["export", "ir", "bad.road"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(text(&out.stdout), "");
}

#[test]
fn check_with_no_paths_checks_the_whole_project() {
    let a = "import { T } from \"./lib/t.road\"\n\nmachine M(t: T) {\n  initial Done\n  final state Done {}\n}\n";
    let t = "type T { x: int }\n";
    let dir = project(
        "all",
        &[
            ("a.road", a),
            ("lib/t.road", t),
            ("target/skip.road", "broken {"),
            ("node_modules/x.road", "broken {"),
        ],
    );
    let out = roadbook(&dir, &["check"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(text(&out.stderr), "");
}

#[test]
fn check_json_is_one_object() {
    let bad = "machine M() {\n  initial Dne\n  final state Done {}\n}\n";
    let dir = project("json", &[("m.road", bad)]);
    let out = roadbook(&dir, &["check", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_str(&text(&out.stdout)).unwrap();
    assert_eq!(v["errors"], 1);
    assert_eq!(v["warnings"], 0);
    let d = &v["diagnostics"][0];
    assert_eq!(d["code"], "RB0203");
    assert_eq!(d["severity"], "error");
    assert_eq!(d["file"], "m.road");
    assert_eq!((d["line"].as_u64(), d["col"].as_u64()), (Some(2), Some(11)));
    assert_eq!(
        (d["endLine"].as_u64(), d["endCol"].as_u64()),
        (Some(2), Some(14))
    );
    assert_eq!(d["fixes"][0]["edits"][0]["text"], "Done");
    assert!(d["help"].is_string());
    assert!(d["labels"].is_array());
    for key in [
        "code", "severity", "message", "file", "line", "col", "endLine", "endCol", "label",
        "labels", "help", "fixes",
    ] {
        assert!(d.get(key).is_some(), "{key}");
    }
    assert_eq!(text(&out.stderr), "");
}

/// A chart with a check of each verdict but `unchecked`.
const CHECKED: &str = "\
fn ok() -> bool

machine Pay() {
  event Go
  event Back

  initial Idle

  state Idle    { on Go if ok() -> Paying }
  state Paying {
    on Back -> Idle
    always -> Charged
  }
  state Charged { on Go -> Done }
  final state Done {}

  check reachable Idle
  /// Charging needs the payment approved.
  check reachable Charged
  check never Charged
  check Charged only after Paying
  check can finish
}
";

#[test]
fn check_counts_the_verdicts_of_checks_and_lists_them_with_checks() {
    let dir = project("checks", &[("pay.road", CHECKED), ("ok.road", OK)]);
    let out = roadbook(&dir, &["check"]);
    // The failing `never` is an error.
    assert_eq!(out.status.code(), Some(1));
    let stderr = text(&out.stderr);
    assert!(
        stderr
            .starts_with("error[RB0501]: `never Charged` does not hold: `Charged` can be active\n"),
        "{stderr}"
    );
    assert!(
        stderr.ends_with("\n1 error\nchecks: 2 proved, 2 conditional, 1 failed\n"),
        "{stderr}"
    );
    let out = roadbook(&dir, &["check", "--checks"]);
    assert_eq!(out.status.code(), Some(1));
    let stderr = text(&out.stderr);
    let list = "\
1 error
pay.road:17: Pay: check reachable Idle: proved
pay.road:19: Pay: check reachable Charged: conditional
    /// Charging needs the payment approved.
    if `ok()` holds on the way
    Idle --Go [ok()]--> Paying --always--> Charged
pay.road:20: Pay: check never Charged: failed
pay.road:21: Pay: check Charged only after Paying: proved
pay.road:22: Pay: check can finish: conditional
    if `ok()` holds on the way
    Idle --Go [ok()]--> Paying --always--> Charged --Go--> Done
checks: 2 proved, 2 conditional, 1 failed
";
    assert!(stderr.ends_with(list), "{stderr}");
    // With no diagnostics, the summary line alone.
    let passing = CHECKED.replace("  check never Charged\n", "");
    let dir = project("checks-pass", &[("pay.road", &passing)]);
    let out = roadbook(&dir, &["check"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(text(&out.stderr), "checks: 2 proved, 2 conditional\n");
    // No checks, no line.
    let dir = project("checks-none", &[("ok.road", OK)]);
    let out = roadbook(&dir, &["check", "--checks"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(text(&out.stderr), "");
}

#[test]
fn check_json_lists_every_check_with_its_verdict() {
    let dir = project("checks-json", &[("pay.road", CHECKED)]);
    let out = roadbook(&dir, &["check", "--json", "--checks"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(text(&out.stderr), "");
    let stdout = text(&out.stdout);
    // `checks` comes last, after `fatal`.
    assert!(
        stdout.contains("\n  \"fatal\": null,\n  \"checks\": [\n"),
        "{stdout}"
    );
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(v["diagnostics"][0]["code"], "RB0501");
    let checks = v["checks"].as_array().unwrap();
    let verdicts: Vec<(&str, &str)> = checks
        .iter()
        .map(|c| (c["check"].as_str().unwrap(), c["verdict"].as_str().unwrap()))
        .collect();
    assert_eq!(
        verdicts,
        [
            ("reachable Idle", "proved"),
            ("reachable Charged", "conditional"),
            ("never Charged", "failed"),
            ("Charged only after Paying", "proved"),
            ("can finish", "conditional"),
        ]
    );
    assert_eq!(
        checks[1],
        serde_json::json!({
            "machine": "Pay", "check": "reachable Charged",
            "doc": "Charging needs the payment approved.",
            "file": "pay.road", "line": 19, "col": 3, "endLine": 19, "endCol": 26,
            "verdict": "conditional",
            "assumptions": ["`ok()` holds"],
            "path": [
                { "from": "Pay.Idle", "index": 0, "trigger": "Go", "guard": "ok()", "to": ["Pay.Paying"] },
                { "from": "Pay.Paying", "index": 1, "trigger": "always", "guard": null, "to": ["Pay.Charged"] }
            ]
        })
    );
    // A proved `reachable` has its witness; a proved `never` none; a failed one its
    // counterexample.
    assert_eq!(checks[0]["path"], serde_json::json!([]));
    assert_eq!(checks[0]["doc"], serde_json::Value::Null);
    assert_eq!(checks[3]["path"], serde_json::json!([]));
    assert_eq!(
        checks[2]["assumptions"],
        serde_json::json!(["`ok()` holds"])
    );
    assert_eq!(checks[2]["path"].as_array().unwrap().len(), 2);
    // `unchecked`: an option the command line does not have, so from the compiler.
    let options = roadbook_compiler::Options {
        max_configurations: Some(1),
        ..Default::default()
    };
    let mut loader = roadbook_compiler::MemoryLoader::new().with("pay.road", CHECKED);
    let out = roadbook_compiler::check(&mut loader, &["pay.road".to_string()], &options).unwrap();
    let v: serde_json::Value = serde_json::from_str(&out.to_json()).unwrap();
    assert!(
        v["checks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["verdict"] == "unchecked" && c["path"] == serde_json::json!([]))
    );
    assert_eq!(v["warnings"], 5);
    // A usage error: no checks.
    let out = roadbook(&dir, &["check", "--json", "nope.road"]);
    let v: serde_json::Value = serde_json::from_str(&text(&out.stdout)).unwrap();
    assert_eq!(v["checks"], serde_json::json!([]));
}

#[test]
fn warn_invoke_timeout_is_opt_in() {
    let src = "task t()\n\nmachine M() {\n  initial A\n  state A {\n    invoke t() {\n      done -> Done\n    }\n  }\n  final state Done {}\n}\n";
    let dir = project("timeout", &[("m.road", src)]);
    let out = roadbook(&dir, &["check"]);
    assert_eq!(text(&out.stderr), "");
    let out = roadbook(&dir, &["check", "--warn", "invoke-timeout"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(text(&out.stderr).contains("warning[RB0904]"));
}

#[test]
fn allow_unassigned_turns_off_definite_assignment_for_its_files() {
    let src = "action log(n: int)\n\nmachine M() {\n  var n: int\n  initial A\n  final state A { entry do log(n) }\n}\n";
    let dir = project(
        "unassigned",
        &[
            ("a/m.road", src),
            ("b/m.road", src),
            ("roadbook.toml", "allow-unassigned = [\"a/*.road\"]\n"),
        ],
    );
    let out = roadbook(&dir, &["check"]);
    assert_eq!(out.status.code(), Some(1));
    let err = text(&out.stderr);
    assert!(
        err.contains("--> b/m.road") && !err.contains("--> a/m.road"),
        "{err}"
    );
    let out = roadbook(&dir, &["export", "ir", "a/m.road"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
}

#[test]
fn fmt_rewrites_in_place_and_check_lists_files() {
    let messy = "machine M() {\n    initial Done\n  final state Done {\n  }\n}\n";
    let dir = project("fmt", &[("a.road", messy), ("b.road", OK)]);
    let out = roadbook(&dir, &["fmt", "--check"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(text(&out.stdout), "a.road\n");
    assert_eq!(std::fs::read_to_string(dir.join("a.road")).unwrap(), messy);

    let out = roadbook(&dir, &["fmt"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(std::fs::read_to_string(dir.join("a.road")).unwrap(), OK);
    let out = roadbook(&dir, &["fmt", "--check"]);
    assert_eq!(out.status.code(), Some(0));

    std::fs::write(dir.join("c.road"), "machine {").unwrap();
    let out = roadbook(&dir, &["fmt", "c.road"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).contains("error[RB0"));
}

#[test]
fn usage_and_io_errors_exit_2() {
    let dir = project("usage", &[]);
    assert_eq!(
        roadbook(&dir, &["check", "missing.road"]).status.code(),
        Some(2)
    );
    assert_eq!(
        roadbook(&dir, &["export", "ir", "missing.road"])
            .status
            .code(),
        Some(2)
    );
    assert_eq!(roadbook(&dir, &["frobnicate"]).status.code(), Some(2));
    assert_eq!(
        roadbook(&dir, &["check", "--warn", "nope"]).status.code(),
        Some(2)
    );
}

#[test]
fn check_json_reports_a_usage_error_as_the_object() {
    let dir = project("json-fatal", &[]);
    let out = roadbook(&dir, &["check", "--json", "nope.road"]);
    assert_eq!(out.status.code(), Some(2));
    let v: serde_json::Value = serde_json::from_str(&text(&out.stdout)).unwrap();
    assert_eq!(v["diagnostics"], serde_json::json!([]));
    assert_eq!(
        (v["errors"].as_u64(), v["warnings"].as_u64()),
        (Some(0), Some(0))
    );
    assert!(v["fatal"].as_str().unwrap().contains("nope.road"));
    assert!(text(&out.stderr).contains("nope.road"));

    let dir = project("json-ok", &[("m.road", OK)]);
    let out = roadbook(&dir, &["check", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&text(&out.stdout)).unwrap();
    assert_eq!(v["fatal"], serde_json::Value::Null);
}

/// Each file is compiled as an entry, with what it imports: charts that do not import one
/// another may reuse names, and a shared import is reported on once.
#[test]
fn independent_charts_may_share_names() {
    let shared = "action log(s: string)\naction unused(s: string)\ntype T { x: int\n";
    let chart = |m: &str| {
        format!(
            "import {{ log }} from \"./lib.road\"\n\nmachine {m}() {{\n  initial A\n  state A {{ entry do log(\"a\") }}\n}}\n"
        )
    };
    let dir = project(
        "independent",
        &[
            ("a.road", &chart("M")),
            ("b.road", &chart("M")),
            (
                "lib.road",
                "action log(s: string)\naction unused(s: string)\n",
            ),
        ],
    );
    let out = roadbook(&dir, &["check"]);
    let err = text(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{err}");
    // `M.A` has no way out, once per chart; `unused` is reported once, `log` not at all.
    assert_eq!(err.matches("warning[RB0902]").count(), 2, "{err}");
    assert_eq!(
        err.matches("action `unused` is never used").count(),
        1,
        "{err}"
    );
    assert!(!err.contains("`log`"), "{err}");

    // A syntax error in one file leaves the others checked fully, warnings included.
    std::fs::write(dir.join("lib.road"), shared).unwrap();
    std::fs::write(
        dir.join("c.road"),
        chart("C").replace("./lib.road", "./d.road"),
    )
    .unwrap();
    std::fs::write(dir.join("d.road"), "action log(s: string)\n").unwrap();
    let out = roadbook(&dir, &["check"]);
    let err = text(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert_eq!(err.matches("error[RB0011]").count(), 1, "{err}");
    assert_eq!(err.matches("error[").count(), 1, "{err}");
    assert!(err.contains("state `C.A` has no way out"), "{err}");
}

#[test]
fn excluded_paths_and_linked_directories_are_skipped() {
    let dir = project(
        "exclude",
        &[
            ("a.road", OK),
            ("broken/x.road", "broken {"),
            ("gen/deep/y.road", "broken {"),
            ("gen/keep.road", OK),
        ],
    );
    std::fs::write(
        dir.join("roadbook.toml"),
        "exclude = [\n  \"broken\", # on purpose\n  \"gen/**/y.road\",\n]\n",
    )
    .unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(".", dir.join("loop")).unwrap();
    let out = roadbook(&dir, &["check"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(text(&out.stderr), "");
    // A file named on the command line is checked even if excluded.
    let out = roadbook(&dir, &["check", "broken/x.road"]);
    assert_eq!(out.status.code(), Some(1));
    std::fs::write(dir.join("roadbook.toml"), "exclude = \"broken\"\n").unwrap();
    assert_eq!(roadbook(&dir, &["check"]).status.code(), Some(2));
}

#[test]
fn the_repository_checks_clean_from_its_root() {
    let out = roadbook(&repo(), &["check"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(!text(&out.stderr).contains("error["));
    let out = roadbook(&repo(), &["check", "spec/conformance/charts"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
}

/// A fresh, empty output directory.
fn out_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("roadbook-viz-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// The JSON a page holds in `<script type="application/json" id="rb-data">`.
fn page_data(html: &str) -> serde_json::Value {
    let start = html.find("id=\"rb-data\">").unwrap() + "id=\"rb-data\">".len();
    let end = start + html[start..].find("</script>").unwrap();
    serde_json::from_str(&html[start..end]).unwrap()
}

#[test]
fn viz_writes_a_page_per_entry_and_an_index() {
    let out = out_dir("shop");
    let o = out.to_str().unwrap();
    let res = roadbook(&repo(), &["viz", "examples/shop", "-o", o]);
    assert_eq!(res.status.code(), Some(0), "{}", text(&res.stderr));
    let mut files: Vec<String> = std::fs::read_dir(&out)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    files.sort();
    // checkout.road and types.road are imported by place_order.road: one entry.
    assert_eq!(
        files,
        ["index.html", "place_order.html", "roadbook-viewer.js"]
    );

    let page = std::fs::read_to_string(out.join("place_order.html")).unwrap();
    assert!(page.contains("<script src=\"roadbook-viewer.js\"></script>"));
    assert!(
        !page.contains("http://") && !page.contains("https://"),
        "{page}"
    );
    // The IR is embedded whole, in source order, as `export ir` writes it.
    let data = page_data(&page);
    assert_eq!(data["kind"], "chart");
    assert_eq!(data["entry"], "examples/shop/place_order.road");
    assert_eq!(data["index"], "./index.html");
    let exported = roadbook(&repo(), &["export", "ir", "examples/shop/place_order.road"]);
    let ir = roadbook_ir::Ir::from_json(&data["ir"].to_string()).unwrap();
    assert_eq!(
        ir,
        roadbook_ir::Ir::from_json(&text(&exported.stdout)).unwrap()
    );
    let ir_at = page.find("\"ir\":").unwrap();
    assert!(page[ir_at..].find("\"PlaceOrder\"") < page[ir_at..].find("\"Checkout\""));

    let index = std::fs::read_to_string(out.join("index.html")).unwrap();
    assert!(!index.contains("http://") && !index.contains("https://"));
    let data = page_data(&index);
    let machines = data["index"]["machines"].as_array().unwrap();
    let names: Vec<&str> = machines
        .iter()
        .map(|m| m["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["PlaceOrder", "Checkout"]);
    assert_eq!(machines[0]["invokes"], serde_json::json!([1]));
    assert_eq!(machines[1]["source"], "examples/shop/checkout.road:4");
    assert_eq!(data["index"]["pages"][0]["href"], "./place_order.html");

    let js = std::fs::read_to_string(out.join("roadbook-viewer.js")).unwrap();
    assert_eq!(
        js,
        include_str!("../assets/roadbook-viewer.js"),
        "the shared script is the embedded viewer"
    );
}

#[test]
fn viz_trace_is_embedded_and_opens_in_replay() {
    let out = out_dir("trace");
    let o = out.to_str().unwrap();
    let trace = "spec/conformance/cases/place-order-checkout/trace.jsonl";
    let res = roadbook(
        &repo(),
        &["viz", "examples/shop", "-o", o, "--trace", trace],
    );
    assert_eq!(res.status.code(), Some(0), "{}", text(&res.stderr));
    // The frozen copy of the shop has the same hashes as the example: no warning.
    assert!(
        !text(&res.stderr).contains("warning"),
        "{}",
        text(&res.stderr)
    );
    let page = std::fs::read_to_string(out.join("place_order.html")).unwrap();
    let data = page_data(&page);
    assert_eq!(data["mode"], "replay");
    assert_eq!(data["trace"]["name"], "trace.jsonl");
    assert_eq!(
        data["trace"]["text"].as_str().unwrap(),
        std::fs::read_to_string(repo().join(trace)).unwrap(),
        "the trace is embedded as it is"
    );

    // Without --trace the page opens in explore mode, with no trace.
    let plain = out_dir("trace-none");
    let res = roadbook(
        &repo(),
        &["viz", "examples/shop", "-o", plain.to_str().unwrap()],
    );
    assert_eq!(res.status.code(), Some(0), "{}", text(&res.stderr));
    let data = page_data(&std::fs::read_to_string(plain.join("place_order.html")).unwrap());
    assert_eq!(data["mode"], "explore");
    assert!(data["trace"].is_null());

    // Another version of the machine: written all the same, with a warning.
    let dir = out_dir("trace-files");
    std::fs::create_dir_all(&dir).unwrap();
    let other = dir.join("other.jsonl");
    let records = std::fs::read_to_string(repo().join(trace)).unwrap();
    let first: serde_json::Value = serde_json::from_str(records.lines().next().unwrap()).unwrap();
    let hash = first["ir"].as_str().unwrap();
    let zeros = format!("sha256:{}", "0".repeat(64));
    std::fs::write(&other, records.replace(hash, &zeros)).unwrap();
    let res = roadbook(
        &repo(),
        &[
            "viz",
            "examples/shop",
            "-o",
            o,
            "--trace",
            other.to_str().unwrap(),
        ],
    );
    assert_eq!(res.status.code(), Some(0), "{}", text(&res.stderr));
    assert!(
        text(&res.stderr)
            .contains("warning: `other.jsonl` was recorded from another version of PlaceOrder"),
        "{}",
        text(&res.stderr)
    );

    // A machine no chart has, and a file that is not a trace, are errors.
    let elsewhere = dir.join("elsewhere.jsonl");
    std::fs::write(
        &elsewhere,
        "{\"input\":{},\"ir\":\"sha256:00\",\"kind\":\"started\",\"machine\":\"Nope\",\"run\":\"r1\",\"t\":0}\n",
    )
    .unwrap();
    let res = roadbook(
        &repo(),
        &[
            "viz",
            "examples/shop",
            "-o",
            o,
            "--trace",
            elsewhere.to_str().unwrap(),
        ],
    );
    assert_eq!(res.status.code(), Some(2));
    assert!(
        text(&res.stderr).contains("its run r1 is of machine Nope, which none of the charts has"),
        "{}",
        text(&res.stderr)
    );
    let junk = dir.join("junk.jsonl");
    std::fs::write(&junk, "{\"kind\":\"started\"}\nnot json\n").unwrap();
    let res = roadbook(
        &repo(),
        &[
            "viz",
            "examples/shop",
            "-o",
            o,
            "--trace",
            junk.to_str().unwrap(),
        ],
    );
    assert_eq!(res.status.code(), Some(2));
    assert!(
        text(&res.stderr).contains("line 1: not a trace record"),
        "{}",
        text(&res.stderr)
    );
}

#[test]
fn viz_self_contained_pages_need_nothing_else() {
    let out = out_dir("single");
    let o = out.to_str().unwrap();
    let res = roadbook(
        &repo(),
        &["viz", "examples/shop", "-o", o, "--self-contained"],
    );
    assert_eq!(res.status.code(), Some(0), "{}", text(&res.stderr));
    assert!(!out.join("roadbook-viewer.js").exists());
    for page in ["place_order.html", "index.html"] {
        let html = std::fs::read_to_string(out.join(page)).unwrap();
        assert!(!html.contains("<script src="), "{page}");
        assert!(!html.contains("<link"), "{page}");
        for attr in [
            "src=\"http",
            "href=\"http",
            "url(http",
            "src='http",
            "href='http",
        ] {
            assert!(!html.contains(attr), "{page} has {attr}");
        }
        // The viewer is inline, and nothing in it ends the script element early.
        let body = &html[html.find("<script>").unwrap()..];
        assert_eq!(body.matches("</script>").count(), 1, "{page}");
        assert!(html.len() > 1_000_000);
    }
}

/// A collection of tasks is listed in the index under `spawnsTasks`, by task, since a task has
/// no box of its own; a stream task invoked with `as` is no relation of the machine's.
#[test]
fn viz_index_lists_the_tasks_a_machine_spawns() {
    let out = out_dir("tasks");
    let o = out.to_str().unwrap();
    let res = roadbook(
        &repo(),
        &[
            "viz",
            "spec/conformance/charts/spawn-tasks.road",
            "spec/conformance/charts/streams.road",
            "-o",
            o,
        ],
    );
    assert_eq!(res.status.code(), Some(0), "{}", text(&res.stderr));
    let index = page_data(&std::fs::read_to_string(out.join("index.html")).unwrap());
    let machines = index["index"]["machines"].as_array().unwrap();
    let at = |name: &str| machines.iter().position(|m| m["name"] == name).unwrap();
    let batch = &machines[at("Batch")];
    assert_eq!(
        batch["spawnsTasks"],
        serde_json::json!([{ "task": "upload", "names": ["uploads"] }])
    );
    assert_eq!(batch["spawns"], serde_json::json!([]));
    assert_eq!(batch["receives"], serde_json::json!([]));
    let clock = &machines[at("Clock")];
    assert_eq!(clock["spawnsTasks"], serde_json::json!([]));
    assert_eq!(clock["receives"], serde_json::json!([]));
}

#[test]
fn viz_names_pages_after_their_entries_and_reports_errors() {
    let out = out_dir("examples");
    let o = out.to_str().unwrap();
    let res = roadbook(&repo(), &["viz", "examples", "-o", o]);
    assert_eq!(res.status.code(), Some(0), "{}", text(&res.stderr));
    for page in [
        "shop-place_order.html",
        "large-ticket.html",
        "password-reset-confirm.html",
        "webhook-webhook.html",
        "import-job-import.html",
        "upload-publish.html",
        "orders-orders.html",
        "index.html",
    ] {
        assert!(out.join(page).is_file(), "{page}");
    }
    let index = page_data(&std::fs::read_to_string(out.join("index.html")).unwrap());
    let machines = index["index"]["machines"].as_array().unwrap();
    assert_eq!(machines.len(), 14);
    // Who talks to whom (`projectIndex` in packages/viewer/src/project.ts builds the same).
    let at = |name: &str| machines.iter().position(|m| m["name"] == name).unwrap();
    let (order, inventory) = (&machines[at("Order")], at("Inventory"));
    assert_eq!(
        order["refs"],
        serde_json::json!([{ "to": inventory, "names": ["inventory"] }])
    );
    assert_eq!(
        order["sends"],
        serde_json::json!([{ "to": inventory, "events": ["Release"] }])
    );
    assert_eq!(order["receives"], serde_json::json!([]));
    assert_eq!(
        order["calls"],
        serde_json::json!([{ "to": inventory, "calls": ["reserve"] }])
    );
    let (publish, upload) = (&machines[at("Publish")], at("Upload"));
    assert_eq!(publish["invokes"], serde_json::json!([upload]));
    assert_eq!(publish["refs"], serde_json::json!([]));
    assert_eq!(
        publish["sends"],
        serde_json::json!([{ "to": upload, "events": ["Pause", "Resume"] }])
    );
    assert_eq!(
        publish["receives"],
        serde_json::json!([{ "from": upload, "events": ["Progress"] }])
    );
    // A machine that spawns children: spawns, sends to and receives from them by collection.
    let batch = &machines[at("Batch")];
    assert_eq!(batch["invokes"], serde_json::json!([]));
    assert_eq!(
        batch["spawns"],
        serde_json::json!([{ "to": upload, "names": ["uploads"] }])
    );
    assert_eq!(
        batch["sends"],
        serde_json::json!([{ "to": upload, "events": ["Pause", "Resume"] }])
    );
    assert_eq!(
        batch["receives"],
        serde_json::json!([{ "from": upload, "events": ["Progress"] }])
    );
    // A machine of another package is named by its package and its path in it.
    assert_eq!(machines[inventory]["source"], "inventory/inventory.road:18");
    for m in machines {
        if m["name"] != "Order" && m["name"] != "Publish" && m["name"] != "Batch" {
            assert_eq!(m["sends"], serde_json::json!([]), "{m}");
            assert_eq!(m["receives"], serde_json::json!([]), "{m}");
        }
        if m["name"] != "Order" {
            assert_eq!(m["calls"], serde_json::json!([]), "{m}");
        }
    }

    let dir = project(
        "viz-bad",
        &[("bad.road", "machine M() {\n  initial Nope\n}\n")],
    );
    let res = roadbook(&dir, &["viz", "-o", "out"]);
    assert_eq!(res.status.code(), Some(1));
    assert!(text(&res.stderr).contains("error["));
    assert!(!dir.join("out").exists());
}

#[test]
fn viz_links_are_relative_whatever_the_file_names() {
    let dir = project(
        "viz-href",
        &[("javascript:alert(1).road", OK), ("a b#c.road", OK)],
    );
    let res = roadbook(&dir, &["viz", "-o", "out"]);
    assert_eq!(res.status.code(), Some(0), "{}", text(&res.stderr));
    let index = page_data(&std::fs::read_to_string(dir.join("out/index.html")).unwrap());
    let hrefs: Vec<&str> = index["index"]["pages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["href"].as_str().unwrap())
        .collect();
    assert_eq!(
        hrefs,
        ["./a%20b%23c.html", "./javascript%3Aalert%281%29.html"]
    );
    for page in ["a b#c.html", "javascript:alert(1).html"] {
        let data = page_data(&std::fs::read_to_string(dir.join("out").join(page)).unwrap());
        assert_eq!(data["index"], "./index.html");
    }
}

/// IR text and file names full of markup reach the pages only as escaped text or JSON.
#[test]
fn viz_escapes_ir_text_in_pages() {
    let chart =
        std::fs::read_to_string(repo().join("packages/viewer/test/charts/escape.road")).unwrap();
    let dir = project("viz-escape", &[("a<b>&\"c\".road", &chart)]);
    let res = roadbook(&dir, &["viz", "-o", "out"]);
    assert_eq!(res.status.code(), Some(0), "{}", text(&res.stderr));
    let page = std::fs::read_to_string(dir.join("out/a<b>&\"c\".html")).unwrap();
    let index = std::fs::read_to_string(dir.join("out/index.html")).unwrap();
    for html in [&page, &index] {
        // The data's script element and the viewer's: nothing else closes one.
        assert_eq!(html.matches("</script").count(), 2, "{html}");
        for raw in ["<img", "<svg", "<script>alert", "<!--", "<b>"] {
            assert!(!html.contains(raw), "{raw} in {html}");
        }
    }
    assert!(
        page.contains("<title>Escape · a&lt;b&gt;&amp;\"c\".road · Roadbook</title>"),
        "{page}"
    );
    let data = page_data(&page);
    let doc = data["ir"]["machines"]["Escape"]["doc"].as_str().unwrap();
    assert!(doc.starts_with("Doc </script><script>alert('doc')</script>"));
    assert_eq!(data["entry"], "a<b>&\"c\".road");
    let data = page_data(&index);
    assert_eq!(
        data["index"]["pages"][0]["href"],
        "./a%3Cb%3E%26%22c%22.html"
    );
}

#[test]
fn viz_watch_serves_and_rebuilds() {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpStream;
    use std::process::Stdio;

    let dir = project("viz-watch", &[("m.road", OK)]);
    let mut child = Command::new(env!("CARGO_BIN_EXE_roadbook"))
        .args(["viz", "--watch", "--port", "0", "-o", "out"])
        .current_dir(&dir)
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut err = BufReader::new(child.stderr.take().unwrap());
    let mut port = None;
    let mut line = String::new();
    while port.is_none() && err.read_line(&mut line).unwrap() > 0 {
        if let Some(rest) = line.split("http://127.0.0.1:").nth(1) {
            port = rest.split('/').next().map(|p| p.parse::<u16>().unwrap());
        }
        line.clear();
    }
    let port = port.expect("the server's address");
    let get_from = |host: &str, path: &str| {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(s, "GET {path} HTTP/1.1\r\nHost: {host}\r\n\r\n").unwrap();
        let mut r = String::new();
        s.read_to_string(&mut r).unwrap();
        r
    };
    let local = format!("localhost:{port}");
    let get = |path: &str| get_from(&local, path);
    // DNS rebinding: a name that is not this machine's is refused, as is another port.
    for host in [format!("evil.example:{port}"), "localhost:1".to_string()] {
        assert!(
            get_from(&host, "/m.html").starts_with("HTTP/1.1 403"),
            "{host}"
        );
    }
    assert!(get_from(&format!("127.0.0.1:{port}"), "/m.html").starts_with("HTTP/1.1 200"));
    let index = get("/");
    assert!(index.starts_with("HTTP/1.1 200"), "{index}");
    assert!(
        index.contains("/__roadbook/version"),
        "the reload script is added"
    );
    assert!(get("/m.html").contains("\"kind\":\"chart\""));
    assert!(get("/../roadbook.toml").starts_with("HTTP/1.1 404"));
    let body = |r: String| r.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    let v1 = body(get("/__roadbook/version"));

    std::fs::write(
        dir.join("m.road"),
        OK.replace("final state Done {}", "final state Finished {}")
            .replace("initial Done", "initial Finished"),
    )
    .unwrap();
    let mut changed = false;
    for _ in 0..40 {
        std::thread::sleep(std::time::Duration::from_millis(250));
        if body(get("/__roadbook/version")) != v1 {
            changed = true;
            break;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    assert!(changed, "the version changes after an edit");
    let page = std::fs::read_to_string(dir.join("out/m.html")).unwrap();
    assert!(page.contains("M.Finished"));
}

#[test]
fn export_mermaid_snapshots_the_shop() {
    let out = roadbook(
        &repo(),
        &["export", "mermaid", "examples/shop/place_order.road"],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    insta::assert_snapshot!("shop_mermaid", text(&out.stdout));
    let out = roadbook(
        &repo(),
        &[
            "export",
            "mermaid",
            "examples/shop/place_order.road",
            "--machine",
            "Checkout",
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    insta::assert_snapshot!("checkout_mermaid", text(&out.stdout));
    let out = roadbook(
        &repo(),
        &[
            "export",
            "mermaid",
            "examples/shop/place_order.road",
            "--machine",
            "Nope",
        ],
    );
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn export_mermaid_snapshots_calls() {
    let out = roadbook(
        &repo(),
        &["export", "mermaid", "spec/conformance/charts/calls.road"],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    insta::assert_snapshot!("calls_mermaid", text(&out.stdout));
}

/// A state's parameters label it, its variables are lines of its description, and a
/// transition's arguments end its label.
#[test]
fn export_mermaid_snapshots_state_locals() {
    let out = roadbook(
        &repo(),
        &[
            "export",
            "mermaid",
            "crates/roadbook-compiler/tests/pass/locals.road",
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    insta::assert_snapshot!("locals_mermaid", text(&out.stdout));
}

/// The copies an `on A | B` is lowered to are one arrow, labelled `A | B`, with `[else]` only on
/// a block's last branch; one with no target is one line of its state's description.
#[test]
fn export_mermaid_merges_or_events() {
    let out = roadbook(
        &repo(),
        &[
            "export",
            "mermaid",
            "spec/conformance/charts/or-events.road",
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let md = text(&out.stdout);
    assert!(
        md.contains("Idle --> Connecting: Retry | Reconnect [attempts < 2] / set reason = event.reason, set attempts = attempts + 1"),
        "{md}"
    );
    assert_eq!(
        md.matches("Connecting --> Idle: Drop | Retry [event.reason == \"\"]")
            .count(),
        1,
        "{md}"
    );
    insta::assert_snapshot!("or_events_mermaid", md);
}

/// A history state's default targets are unlabelled arrows from its `H`, in the block where
/// the history state and the target meet (above a parallel state, as for any arrow).
#[test]
fn export_mermaid_draws_history_defaults() {
    let out = roadbook(
        &repo(),
        &[
            "export",
            "mermaid",
            "spec/conformance/charts/history-default.road",
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let md = text(&out.stdout);
    assert!(md.contains("    Doc_Previous --> Doc_Editing\n"), "{md}");
    assert!(
        md.contains("    Notebook_Last --> Notebook_Pages_Second\n"),
        "{md}"
    );
    assert!(md.contains("  View_Restore --> View_Main_Split\n"), "{md}");
    assert!(md.contains("  View_Restore --> View_Side_Wide\n"), "{md}");
    insta::assert_snapshot!("history_default_mermaid", md);
}

/// A choice state is Mermaid's `<<choice>>` diamond, its branches arrows labelled with their
/// guards (`[else]` for the last), with no `always`.
#[test]
fn export_mermaid_draws_choices() {
    let out = roadbook(
        &repo(),
        &["export", "mermaid", "spec/conformance/charts/choice.road"],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let md = text(&out.stdout);
    assert!(md.contains("  state Start <<choice>>\n"), "{md}");
    assert!(md.contains("    state Desk_Route <<choice>>\n"), "{md}");
    assert!(
        md.contains("    Desk_Route --> Desk_Vip: [vip(amount)]\n"),
        "{md}"
    );
    assert!(md.contains("  Start --> Idle: [else]\n"), "{md}");
    insta::assert_snapshot!("choice_mermaid", md);
}

/// A transition with several targets is an arrow to each, with the same label.
#[test]
fn export_mermaid_snapshots_several_targets() {
    let out = roadbook(
        &repo(),
        &["export", "mermaid", "spec/conformance/charts/targets.road"],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    insta::assert_snapshot!("targets_mermaid", text(&out.stdout));
}

/// What a task emits: `up.Progress` on an edge with its guard, and `on up.Progress` in a state's
/// description when it has no target; a stream task's invoke, with no outcomes; a collection of
/// tasks and its `spawn`, `uploads.Progress`, `uploads.done` and `uploads.fail *`.
#[test]
fn export_mermaid_snapshots_tasks_that_emit() {
    for (chart, snapshot) in [
        (
            "spec/conformance/charts/task-emits.road",
            "task_emits_mermaid",
        ),
        ("spec/conformance/charts/streams.road", "streams_mermaid"),
        (
            "spec/conformance/charts/spawn-tasks.road",
            "spawn_tasks_mermaid",
        ),
    ] {
        let out = roadbook(&repo(), &["export", "mermaid", chart]);
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        insta::assert_snapshot!(snapshot, text(&out.stdout));
    }
}

/// Raises and cancels in edge labels and state descriptions: `raise Ping { n: 1 }`,
/// `raise Remind { … } after 1s as reminder`, `cancel reminder`.
#[test]
fn export_mermaid_snapshots_raises() {
    let out = roadbook(
        &repo(),
        &["export", "mermaid", "spec/conformance/charts/raise.road"],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    insta::assert_snapshot!("raise_mermaid", text(&out.stdout));
}

/// A peer's end: `x.ended` on an edge with its guard (from the machine, for one in its body),
/// and `on x.ended` in a state's description when it has no target, `ignore` included.
#[test]
fn export_mermaid_draws_a_peer_s_end() {
    let chart = "action note(status: Ended)

machine Peer() {
  event Stop
  initial A
  state A { on Stop -> B }
  final state B {}
}

machine Holder(peer: ref Peer, spare: ref Peer?) {
  var current: ref Peer = peer
  initial Watching
  on peer.ended if event.status == Ended.Defect -> Alarm
  state Watching {
    on current.ended do note(event.status)
    on spare.ended ignore
  }
  final state Alarm {}
}
";
    let dir = project("mermaid-ended", &[("m.road", chart)]);
    let out = roadbook(
        &dir,
        &["export", "mermaid", "m.road", "--machine", "Holder"],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let md = text(&out.stdout);
    for line in [
        "Holder_machine --> Alarm: peer.ended [event.status == Ended.Defect]",
        "Watching: on current.ended / do note(event.status)",
        "Watching: on spare.ended ignore",
    ] {
        assert!(md.contains(line), "no `{line}` in\n{md}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn export_mermaid_nests_and_splits_regions() {
    let out = roadbook(
        &repo(),
        &[
            "export",
            "mermaid",
            "packages/viewer/test/charts/stress.road",
            "--machine",
            "Stress",
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let m = text(&out.stdout);
    assert!(
        m.starts_with(
            "stateDiagram-v2\n  [*] --> Stress_machine\n  state \"Stress\" as Stress_machine\n"
        ),
        "{m}"
    );
    assert!(m.contains("    state Running {\n"), "{m}");
    assert!(
        m.contains("\n      --\n"),
        "parallel regions are split: {m}"
    );
    assert!(
        m.contains("Running_Right_X_X1 --> Running_Right_X_X1: Poke"),
        "{m}"
    );
    assert!(m.contains("Stress_machine --> Start: Reset"), "{m}");
    assert!(
        m.contains("Start: on * / do note(#quot;ignored#quot;)")
            || m.contains("Start: on * / do note(\"ignored\")"),
        "{m}"
    );
}

/// The viewer bundle the CLI embeds was built from `packages/viewer` and `packages/core` as
/// they are now, and not edited since: its header names the hash of the sources, computed as
/// `packages/viewer/build.mjs` does, the versions of elkjs and esbuild in `pnpm-lock.yaml`,
/// and the hash of the script after it.
#[test]
fn viewer_asset_is_fresh() {
    use sha2::{Digest, Sha256};
    let hex = |d: &[u8]| -> String { d.iter().map(|b| format!("{b:02x}")).collect() };
    let packages = repo().join("packages");
    let pkg = packages.join("viewer");
    let mut files = Vec::new();
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let p = e.path();
            // Like regen.sh's `find`: links (`node_modules` holds some) are not followed.
            if e.file_type().unwrap().is_symlink() || p.ends_with("node_modules") {
                continue;
            }
            if p.is_dir() {
                walk(&p, out);
            } else {
                out.push(p);
            }
        }
    }
    walk(&pkg.join("src"), &mut files);
    walk(&packages.join("core/src"), &mut files);
    files.push(pkg.join("build.mjs"));
    // Named relative to `packages/`: `viewer/src/…`, `core/src/…`, `viewer/build.mjs`.
    let mut names: Vec<(String, PathBuf)> = files
        .into_iter()
        .map(|p| {
            let rel = p.strip_prefix(&packages).unwrap();
            let name: Vec<String> = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            (name.join("/"), p)
        })
        .collect();
    names.sort();
    let mut h = Sha256::new();
    for (name, p) in &names {
        h.update(name.as_bytes());
        h.update([0]);
        h.update(std::fs::read(p).unwrap());
        h.update([0]);
    }
    let sources = hex(&h.finalize());

    // `depsText` in build.mjs: in the lockfile's `packages/viewer:` importer, each package's
    // `version:`.
    let lock = std::fs::read_to_string(repo().join("pnpm-lock.yaml")).unwrap();
    let lines: Vec<&str> = lock.lines().collect();
    let start = lines
        .iter()
        .position(|l| *l == "  packages/viewer:")
        .expect("pnpm-lock.yaml has the packages/viewer importer");
    let block: Vec<&str> = lines[start + 1..]
        .iter()
        .take_while(|l| l.starts_with("   "))
        .copied()
        .collect();
    let deps: Vec<String> = ["elkjs", "esbuild"]
        .iter()
        .map(|dep| {
            let at = block
                .iter()
                .position(|l| *l == format!("      {dep}:"))
                .unwrap_or_else(|| panic!("no {dep} in the lockfile"));
            let version = block[at + 1..(at + 3).min(block.len())]
                .iter()
                .find_map(|l| l.strip_prefix("        version: "))
                .unwrap_or_else(|| panic!("no version of {dep} in the lockfile"));
            format!("{dep}@{}", version.trim())
        })
        .collect();

    let asset = include_str!("../assets/roadbook-viewer.js");
    let (header, body) = asset.split_once('\n').unwrap();
    let stale = "crates/roadbook-cli/assets/roadbook-viewer.js is stale: run `pnpm --filter @dire-kiwi/roadbook-viewer build` and commit it";
    assert!(
        header.contains(&format!("sources sha256:{sources} ")),
        "{stale} (the sources changed)\n{header}"
    );
    assert!(
        header.contains(&format!(" deps {} body ", deps.join(" "))),
        "{stale} (elkjs or esbuild changed: {})\n{header}",
        deps.join(" ")
    );
    let body_hash = hex(&Sha256::digest(body.as_bytes()));
    assert!(
        header.contains(&format!(" body sha256:{body_hash} ")),
        "{stale} (it was edited by hand)\n{header}"
    );
}

/// The IR the viewer's tests lay out is what the compiler writes now
/// (`packages/viewer/test/charts/regen.sh` regenerates it).
#[test]
fn viewer_test_charts_are_fresh() {
    let dir = repo().join("packages/viewer/test/charts");
    for (chart, name) in [
        ("examples/shop/place_order.road", "shop-place_order"),
        ("examples/large/ticket.road", "large-ticket"),
        (
            "examples/password-reset/confirm.road",
            "password-reset-confirm",
        ),
        ("examples/webhook/webhook.road", "webhook-webhook"),
        ("examples/import-job/import.road", "import-job-import"),
        ("examples/upload/publish.road", "upload-publish"),
        ("examples/orders/orders.road", "orders-orders"),
        ("packages/viewer/test/charts/stress.road", "charts-stress"),
        ("packages/viewer/test/charts/edges.road", "charts-edges"),
        ("packages/viewer/test/charts/spawn.road", "charts-spawn"),
        ("packages/viewer/test/charts/escape.road", "charts-escape"),
        ("packages/viewer/test/charts/history.road", "charts-history"),
    ] {
        let out = roadbook(&repo(), &["export", "ir", chart]);
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        let committed = std::fs::read_to_string(dir.join(format!("{name}.ir.json"))).unwrap();
        assert!(
            text(&out.stdout) == committed,
            "{name}.ir.json is stale: run packages/viewer/test/charts/regen.sh"
        );
    }
}

/// `packages/viewer/test/mermaid` holds `export mermaid` of every chart under `examples`,
/// `spec/conformance/charts` and `packages/viewer/test/charts` that declares a machine, as the
/// CLI writes it now; the viewer's tests run each through Mermaid's parser.
#[test]
fn viewer_mermaid_fixtures_are_fresh() {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let p = e.path();
            // Like regen.sh's `find`: links (`node_modules` holds some) are not followed.
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
    let root = repo();
    let mut charts = Vec::new();
    for dir in [
        "examples",
        "spec/conformance/charts",
        "packages/viewer/test/charts",
    ] {
        walk(&root.join(dir), &mut charts);
    }
    let fixtures = root.join("packages/viewer/test/mermaid");
    let mut expected = Vec::new();
    for chart in charts {
        let rel = chart.strip_prefix(&root).unwrap();
        let rel: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        let rel = rel.join("/");
        let out = roadbook(&root, &["export", "mermaid", &rel]);
        assert_eq!(out.status.code(), Some(0), "{rel}: {}", text(&out.stderr));
        if out.stdout.is_empty() {
            continue;
        }
        let name = format!("{}.md", rel.trim_end_matches(".road").replace('/', "-"));
        let committed = std::fs::read_to_string(fixtures.join(&name)).unwrap_or_default();
        assert!(
            text(&out.stdout) == committed,
            "test/mermaid/{name} is stale: run packages/viewer/test/charts/regen.sh"
        );
        expected.push(name);
    }
    expected.sort();
    let mut found: Vec<String> = std::fs::read_dir(&fixtures)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    found.sort();
    assert_eq!(found, expected, "run packages/viewer/test/charts/regen.sh");
}

#[test]
fn gen_writes_a_module_next_to_each_chart() {
    let types = "type Item { sku: string }\nerror Gone\n";
    let main = "import { Item, Gone } from \"../lib/types.road\"\n\
                machine M(item: Item) ! Gone {\n  initial Done\n  final state Done { fail Gone }\n}\n";
    let bad = "machine B() {\n  initial Nowhere\n}\n";
    let dir = project(
        "gen",
        &[
            ("lib/types.road", types),
            ("app/main.road", main),
            ("bad.road", bad),
        ],
    );
    let out = roadbook(&dir, &["gen", "--check", "lib", "app"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(text(&out.stdout), "app/main.road.ts\nlib/types.road.ts\n");
    assert!(!dir.join("app/main.road.ts").exists());

    // A file with errors fails the run; the others are still generated.
    let out = roadbook(&dir, &["gen"]);
    assert_eq!(out.status.code(), Some(1));
    let err = text(&out.stderr);
    assert!(err.contains("error[RB0"), "{err}");
    assert!(err.contains("`bad.road` has errors"), "{err}");
    assert!(!dir.join("bad.road.ts").exists());
    let generated = std::fs::read_to_string(dir.join("app/main.road.ts")).unwrap();
    assert!(
        generated.starts_with("// Generated by roadbook gen from app/main.road. Do not edit.\n")
    );
    assert!(generated.contains("import { Gone, Item } from \"../lib/types.road.js\"\n"));
    assert!(generated.contains("export const M = Roadbook.make<MDef>(ir, \"M\", errors)\n"));
    let types_ts = std::fs::read_to_string(dir.join("lib/types.road.ts")).unwrap();
    assert!(types_ts.contains("export const Item = Schema.Struct({ sku: Schema.String })\n"));

    // Fresh now, and generating again changes nothing.
    std::fs::remove_file(dir.join("bad.road")).unwrap();
    let out = roadbook(&dir, &["gen", "--check"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stdout));
    assert_eq!(text(&out.stdout), "");
    let out = roadbook(&dir, &["gen"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(
        std::fs::read_to_string(dir.join("app/main.road.ts")).unwrap(),
        generated
    );

    // A stale file is listed and left alone.
    std::fs::write(dir.join("lib/types.road.ts"), "// edited\n").unwrap();
    let out = roadbook(&dir, &["gen", "--check", "lib/types.road"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(text(&out.stdout), "lib/types.road.ts\n");
    assert_eq!(
        std::fs::read_to_string(dir.join("lib/types.road.ts")).unwrap(),
        "// edited\n"
    );
}

#[test]
fn gen_skips_gen_exclude_and_reports_orphaned_modules() {
    let dir = project(
        "gen-orphans",
        &[
            ("a.road", OK),
            ("fixtures/f.road", OK),
            ("lib/gone.road", OK),
            ("lib/hand.road.ts", "// written by hand, not generated\n"),
        ],
    );
    std::fs::write(
        dir.join("roadbook.toml"),
        "gen-exclude = [\"fixtures\"] # read as IR\n",
    )
    .unwrap();
    let out = roadbook(&dir, &["gen"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(dir.join("a.road.ts").exists());
    assert!(!dir.join("fixtures/f.road.ts").exists());
    // gen-exclude is for `gen` only, and a file named on the command line is generated.
    let out = roadbook(&dir, &["check"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let out = roadbook(&dir, &["gen", "fixtures/f.road"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(dir.join("fixtures/f.road.ts").exists());
    std::fs::remove_file(dir.join("fixtures/f.road.ts")).unwrap();

    // Deleting a chart leaves its module behind: --check lists it, gen warns.
    std::fs::remove_file(dir.join("lib/gone.road")).unwrap();
    let out = roadbook(&dir, &["gen", "--check"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(text(&out.stdout), "lib/gone.road.ts\n");
    assert_eq!(
        text(&out.stderr),
        "error: `lib/gone.road.ts` was generated from `lib/gone.road`, which no longer exists; \
         delete it\n"
    );
    let out = roadbook(&dir, &["gen", "--check", "lib"]);
    assert_eq!(text(&out.stdout), "lib/gone.road.ts\n");
    let out = roadbook(&dir, &["gen", "--check", "a.road"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stdout));
    let out = roadbook(&dir, &["gen"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(text(&out.stderr).starts_with("warning: `lib/gone.road.ts` was generated"));
    assert!(dir.join("lib/gone.road.ts").exists());
    std::fs::remove_file(dir.join("lib/gone.road.ts")).unwrap();
    let out = roadbook(&dir, &["gen", "--check"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stdout));
}

/// The repository's own project: every chart that is not `gen-exclude`d has a fresh module,
/// and no module is left over.
#[test]
fn the_repository_generated_code_is_fresh() {
    let out = roadbook(&repo(), &["gen", "--check"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "regenerate with `cargo run -p roadbook-cli -- gen`:\n{}{}",
        text(&out.stdout),
        text(&out.stderr)
    );
}

#[test]
fn gen_rejects_an_error_field_its_class_cannot_have() {
    let src = "// An error whose class would be `never`.\nerror Weird {\n  why: string,\n  name: bool,\n}\n";
    let dir = project("gen-error-field", &[("w.road", src), ("ok.road", OK)]);
    let out = roadbook(&dir, &["gen"]);
    assert_eq!(out.status.code(), Some(1));
    let err = text(&out.stderr);
    assert!(
        err.starts_with(
            "error: w.road:4: the error type `Weird` cannot have the field `name: bool` in \
             TypeScript: every error is a JavaScript `Error`, whose `name`, `message` and \
             `stack` are strings;"
        ),
        "{err}"
    );
    assert!(!dir.join("w.road.ts").exists());
    assert!(dir.join("ok.road.ts").exists());
}

/// The modules `roadbook gen` writes import other packages' modules where they are: through
/// `exports` and from an absolute directory for `[packages]`, by the specifier `exports` maps
/// to the module for `node_modules`, and between two packages that import each other. They
/// typecheck with `tsc` (skipped, with a note, if `pnpm install` has not been run).
#[cfg(unix)]
#[test]
fn gen_imports_other_packages_modules_where_they_are() {
    let order = "\
import { Inv } from \"@shop/inv/inv.road\"
import { Item } from \"@shop/inv/types.road\"
import { Stock } from \"stock/stock.road\"
import { Far } from \"far/far.road\"
import { A } from \"a/a.road\"

machine Order(item: Item, i: ref Inv, s: ref Stock, f: ref Far, a: ref A) {
  initial Done
  final state Done {}
}
";
    let peer = |name: &str, other: &str| {
        format!(
            "import {{ {other} }} from \"{}/{}.road\"\n\nmachine {name}(peer: ref {other}) {{\n  \
             event Poke\n  emits Poked\n\n  initial X\n  state X {{\n    on Poke -> X  emit Poked\n    \
             on peer.Poked -> X\n  }}\n}}\n",
            other.to_lowercase(),
            other.to_lowercase()
        )
    };
    let leaf = |name: &str| format!("machine {name}() {{\n  initial X\n  final state X {{}}\n}}\n");
    let (a, b, inv, stock, far) = (
        peer("A", "B"),
        peer("B", "A"),
        format!("import {{ Item }} from \"./types.road\"\n\n{}", leaf("Inv")),
        leaf("Stock"),
        leaf("Far"),
    );
    let dir = project(
        "gen-packages",
        &[
            ("app/order.road", order),
            ("a/a.road", &a),
            ("b/b.road", &b),
            ("pkgs/inv/charts/inv.road", &inv),
            ("pkgs/inv/charts/types.road", "type Item { sku: string }\n"),
            (
                "pkgs/inv/package.json",
                r#"{ "name": "@shop/inv", "type": "module", "exports": { ".": "./index.js", "./*.road": "./charts/*.road", "./*.road.js": "./charts/*.road.js" } }"#,
            ),
            ("libs/stock/src/stock.road", &stock),
            (
                "libs/stock/package.json",
                r#"{ "name": "stock", "type": "module", "exports": { "./*.road": "./src/*.road" } }"#,
            ),
            ("package.json", r#"{ "type": "module" }"#),
        ],
    );
    let outside = dir.with_file_name(format!(
        "{}-outside",
        dir.file_name().unwrap().to_string_lossy()
    ));
    let _ = std::fs::remove_dir_all(&outside);
    std::fs::create_dir_all(outside.join("far")).unwrap();
    std::fs::write(outside.join("roadbook.toml"), "").unwrap();
    std::fs::write(outside.join("package.json"), r#"{ "type": "module" }"#).unwrap();
    std::fs::write(outside.join("far/far.road"), far).unwrap();
    std::fs::create_dir_all(dir.join("node_modules/@shop")).unwrap();
    std::os::unix::fs::symlink("../../pkgs/inv", dir.join("node_modules/@shop/inv")).unwrap();
    std::fs::write(
        dir.join("roadbook.toml"),
        format!(
            "[packages]\nstock = \"libs/stock\"\nfar = \"{}\"\na = \"a\"\nb = \"b\"\n",
            outside.join("far").display()
        ),
    )
    .unwrap();

    let out = roadbook(&outside, &["gen"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let out = roadbook(&dir, &["gen"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let module = |path: &str| std::fs::read_to_string(dir.join(path)).unwrap();
    let imports = |code: String| -> Vec<String> {
        code.lines()
            .filter(|l| l.starts_with("import ") && l.contains(".road.js"))
            .map(str::to_owned)
            .collect()
    };
    let outside_name = outside.file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(
        imports(module("app/order.road.ts")),
        [
            "import type { InvDef } from \"@shop/inv/inv.road.js\"".to_owned(),
            "import { Item } from \"@shop/inv/types.road.js\"".to_owned(),
            "import type { StockDef } from \"../libs/stock/src/stock.road.js\"".to_owned(),
            format!("import type {{ FarDef }} from \"../../{outside_name}/far/far.road.js\""),
            "import type { ADef } from \"../a/a.road.js\"".to_owned(),
        ]
    );
    assert_eq!(
        imports(module("a/a.road.ts")),
        ["import type { BDef } from \"../b/b.road.js\""]
    );
    assert_eq!(
        imports(module("b/b.road.ts")),
        ["import type { ADef } from \"../a/a.road.js\""]
    );

    typecheck(&dir, r#""app/*.road.ts", "a/*.road.ts", "b/*.road.ts""#);

    // A package in `node_modules` that does not export the generated modules is an error:
    // the import could not be resolved.
    std::fs::write(
        dir.join("pkgs/inv/package.json"),
        r#"{ "name": "@shop/inv", "exports": { "./*.road": "./charts/*.road" } }"#,
    )
    .unwrap();
    let out = roadbook(&dir, &["gen", "--check", "app"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains(
            "cannot import the generated module of `@shop/inv/inv.road`: its `package.json` \
             `exports` maps no subpath to `./charts/inv.road.js`"
        ),
        "{}",
        text(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&outside);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Resolves what the generated modules import when Node runs them in a test project: `effect`
/// and `@dire-kiwi/roadbook-effect` as `packages/effect` does, and `./x.road.js` to `./x.road.ts`.
const NODE_HOOKS: &str = r#"
import { registerHooks } from "node:module"
import { existsSync } from "node:fs"
import { fileURLToPath, pathToFileURL } from "node:url"
const effect = pathToFileURL(process.env.ROADBOOK_EFFECT_PACKAGE).href
registerHooks({
  resolve(specifier, context, next) {
    if (specifier === "effect" || specifier === "@dire-kiwi/roadbook-effect") {
      return next(specifier, { ...context, parentURL: effect })
    }
    if (specifier.endsWith(".road.js") && context.parentURL !== undefined) {
      const ts = new URL(specifier.slice(0, -3) + ".ts", context.parentURL)
      if (existsSync(fileURLToPath(ts))) return next(ts.href, context)
    }
    return next(specifier, context)
  }
})
"#;

/// Typechecks the generated modules `include` matches in `dir` with `tsc` under the
/// repository's settings (`verbatimModuleSyntax` among them). False, with a note, if `pnpm
/// install` has not been run.
fn typecheck(dir: &Path, include: &str) -> bool {
    let tsc = repo().join("packages/effect/node_modules/.bin/tsc");
    if !tsc.exists() {
        eprintln!(
            "note: {} is missing (run `pnpm install`); not typechecked",
            tsc.display()
        );
        return false;
    }
    std::fs::write(
        dir.join("tsconfig.json"),
        format!(
            r#"{{ "extends": "{}", "compilerOptions": {{ "types": [] }}, "include": [{include}] }}"#,
            repo().join("packages/effect/tsconfig.json").display()
        ),
    )
    .unwrap();
    let out = Command::new(&tsc)
        .args(["-p", "tsconfig.json", "--noEmit"])
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "tsc:\n{}{}",
        text(&out.stdout),
        text(&out.stderr)
    );
    true
}

/// Modules of charts that import each other load and run under Node whichever is loaded
/// first: what one takes from a module in its import cycle it reads lazily, since that module
/// may not have run yet. A name that exists only as a type (an alias of a `ref`) is imported
/// with `type`, as `verbatimModuleSyntax` requires and as Node needs, the module having no such
/// export. (Skipped, with a note, without `pnpm install` or Node.)
#[test]
fn gen_modules_that_import_each_other_load_and_run() {
    let a = "\
import { Bad } from \"./b.road\"

type T { n: int }

task work(t: T) -> T ! Bad

machine A(t: T) -> T ! Bad {
  var out: T
  initial Working
  state Working {
    invoke work(t) {
      done     -> Done    set out = result
      fail Bad -> Failed
    }
  }
  final state Done   { output out }
  final state Failed { fail Bad { t: t } }
}
";
    let b = "\
import { T } from \"./a.road\"

error Bad { t: T }

machine B() {
  event Go { t: T }

  initial Idle
  state Idle {
    on Go -> Gone
  }
  final state Gone {}
}
";
    let w =
        "machine Worker() {\n  initial X\n  final state X {}\n}\n\ntype WorkerRef = ref Worker\n";
    let boss = "\
import { WorkerRef } from \"./w.road\"
import { Bad } from \"./b.road\"

machine Boss(worker: WorkerRef) ! Bad {
  initial X
  final state X {}
}
";
    let main = r#"
import { Cause, Effect, Exit, Schema } from "effect"
// The modules in the order given: the first one loaded runs after the modules it imports.
const modules = {}
for (const name of process.argv.slice(2)) modules[name] = await import(`./${name}.road.js`)
const { A, T } = modules.a
const { Bad, Go } = modules.b
const check = (ok, what) => {
  if (!ok) throw new Error(`failed: ${what}`)
}
const bad = Schema.decodeUnknownSync(Bad)({ _tag: "Bad", t: { n: 1 } })
check(bad instanceof Bad && bad.t.n === 1, "a Bad decodes, with its field of a.road's type")
check(Exit.isFailure(Schema.decodeUnknownExit(Bad)({ _tag: "Bad", t: { n: "one" } })), "a wrong T is refused")
check(Schema.decodeUnknownSync(Go)({ _tag: "Go", t: { n: 2 } }).t.n === 2, "an event decodes")
check(Schema.decodeUnknownSync(T)({ n: 3 }).n === 3, "a T decodes")
const run = (work) => Effect.runPromiseExit(A.implement({ tasks: { work } }).run({ t: { n: 4 } }))
const done = await run(({ t }) => Effect.succeed({ n: t.n + 1 }))
check(Exit.isSuccess(done) && done.value.n === 5, "A succeeds")
const failed = await run(({ t }) => Effect.fail(new Bad({ t })))
const error = Exit.isFailure(failed) ? failed.cause.reasons.find(Cause.isFailReason)?.error : undefined
check(error instanceof Bad && error.t.n === 4, "A fails with an instance of Bad")
console.log("ok")
"#;
    let dir = project(
        "gen-cycle",
        &[
            ("a.road", a),
            ("b.road", b),
            ("w.road", w),
            ("boss.road", boss),
            ("package.json", r#"{ "type": "module" }"#),
            ("hooks.mjs", NODE_HOOKS),
            ("main.mjs", main),
        ],
    );
    let out = roadbook(&dir, &["gen"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let module = |path: &str| std::fs::read_to_string(dir.join(path)).unwrap();
    let (a, b, boss) = (
        module("a.road.ts"),
        module("b.road.ts"),
        module("boss.road.ts"),
    );
    assert!(a.contains("import { Bad } from \"./b.road.js\"\n"), "{a}");
    assert!(
        a.contains("const errors = { get Bad() { return Bad } }\n"),
        "{a}"
    );
    assert!(
        b.contains("export class Bad extends Schema.TaggedError<Bad>()(\"Bad\", { t: Schema.suspend(() => T) }) {}\n"),
        "{b}"
    );
    assert!(
        boss.contains("import { Bad } from \"./b.road.js\"\n"),
        "{boss}"
    );
    assert!(
        boss.contains("import type { WorkerRef } from \"./w.road.js\"\n"),
        "{boss}"
    );
    assert!(boss.contains("const errors = { Bad }\n"), "{boss}");

    if !typecheck(&dir, r#""*.road.ts""#) {
        return;
    }
    for order in [["a", "b", "boss"], ["b", "a", "boss"], ["boss", "b", "a"]] {
        let out = match Command::new("node")
            .args(["--import", "./hooks.mjs", "main.mjs"])
            .args(order)
            .env(
                "ROADBOOK_EFFECT_PACKAGE",
                repo().join("packages/effect/package.json"),
            )
            .current_dir(&dir)
            .output()
        {
            Ok(out) => out,
            Err(e) => {
                eprintln!("note: cannot run node ({e}); the modules were not run");
                return;
            }
        };
        assert!(
            out.status.success() && text(&out.stdout) == "ok\n",
            "node, loading {order:?}:\n{}{}",
            text(&out.stdout),
            text(&out.stderr)
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", text(&out.stderr));
}

#[test]
fn viz_diff_draws_two_commits_as_one_chart() {
    // A repository whose project is a subdirectory, with the shop and two small charts.
    let shop = |f: &str| std::fs::read_to_string(repo().join("examples/shop").join(f)).unwrap();
    let old = "machine Old() {\n  initial A\n  final state A {}\n}\n";
    let dir = project(
        "diff/proj",
        &[
            ("shop/checkout.road", &shop("checkout.road")),
            ("shop/place_order.road", &shop("place_order.road")),
            ("shop/types.road", &shop("types.road")),
            ("old.road", old),
        ],
    );
    let top = dir.parent().unwrap().to_path_buf();
    git(&top, &["init", "-q", "-b", "main"]);
    git(&top, &["add", "."]);
    git(&top, &["commit", "-q", "-m", "one"]);
    // The second commit changes a guard and a target, adds a state, renames another (removed
    // and added), removes a chart and adds one.
    let checkout = shop("checkout.road")
        .replace("if attempts < 3       -> Backoff", "if attempts < 5       -> Backoff")
        .replace(
            "done                    -> Charged   set receipt = result",
            "done                    -> Notifying set receipt = result",
        )
        .replace(
            "  final state Charged  { output receipt }",
            "  state Notifying { after 1s -> Charged }\n\n  final state Charged  { output receipt }",
        )
        .replace("-> GaveUp", "-> GivenUp")
        .replace("final state GaveUp", "final state GivenUp");
    std::fs::write(dir.join("shop/checkout.road"), &checkout).unwrap();
    std::fs::remove_file(dir.join("old.road")).unwrap();
    std::fs::write(
        dir.join("new.road"),
        "machine New() {\n  initial A\n  final state A {}\n}\n",
    )
    .unwrap();
    git(&top, &["add", "-A"]);
    git(&top, &["commit", "-q", "-m", "two"]);

    let out = out_dir("diff");
    let o = out.to_str().unwrap();
    let res = roadbook(&dir, &["viz", "--diff", "HEAD~1..HEAD", "-o", o]);
    assert_eq!(res.status.code(), Some(0), "{}", text(&res.stderr));
    let mut files: Vec<String> = std::fs::read_dir(&out)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    files.sort();
    assert_eq!(
        files,
        [
            "index.html",
            "new.html",
            "old.html",
            "roadbook-viewer.js",
            "shop-place_order.html",
            "summary.json",
            "summary.md"
        ]
    );
    let summary: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("summary.json")).unwrap()).unwrap();
    assert_eq!(summary["from"], "HEAD~1");
    assert_eq!(summary["to"], "HEAD");
    assert_eq!(summary["changed"], true);
    assert_eq!(
        summary["machines"],
        serde_json::json!({ "added": 1, "removed": 1, "changed": 1, "moved": 0 })
    );
    // Notifying and GivenUp added, GaveUp removed (and Old's and New's one state each).
    assert_eq!(
        summary["states"],
        serde_json::json!({ "added": 3, "removed": 2, "changed": 0 })
    );
    let changes: Vec<(&str, &str)> = summary["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["machine"].as_str().unwrap(),
                c["status"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        changes,
        [
            ("New", "added"),
            ("Old", "removed"),
            ("Checkout", "changed")
        ]
    );
    let md = std::fs::read_to_string(out.join("summary.md")).unwrap();
    assert!(
        md.contains("| ~ changed | `Checkout` | `shop/place_order.road` | +2 −1 | +1 ~3 |"),
        "{md}"
    );
    assert_eq!(text(&res.stdout), md);

    // The page draws the merged chart, with what changed.
    let data = page_data(&std::fs::read_to_string(out.join("shop-place_order.html")).unwrap());
    assert_eq!(data["mode"], "diff");
    let d = &data["diff"];
    assert_eq!(d["status"], "changed");
    let checkout = &d["machines"]["Checkout"];
    assert_eq!(checkout["status"], "changed");
    assert_eq!(checkout["states"]["Checkout.GaveUp"]["status"], "removed");
    assert_eq!(checkout["states"]["Checkout.Notifying"]["status"], "added");
    let guard = &checkout["transitions"]["Checkout.Charging#2"]["changes"][0];
    assert_eq!(
        guard,
        &serde_json::json!({ "what": "guard", "before": "[attempts < 3]", "after": "[attempts < 5]" })
    );
    assert_eq!(d["machines"]["PlaceOrder"]["status"], "unchanged");
    let ir = roadbook_ir::Ir::from_json(&data["ir"].to_string()).unwrap();
    let names: Vec<&str> = ir.machines["Checkout"]
        .root
        .states
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(
        names,
        [
            "Charging",
            "Backoff",
            "Notifying",
            "Charged",
            "Declined",
            "GaveUp",
            "GivenUp"
        ]
    );
    assert!(
        ir.sources
            .iter()
            .any(|s| s.path == "shop/checkout.road @ HEAD~1")
    );
    let old_page = page_data(&std::fs::read_to_string(out.join("old.html")).unwrap());
    assert_eq!(old_page["diff"]["status"], "removed");

    // The index puts what changed first.
    let index = page_data(&std::fs::read_to_string(out.join("index.html")).unwrap());
    assert_eq!(index["index"]["diff"]["summary"]["machines"]["changed"], 1);
    assert_eq!(index["index"]["diff"]["charts"][0]["status"], "added");

    // One revision against the working tree, where a chart no longer compiles: its page shows
    // the diagnostics.
    std::fs::write(
        dir.join("new.road"),
        "machine New() {\n  initial Missing\n  final state A {}\n}\n",
    )
    .unwrap();
    let out = out_dir("diff-worktree");
    let o = out.to_str().unwrap();
    let res = roadbook(&dir, &["viz", "--diff", "HEAD", "-o", o, "new.road"]);
    assert_eq!(res.status.code(), Some(0), "{}", text(&res.stderr));
    assert!(text(&res.stderr).contains("`new.road` does not compile at working tree"));
    let data = page_data(&std::fs::read_to_string(out.join("new.html")).unwrap());
    assert_eq!(data["diff"]["status"], "error");
    assert_eq!(data["diff"]["errors"][0]["side"], "after");
    assert!(
        data["diff"]["errors"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Missing")
    );

    let res = roadbook(&dir, &["viz", "--diff", "nope..HEAD", "-o", o]);
    assert_eq!(res.status.code(), Some(2));
    assert!(text(&res.stderr).contains("`nope` is not a revision"));
}

#[test]
fn viz_coverage_embeds_the_traces_of_each_page() {
    let out = out_dir("coverage");
    let o = out.to_str().unwrap();
    let res = roadbook(
        &repo(),
        &[
            "viz",
            "examples/shop",
            "--coverage",
            "spec/conformance/cases",
            "-o",
            o,
        ],
    );
    assert_eq!(res.status.code(), Some(0), "{}", text(&res.stderr));
    assert!(text(&res.stderr).contains("13 of"), "{}", text(&res.stderr));
    let data = page_data(&std::fs::read_to_string(out.join("place_order.html")).unwrap());
    assert_eq!(data["mode"], "coverage");
    let traces = data["coverage"].as_array().unwrap();
    // Only the traces with a run of PlaceOrder or Checkout.
    assert!(traces.len() >= 8 && traces.len() < 20, "{}", traces.len());
    assert!(traces.iter().all(|t| {
        let text = t["text"].as_str().unwrap();
        text.contains("\"machine\":\"Checkout\"") || text.contains("\"machine\":\"PlaceOrder\"")
    }));
    let res = roadbook(
        &repo(),
        &[
            "viz",
            "examples/shop",
            "--coverage",
            "spec/conformance/cases/gate-guards",
            "-o",
            o,
        ],
    );
    assert_eq!(res.status.code(), Some(2));
    assert!(text(&res.stderr).contains("none of the traces"));

    // `--coverage` takes one value: the paths after it are charts, not traces. A truncated
    // last line, usual in a production trace, is skipped with a warning.
    let traces = out_dir("coverage-traces");
    std::fs::create_dir_all(&traces).unwrap();
    let mut cut =
        std::fs::read_to_string(repo().join("spec/conformance/cases/checkout-charged/trace.jsonl"))
            .unwrap();
    cut.push_str("{\"kind\":\"entered\",\"run\":\"r1\",\"sta");
    let cut_path = traces.join("cut.jsonl");
    std::fs::write(&cut_path, &cut).unwrap();
    let retry = repo().join("spec/conformance/cases/checkout-retry/trace.jsonl");
    let out = out_dir("coverage-one-value");
    let o = out.to_str().unwrap();
    let res = roadbook(
        &repo(),
        &[
            "viz",
            "--coverage",
            cut_path.to_str().unwrap(),
            "--coverage",
            retry.to_str().unwrap(),
            "examples/shop/checkout.road",
            "-o",
            o,
        ],
    );
    assert_eq!(res.status.code(), Some(0), "{}", text(&res.stderr));
    let err = text(&res.stderr);
    assert!(
        err.contains("cut.jsonl`: 1 line is not a trace record (line "),
        "{err}"
    );
    assert!(err.contains("2 trace files"), "{err}");
    let mut files: Vec<String> = std::fs::read_dir(&out)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    files.sort();
    assert_eq!(files, ["checkout.html", "index.html", "roadbook-viewer.js"]);
    let data = page_data(&std::fs::read_to_string(out.join("checkout.html")).unwrap());
    assert_eq!(data["coverage"].as_array().unwrap().len(), 2);
    let _ = std::fs::remove_dir_all(&traces);
}

/// A repository holding a project, with its files committed as `main`'s first commit.
fn diff_repo(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = project(name, files);
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "one"]);
    dir
}

/// Runs `roadbook viz --diff HEAD` (against the working tree) and reads the summaries.
fn diff_summary(dir: &Path, name: &str) -> (PathBuf, serde_json::Value, String) {
    let out = out_dir(name);
    let res = roadbook(dir, &["viz", "--diff", "HEAD", "-o", out.to_str().unwrap()]);
    assert_eq!(res.status.code(), Some(0), "{}", text(&res.stderr));
    let json =
        serde_json::from_str(&std::fs::read_to_string(out.join("summary.json")).unwrap()).unwrap();
    let md = std::fs::read_to_string(out.join("summary.md")).unwrap();
    (out, json, md)
}

const TICKET: &str = "type Ticket { id: string }\n";
const DESK: &str = "import { Ticket } from \"./ticket.road\"\n\nmachine Desk(t: Ticket) {\n  event Next\n  initial Open\n  state Open { on Next -> Closed }\n  final state Closed {}\n}\n";
const LAMP: &str = "machine Lamp() {\n  event Flip\n  initial Off\n  state Off { on Flip -> On }\n  final state On {}\n}\n";

const EDITOR: &str = "machine Editor() {\n  event Help\n  event Back\n  initial Work\n  state Work {\n    initial Draft\n    history Last\n    history Old\n    state Draft {}\n    on Help -> Help\n  }\n  state Help { on Back -> Work.Last }\n}\n";

#[test]
fn viz_diff_summarises_history_states() {
    // `Last` becomes deep, `Old` goes and `New` comes: history states counted with the states,
    // and on their own.
    let dir = diff_repo("diff-history", &[("editor.road", EDITOR)]);
    let after = EDITOR
        .replace("    history Last\n", "    deep history Last\n")
        .replace("    history Old\n", "    history New\n");
    std::fs::write(dir.join("editor.road"), after).unwrap();
    let (out, json, md) = diff_summary(&dir, "diff-history");
    let counts = serde_json::json!({ "added": 1, "removed": 1, "changed": 1 });
    assert_eq!(json["states"], counts);
    assert_eq!(json["historyStates"], counts);
    assert_eq!(json["changes"][0]["historyStates"], counts);
    assert!(
        md.contains(
            "States: 1 added, 1 removed, 1 changed (history states: 1 added, 1 removed, 1 changed)."
        ),
        "{md}"
    );
    assert!(
        md.contains(
            "| ~ changed | `Editor` | `editor.road` | +1 −1 ~1 (history +1 −1 ~1) | none |"
        ),
        "{md}"
    );
    let data = page_data(&std::fs::read_to_string(out.join("editor.html")).unwrap());
    let editor = &data["diff"]["machines"]["Editor"];
    assert_eq!(editor["counts"]["history"], counts);
    assert_eq!(
        editor["states"]["Editor.Work.Last"]["changes"][0],
        serde_json::json!({ "what": "kind", "before": "history", "after": "deep history" })
    );
    assert_eq!(editor["states"]["Editor.Work.Old"]["status"], "removed");
    assert_eq!(editor["states"]["Editor.Work.New"]["status"], "added");

    // A chart with no history state says nothing of them in Markdown.
    let dir = diff_repo("diff-no-history", &[("lamp.road", LAMP)]);
    std::fs::write(
        dir.join("lamp.road"),
        LAMP.replace("on Flip -> On", "on Flip -> On\n  }\n  state Dim {"),
    )
    .unwrap();
    let (_, json, md) = diff_summary(&dir, "diff-no-history");
    assert_eq!(
        json["historyStates"],
        serde_json::json!({ "added": 0, "removed": 0, "changed": 0 })
    );
    assert!(!md.contains("history"), "{md}");
}

#[test]
fn viz_diff_matches_machines_across_files() {
    // A chart renamed: its machine moved, not removed and added; no page for the old name.
    let dir = diff_repo(
        "diff-moved",
        &[
            ("desk.road", DESK),
            ("ticket.road", TICKET),
            ("lamp.road", LAMP),
        ],
    );
    std::fs::rename(dir.join("lamp.road"), dir.join("light.road")).unwrap();
    let (out, json, md) = diff_summary(&dir, "diff-moved");
    assert!(!out.join("lamp.html").exists());
    assert_eq!(
        json["machines"],
        serde_json::json!({ "added": 0, "removed": 0, "changed": 0, "moved": 1 })
    );
    assert_eq!(json["changes"][0]["machine"], "Lamp");
    assert_eq!(json["changes"][0]["status"], "moved");
    assert_eq!(
        json["changes"][0]["moved"],
        serde_json::json!({ "from": "lamp.road", "to": "light.road" })
    );
    assert!(
        md.contains("| → moved | `Lamp` | `light.road` (moved from `lamp.road` to `light.road`) | none | none |"),
        "{md}"
    );
    let page = page_data(&std::fs::read_to_string(out.join("light.html")).unwrap());
    let lamp = &page["diff"]["machines"]["Lamp"];
    assert_eq!(lamp["changes"][0]["what"], "file");
    assert_eq!(page["diff"]["status"], "changed");

    // A chart that starts importing another, which is then no entry of its own: nothing
    // changed in any machine.
    std::fs::rename(dir.join("light.road"), dir.join("lamp.road")).unwrap();
    std::fs::write(
        dir.join("desk.road"),
        format!("import {{ Lamp }} from \"./lamp.road\"\n{DESK}"),
    )
    .unwrap();
    let (out, json, md) = diff_summary(&dir, "diff-imported");
    assert!(!out.join("lamp.html").exists());
    assert_eq!(json["changed"], false, "{json:#}");
    assert_eq!(md.lines().nth(2), Some("No machine changed."), "{md}");
}

#[test]
fn viz_diff_reads_files_outside_the_project_from_each_revision() {
    // A repository whose project is a subdirectory. Imports cannot climb above the project
    // root, so it reaches files outside it through `[packages]`: two packages elsewhere in the
    // repository and one outside the repository.
    let desk = "import { Ticket, Lamp } from \"shared/lib.road\"\n\
                import { Stock } from \"stock/stock.road\"\n\
                import { Note } from \"ext/note.road\"\n\n\
                machine Desk(t: Ticket, n: Note) {\n  event Next\n  initial Open\n  \
                state Open { on Next -> Closed }\n  final state Closed {}\n}\n";
    let stock = "machine Stock() {\n  event Count\n  initial Idle\n  \
                 state Idle { on Count -> Done }\n  final state Done {}\n}\n";
    let base = project(
        "diff-outside",
        &[
            (
                "repo/proj/roadbook.toml",
                "[packages]\nstock = \"../libs/stock\"\nshared = \"../shared\"\next = \"../../ext\"\n",
            ),
            ("repo/proj/desk.road", desk),
            ("repo/shared/lib.road", &format!("{TICKET}\n{LAMP}")),
            ("repo/libs/stock/stock.road", stock),
            ("ext/note.road", "type Note { text: string }\n"),
        ],
    );
    let (top, dir) = (base.join("repo"), base.join("repo/proj"));
    git(&top, &["init", "-q", "-b", "main"]);
    git(&top, &["add", "."]);
    git(&top, &["commit", "-q", "-m", "one"]);
    // A commit that changes only the shared file and the package.
    let lamp = LAMP.replace(
        "state Off { on Flip -> On }",
        "state Off { on Flip -> On }\n  state Dim { on Flip -> Off }",
    );
    std::fs::write(top.join("shared/lib.road"), format!("{TICKET}\n{lamp}")).unwrap();
    std::fs::write(
        top.join("libs/stock/stock.road"),
        stock.replace("on Count -> Done", "on Count -> Done\n    after 1s -> Done"),
    )
    .unwrap();
    git(&top, &["commit", "-q", "-am", "two"]);

    let out = out_dir("diff-outside");
    let res = roadbook(
        &dir,
        &["viz", "--diff", "HEAD~1..HEAD", "-o", out.to_str().unwrap()],
    );
    assert_eq!(res.status.code(), Some(0), "{}", text(&res.stderr));
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("summary.json")).unwrap()).unwrap();
    let changes: Vec<(&str, &str)> = json["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["machine"].as_str().unwrap(),
                c["status"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        changes,
        [("Lamp", "changed"), ("Stock", "changed")],
        "{json:#}"
    );
    assert_eq!(json["states"]["added"], 1);
    assert_eq!(json["transitions"]["added"], 2);
    // The file outside the repository is read from disk for both sides, and the summary says
    // so.
    assert_eq!(json["outside"], serde_json::json!(["../../ext/note.road"]));
    let md = std::fs::read_to_string(out.join("summary.md")).unwrap();
    assert!(
        md.ends_with(
            "\nRead from disk for both sides, being outside the repository: \
             `../../ext/note.road`.\n"
        ),
        "{md}"
    );
    assert!(text(&res.stderr).contains("`../../ext/note.road`"));

    // A revision against the working tree reads the shared file from disk.
    std::fs::write(top.join("shared/lib.road"), format!("{TICKET}\n{LAMP}")).unwrap();
    let (_, json, _) = diff_summary(&dir, "diff-outside-worktree");
    assert_eq!(json["changes"][0]["machine"], "Lamp");
    assert_eq!(json["changes"][0]["status"], "changed");
    assert_eq!(json["states"]["removed"], 1);
    assert_eq!(json["outside"], serde_json::json!(["../../ext/note.road"]));

    // A file of the repository that a revision does not have is missing there, not read
    // from disk.
    std::fs::write(
        dir.join("desk.road"),
        desk.replace("shared/lib.road", "shared/new.road"),
    )
    .unwrap();
    std::fs::copy(top.join("shared/lib.road"), top.join("shared/new.road")).unwrap();
    let (_, json, _) = diff_summary(&dir, "diff-outside-untracked");
    assert_eq!(json["errors"], serde_json::json!([]), "{json:#}");
    let out = out_dir("diff-outside-missing");
    git(&top, &["add", "-A"]);
    git(&top, &["commit", "-q", "-m", "three"]);
    git(&top, &["rm", "-q", "shared/new.road"]);
    git(&top, &["commit", "-q", "-m", "four"]);
    std::fs::copy(top.join("shared/lib.road"), top.join("shared/new.road")).unwrap();
    let res = roadbook(
        &dir,
        &["viz", "--diff", "HEAD~1..HEAD", "-o", out.to_str().unwrap()],
    );
    assert_eq!(res.status.code(), Some(0), "{}", text(&res.stderr));
    let data = page_data(&std::fs::read_to_string(out.join("desk.html")).unwrap());
    assert_eq!(data["diff"]["errors"][0]["side"], "after");
    assert!(
        data["diff"]["errors"][0]["text"]
            .as_str()
            .unwrap()
            .contains("no such file at HEAD"),
        "{data:#}"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn viz_diff_reports_declarations_and_the_machines_that_use_them() {
    let dir = diff_repo(
        "diff-decls",
        &[
            ("desk.road", DESK),
            ("ticket.road", TICKET),
            ("lamp.road", LAMP),
        ],
    );
    std::fs::write(
        dir.join("ticket.road"),
        "type Ticket { id: string, title: string }\n",
    )
    .unwrap();
    let (_, json, md) = diff_summary(&dir, "diff-decls");
    assert_eq!(json["changed"], true);
    assert_eq!(json["machines"]["changed"], 0);
    assert_eq!(
        json["decls"],
        serde_json::json!([{ "name": "Ticket", "status": "changed", "machines": ["Desk"] }])
    );
    assert_eq!(
        json["declarations"],
        serde_json::json!({ "added": 0, "removed": 0, "changed": 1 })
    );
    assert!(!md.contains("No machine changed"), "{md}");
    assert!(md.contains("Machines: none changed."), "{md}");
    assert!(md.contains("| ~ changed | `Ticket` | `Desk` |"), "{md}");
}

#[test]
fn viz_diff_summary_escapes_what_comes_from_the_charts() {
    // A file name that would end the code span and the table row, add HTML, a mention, a link
    // and a heading.
    let evil = "evil`|<img src=x> @octocat [x](https:e.vil)\n## injected.road";
    let dir = diff_repo("diff-evil", &[("lamp.road", LAMP)]);
    std::fs::write(dir.join(evil), LAMP.replace("Lamp", "Evil")).unwrap();
    let (_, json, md) = diff_summary(&dir, "diff-evil");
    assert_eq!(json["changes"][0]["chart"], evil);
    // Every line is the heading, a sentence, or a table row of five cells.
    let lines: Vec<&str> = md.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 5, "{md}");
    assert!(lines[0].starts_with("### Chart changes: "));
    assert!(lines[1].starts_with("Machines: 1 added"));
    let row = lines[4];
    assert_eq!(row.matches('|').count(), 6, "{row}");
    assert!(row.starts_with("| + added | `Evil` | <code>evil"), "{row}");
    // Nothing of the name is markup: no backtick, pipe, angle bracket, at sign, bracket or line
    // break of its own, only character references inside `<code>`.
    let cell = &row[row.find("<code>").unwrap() + 6..row.find("</code>").unwrap()];
    let mut bare = String::new();
    for (i, part) in cell.split("&#").enumerate() {
        // Each part after the first begins with a reference's number and `;`.
        let rest = if i == 0 {
            part
        } else {
            part.split_once(';').unwrap().1
        };
        bare.push_str(rest);
    }
    assert_eq!(
        bare, "evilimg srcx octocat xhttpsevil injectedroad",
        "{cell}"
    );
    assert!(cell.contains("&#64;octocat"), "{cell}");
    assert!(
        cell.contains("&#9166;"),
        "the line break shows as ⏎: {cell}"
    );
    assert!(!md.contains("\n## injected"), "{md}");
}
