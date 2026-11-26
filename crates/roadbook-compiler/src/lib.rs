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
//! `analysis` (`always` cycles, recursion, warnings) and finally the machine hashes. Each
//! pass reports what it finds; a pass whose input has errors is skipped, so one mistake does
//! not produce a cascade. Diagnostic codes are in [`codes`].

mod analysis;
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

/// Choices that change what is reported, not what is compiled.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// Warn about an `invoke` whose state, and every state containing it, has no `after`
    /// (`RB0904`). Off by default.
    pub warn_invoke_timeout: bool,
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
pub fn compile(
    loader: &mut dyn Loader,
    entry: &str,
    options: &Options,
) -> Result<Compilation, LoadError> {
    run(loader, &[entry.to_string()], options, true)
}

/// Checks several files together, as one project: every root, everything they import, and
/// declaration names unique across all of them. Produces no IR. "Unused" warnings are given
/// for declarations in the roots that nothing loaded uses.
pub fn check(
    loader: &mut dyn Loader,
    roots: &[String],
    options: &Options,
) -> Result<Compilation, LoadError> {
    run(loader, roots, options, false)
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

fn run(
    loader: &mut dyn Loader,
    roots: &[String],
    options: &Options,
    want_ir: bool,
) -> Result<Compilation, LoadError> {
    let project = project::Project::load(loader, roots)?;
    let mut diagnostics = project.diagnostics.clone();
    let mut ir = None;
    if !project.syntax_errors {
        let mut ck = check::Checker::new(&project);
        let mut doc = ck.lower(&project);
        diagnostics.append(&mut ck.diags);
        if !diagnostics.iter().any(Diagnostic::is_error) {
            analysis::run(&doc, &ck, &project, options, &mut diagnostics);
        }
        if want_ir && !diagnostics.iter().any(Diagnostic::is_error) {
            roadbook_ir::fill_hashes(&mut doc).expect("a checked IR names only declared items");
            ir = Some(doc);
        }
    }
    diagnostics.sort_by_key(|d| (d.span.file, d.span.start.offset, d.span.end.offset));
    Ok(Compilation {
        ir,
        diagnostics,
        files: project.file_list(),
    })
}
