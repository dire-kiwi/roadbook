//! The Roadbook compiler: loads an entry `.road` file and everything it imports, resolves
//! names, checks structure and types, runs the analyses, and lowers the result to the IR.
//!
//! ```
//! use roadbook_compiler::{MemoryLoader, Options, compile};
//!
//! let mut files = MemoryLoader::new().with(
//!     "hello.road",
//!     "machine Hello() -> string {\n  initial Done\n  final state Done { output \"hi\" }\n}\n",
//! );
//! let out = compile(&mut files, "hello.road", &Options::default()).unwrap();
//! assert!(out.diagnostics.is_empty(), "{}", out.render());
//! let ir = out.ir.unwrap();
//! assert_eq!(ir.machines["Hello"].root.initial.as_deref(), Some("Hello.Done"));
//! ```
//!
//! The passes, in order: [`loader`] and `project` (files and imports), `names` (the
//! declaration table and each file's scope), `check` and `machine` (declarations, machines,
//! targets, invokes), `expr` (expressions and their types, with the rules in `types`), then
//! `analysis` (`always` cycles, recursion, definite assignment in `assign`, warnings) and finally the machine hashes. Each
//! pass reports what it finds; a pass whose input has errors is skipped, so one mistake does
//! not produce a cascade. Diagnostic codes are in [`codes`].

mod analysis;
mod assign;
mod check;
pub mod codes;
mod expr;
pub mod json;
pub mod loader;
mod machine;
mod names;
mod project;
mod types;

pub use loader::{FsLoader, Loaded, Loader, MemoryLoader};
pub use project::LoadError;
pub use roadbook_ir::Ir;
pub use roadbook_syntax::{Diagnostic, Severity};

use std::collections::{HashMap, HashSet};

use roadbook_syntax::FileId;

/// Choices that change what is reported, not what is compiled.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// Warn about an `invoke` whose state, and every state containing it, has no `after`
    /// (`RB0904`). Off by default.
    pub warn_invoke_timeout: bool,
    /// Project paths of files whose machines are not checked for reads of variables that
    /// might not be assigned yet (`RB0403`): fixtures for a runtime's `unassigned` defect,
    /// which a chart can reach only this way. `roadbook.toml`'s `allow-unassigned`.
    pub allow_unassigned: Vec<String>,
}

/// The result of a compilation.
#[derive(Clone, Debug)]
pub struct Compilation {
    /// The IR, if there were no errors (warnings allowed). Always `None` from [`check`].
    pub ir: Option<Ir>,
    /// Errors and warnings, ordered by file (in load order) and position.
    pub diagnostics: Vec<Diagnostic>,
    /// `(project path, text)` of every loaded file, indexed by the `FileId` in diagnostic
    /// spans: the root files and everything they import, in load order. For [`compile`] this
    /// is the IR's `sources`.
    pub files: Vec<(String, String)>,
}

impl Compilation {
    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(Diagnostic::is_error)
    }

    pub fn error_count(&self) -> usize {
        self.diagnostics.iter().filter(|d| d.is_error()).count()
    }

    pub fn warning_count(&self) -> usize {
        self.diagnostics.len() - self.error_count()
    }

    /// Every diagnostic in the human-readable form, separated by blank lines.
    pub fn render(&self) -> String {
        self.diagnostics
            .iter()
            .map(|d| d.render(&self.files))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The machine-readable form printed by `roadbook check --json`, as text with members in
    /// the documented order. See [`json`].
    pub fn to_json(&self) -> String {
        json::diagnostics_string(&self.diagnostics, &self.files)
    }
}

/// Compiles one entry file (a project path, see [`loader`]) and everything it imports.
/// Fails only if the entry file itself cannot be read; everything else is a diagnostic.
///
/// The files of a compilation are the entry and everything it imports, transitively, and
/// declaration names must be unique among them, because the IR is keyed by name. "Unused"
/// warnings are given for the entry's functions, actions and tasks that nothing uses.
pub fn compile(
    loader: &mut dyn Loader,
    entry: &str,
    options: &Options,
) -> Result<Compilation, LoadError> {
    let mut unit = Unit::compile(loader, entry, options, true)?;
    let entry = unit.compilation.files[0].0.clone();
    for c in &unit.candidates {
        if c.key.0 == entry && !unit.used.contains(&c.key) {
            unit.compilation
                .diagnostics
                .push(analysis::unused_warning(c));
        }
    }
    sort(&mut unit.compilation.diagnostics);
    Ok(unit.compilation)
}

