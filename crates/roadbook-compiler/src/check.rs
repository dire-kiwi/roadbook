//! Declarations: resolving their types, checking them, and lowering them to the IR. Machines
//! are lowered in `machine.rs`.

use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;
use roadbook_ir as ir;
use roadbook_syntax::ast::{self, DeclKind, Field, Ident};
use roadbook_syntax::{Diagnostic, Span};

use crate::codes;
use crate::machine::MachineInfo;
use crate::names::{Found, Names, is_type};
use crate::project::Project;
use crate::types::{Ty, TypeDef, TypeEnv, builtin, ir_type};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SigKind {
    Fn,
    Action,
    Task,
    Machine,
}

/// The signature of a function, action, task or machine, with resolved types.
#[derive(Clone, Debug)]
pub(crate) struct Sig {
    pub params: Vec<(String, Ty)>,
    pub returns: Option<Ty>,
    pub errors: Vec<String>,
    /// The declaration's name, for "declared here" labels.
    pub name_span: Span,
}

impl Sig {
    /// `-> Receipt ! CardDeclined | GatewayUnavailable`, as declared.
    pub fn result_text(&self) -> String {
        let mut parts = Vec::new();
        if let Some(r) = &self.returns {
            parts.push(format!("-> {r}"));
        }
        if !self.errors.is_empty() {
            parts.push(format!("! {}", self.errors.join(" | ")));
        }
        parts.join(" ")
    }
}

pub(crate) struct Checker<'p> {
    pub names: Names<'p>,
    pub env: TypeEnv,
    pub sigs: HashMap<String, Sig>,
    pub diags: Vec<Diagnostic>,
    /// Names of the functions, actions, tasks and machines something uses.
    pub used: HashSet<String>,
    pub machines: Vec<MachineInfo>,
    /// Whether every declared type is resolved, so that aliases can be expanded.
    types_resolved: bool,
    /// The optional types `(e?, e, e resolved)` met before then, checked once it is.
    optionals: Vec<(ast::TypeRef, ast::TypeRef, Ty)>,
}

pub(crate) fn ir_span(s: Span) -> ir::Span {
    ir::Span {
        file: s.file.0,
        line: s.start.line,
        col: s.start.col,
        end_line: s.end.line,
        end_col: s.end.col,
    }
}

pub(crate) fn doc(d: &Option<ast::Doc>) -> String {
    d.as_ref().map(|d| d.text.clone()).unwrap_or_default()
}

fn base(t: &Ty) -> &Ty {
    match t {
        Ty::List(e) | Ty::Optional(e) => base(e),
        t => t,
    }
}

fn ir_fields(fields: &[Field]) -> Vec<ir::Field> {
    fields
        .iter()
        .map(|f| ir::Field {
            name: f.name.name.clone(),
            ty: ir_type(&f.ty),
            doc: String::new(),
        })
        .collect()
}

