//! Loading a project: the root files, everything they import, parsed; and the files an
//! already compiled IR stands for (see [`Loaded::Compiled`]).

use std::collections::HashMap;
use std::path::PathBuf;

use indexmap::IndexMap;
use roadbook_ir as ir;
use roadbook_syntax::ast::{self, Decl, DeclKind, File};
use roadbook_syntax::{Diagnostic, FileId, Pos, Span, parse_file};

use crate::codes;
use crate::loader::{FileRef, Loaded, Loader, join, normalize};

/// The declarations of a file that an already compiled IR stands for, as the IR has them,
/// their spans renumbered for this compilation. They are copied into the output.
#[derive(Default)]
pub(crate) struct Compiled {
    pub types: IndexMap<String, ir::TypeDecl>,
    pub functions: IndexMap<String, ir::FunctionDecl>,
    pub actions: IndexMap<String, ir::ActionDecl>,
    pub tasks: IndexMap<String, ir::TaskDecl>,
    pub machines: IndexMap<String, ir::Machine>,
}

/// One loaded source file.
pub(crate) struct SourceFile {
    pub file: FileRef,
    /// How diagnostics name the file: its project path, or for a file of a package what the
    /// loader says ([`Loader::display`]).
    pub path: String,
    pub text: String,
    /// For a file of a compiled IR, a tree with its declarations only: the machines hold
    /// their `event` and `emits` declarations and nothing else.
    pub ast: File,
    /// Whether the file has syntax errors. Its tree is partial, so its declarations are left
    /// out and its names are poisoned in the files that import it: the other files are still
    /// checked, without diagnostics that a partial tree would cause.
    pub syntax_errors: bool,
    /// For each import of the file, in order: the index of the imported file, or `None` if it
    /// could not be loaded.
    pub imports: Vec<Option<usize>>,
    /// For a file of a compiled IR: the index of its IR in [`Project::groups`], and its
    /// declarations, which are not checked again.
    pub compiled: Option<(usize, Compiled)>,
}

/// The files of one compilation, in load order. A file's index is its [`FileId`] and its
/// index in the IR's `sources`.
pub(crate) struct Project {
    pub files: Vec<SourceFile>,
    /// Indices of the files the compilation was asked for, as opposed to the ones they import.
    pub roots: Vec<usize>,
    pub diagnostics: Vec<Diagnostic>,
    /// For each compiled IR loaded, the indices of the files its sources are, in order. The
    /// names declared in any of them are in scope in each of them.
    pub groups: Vec<Vec<usize>>,
    /// Each file by every name it has been reached by.
    by_path: HashMap<FileRef, usize>,
    /// Each file by its [`Loader::location`], if the loader gives one.
    by_location: HashMap<PathBuf, usize>,
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
    /// Loads each root (a project path) and, depth first in the order written, everything it
    /// imports. A file is loaded once however often it is imported, which also ends import
    /// cycles.
    pub fn load(loader: &mut dyn Loader, roots: &[String]) -> Result<Project, LoadError> {
        let mut p = Project {
            files: Vec::new(),
            roots: Vec::new(),
            diagnostics: Vec::new(),
            groups: Vec::new(),
            by_path: HashMap::new(),
            by_location: HashMap::new(),
        };
        for root in roots {
            let file = FileRef::project(root);
            let index = match p.known(loader, &file) {
                Some(i) => i,
                None => {
                    let loaded = loader.load(&file).map_err(|reason| LoadError {
                        path: file.path.clone(),
                        reason,
                    })?;
                    p.add(loader, file.clone(), loaded)
                        .map_err(|reason| LoadError {
                            path: file.path.clone(),
                            reason,
                        })?
                }
            };
            if !p.roots.contains(&index) {
                p.roots.push(index);
            }
        }
        Ok(p)
    }