/// Checks several files, compiling each as an entry (see [`compile`]), and produces no IR.
/// Names need only be unique within each entry's compilation, so independent charts may
/// share names. A file imported by several entries is reported on once: diagnostics that
/// several compilations give alike are given once. "Unused" warnings are given for the
/// functions, actions and tasks of the checked files that no checked file uses.
pub fn check(
    loader: &mut dyn Loader,
    roots: &[String],
    options: &Options,
) -> Result<Compilation, LoadError> {
    let mut out = Compilation {
        ir: None,
        diagnostics: Vec::new(),
        files: Vec::new(),
    };
    let mut ids: HashMap<String, FileId> = HashMap::new();
    let mut used: HashSet<analysis::DeclKey> = HashSet::new();
    let mut candidates: Vec<analysis::UnusedCandidate> = Vec::new();
    let mut checked: HashSet<String> = HashSet::new();
    for root in roots {
        let unit = Unit::compile(loader, root, options, false)?;
        checked.insert(unit.compilation.files[0].0.clone());
        let map: Vec<FileId> = unit
            .compilation
            .files
            .into_iter()
            .map(|(path, text)| {
                *ids.entry(path.clone()).or_insert_with(|| {
                    out.files.push((path, text));
                    FileId(out.files.len() as u32 - 1)
                })
            })
            .collect();
        for mut d in unit.compilation.diagnostics {
            remap(&mut d, &map);
            if !out.diagnostics.contains(&d) {
                out.diagnostics.push(d);
            }
        }
        used.extend(unit.used);
        for mut c in unit.candidates {
            c.span.file = map[c.span.file.0 as usize];
            if !candidates.iter().any(|x| x.key == c.key) {
                candidates.push(c);
            }
        }
    }
    for c in &candidates {
        if checked.contains(&c.key.0) && !used.contains(&c.key) {
            out.diagnostics.push(analysis::unused_warning(c));
        }
    }
    sort(&mut out.diagnostics);
    Ok(out)
}

/// Compiles a single file held in memory, with no imports available.
pub fn compile_source(path: &str, source: &str, options: &Options) -> Compilation {
    let mut loader = MemoryLoader::new().with(path, source);
    let path = loader::normalize(path);
    match compile(&mut loader, &path, options) {
        Ok(c) => c,
        // The loader holds the file, so this cannot happen.
        Err(_) => Compilation {
            ir: None,
            diagnostics: Vec::new(),
            files: Vec::new(),
        },
    }
}

/// One entry's compilation, before the "unused" warnings, which depend on what else is
/// checked.
struct Unit {
    compilation: Compilation,
    /// The functions, actions and tasks an "unused" warning may be given for: none if the
    /// compilation has errors, since the analyses and warnings then do not run.
    candidates: Vec<analysis::UnusedCandidate>,
    used: HashSet<analysis::DeclKey>,
}

impl Unit {
    fn compile(
        loader: &mut dyn Loader,
        entry: &str,
        options: &Options,
        want_ir: bool,
    ) -> Result<Unit, LoadError> {
        let project = project::Project::load(loader, &[entry.to_string()])?;
        let mut diagnostics = project.diagnostics.clone();
        let mut ck = check::Checker::new(&project);
        let mut doc = ck.lower(&project);
        diagnostics.append(&mut ck.diags);
        let ok = !diagnostics.iter().any(Diagnostic::is_error);
        if ok {
            analysis::run(&doc, &ck, &project, options, &mut diagnostics);
        }
        let (mut candidates, used) = analysis::uses(&ck, &project);
        let mut ir = None;
        if !diagnostics.iter().any(Diagnostic::is_error) {
            if want_ir {
                roadbook_ir::fill_hashes(&mut doc).expect("a checked IR names only declared items");
                ir = Some(doc);
            }
        } else {
            candidates.clear();
        }
        sort(&mut diagnostics);
        Ok(Unit {
            compilation: Compilation {
                ir,
                diagnostics,
                files: project.file_list(),
            },
            candidates,
            used,
        })
    }
}

fn sort(diagnostics: &mut [Diagnostic]) {
    diagnostics.sort_by_key(|d| (d.span.file, d.span.start.offset, d.span.end.offset));
}

/// Moves a diagnostic's spans from one compilation's file ids to another's.
fn remap(d: &mut Diagnostic, map: &[FileId]) {
    let id = |f: FileId| map[f.0 as usize];
    d.file = id(d.file);
    d.span.file = id(d.span.file);
    for l in &mut d.labels {
        l.span.file = id(l.span.file);
    }
    for f in &mut d.fixes {
        for e in &mut f.edits {
            e.span.file = id(e.span.file);
        }
    }
}
