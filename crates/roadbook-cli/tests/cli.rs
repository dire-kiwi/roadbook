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
fn the_shop_checks_and_formats_clean() {
    let out = roadbook(&repo(), &["check", "examples/shop"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(text(&out.stderr), "");
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
    assert!(written.contains("\"irVersion\": \"0.1\""));
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