impl<'p> Checker<'p> {
    pub fn new(project: &'p Project) -> Checker<'p> {
        let mut diags = Vec::new();
        let names = Names::build(project, &mut diags);
        let mut c = Checker {
            names,
            env: TypeEnv::default(),
            sigs: HashMap::new(),
            diags,
            used: HashSet::new(),
            machines: Vec::new(),
            types_resolved: false,
            optionals: Vec::new(),
        };
        c.resolve_types();
        c.types_resolved = true;
        for (t, e, inner) in std::mem::take(&mut c.optionals) {
            c.double_optional(&t, &e, &inner);
        }
        c.resolve_signatures();
        c
    }

    /// The non-duplicate declarations, in load order, with their file.
    fn decls(&self) -> Vec<(usize, &'p ast::Decl)> {
        self.names
            .decls
            .iter()
            .filter(|e| !e.duplicate)
            .map(|e| (e.file, e.decl))
            .collect()
    }

    /// A type as written in `file`, resolved.
    pub fn resolve(&mut self, file: usize, t: &ast::TypeRef) -> Ty {
        match &t.kind {
            ast::TypeKind::Builtin(b) => Ty::Builtin(builtin(*b)),
            ast::TypeKind::List(e) => Ty::List(Box::new(self.resolve(file, e))),
            ast::TypeKind::Optional(e) => {
                let inner = self.resolve(file, e);
                if self.types_resolved {
                    if self.double_optional(t, e, &inner) {
                        return inner;
                    }
                } else {
                    // An alias it names may not be resolved yet.
                    self.optionals
                        .push((t.clone(), (**e).clone(), inner.clone()));
                }
                Ty::Optional(Box::new(inner))
            }
            ast::TypeKind::Named(n) => match self.names.lookup(file, &n.name) {
                Found::Decl(id) if is_type(&self.names.decl(id).kind) => Ty::Named(n.name.clone()),
                Found::Decl(id) => {
                    let d = self.names.wrong_kind(n, id, "a type");
                    self.diags.push(d);
                    Ty::Unknown
                }
                Found::Poisoned => Ty::Unknown,
                Found::Missing => {
                    let d = self
                        .names
                        .unknown(file, n, codes::UNKNOWN_TYPE, "type", is_type);
                    self.diags.push(d);
                    Ty::Unknown
                }
            },
        }
    }

    /// Reports `t`, written `e?`, if `e` (resolved as `inner`, through aliases) is already
    /// optional: `string??`, or `S?` with `type S = string?`.
    fn double_optional(&mut self, t: &ast::TypeRef, e: &ast::TypeRef, inner: &Ty) -> bool {
        if !matches!(self.env.expand(inner), Ty::Optional(_)) {
            return false;
        }
        let message = if matches!(e.kind, ast::TypeKind::Optional(_)) {
            format!("`{}` is optional twice", ir_type(t))
        } else {
            format!(
                "`{}` is optional twice: `{}` is `{}`",
                ir_type(t),
                ir_type(e),
                self.env.expand(inner)
            )
        };
        self.diags.push(
            Diagnostic::error(codes::DOUBLE_OPTIONAL, t.span, message)
                .with_help("a value is either present or absent; one `?` says so")
                .with_fix("remove the last `?`", t.span, ir_type(e).to_string()),
        );
        true
    }

    /// An entry of an `errors` list (`! A | B`), which must name an error type.
    pub fn resolve_error(&mut self, file: usize, n: &Ident) -> bool {
        let is_error = |k: &DeclKind| matches!(k, DeclKind::Error { .. });
        match self.names.lookup(file, &n.name) {
            Found::Decl(id) if is_error(&self.names.decl(id).kind) => true,
            Found::Decl(id) => {
                let d = self.names.wrong_kind(n, id, "an error");
                self.diags.push(d);
                false
            }
            Found::Poisoned => false,
            Found::Missing => {
                let d = self
                    .names
                    .unknown(file, n, codes::UNKNOWN_TYPE, "error", is_error);
                self.diags.push(d);
                false
            }
        }
    }

    /// Reports names listed twice. `what` is "field", "parameter", "variant".
    pub fn unique<'a>(&mut self, idents: impl Iterator<Item = &'a Ident>, what: &str) {
        let mut seen: HashMap<&str, Span> = HashMap::new();
        for i in idents {
            if let Some(first) = seen.get(i.name.as_str()) {
                self.diags.push(
                    Diagnostic::error(
                        codes::DUPLICATE_NAME,
                        i.span,
                        format!("{what} `{}` is declared twice", i.name),
                    )
                    .with_secondary(*first, "first declared here"),
                );
            } else {
                seen.insert(&i.name, i.span);
            }
        }
    }

    fn fields(&mut self, file: usize, fields: &[Field], what: &str) -> Vec<(String, Ty)> {
        self.unique(fields.iter().map(|f| &f.name), what);
        fields
            .iter()
            .map(|f| (f.name.name.clone(), self.resolve(file, &f.ty)))
            .collect()
    }