    fn add(
        &mut self,
        loader: &mut dyn Loader,
        file: FileRef,
        loaded: Loaded,
    ) -> Result<usize, String> {
        let text = match loaded {
            Loaded::Source(text) => text,
            Loaded::Compiled(ir) => return self.add_compiled(loader, file, *ir),
        };
        let index = self.files.len();
        let (ast, diags) = parse_file(FileId(index as u32), &text);
        let syntax_errors = diags.iter().any(Diagnostic::is_error);
        self.diagnostics.extend(diags);
        let imports: Vec<(String, Span)> = ast
            .imports()
            .map(|i| (i.path.value.clone(), i.path.span))
            .collect();
        self.register(loader, &file, index);
        self.files.push(SourceFile {
            path: display(loader, &file),
            file: file.clone(),
            text,
            ast,
            syntax_errors,
            imports: Vec::new(),
            compiled: None,
        });
        let mut resolved = Vec::new();
        for (spec, span) in imports {
            resolved.push(self.import(loader, &file, &spec, span));
        }
        self.files[index].imports = resolved;
        Ok(index)
    }

    /// The files of a compiled IR whose entry is `file`. Each source of the IR becomes a file
    /// of this compilation, unless the compilation already has that file.
    fn add_compiled(
        &mut self,
        loader: &dyn Loader,
        file: FileRef,
        ir: ir::Ir,
    ) -> Result<usize, String> {
        roadbook_ir::check_version(&ir.ir_version).map_err(|e| e.to_string())?;
        let Some(entry) = ir.sources.first() else {
            return Err("the compiled IR has no sources".into());
        };
        let entry_dir = entry
            .path
            .rsplit_once('/')
            .map_or("", |(d, _)| d)
            .to_string();
        let group = self.groups.len();
        let mut map: Vec<usize> = Vec::new();
        let mut new: Vec<bool> = Vec::new();
        for (i, src) in ir.sources.iter().enumerate() {
            let fref = if i == 0 {
                file.clone()
            } else if let Some(p) = &src.package {
                FileRef::in_package(p, &src.path)
            } else {
                // Where the file is relative to the entry, from the entry as imported here.
                let rel = src
                    .path
                    .strip_prefix(&format!("{entry_dir}/"))
                    .map_or_else(|| relative_dir(&entry_dir, &src.path), str::to_string);
                FileRef {
                    package: file.package.clone(),
                    path: normalize(&join(&file.path, &format!("./{rel}"))),
                }
            };
            if let Some(idx) = self.known(loader, &fref) {
                map.push(idx);
                new.push(false);
                continue;
            }
            let idx = self.files.len();
            self.register(loader, &fref, idx);
            self.files.push(SourceFile {
                path: display(loader, &fref),
                file: fref,
                text: String::new(),
                ast: File {
                    items: Vec::new(),
                    comments: Vec::new(),
                    span: Span::default(),
                },
                syntax_errors: false,
                imports: Vec::new(),
                compiled: Some((group, Compiled::default())),
            });
            map.push(idx);
            new.push(true);
        }
        let target = |span: &ir::Span| {
            let i = span.file as usize;
            (i < map.len() && new[i]).then(|| map[i])
        };
        let remap = |span: &mut ir::Span| {
            span.file = map.get(span.file as usize).copied().unwrap_or(map[0]) as u32;
        };
        let ir::Ir {
            types,
            functions,
            actions,
            tasks,
            machines,
            ..
        } = ir;
        for (name, mut d) in types {
            if let Some(f) = target(&d.span) {
                remap(&mut d.span);
                let kind = match &d.def {
                    ir::TypeDef::Record { fields } => DeclKind::Record {
                        fields: ast_fields(fields, d.span),
                    },
                    ir::TypeDef::Alias { ty } => DeclKind::Alias {
                        target: ast_type(ty, d.span),
                    },
                    ir::TypeDef::Enum { variants } => DeclKind::Enum {
                        variants: variants.iter().map(|v| ident(v, d.span)).collect(),
                    },
                    ir::TypeDef::Error { fields } => DeclKind::Error {
                        fields: ast_fields(fields, d.span),
                    },
                    ir::TypeDef::Extern => DeclKind::Extern,
                };
                self.push_decl(f, &name, &d.doc, d.span, kind);
                self.compiled(f).types.insert(name, d);
            }
        }
        for (name, mut d) in functions {
            if let Some(f) = target(&d.span) {
                remap(&mut d.span);
                let kind = DeclKind::Fn {
                    params: ast_fields(&d.params, d.span),
                    returns: ast_type(&d.returns, d.span),
                };
                self.push_decl(f, &name, &d.doc, d.span, kind);
                self.compiled(f).functions.insert(name, d);
            }
        }
        for (name, mut d) in actions {
            if let Some(f) = target(&d.span) {
                remap(&mut d.span);
                let kind = DeclKind::Action {
                    params: ast_fields(&d.params, d.span),
                };
                self.push_decl(f, &name, &d.doc, d.span, kind);
                self.compiled(f).actions.insert(name, d);
            }
        }
        for (name, mut d) in tasks {
            if let Some(f) = target(&d.span) {
                remap(&mut d.span);
                let kind = DeclKind::Task {
                    params: ast_fields(&d.params, d.span),
                    returns: d.returns.as_ref().map(|t| ast_type(t, d.span)),
                    errors: d.errors.iter().map(|e| ident(e, d.span)).collect(),
                };
                self.push_decl(f, &name, &d.doc, d.span, kind);
                self.compiled(f).tasks.insert(name, d);
            }
        }
        for (name, mut m) in machines {
            if let Some(f) = target(&m.span) {
                remap_machine(&mut m, &remap);
                let span = m.span;
                let mut items = Vec::new();
                for (e, d) in &m.events {
                    items.push(ast::Item::Event(ast_event(e, d)));
                }
                for (e, d) in &m.emits {
                    items.push(ast::Item::Emits(ast_event(e, d)));
                }
                let kind = DeclKind::Machine(ast::Machine {
                    params: ast_fields(&m.params, span),
                    returns: m.returns.as_ref().map(|t| ast_type(t, span)),
                    errors: m.errors.iter().map(|e| ident(e, span)).collect(),
                    body: ast::Body {
                        items,
                        span: ast_span(span),
                    },
                });
                self.push_decl(f, &name, &m.doc, span, kind);
                self.compiled(f).machines.insert(name, m);
            }
        }
        self.groups.push(map.clone());
        Ok(map[0])
    }

