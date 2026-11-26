//! What the WebAssembly functions do, in plain Rust over any [`FileSystem`], so that it can be
//! tested on the host. Each function returns a JSON object (see the crate documentation).

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use roadbook_compiler::loader::{FileRef, NotExported, export_of, relative_path};
use roadbook_compiler::{Compilation, FileSystem, FsLoader, Ir, Options};
use roadbook_gen_ts::PackageModule;
use roadbook_syntax::diagnostic::SingleFile;
use serde::Deserialize;
use serde_json::{Value, json};

/// The project a function works on, as the host found it (`roadbook.toml`).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// The project root: an absolute path with `/` separators.
    pub root: String,
    /// `[packages]`: package name to directory, relative to the root.
    pub packages: Vec<(String, String)>,
    /// Whether packages not in `packages` are looked up in `node_modules`, as Node does.
    pub node_modules: bool,
    /// `allow-unassigned`: the project paths of the files compiled without `RB0403`.
    pub allow_unassigned: Vec<String>,
    /// `roadbook check --warn invoke-timeout`.
    pub warn_invoke_timeout: bool,
    /// Files held by the host in memory, by absolute path: read instead of the host's
    /// file system (unsaved editor buffers, or a project with no file system at all).
    pub files: BTreeMap<String, String>,
}

impl Config {
    pub fn from_json(text: &str) -> Result<Config, String> {
        serde_json::from_str(text).map_err(|e| format!("bad configuration: {e}"))
    }

    fn options(&self) -> Options {
        Options {
            warn_invoke_timeout: self.warn_invoke_timeout,
            allow_unassigned: self.allow_unassigned.clone(),
        }
    }

    fn loader<F: FileSystem>(&self, fs: F) -> FsLoader<F> {
        let mut loader = FsLoader::with_fs(&self.root, fs).with_node_modules(self.node_modules);
        for (name, dir) in &self.packages {
            loader = loader.with_package(name, dir);
        }
        loader
    }
}

/// The diagnostics of a compilation: the documented JSON (`report`, as `roadbook check
/// --json` prints it), the human-readable form (`rendered`) and the counts.
fn diagnostics(out: &Compilation) -> serde_json::Map<String, Value> {
    let mut m = serde_json::Map::new();
    m.insert("report".into(), out.to_json().into());
    m.insert("rendered".into(), out.render().into());
    m.insert("errors".into(), out.error_count().into());
    m.insert("warnings".into(), out.warning_count().into());
    m
}

fn fatal(message: impl std::fmt::Display) -> Value {
    json!({ "fatal": message.to_string() })
}

/// `roadbook check`: each root (a project path) compiled as an entry, no IR.
pub fn check<F: FileSystem>(fs: F, config: &Config, roots: &[String]) -> Value {
    match roadbook_compiler::check(&mut config.loader(fs), roots, &config.options()) {
        Ok(out) => Value::Object(diagnostics(&out)),
        Err(e) => fatal(e),
    }
}

/// `roadbook export ir`: the entry (a project path) compiled, with the IR as committed-file
/// JSON in `ir`, `null` if there are errors.
pub fn compile<F: FileSystem>(fs: F, config: &Config, entry: &str) -> Value {
    match roadbook_compiler::compile(&mut config.loader(fs), entry, &config.options()) {
        Ok(out) => {
            let mut m = diagnostics(&out);
            m.insert("ir".into(), out.ir.as_ref().map(Ir::to_json).into());
            Value::Object(m)
        }
        Err(e) => fatal(e),
    }
}

/// `roadbook gen` for one entry: the TypeScript module in `code`, or why there is none:
/// errors in the diagnostics, or a message in `error` (which the command line prints after
/// `error: `).
pub fn generate<F: FileSystem>(fs: F, config: &Config, entry: &str) -> Value {
    let mut loader = config.loader(fs);
    let out = match roadbook_compiler::compile(&mut loader, entry, &config.options()) {
        Ok(out) => out,
        Err(e) => return fatal(e),
    };
    let mut m = diagnostics(&out);
    let (code, error) = match out.ir.as_ref().filter(|_| !out.has_errors()) {
        None => (None, None),
        Some(ir) => match package_modules(config, &loader, ir) {
            Err(e) => (None, Some(format!("`{entry}`: {e}"))),
            Ok(modules) => {
                let options = roadbook_gen_ts::Options::default().with_modules(modules);
                match roadbook_gen_ts::generate(ir, &options) {
                    Ok(code) => (Some(code), None),
                    Err(mut e) => {
                        if let roadbook_gen_ts::GenError::ErrorField {
                            file, line, field, ..
                        } = &mut e
                        {
                            // The IR knows only the declaration's line: find the field's.
                            let disk = Path::new(&config.root).join(&*file);
                            let text = loader.fs().read_to_string(&disk).unwrap_or_default();
                            if let Some(found) = field_line(&text, *line, field) {
                                *line = found;
                            }
                            (None, Some(e.to_string()))
                        } else {
                            (None, Some(format!("`{entry}`: {e}")))
                        }
                    }
                }
            }
        },
    };
    m.insert("code".into(), code.into());
    m.insert("error".into(), error.into());
    Value::Object(m)
}

