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
        "index.html",
    ] {
        assert!(out.join(page).is_file(), "{page}");
    }
    let index = page_data(&std::fs::read_to_string(out.join("index.html")).unwrap());
    assert_eq!(index["index"]["machines"].as_array().unwrap().len(), 7);

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

/// The viewer bundle the CLI embeds was built from `packages/viewer` as it is now, and not
/// edited since: its header names the hash of the sources, computed as
/// `packages/viewer/build.mjs` does, the versions of elkjs and esbuild in `pnpm-lock.yaml`,
/// and the hash of the script after it.
#[test]
fn viewer_asset_is_fresh() {
    use sha2::{Digest, Sha256};
    let hex = |d: &[u8]| -> String { d.iter().map(|b| format!("{b:02x}")).collect() };
    let pkg = repo().join("packages/viewer");
    let mut files = Vec::new();
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(&p, out);
            } else {
                out.push(p);
            }
        }
    }
    walk(&pkg.join("src"), &mut files);
    files.push(pkg.join("build.mjs"));
    let mut names: Vec<(String, PathBuf)> = files
        .into_iter()
        .map(|p| {
            let rel = p.strip_prefix(&pkg).unwrap();
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
    let stale = "crates/roadbook-cli/assets/roadbook-viewer.js is stale: run `pnpm --filter @roadbook/viewer build` and commit it";
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
        ("packages/viewer/test/charts/stress.road", "charts-stress"),
        ("packages/viewer/test/charts/edges.road", "charts-edges"),
        ("packages/viewer/test/charts/escape.road", "charts-escape"),
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
            let p = e.unwrap().path();
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

/// The generated TypeScript committed next to the examples is what `roadbook gen` writes now.
#[test]
fn the_examples_generated_code_is_fresh() {
    let out = roadbook(&repo(), &["gen", "--check", "examples"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stale, regenerate with `cargo run -p roadbook-cli -- gen examples`:\n{}{}",
        text(&out.stdout),
        text(&out.stderr)
    );
}