    fn compiled(&mut self, file: usize) -> &mut Compiled {
        &mut self.files[file]
            .compiled
            .as_mut()
            .expect("a file of a compiled IR")
            .1
    }

    fn push_decl(&mut self, file: usize, name: &str, doc: &str, span: ir::Span, kind: DeclKind) {
        self.files[file].ast.items.push(ast::TopItem::Decl(Decl {
            doc: (!doc.is_empty()).then(|| ast::Doc {
                text: doc.to_string(),
                span: ast_span(span),
            }),
            name: ident(name, span),
            kind,
            span: ast_span(span),
        }));
    }

    fn import(
        &mut self,
        loader: &mut dyn Loader,
        importer: &FileRef,
        spec: &str,
        span: Span,
    ) -> Option<usize> {
        let file = match loader.resolve(importer, spec) {
            Ok(p) => p,
            Err(reason) => {
                self.diagnostics.push(Diagnostic::error(
                    codes::IMPORT_NOT_FOUND,
                    span,
                    format!("cannot import `{spec}`: {reason}"),
                ));
                return None;
            }
        };
        if let Some(i) = self.known(loader, &file) {
            return Some(i);
        }
        let shown = display(loader, &file);
        let help = if file.package.is_some() && file.package != importer.package {
            "a path that does not start with `./`, `../` or `/` is a file in another package"
        } else {
            "import paths are relative to the importing file"
        };
        let failed = |reason: String| {
            Diagnostic::error(
                codes::IMPORT_NOT_FOUND,
                span,
                format!("cannot import `{spec}`: {reason}"),
            )
            .with_label(format!("resolved to `{shown}`"))
            .with_help(help)
        };
        match loader.load(&file) {
            Ok(loaded) => match self.add(loader, file, loaded) {
                Ok(i) => Some(i),
                Err(reason) => {
                    self.diagnostics.push(failed(reason));
                    None
                }
            },
            Err(reason) => {
                self.diagnostics.push(failed(reason));
                None
            }
        }
    }

