//! Loading a project: the root files, everything they import, parsed.

use std::collections::HashMap;

use roadbook_syntax::ast::File;
use roadbook_syntax::{Diagnostic, FileId, Span, parse_file};

use crate::codes;
use crate::loader::{Loaded, Loader, normalize};

/// One loaded source file.
pub(crate) struct SourceFile {
    /// Project path.
    pub path: String,
    pub text: String,
    pub ast: File,
    /// For each import of the file, in order: the index of the imported file, or `None` if it
    /// could not be loaded.
    pub imports: Vec<Option<usize>>,
}

/// The files of one compilation, in load order. A file's index is its [`FileId`] and its
/// index in the IR's `sources`.
pub(crate) struct Project {
    pub files: Vec<SourceFile>,
    /// Indices of the files the compilation was asked for, as opposed to the ones they import.
    pub roots: Vec<usize>,
    pub diagnostics: Vec<Diagnostic>,
    /// Whether any file had a syntax error. The later passes are skipped if so, because a
    /// partial tree would produce misleading errors.
    pub syntax_errors: bool,
    by_path: HashMap<String, usize>,
}

/// A root file that could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadError {
    /// Its project path.
    pub path: String,
    /// The loader's reason.
    pub reason: String,
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cannot read `{}`: {}", self.path, self.reason)
    }
}

impl std::error::Error for LoadError {}

impl Project {
    /// Loads each root and, depth first in the order written, everything it imports. A file
    /// is loaded once however often it is imported, which also ends import cycles.
    pub fn load(loader: &mut dyn Loader, roots: &[String]) -> Result<Project, LoadError> {
        let mut p = Project {
            files: Vec::new(),
            roots: Vec::new(),
            diagnostics: Vec::new(),
            syntax_errors: false,
            by_path: HashMap::new(),
        };
        for root in roots {
            let path = normalize(root);
            let index = match p.by_path.get(&path) {
                Some(&i) => i,
                None => match loader.load(&path) {
                    Ok(loaded) => p.add(loader, path, loaded),
                    Err(reason) => return Err(LoadError { path, reason }),
                },
            };
            if !p.roots.contains(&index) {
                p.roots.push(index);
            }
        }
        Ok(p)
    }

    fn add(&mut self, loader: &mut dyn Loader, path: String, loaded: Loaded) -> usize {
        let Loaded::Source(text) = loaded;
        let index = self.files.len();
        let (ast, diags) = parse_file(FileId(index as u32), &text);
        self.syntax_errors |= diags.iter().any(Diagnostic::is_error);
        self.diagnostics.extend(diags);
        let imports: Vec<(String, Span)> = ast
            .imports()
            .map(|i| (i.path.value.clone(), i.path.span))
            .collect();
        self.by_path.insert(path.clone(), index);
        self.files.push(SourceFile {
            path: path.clone(),
            text,
            ast,
            imports: Vec::new(),
        });
        let mut resolved = Vec::new();
        for (spec, span) in imports {
            resolved.push(self.import(loader, &path, &spec, span));
        }
        self.files[index].imports = resolved;
        index
    }

    fn import(
        &mut self,
        loader: &mut dyn Loader,
        importer: &str,
        spec: &str,
        span: Span,
    ) -> Option<usize> {
        let path = match loader.resolve(importer, spec) {
            Ok(p) => normalize(&p),
            Err(reason) => {
                self.diagnostics.push(Diagnostic::error(
                    codes::IMPORT_NOT_FOUND,
                    span,
                    format!("cannot import `{spec}`: {reason}"),
                ));
                return None;
            }
        };
        if let Some(&i) = self.by_path.get(&path) {
            return Some(i);
        }
        match loader.load(&path) {
            Ok(loaded) => Some(self.add(loader, path, loaded)),
            Err(reason) => {
                self.diagnostics.push(
                    Diagnostic::error(
                        codes::IMPORT_NOT_FOUND,
                        span,
                        format!("cannot import `{spec}`: {reason}"),
                    )
                    .with_label(format!("resolved to `{path}`"))
                    .with_help("import paths are relative to the importing file"),
                );
                None
            }
        }
    }

    /// `(path, text)` of every file, indexed by [`FileId`], for rendering diagnostics.
    pub fn file_list(&self) -> Vec<(String, String)> {
        self.files
            .iter()
            .map(|f| (f.path.clone(), f.text.clone()))
            .collect()
    }
}
