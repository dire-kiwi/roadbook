//! Declarations: resolving their types, checking them, and lowering them to the IR. Machines
//! are lowered in `machine.rs`.

use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;
use roadbook_ir as ir;
use roadbook_syntax::ast::{self, DeclKind, Field, Ident};
use roadbook_syntax::{Diagnostic, Span};

use crate::codes;
use crate::index::{self, Symbol, SymbolId, SymbolIndex, SymbolKind};
use crate::machine::MachineInfo;
use crate::names::{Found, Names, is_type};
use crate::project::Project;
use crate::sensitive::Sensitive;
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

/// What other machines see of a machine: the events it accepts and the events it emits, with
/// their fields' types.
#[derive(Clone, Debug, Default)]
pub(crate) struct Interface {
    pub events: IndexMap<String, Vec<(String, Ty)>>,
    pub emits: IndexMap<String, Vec<(String, Ty)>>,
}

pub(crate) struct Checker<'p> {
    pub names: Names<'p>,
    pub env: TypeEnv,
    pub sigs: HashMap<String, Sig>,
    /// Every machine's interface, by name.
    pub interfaces: HashMap<String, Interface>,
    /// What is marked `sensitive`.
    pub sensitive: Sensitive,
    /// `(machine, event)` for every `on name.Event` of the compilation.
    pub received: HashSet<(String, String)>,
    pub diags: Vec<Diagnostic>,
    /// Names of the functions, actions, tasks and machines something uses.
    pub used: HashSet<String>,
    pub machines: Vec<MachineInfo>,
    /// Whether every declared type is resolved, so that aliases can be expanded.
    types_resolved: bool,
    /// The optional types `(e?, e, e resolved)` met before then, checked once it is.
    optionals: Vec<(ast::TypeRef, ast::TypeRef, Ty)>,
    /// Every name declared and used, built as names are resolved.
    pub index: SymbolIndex,
    /// The symbol of each declaration, by [`DeclId`](crate::names::DeclId).
    decl_symbols: Vec<SymbolId>,
    /// The members of a symbol by name: the fields of a record, error or event, the variants
    /// of an enum, and the parameters, variables, events and emitted events of a machine. The
    /// first declared wins, as it does for the compiler.
    members: HashMap<(SymbolId, String), SymbolId>,
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
            sensitive: f.sensitive,
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
            interfaces: HashMap::new(),
            sensitive: Sensitive::default(),
            received: HashSet::new(),
            diags,
            used: HashSet::new(),
            machines: Vec::new(),
            types_resolved: false,
            optionals: Vec::new(),
            index: SymbolIndex::default(),
            decl_symbols: Vec::new(),
            members: HashMap::new(),
        };
        c.sensitive = Sensitive::build(c.decls().into_iter().map(|(_, d)| d));
        c.index_declarations();
        c.resolve_types();
        c.types_resolved = true;
        for (t, e, inner) in std::mem::take(&mut c.optionals) {
            c.double_optional(&t, &e, &inner);
        }
        c.no_ref_in_fields();
        c.resolve_signatures();
        c.resolve_interfaces();
        c
    }

    /// Whether `file` is a file of a compiled IR, whose declarations were checked when it was
    /// compiled.
    pub fn is_compiled(&self, file: usize) -> bool {
        self.names.project.files[file].compiled.is_some()
    }

    /// Adds every declaration to the symbol index, with its members, and the names of import
    /// lists as uses.
    fn index_declarations(&mut self) {
        for i in 0..self.names.decls.len() {
            let d = self.names.decls[i].decl;
            let (kind, signature) = index::declaration(d);
            let id = self.define(kind, &d.name, d.span, None, &d.doc, signature);
            self.decl_symbols.push(id);
            match &d.kind {
                DeclKind::Record { fields } | DeclKind::Error { fields } => {
                    self.index_fields(id, fields);
                }
                DeclKind::Enum { variants } => {
                    for v in variants {
                        let signature = format!("{}.{}", d.name.name, v.name);
                        self.member(id, SymbolKind::Variant, v, v.span, &None, signature);
                    }
                }
                DeclKind::Machine(m) => {
                    for p in &m.params {
                        let signature = index::field_text(p);
                        self.member(id, SymbolKind::Parameter, &p.name, p.span, &None, signature);
                    }
                    for v in m.body.vars() {
                        let mut signature = format!(
                            "{}var {}",
                            if v.sensitive { "sensitive " } else { "" },
                            index::field_text_of(&v.name, &v.ty)
                        );
                        if let Some(init) = &v.init {
                            signature.push_str(&format!(" = {}", roadbook_syntax::expr_text(init)));
                        }
                        self.member(id, SymbolKind::Variable, &v.name, v.span, &v.doc, signature);
                    }
                    for item in &m.body.items {
                        let (kind, word, e) = match item {
                            ast::Item::Event(e) => (SymbolKind::Event, "event", e),
                            ast::Item::Emits(e) => (SymbolKind::EmittedEvent, "emits", e),
                            _ => continue,
                        };
                        let signature =
                            format!("{word} {}{}", e.name.name, index::braces(&e.fields));
                        let ev = self.member(id, kind, &e.name, e.span, &e.doc, signature);
                        self.index_fields(ev, &e.fields);
                    }
                }
                _ => {}
            }
        }
        for (span, id) in std::mem::take(&mut self.names.imported) {
            let symbol = self.decl_symbols[id.0];
            self.index.refer(symbol, span);
        }
    }

    fn index_fields(&mut self, owner: SymbolId, fields: &[Field]) {
        for f in fields {
            let signature = index::field_text(f);
            self.member(owner, SymbolKind::Field, &f.name, f.span, &None, signature);
        }
    }

    pub(crate) fn define(
        &mut self,
        kind: SymbolKind,
        name: &Ident,
        decl_span: Span,
        parent: Option<SymbolId>,
        doc: &Option<ast::Doc>,
        signature: String,
    ) -> SymbolId {
        self.index.define(Symbol {
            kind,
            name: name.name.clone(),
            span: name.span,
            decl_span,
            parent,
            doc: doc.as_ref().map(|d| d.text.clone()),
            signature,
        })
    }

    /// Defines a member of `owner`, findable by name with [`Checker::member_symbol`].
    pub(crate) fn member(
        &mut self,
        owner: SymbolId,
        kind: SymbolKind,
        name: &Ident,
        decl_span: Span,
        doc: &Option<ast::Doc>,
        signature: String,
    ) -> SymbolId {
        let id = self.define(kind, name, decl_span, Some(owner), doc, signature);
        self.members.entry((owner, name.name.clone())).or_insert(id);
        id
    }

    /// The member of `owner` named `name`, if it has one.
    pub(crate) fn member_symbol(&self, owner: SymbolId, name: &str) -> Option<SymbolId> {
        self.members.get(&(owner, name.to_string())).copied()
    }

    /// The symbol of the declaration named `name` in the compilation.
    pub(crate) fn decl_symbol(&self, name: &str) -> Option<SymbolId> {
        let id = self.names.global.get(name)?;
        Some(self.decl_symbols[id.0])
    }

    /// The symbol of the declaration `name` means in `file`, if it means one.
    pub(crate) fn lookup_symbol(&self, file: usize, name: &str) -> Option<SymbolId> {
        match self.names.lookup(file, name) {
            Found::Decl(id) => Some(self.decl_symbols[id.0]),
            _ => None,
        }
    }

    /// Records a use of the declaration `n` names in `file`, if it names one.
    pub(crate) fn refer_decl(&mut self, file: usize, n: &Ident) {
        if !self.is_compiled(file)
            && let Some(id) = self.lookup_symbol(file, &n.name)
        {
            self.index.refer(id, n.span);
        }
    }

    /// Records a use of the member `n` of `owner`, if it has one.
    pub(crate) fn refer_member(&mut self, file: usize, owner: Option<SymbolId>, n: &Ident) {
        if !self.is_compiled(file)
            && let Some(id) = owner.and_then(|o| self.member_symbol(o, &n.name))
        {
            self.index.refer(id, n.span);
        }
    }

    /// Reports a `ref` where none can be: in a field of a record, error or event, or in what a
    /// function, task or machine returns. A machine gets a handle only as a parameter or
    /// through `invoke … as`, so that the signatures show who can talk to whom.
    fn no_ref(&mut self, file: usize, t: &ast::TypeRef, ty: &Ty, what: &str) {
        if self.is_compiled(file) || !self.env.has_ref(ty) {
            return;
        }
        let message = if matches!(ty, Ty::Ref(_)) {
            format!("{what} cannot be `{ty}`")
        } else {
            format!("{what} cannot be `{ty}`: it holds a `ref`")
        };
        self.diags.push(
            Diagnostic::error(codes::REF_NOT_ALLOWED, t.span, message)
            .with_help("a machine gets a `ref` only as a parameter of its own, or through `invoke … as name`, so that who can talk to whom shows in the charts' signatures; pass it as a parameter instead"),
        );
    }

    fn no_ref_in_fields(&mut self) {
        for (file, d) in self.decls() {
            let (DeclKind::Record { fields } | DeclKind::Error { fields }) = &d.kind else {
                continue;
            };
            let resolved: Vec<Ty> = match self.env.defs.get(&d.name.name) {
                Some(TypeDef::Record(fs) | TypeDef::Error(fs)) => {
                    fs.iter().map(|(_, t)| t.clone()).collect()
                }
                _ => continue,
            };
            for (f, t) in fields.iter().zip(resolved) {
                let what = format!("field `{}` of `{}`", f.name.name, d.name.name);
                self.no_ref(file, &f.ty, &t, &what);
            }
        }
    }

    /// The events every machine accepts and emits. Their fields are resolved here, once; a
    /// name declared twice is reported with the machine and keeps its first declaration.
    fn resolve_interfaces(&mut self) {
        for (file, d) in self.decls() {
            let DeclKind::Machine(m) = &d.kind else {
                continue;
            };
            let mut iface = Interface::default();
            for (emits, e) in m
                .body
                .events()
                .map(|e| (false, e))
                .chain(m.body.emits().map(|e| (true, e)))
            {
                if iface.events.contains_key(&e.name.name) || iface.emits.contains_key(&e.name.name)
                {
                    continue;
                }
                self.unique(e.fields.iter().map(|f| &f.name), "field");
                let mut fields = Vec::new();
                for f in &e.fields {
                    let t = self.resolve(file, &f.ty);
                    let what = format!("field `{}` of event `{}`", f.name.name, e.name.name);
                    self.no_ref(file, &f.ty, &t, &what);
                    fields.push((f.name.name.clone(), t));
                }
                let map = if emits {
                    &mut iface.emits
                } else {
                    &mut iface.events
                };
                map.insert(e.name.name.clone(), fields);
            }
            self.interfaces.insert(d.name.name.clone(), iface);
        }
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
            ast::TypeKind::Named(n) => {
                self.refer_decl(file, n);
                self.named_type(file, n)
            }
            ast::TypeKind::Ref(n) => {
                self.refer_decl(file, n);
                let is_machine = |k: &DeclKind| matches!(k, DeclKind::Machine(_));
                match self.names.lookup(file, &n.name) {
                    Found::Decl(id) if is_machine(&self.names.decl(id).kind) => {
                        Ty::Ref(n.name.clone())
                    }
                    Found::Decl(id) => {
                        let d = self.names.wrong_kind(n, id, "a machine").with_help(
                            "`ref` names a machine: a handle to a running instance of it",
                        );
                        self.diags.push(d);
                        Ty::Unknown
                    }
                    Found::Poisoned => Ty::Unknown,
                    Found::Missing => {
                        let d =
                            self.names
                                .unknown(file, n, codes::UNKNOWN_TYPE, "machine", is_machine);
                        self.diags.push(d);
                        Ty::Unknown
                    }
                }
            }
        }
    }

    /// A declared type named in `file`.
    fn named_type(&mut self, file: usize, n: &Ident) -> Ty {
        match self.names.lookup(file, &n.name) {
            Found::Decl(id) if is_type(&self.names.decl(id).kind) => Ty::Named(n.name.clone()),
            Found::Decl(id) => {
                let mut d = self.names.wrong_kind(n, id, "a type");
                if matches!(self.names.decl(id).kind, DeclKind::Machine(_)) {
                    d = d
                        .with_help(format!(
                            "a handle to a running `{}` is written `ref {}`",
                            n.name, n.name
                        ))
                        .with_fix("write `ref`", n.span.shrink_to_start(), "ref ");
                }
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
        self.refer_decl(file, n);
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
            let returns = returns.map(|r| {
                let t = self.resolve(file, r);
                let what = format!("what `{}` returns", d.name.name);
                self.no_ref(file, r, &t, &what);
                t
            });
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
                    path: f.file.path.clone(),
                    package: f.file.package.clone(),
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
            if let Some((_, c)) = &project.files[file].compiled {
                // Checked when it was compiled: copied as it is.
                if let Some(t) = c.types.get(&name) {
                    out.types.insert(name, t.clone());
                } else if let Some(f) = c.functions.get(&name) {
                    out.functions.insert(name, f.clone());
                } else if let Some(a) = c.actions.get(&name) {
                    out.actions.insert(name, a.clone());
                } else if let Some(t) = c.tasks.get(&name) {
                    out.tasks.insert(name, t.clone());
                } else if let Some(m) = c.machines.get(&name) {
                    out.machines.insert(name, m.clone());
                }
                continue;
            }
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