    /// The index of `file` if it is loaded already, under this name or another one for the same
    /// location. A file keeps the name it was first reached by, in load order (the roots in
    /// order, each followed depth first by what it imports): that is how the IR's `sources`
    /// and diagnostics name it, and its own imports are resolved from it.
    fn known(&mut self, loader: &dyn Loader, file: &FileRef) -> Option<usize> {
        if let Some(&i) = self.by_path.get(file) {
            return Some(i);
        }
        let i = *self.by_location.get(&loader.location(file)?)?;
        self.by_path.insert(file.clone(), i);
        Some(i)
    }

    fn register(&mut self, loader: &dyn Loader, file: &FileRef, index: usize) {
        self.by_path.insert(file.clone(), index);
        if let Some(location) = loader.location(file) {
            self.by_location.entry(location).or_insert(index);
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

fn display(loader: &dyn Loader, file: &FileRef) -> String {
    match &file.package {
        None => file.path.clone(),
        Some(_) => loader.display(file),
    }
}

/// `path` relative to directory `dir`, both relative to the same root.
fn relative_dir(dir: &str, path: &str) -> String {
    let d: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
    let p: Vec<&str> = path.split('/').collect();
    let common = d.iter().zip(&p).take_while(|(a, b)| a == b).count();
    let mut out: Vec<&str> = vec![".."; d.len() - common];
    out.extend(&p[common..]);
    out.join("/")
}

/// A span of a file with no text: positions without offsets.
fn ast_span(s: ir::Span) -> Span {
    Span {
        file: FileId(s.file),
        start: Pos {
            offset: 0,
            line: s.line,
            col: s.col,
        },
        end: Pos {
            offset: 0,
            line: s.end_line,
            col: s.end_col,
        },
    }
}

fn ident(name: &str, span: ir::Span) -> ast::Ident {
    ast::Ident {
        name: name.to_string(),
        span: ast_span(span),
    }
}

fn ast_type(t: &ir::TypeRef, span: ir::Span) -> ast::TypeRef {
    let kind = match t {
        ir::TypeRef::Builtin(b) => {
            ast::TypeKind::Builtin(ast::Builtin::lookup(b.name()).unwrap_or(ast::Builtin::Json))
        }
        ir::TypeRef::Named(n) => ast::TypeKind::Named(ident(n, span)),
        ir::TypeRef::Ref(n) => ast::TypeKind::Ref(ident(n, span)),
        ir::TypeRef::List(e) => ast::TypeKind::List(Box::new(ast_type(e, span))),
        ir::TypeRef::Optional(e) => ast::TypeKind::Optional(Box::new(ast_type(e, span))),
    };
    ast::TypeRef {
        kind,
        span: ast_span(span),
    }
}

fn ast_fields(fields: &[ir::Field], span: ir::Span) -> Vec<ast::Field> {
    fields
        .iter()
        .map(|f| ast::Field {
            sensitive: f.sensitive,
            name: ident(&f.name, span),
            ty: ast_type(&f.ty, span),
            span: ast_span(span),
        })
        .collect()
}

fn ast_event(name: &str, e: &ir::EventDecl) -> ast::EventDecl {
    ast::EventDecl {
        doc: (!e.doc.is_empty()).then(|| ast::Doc {
            text: e.doc.clone(),
            span: ast_span(e.span),
        }),
        name: ident(name, e.span),
        fields: ast_fields(&e.fields, e.span),
        span: ast_span(e.span),
    }
}

/// Renumbers every span of a machine.
fn remap_machine(m: &mut ir::Machine, f: &impl Fn(&mut ir::Span)) {
    f(&mut m.span);
    for v in &mut m.vars {
        f(&mut v.span);
    }
    for e in m.events.values_mut().chain(m.emits.values_mut()) {
        f(&mut e.span);
    }
    remap_state(&mut m.root, f);
}

fn remap_state(s: &mut ir::State, f: &impl Fn(&mut ir::Span)) {
    f(&mut s.span);
    if let Some(inv) = &mut s.invoke {
        f(&mut inv.span);
    }
    for t in &mut s.transitions {
        f(&mut t.span);
    }
    for c in &mut s.states {
        remap_state(c, f);
    }
}