/// `roadbook fmt` for one file: the canonical text in `formatted`, or the syntax errors,
/// each rendered, in `errors`. `name` is how the errors name the file.
pub fn format(name: &str, source: &str) -> Value {
    match roadbook_syntax::format(source) {
        Ok(formatted) => json!({ "formatted": formatted }),
        Err(diags) => {
            let files = SingleFile { name, source };
            let errors: Vec<String> = diags.iter().map(|d| d.render(&files)).collect();
            json!({ "errors": errors })
        }
    }
}

/// The conditions TypeScript and Node match in an `exports` map when the generated module
/// imports another package's module.
const MODULE_CONDITIONS: &[&str] = &["types", "import", "node", "default"];

/// How the module generated from `ir` imports the modules of the files of other packages,
/// as `roadbook gen` decides it (`crates/roadbook-cli/src/commands/gen.rs`, which this
/// follows line for line; the npm package's tests compare the two).
fn package_modules<F: FileSystem>(
    config: &Config,
    loader: &FsLoader<F>,
    ir: &Ir,
) -> Result<BTreeMap<(String, String), PackageModule>, String> {
    let root = PathBuf::from(&config.root);
    let mut out = BTreeMap::new();
    for source in &ir.sources {
        let Some(package) = &source.package else {
            continue;
        };
        let Some(found) = loader.package_file(&FileRef::in_package(package, &source.path)) else {
            continue;
        };
        let module = if found.listed {
            PackageModule::Path(relative_path(&root, &found.disk))
        } else {
            let inside = relative_path(&found.dir, &found.disk);
            let json = loader
                .fs()
                .read_to_string(&found.dir.join("package.json"))
                .unwrap_or_default();
            let targets = [format!("{inside}.js"), format!("{inside}.ts")];
            match export_of(&json, &targets, MODULE_CONDITIONS) {
                Ok(None) => PackageModule::Specifier(format!("{package}/{inside}.js")),
                Ok(Some(subpath)) => PackageModule::Specifier(format!("{package}/{subpath}")),
                Err(why) => {
                    let why = match why {
                        NotExported::Missing => format!(
                            "its `package.json` `exports` maps no subpath to `./{inside}.js`; \
                             export the generated modules next to the charts, as \
                             `\"./{}.js\": \"./{inside}.js\"` or a pattern such as \
                             `\"./*.road.js\": \"./<dir>/*.road.js\"`",
                            source.path
                        ),
                        NotExported::Invalid(why) => why,
                    };
                    return Err(format!(
                        "cannot import the generated module of `{package}/{}`: {why}",
                        source.path
                    ));
                }
            }
        };
        out.insert((package.clone(), source.path.clone()), module);
    }
    Ok(out)
}

/// The first line from `from` on (1-based) where `field` is declared: the name as a whole
/// word followed by `:`.
fn field_line(text: &str, from: u32, field: &str) -> Option<u32> {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    for (i, line) in text
        .lines()
        .enumerate()
        .skip(from.saturating_sub(1) as usize)
    {
        let code = line.split("//").next().unwrap_or_default();
        for (at, _) in code.match_indices(field) {
            let before = code[..at].chars().next_back();
            let after = code[at + field.len()..].trim_start();
            if !before.is_some_and(word) && after.starts_with(':') {
                return Some(i as u32 + 1);
            }
        }
    }
    None
}

/// Files held in memory, by absolute path, in front of another file system (or none).
pub struct Overlay<F> {
    pub files: BTreeMap<String, String>,
    pub dirs: HashSet<String>,
    pub below: Option<F>,
}

impl<F> Overlay<F> {
    pub fn new(files: BTreeMap<String, String>, below: Option<F>) -> Overlay<F> {
        let mut dirs = HashSet::new();
        for path in files.keys() {
            let mut p = path.as_str();
            while let Some((dir, _)) = p.rsplit_once('/') {
                if !dirs.insert(if dir.is_empty() { "/" } else { dir }.to_string()) {
                    break;
                }
                p = dir;
            }
        }
        Overlay { files, dirs, below }
    }
}

