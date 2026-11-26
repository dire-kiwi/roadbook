//! The Roadbook compiler as WebAssembly (`wasm32-unknown-unknown`, through `wasm-bindgen`),
//! for the npm package `@roadbook/cli` (`packages/cli`), which commits the built module.
//!
//! The module has no file system. It reads files through `FsLoader` (the same loader the
//! command line uses, so packages, `node_modules` and `exports` are found the same way) over
//! a file system the host supplies:
//!
//! - `config.files`: an object of absolute path to text, read first. With no host object,
//!   these are the only files: a project held entirely in memory.
//! - `host`, optional: an object with `readFile(path): string` (throwing the reason, as a
//!   string, if it cannot be read), `isDir(path): boolean` and
//!   `realpath(path): string | undefined`. Node's `fs` makes one in a few lines
//!   (`packages/cli/src/host.js`).
//!
//! Paths are absolute and `/`-separated. Every function takes and returns JSON text:
//!
//! - `config` (all members optional): `{ "root": "/abs/project", "packages": [["inventory",
//!   "examples/inventory"]], "nodeModules": true, "allowUnassigned": ["a.road"],
//!   "warnInvokeTimeout": false, "files": { "/abs/project/a.road": "…" } }`.
//! - `check(host, config, roots)` (`roots`: a JSON array of project paths) and
//!   `compile(host, config, entry)` return `{ "report", "rendered", "errors", "warnings" }`:
//!   `report` is the text `roadbook check --json` prints (documented in
//!   `docs/design/language.md`, "Diagnostics as JSON"), `rendered` the human-readable
//!   diagnostics. `compile` adds `"ir"`: the IR as committed-file JSON text, `null` if there
//!   are errors.
//! - `gen(host, config, entry)` adds `"code"` (the `.road.ts` module, or `null`) and
//!   `"error"` (why no module was generated, other than diagnostics, or `null`).
//! - Each of those returns `{ "fatal": "message" }` instead when the entry cannot be read.
//! - `format(name, source)` returns `{ "formatted" }` or `{ "errors": [rendered, …] }`.
//! - `fatalReport(message)` is the `--json` report of a usage or I/O error;
//!   `version()` the compiler's version.

pub mod api;

use std::path::{Path, PathBuf};

use roadbook_compiler::FileSystem;
use wasm_bindgen::prelude::*;

use api::{Config, Overlay};

#[wasm_bindgen]
extern "C" {
    /// The host's file system: see the crate documentation.
    pub type Host;

    #[wasm_bindgen(method, catch, js_name = readFile)]
    fn read_file(this: &Host, path: &str) -> Result<String, JsValue>;

    #[wasm_bindgen(method, js_name = isDir)]
    fn is_dir(this: &Host, path: &str) -> bool;

    #[wasm_bindgen(method)]
    fn realpath(this: &Host, path: &str) -> Option<String>;
}

struct HostFs(Host);

impl FileSystem for HostFs {
    fn read_to_string(&self, path: &Path) -> Result<String, String> {
        self.0
            .read_file(&path.to_string_lossy())
            .map_err(|e| e.as_string().unwrap_or_else(|| "cannot be read".into()))
    }

    fn is_dir(&self, path: &Path) -> bool {
        self.0.is_dir(&path.to_string_lossy())
    }

    fn canonicalize(&self, path: &Path) -> Option<PathBuf> {
        self.0.realpath(&path.to_string_lossy()).map(PathBuf::from)
    }
}

/// The configuration and the file system for a call, or the `fatal` result.
fn setup(host: JsValue, config: &str) -> Result<(Config, Overlay<HostFs>), String> {
    let mut config = Config::from_json(config).map_err(|e| api_fatal(&e))?;
    let host = (!host.is_undefined() && !host.is_null()).then(|| HostFs(host.unchecked_into()));
    let fs = Overlay::new(std::mem::take(&mut config.files), host);
    Ok((config, fs))
}

fn api_fatal(message: &str) -> String {
    serde_json::json!({ "fatal": message }).to_string()
}

/// Checks each root (a JSON array of project paths), as `roadbook check` does.
#[wasm_bindgen]
pub fn check(host: JsValue, config: &str, roots: &str) -> String {
    let (config, fs) = match setup(host, config) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let roots: Vec<String> = match serde_json::from_str(roots) {
        Ok(r) => r,
        Err(e) => return api_fatal(&format!("bad roots: {e}")),
    };
    api::check(fs, &config, &roots).to_string()
}

/// Compiles an entry (a project path) to the IR, as `roadbook export ir` does.
#[wasm_bindgen]
pub fn compile(host: JsValue, config: &str, entry: &str) -> String {
    match setup(host, config) {
        Ok((config, fs)) => api::compile(fs, &config, entry).to_string(),
        Err(e) => e,
    }
}

/// Generates the TypeScript module for an entry (a project path), as `roadbook gen` does.
#[wasm_bindgen(js_name = "gen")]
pub fn generate(host: JsValue, config: &str, entry: &str) -> String {
    match setup(host, config) {
        Ok((config, fs)) => api::generate(fs, &config, entry).to_string(),
        Err(e) => e,
    }
}

/// Formats a source canonically, as `roadbook fmt` does.
#[wasm_bindgen]
pub fn format(name: &str, source: &str) -> String {
    api::format(name, source).to_string()
}

/// The `--json` report of a usage or I/O error that stopped a check.
#[wasm_bindgen(js_name = "fatalReport")]
pub fn fatal_report(message: &str) -> String {
    roadbook_compiler::json::fatal_string(message)
}

/// The compiler's version.
#[wasm_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}
