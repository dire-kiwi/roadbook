//! Shared helpers: loading test charts, reading their expectations, validating IR.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use roadbook_compiler::{Compilation, MemoryLoader, Options, check, compile};
use serde_json::Value;

/// A test chart: a single `.road` file, or a directory whose entry is `main.road`.
pub struct Case {
    pub name: String,
    pub loader: MemoryLoader,
    pub entry: String,
    pub options: Options,
    /// `(file, code, line, col)` from the `// expect: CODE at LINE:COL` lines of each file.
    pub expected: Vec<(String, String, u32, u32)>,
}

pub fn dir(sub: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join(sub)
}

/// Every file under `dir`, sorted.
fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out.sort();
    out
}

pub fn cases(sub: &str) -> Vec<Case> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir(sub))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir() || p.extension().is_some_and(|e| e == "road"))
        .collect();
    entries.sort();
    entries
        .into_iter()
        .map(|p| {
            let name = p.file_stem().unwrap().to_string_lossy().into_owned();
            let mut loader = MemoryLoader::new();
            let entry = if p.is_dir() {
                for f in walk(&p) {
                    let rel = f.strip_prefix(&p).unwrap().to_string_lossy().into_owned();
                    loader.insert(&rel, std::fs::read_to_string(&f).unwrap());
                }
                "main.road".to_string()
            } else {
                let rel = format!("{name}.road");
                loader.insert(&rel, std::fs::read_to_string(&p).unwrap());
                rel
            };
            let mut options = Options::default();
            let mut expected = Vec::new();
            for (file, text) in &loader.files {
                for line in text.lines() {
                    if let Some(rest) = line.trim().strip_prefix("// expect: ") {
                        let (code, pos) = rest.split_once(" at ").expect("`CODE at L:C`");
                        let (l, c) = pos.trim().split_once(':').expect("`L:C`");
                        expected.push((
                            file.clone(),
                            code.trim().to_string(),
                            l.parse().unwrap(),
                            c.parse().unwrap(),
                        ));
                    }
                    if line.trim() == "// options: warn-invoke-timeout" {
                        options.warn_invoke_timeout = true;
                    }
                    if let Some(n) = line.trim().strip_prefix("// options: max-configurations ") {
                        options.max_configurations = Some(n.parse().unwrap());
                    }
                }
            }
            expected.sort();
            Case {
                name,
                loader,
                entry,
                options,
                expected,
            }
        })
        .collect()
}

impl Case {
    pub fn compile(&self) -> Compilation {
        let mut loader = self.loader.clone();
        compile(&mut loader, &self.entry, &self.options).unwrap()
    }

    /// As `roadbook check` checks the entry: with its `check` lines evaluated, and no IR.
    pub fn check(&self) -> Compilation {
        let mut loader = self.loader.clone();
        check(
            &mut loader,
            std::slice::from_ref(&self.entry),
            &self.options,
        )
        .unwrap()
    }
}

/// `(file, code, line, col)` of every diagnostic, sorted.
pub fn found(c: &Compilation) -> Vec<(String, String, u32, u32)> {
    let mut v: Vec<_> = c
        .diagnostics
        .iter()
        .map(|d| {
            (
                c.files[d.span.file.0 as usize].0.clone(),
                d.code.to_string(),
                d.span.start.line,
                d.span.start.col,
            )
        })
        .collect();
    v.sort();
    v
}

pub fn validator() -> jsonschema::Validator {
    let schema: Value =
        serde_json::from_str(include_str!("../../../../spec/ir.schema.json")).unwrap();
    jsonschema::draft202012::new(&schema).unwrap()
}

/// Schema errors in the IR of a successful compilation.
pub fn schema_errors(v: &jsonschema::Validator, c: &Compilation) -> Vec<String> {
    let ir =
        c.ir.as_ref()
            .expect("a compilation without errors has an IR");
    let doc: Value = serde_json::from_str(&ir.to_json()).unwrap();
    v.iter_errors(&doc)
        .map(|e| format!("{e} at {}", e.instance_path))
        .collect()
}
