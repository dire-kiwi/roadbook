//! Compile errors from `roadbook::include!`, checked with trybuild against `tests/ui/*.stderr`.
//!
//! trybuild builds the cases as a package of its own under
//! `<target dir>/tests/trybuild/roadbook/`, so `CARGO_MANIFEST_DIR` is that directory while
//! they build, wherever the target directory is. Before running them this copies the charts
//! of `tests/ui/charts/` into it, with a `roadbook.toml` that makes it the project root, so the
//! cases name their charts as `charts/…` and diagnostics show `charts/…` too, independently of
//! where the target directory is.

use std::path::{Path, PathBuf};
use std::process::Command;

#[test]
fn ui() {
    let dir = trybuild_project_dir();
    let charts = dir.join("charts");
    std::fs::create_dir_all(&charts).unwrap();
    std::fs::write(
        dir.join("roadbook.toml"),
        "# Written by tests/include_ui.rs: the project root of the trybuild cases' charts.\n",
    )
    .unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/ui/charts");
    for entry in std::fs::read_dir(&source).unwrap() {
        let path = entry.unwrap().path();
        std::fs::copy(&path, charts.join(path.file_name().unwrap())).unwrap();
    }

    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/*.rs");
}

/// Where trybuild puts its package: `<target dir>/tests/trybuild/<crate name>/`, the target
/// dir as `cargo metadata` reports it (which is how trybuild finds it).
fn trybuild_project_dir() -> PathBuf {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let out = Command::new(cargo)
        .args(["metadata", "--no-deps", "--format-version=1"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("cargo metadata runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let metadata: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let target = metadata["target_directory"]
        .as_str()
        .expect("cargo metadata gives the target directory");
    Path::new(target)
        .join("tests/trybuild")
        .join(env!("CARGO_PKG_NAME"))
}