    fn resolve_types(&mut self) {
        for (file, d) in self.decls() {
            let def = match &d.kind {
                DeclKind::Record { fields } => TypeDef::Record(self.fields(file, fields, "field")),
                DeclKind::Error { fields } => TypeDef::Error(self.fields(file, fields, "field")),
                DeclKind::Alias { target } => TypeDef::Alias(self.resolve(file, target)),
                DeclKind::Enum { variants } => {
                    self.unique(variants.iter(), "variant");
                    TypeDef::Enum(variants.iter().map(|v| v.name.clone()).collect())
                }
                DeclKind::Extern => TypeDef::Extern,
                _ => continue,
            };
            self.env.defs.insert(d.name.name.clone(), def);
        }
        // Break alias cycles, reporting each once, at the alias declared first.
        let aliases: Vec<(usize, &'p ast::Decl)> = self
            .decls()
            .into_iter()
            .filter(|(_, d)| matches!(d.kind, DeclKind::Alias { .. }))
            .collect();
        let mut broken: HashSet<String> = HashSet::new();
        for (_, d) in aliases {
            let start = d.name.name.clone();
            if broken.contains(&start) {
                continue;
            }
            let mut chain = vec![start.clone()];
            let mut cur = start.clone();
            // An alias is transparent, so one that refers to itself through `[]` or `?` would
            // be an infinite type.
            while let Some(TypeDef::Alias(t)) = self.env.defs.get(&cur) {
                let Ty::Named(next) = base(t) else {
                    break;
                };
                let next = next.clone();
                if next == start {
                    let path = chain
                        .iter()
                        .chain(std::iter::once(&start))
                        .map(|n| format!("`{n}`"))
                        .collect::<Vec<_>>()
                        .join(" = ");
                    self.diags.push(
                        Diagnostic::error(
                            codes::ALIAS_CYCLE,
                            d.name.span,
                            format!("type alias `{start}` refers to itself: {path}"),
                        )
                        .with_help("an alias must end in a built-in or a declared record, enum, error or extern type"),
                    );
                    for n in &chain {
                        broken.insert(n.clone());
                        self.env.defs.insert(n.clone(), TypeDef::Alias(Ty::Unknown));
                    }
                    break;
                }
                if chain.contains(&next) {
                    break; // a cycle not through `start`: reported from its own members
                }
                chain.push(next.clone());
                cur = next;
            }
        }
    }

    fn resolve_signatures(&mut self) {
        for (file, d) in self.decls() {
            let (params, returns, errors): (&[Field], _, &[Ident]) = match &d.kind {
                DeclKind::Fn { params, returns } => (params, Some(returns), &[]),
                DeclKind::Action { params } => (params, None, &[]),
                DeclKind::Task {
                    params,
                    returns,
                    errors,
                } => (params, returns.as_ref(), errors),
                DeclKind::Machine(m) => (&m.params, m.returns.as_ref(), &m.errors),
                _ => continue,
            };
            let params = self.fields(file, params, "parameter");
            let returns = returns.map(|r| self.resolve(file, r));
            self.unique(errors.iter(), "error");
            let mut errs = Vec::new();
            for e in errors {
                self.resolve_error(file, e);
                if !errs.contains(&e.name) {
                    errs.push(e.name.clone());
                }
            }
            self.sigs.insert(
                d.name.name.clone(),
                Sig {
                    params,
                    returns,
                    errors: errs,
                    name_span: d.name.span,
                },
            );
        }
    }

    /// Checks every machine and lowers the compilation to an IR document. The document is
    /// only meaningful if no errors were reported.
    pub fn lower(&mut self, project: &Project) -> ir::Ir {
        let mut out = ir::Ir {
            ir_version: ir::IR_VERSION.to_string(),
            sources: project
                .files
                .iter()
                .map(|f| ir::Source {
                    path: f.path.clone(),
                })
                .collect(),
            types: IndexMap::new(),
            functions: IndexMap::new(),
            actions: IndexMap::new(),
            tasks: IndexMap::new(),
            machines: IndexMap::new(),
        };
        for (file, d) in self.decls() {
            let name = d.name.name.clone();
            let (doc, span) = (doc(&d.doc), ir_span(d.span));
            let def = match &d.kind {
                DeclKind::Record { fields } => Some(ir::TypeDef::Record {
                    fields: ir_fields(fields),
                }),
                DeclKind::Error { fields } => Some(ir::TypeDef::Error {
                    fields: ir_fields(fields),
                }),
                DeclKind::Alias { target } => Some(ir::TypeDef::Alias {
                    ty: ir_type(target),
                }),
                DeclKind::Enum { variants } => Some(ir::TypeDef::Enum {
                    variants: variants.iter().map(|v| v.name.clone()).collect(),
                }),
                DeclKind::Extern => Some(ir::TypeDef::Extern),
                _ => None,
            };
            if let Some(def) = def {
                out.types.insert(name, ir::TypeDecl { def, doc, span });
                continue;
            }
            match &d.kind {
                DeclKind::Fn { params, returns } => {
                    out.functions.insert(
                        name,
                        ir::FunctionDecl {
                            params: ir_fields(params),
                            returns: ir_type(returns),
                            doc,
                            span,
                        },
                    );
                }
                DeclKind::Action { params } => {
                    out.actions.insert(
                        name,
                        ir::ActionDecl {
                            params: ir_fields(params),
                            doc,
                            span,
                        },
                    );
                }
                DeclKind::Task {
                    params,
                    returns,
                    errors,
                } => {
                    out.tasks.insert(
                        name,
                        ir::TaskDecl {
                            params: ir_fields(params),
                            returns: returns.as_ref().map(ir_type),
                            errors: errors.iter().map(|e| e.name.clone()).collect(),
                            doc,
                            span,
                        },
                    );
                }
                DeclKind::Machine(m) => {
                    let machine = crate::machine::lower(self, file, d, m);
                    out.machines.insert(name, machine);
                }
                _ => {}
            }
        }
        out
    }
}