/// A path as the overlay's keys are written: `/`-separated, `.` and `..` applied.
fn key(path: &Path) -> String {
    let text = path.to_string_lossy();
    let absolute = text.starts_with('/');
    let normal = roadbook_compiler::loader::normalize(&text);
    if absolute {
        format!("/{normal}")
    } else {
        normal
    }
}

impl<F: FileSystem> FileSystem for Overlay<F> {
    fn read_to_string(&self, path: &Path) -> Result<String, String> {
        if let Some(text) = self.files.get(&key(path)) {
            return Ok(text.clone());
        }
        match &self.below {
            Some(fs) => fs.read_to_string(path),
            None => Err("No such file or directory (os error 2)".into()),
        }
    }

    fn is_dir(&self, path: &Path) -> bool {
        self.dirs.contains(&key(path)) || self.below.as_ref().is_some_and(|fs| fs.is_dir(path))
    }

    fn canonicalize(&self, path: &Path) -> Option<PathBuf> {
        if let Some(real) = self.below.as_ref().and_then(|fs| fs.canonicalize(path)) {
            return Some(real);
        }
        let k = key(path);
        (self.files.contains_key(&k) || self.dirs.contains(&k)).then(|| PathBuf::from(k))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use roadbook_compiler::StdFs;

    const LEAF: &str = "machine Stock() {\n  initial X\n  final state X {}\n}\n";

    /// A project held in memory, with a package in `node_modules` that maps its charts with
    /// `exports`.
    fn project() -> (Config, Overlay<StdFs>) {
        let files = [
            (
                "/p/app/order.road",
                "import { Stock } from \"stock/stock.road\"\n\nmachine Order(s: ref Stock) {\n  initial Done\n  final state Done {}\n}\n",
            ),
            ("/p/node_modules/stock/src/stock.road", LEAF),
            (
                "/p/node_modules/stock/package.json",
                r#"{ "exports": { "./*.road": "./src/*.road", "./*.road.js": "./src/*.road.js" } }"#,
            ),
            ("/p/bad.road", "machine Bad() {\n  initial Nope\n}\n"),
        ];
        let files = files
            .iter()
            .map(|(p, t)| (p.to_string(), t.to_string()))
            .collect();
        let config = Config {
            root: "/p".into(),
            node_modules: true,
            ..Config::default()
        };
        (config, Overlay::new(files, None))
    }

    #[test]
    fn compiles_a_project_in_memory_with_a_package() {
        let (config, fs) = project();
        let out = compile(fs, &config, "app/order.road");
        assert_eq!(out["errors"], 0, "{}", out["rendered"]);
        let ir = Ir::from_json(out["ir"].as_str().unwrap()).unwrap();
        assert_eq!(ir.sources[1].package.as_deref(), Some("stock"));
        assert_eq!(ir.sources[1].path, "stock.road");
    }

    #[test]
    fn generates_with_the_specifier_exports_gives() {
        let (config, fs) = project();
        let out = generate(fs, &config, "app/order.road");
        let code = out["code"].as_str().unwrap();
        assert!(
            code.contains("import { type StockDef } from \"stock/stock.road.js\""),
            "{code}"
        );
        assert!(out["error"].is_null());
    }

    #[test]
    fn reports_diagnostics_and_fatal_errors() {
        let (config, fs) = project();
        let out = check(fs, &config, &["bad.road".into()]);
        assert_eq!(out["errors"], 1);
        let report: Value = serde_json::from_str(out["report"].as_str().unwrap()).unwrap();
        assert_eq!(report["diagnostics"][0]["file"], "bad.road");
        assert!(
            out["rendered"]
                .as_str()
                .unwrap()
                .contains("--> bad.road:1:9")
        );
        let (config, fs) = project();
        let out = compile(fs, &config, "nope.road");
        assert_eq!(
            out["fatal"],
            "cannot read `nope.road`: No such file or directory (os error 2)"
        );
    }

    #[test]
    fn formats() {
        assert_eq!(
            format(
                "a.road",
                "machine M() {\ninitial X\n  final state X {}\n}\n"
            )["formatted"],
            "machine M() {\n  initial X\n  final state X {}\n}\n"
        );
        let out = format("a.road", "machine M( {\n");
        assert!(
            out["errors"][0]
                .as_str()
                .unwrap()
                .contains("--> a.road:1:12")
        );
    }

    #[test]
    fn reads_the_configuration() {
        let c = Config::from_json(
            r#"{"root": "/r", "packages": [["a", "b"]], "allowUnassigned": ["x.road"]}"#,
        )
        .unwrap();
        assert_eq!(c.packages, [("a".to_string(), "b".to_string())]);
        assert_eq!(c.allow_unassigned, ["x.road"]);
        assert!(Config::from_json(r#"{"rooot": "/r"}"#).is_err());
    }
}
