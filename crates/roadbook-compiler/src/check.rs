//! Declarations: resolving their types, checking them, and lowering them to the IR. Machines
//! are lowered in `machine.rs`.

use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;
use roadbook_ir as ir;
use roadbook_syntax::ast::{self, DeclKind, Field, Ident};
use roadbook_syntax::{Diagnostic, Span};

use crate::checks::Check;
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
/// their fields' types, and the calls it answers.
#[derive(Clone, Debug, Default)]
pub(crate) struct Interface {
    pub events: IndexMap<String, Vec<(String, Ty)>>,
    pub emits: IndexMap<String, Vec<(String, Ty)>>,
    pub calls: IndexMap<String, Sig>,
}

/// The built-in error every call can fail with besides its declared ones.
pub(crate) const UNANSWERED: &str = roadbook_ir::UNANSWERED;

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
    /// The `check` lines of every machine lowered, resolved. Not in the IR.
    pub(crate) checks: Vec<Check>,
    /// Whether every declared type is resolved, so that aliases can be expanded.
    types_resolved: bool,
    /// The checks of types met before then that need aliases expanded, made once it is.
    deferred: Vec<Deferred>,
    /// While a generic declaration's members are resolved: its symbol and its parameters,
    /// which its fields or target may name.
    type_params: Option<(SymbolId, Vec<String>)>,
    /// For each generic type, which of its parameters it makes optional, once worked out.
    optional_params: Option<HashMap<String, Vec<bool>>>,
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

/// A check of a type as written that needs every alias resolved.
enum Deferred {
    /// `e?`, with `e` resolved: not optional already.
    Optional(ast::TypeRef, ast::TypeRef, Ty),
    /// The key type of a `map`: `string`, through aliases.
    MapKey(Span, Ty),
    /// An instance of a generic type: none of its members optional twice.
    Instance(Span, Ty),
}

/// The type parameters of a declaration, as written: empty unless it is a generic record or
/// alias.
pub(crate) fn ast_params(kind: &DeclKind) -> &[Ident] {
    match kind {
        DeclKind::Record { params, .. } | DeclKind::Alias { params, .. } => params,
        _ => &[],
    }
}

/// Every name a type as written mentions.
fn names_written<'a>(t: &'a ast::TypeRef, out: &mut HashSet<&'a str>) {
    match &t.kind {
        ast::TypeKind::Named(n) | ast::TypeKind::Ref(n) => {
            out.insert(&n.name);
        }
        ast::TypeKind::Generic(n, args) => {
            out.insert(&n.name);
            for a in args {
                names_written(a, out);
            }
        }
        ast::TypeKind::Map(k, v) => {
            names_written(k, out);
            names_written(v, out);
        }
        ast::TypeKind::List(e) | ast::TypeKind::Optional(e) => names_written(e, out),
        ast::TypeKind::Builtin(_) | ast::TypeKind::Call(_) => {}
    }
}

/// Every `Name<…>` in a type as written, outermost first.
fn generic_uses<'a>(t: &'a ast::TypeRef, out: &mut Vec<&'a ast::TypeRef>) {
    match &t.kind {
        ast::TypeKind::Generic(_, args) => {
            out.push(t);
            for a in args {
                generic_uses(a, out);
            }
        }
        ast::TypeKind::Map(k, v) => {
            generic_uses(k, out);
            generic_uses(v, out);
        }
        ast::TypeKind::List(e) | ast::TypeKind::Optional(e) => generic_uses(e, out),
        _ => {}
    }
}

/// Every declared name a type mentions (type parameters excepted), outermost first.
fn names_in(t: &Ty, out: &mut Vec<String>) {
    t.walk(&mut |t| {
        if let Ty::Named(n) | Ty::Generic(n, _) = t {
            out.push(n.clone());
        }
    });
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
            checks: Vec::new(),
            types_resolved: false,
            deferred: Vec::new(),
            type_params: None,
            optional_params: None,
            index: SymbolIndex::default(),
            decl_symbols: Vec::new(),
            members: HashMap::new(),
        };
        c.sensitive = Sensitive::build(c.decls().into_iter().map(|(_, d)| d));
        c.index_declarations();
        c.resolve_types();
        c.types_resolved = true;
        for d in std::mem::take(&mut c.deferred) {
            c.deferred_check(d);
        }
        c.check_generics();
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
            for p in ast_params(&d.kind) {
                let signature = format!("{}<{}>", d.name.name, p.name);
                self.member(id, SymbolKind::TypeParameter, p, p.span, &None, signature);
            }
            match &d.kind {
                DeclKind::Record { fields, .. } | DeclKind::Error { fields } => {
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
                    for c in m.body.children() {
                        let signature = format!(
                            "children {}: {} by {}",
                            c.name.name,
                            c.machine.name,
                            index::type_text(&c.key)
                        );
                        self.member(id, SymbolKind::Children, &c.name, c.span, &c.doc, signature);
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
                    for c in m.body.calls() {
                        let signature = format!(
                            "call {}({}){}",
                            c.name.name,
                            index::list(&c.params),
                            index::result_text(c.returns.as_ref(), &c.errors)
                        );
                        let cid = self.define(
                            SymbolKind::Call,
                            &c.name,
                            c.span,
                            Some(id),
                            &c.doc,
                            signature,
                        );
                        // Calls have names of their own, apart from the parameters and variables.
                        self.members
                            .entry((id, format!("call {}", c.name.name)))
                            .or_insert(cid);
                        for p in &c.params {
                            let signature = index::field_text(p);
                            self.member(
                                cid,
                                SymbolKind::Parameter,
                                &p.name,
                                p.span,
                                &None,
                                signature,
                            );
                        }
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

    /// The call `name` of the machine whose symbol is `machine`, if it has one.
    pub(crate) fn call_symbol(&self, machine: Option<SymbolId>, name: &str) -> Option<SymbolId> {
        self.members
            .get(&(machine?, format!("call {name}")))
            .copied()
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
            let (DeclKind::Record { fields, .. } | DeclKind::Error { fields }) = &d.kind else {
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
            self.unique(m.body.calls().map(|c| &c.name), "call");
            for c in m.body.calls() {
                if iface.calls.contains_key(&c.name.name) {
                    continue;
                }
                let sig = self.signature(file, &c.name, &c.params, c.returns.as_ref(), &c.errors);
                // A call's arguments arrive like an event's fields, so they cannot carry a
                // `ref` either: the runs a machine holds are fixed when it starts.
                for (p, (_, t)) in c.params.iter().zip(&sig.params) {
                    let what = format!("parameter `{}` of call `{}`", p.name.name, c.name.name);
                    self.no_ref(file, &p.ty, t, &what);
                }
                iface.calls.insert(c.name.name.clone(), sig);
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
                    self.deferred
                        .push(Deferred::Optional(t.clone(), (**e).clone(), inner.clone()));
                }
                Ty::Optional(Box::new(inner))
            }
            ast::TypeKind::Named(n) => {
                if let Some(p) = self.type_param(file, n) {
                    return p;
                }
                self.refer_decl(file, n);
                let ty = self.named_type(file, n);
                if let Ty::Named(name) = &ty
                    && let Some(id) = self.names.global.get(name.as_str()).copied()
                {
                    let params = ast_params(&self.names.decl(id).kind);
                    if !params.is_empty() {
                        let list: Vec<&str> = params.iter().map(|p| p.name.as_str()).collect();
                        self.diags.push(
                            Diagnostic::error(
                                codes::TYPE_ARGUMENT_COUNT,
                                n.span,
                                format!(
                                    "`{}` is generic: it takes {} type argument{}",
                                    n.name,
                                    params.len(),
                                    if params.len() == 1 { "" } else { "s" }
                                ),
                            )
                            .with_help(format!(
                                "write `{}<{}>` with a type for each",
                                n.name,
                                list.join(", ")
                            )),
                        );
                        return Ty::Unknown;
                    }
                }
                ty
            }
            ast::TypeKind::Generic(n, args) => {
                if self.type_param(file, n).is_some() {
                    self.diags.push(Diagnostic::error(
                        codes::TYPE_ARGUMENT_COUNT,
                        n.span,
                        format!("type parameter `{}` takes no type arguments", n.name),
                    ));
                    return Ty::Unknown;
                }
                self.refer_decl(file, n);
                let ty = self.named_type(file, n);
                let resolved: Vec<Ty> = args.iter().map(|a| self.resolve(file, a)).collect();
                let Ty::Named(name) = ty else {
                    return Ty::Unknown;
                };
                let Some(id) = self.names.global.get(name.as_str()).copied() else {
                    return Ty::Unknown;
                };
                let params = ast_params(&self.names.decl(id).kind);
                if params.len() != args.len() {
                    let message = if params.is_empty() {
                        format!("`{name}` is not generic: it takes no type arguments")
                    } else {
                        format!(
                            "`{name}` takes {} type argument{}, not {}",
                            params.len(),
                            if params.len() == 1 { "" } else { "s" },
                            args.len()
                        )
                    };
                    let mut d = Diagnostic::error(codes::TYPE_ARGUMENT_COUNT, t.span, message);
                    if params.is_empty() {
                        d = d.with_fix(format!("write `{name}`"), t.span, name.clone());
                    }
                    self.diags.push(d);
                    return Ty::Unknown;
                }
                let inst = Ty::Generic(name, resolved);
                if self.is_compiled(file) {
                    // Checked when it was compiled.
                } else if self.types_resolved {
                    self.check_instance(t.span, &inst);
                } else {
                    self.deferred.push(Deferred::Instance(t.span, inst.clone()));
                }
                inst
            }
            ast::TypeKind::Map(k, v) => {
                let key = self.resolve(file, k);
                let value = self.resolve(file, v);
                if self.is_compiled(file) {
                    // Checked when it was compiled.
                } else if self.types_resolved {
                    self.check_map_key(k.span, &key);
                } else {
                    self.deferred.push(Deferred::MapKey(k.span, key.clone()));
                }
                Ty::Map(Box::new(key), Box::new(value))
            }
            ast::TypeKind::Call(n) => {
                // A machine variable's own type is resolved by the machine, which knows its
                // calls; anywhere else, this is reached.
                if !self.is_compiled(file) {
                    self.diags.push(call_type_not_allowed(t, n));
                }
                Ty::Unknown
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

    /// The type parameter `n` names, inside the declaration of a generic type that has it.
    fn type_param(&mut self, file: usize, n: &Ident) -> Option<Ty> {
        let (owner, params) = self.type_params.as_ref()?;
        if !params.contains(&n.name) {
            return None;
        }
        let owner = *owner;
        self.refer_member(file, Some(owner), n);
        Some(Ty::Param(n.name.clone()))
    }

    fn deferred_check(&mut self, d: Deferred) {
        match d {
            Deferred::Optional(t, e, inner) => {
                self.double_optional(&t, &e, &inner);
            }
            Deferred::MapKey(span, key) => self.check_map_key(span, &key),
            Deferred::Instance(span, inst) => self.check_instance(span, &inst),
        }
    }

    /// Reports a map key type that is not `string`, through aliases.
    fn check_map_key(&mut self, span: Span, key: &Ty) {
        if matches!(
            self.env.expand(key),
            Ty::Builtin(ir::Builtin::String) | Ty::Unknown
        ) {
            return;
        }
        self.diags.push(
            Diagnostic::error(
                codes::MAP_KEY,
                span,
                format!("a map's keys are strings: `{key}` is not `string` or an alias of it"),
            )
            .with_help("a map is a JSON object, whose keys are strings; write `map<string, …>`"),
        );
    }

    /// Checks an instance of a generic type written at `span` (or inferred for a record value
    /// there): no argument holds a `ref`, and none is optional where the type makes its
    /// parameter optional, which would make a member optional twice.
    pub(crate) fn check_instance(&mut self, span: Span, inst: &Ty) {
        let Ty::Generic(name, args) = inst else {
            return;
        };
        if let Some(a) = args.iter().find(|a| self.env.has_ref(a)) {
            self.diags.push(
                Diagnostic::error(
                    codes::REF_NOT_ALLOWED,
                    span,
                    format!("a type argument cannot be `{a}`: it holds a `ref`"),
                )
                .with_help(format!("the type arguments of `{name}` become its fields, which cannot hold a `ref`; pass the handle as a parameter instead")),
            );
            return;
        }
        let optional = self
            .optional_params()
            .get(name)
            .cloned()
            .unwrap_or_default();
        let params = self.env.params_of(name).to_vec();
        for ((a, p), opt) in args.iter().zip(&params).zip(optional) {
            if opt && matches!(self.env.expand(a), Ty::Optional(_)) {
                self.diags.push(
                    Diagnostic::error(
                        codes::DOUBLE_OPTIONAL,
                        span,
                        format!(
                            "`{inst}` makes a type optional twice: `{name}` makes its parameter `{p}` optional, and `{a}` already is"
                        ),
                    )
                    .with_help("a value is either present or absent; pass a type argument that is not optional"),
                );
                return;
            }
        }
    }

    /// For each generic type, which of its parameters it makes optional: a parameter written
    /// `T?` in its fields or aliased type, or passed where another generic type makes its
    /// parameter optional, or passed transparently (`type Id<T> = T`) under a `?`. Worked out
    /// once per declaration, to a fixed point, so that checking an instance never expands it.
    fn optional_params(&mut self) -> &HashMap<String, Vec<bool>> {
        if self.optional_params.is_none() {
            let mut opt: HashMap<String, Vec<bool>> = HashMap::new();
            // Whether an alias is, at the top, its parameter: `type Id<T> = T`.
            let mut transparent: HashMap<String, Vec<bool>> = HashMap::new();
            for (n, ps) in &self.env.params {
                opt.insert(n.clone(), vec![false; ps.len()]);
                transparent.insert(n.clone(), vec![false; ps.len()]);
            }
            fn walk(
                t: &Ty,
                under: bool,
                params: &[String],
                opt: &HashMap<String, Vec<bool>>,
                transparent: &HashMap<String, Vec<bool>>,
                out: &mut [bool],
            ) {
                match t {
                    Ty::Param(p) => {
                        if under && let Some(i) = params.iter().position(|x| x == p) {
                            out[i] = true;
                        }
                    }
                    Ty::Optional(e) => walk(e, true, params, opt, transparent, out),
                    Ty::List(e) => walk(e, false, params, opt, transparent, out),
                    Ty::Map(k, v) => {
                        walk(k, false, params, opt, transparent, out);
                        walk(v, false, params, opt, transparent, out);
                    }
                    Ty::Generic(h, args) => {
                        for (j, a) in args.iter().enumerate() {
                            let made = opt.get(h).and_then(|o| o.get(j)).copied().unwrap_or(false);
                            let through = transparent
                                .get(h)
                                .and_then(|o| o.get(j))
                                .copied()
                                .unwrap_or(false);
                            walk(a, made || (under && through), params, opt, transparent, out);
                        }
                    }
                    _ => {}
                }
            }
            loop {
                let mut changed = false;
                for (n, ps) in &self.env.params {
                    let members: Vec<&Ty> = match self.env.defs.get(n) {
                        Some(TypeDef::Record(fs)) => fs.iter().map(|(_, t)| t).collect(),
                        Some(TypeDef::Alias(t)) => vec![t],
                        _ => continue,
                    };
                    let mut now = vec![false; ps.len()];
                    for m in &members {
                        walk(m, false, ps, &opt, &transparent, &mut now);
                    }
                    let mut through = vec![false; ps.len()];
                    if let Some(TypeDef::Alias(t)) = self.env.defs.get(n) {
                        match t {
                            Ty::Param(p) => {
                                if let Some(i) = ps.iter().position(|x| x == p) {
                                    through[i] = true;
                                }
                            }
                            Ty::Generic(h, args) => {
                                for (j, a) in args.iter().enumerate() {
                                    if let Ty::Param(p) = a
                                        && transparent.get(h).and_then(|o| o.get(j)) == Some(&true)
                                        && let Some(i) = ps.iter().position(|x| x == p)
                                    {
                                        through[i] = true;
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    for (old, new) in [(opt.get_mut(n), now), (transparent.get_mut(n), through)] {
                        if let Some(old) = old {
                            for (o, x) in old.iter_mut().zip(new) {
                                if x && !*o {
                                    *o = true;
                                    changed = true;
                                }
                            }
                        }
                    }
                }
                if !changed {
                    break;
                }
            }
            self.optional_params = Some(opt);
        }
        self.optional_params.as_ref().expect("just computed")
    }

    /// The checks of generic declarations, once every type is resolved: every parameter is
    /// used, and a generic type refers to itself, directly or through other types, only with
    /// its own parameters, so that it has finitely many instances.
    fn check_generics(&mut self) {
        // The types each type's definition names.
        let mut edges: HashMap<String, Vec<String>> = HashMap::new();
        for (name, def) in &self.env.defs {
            let mut out = Vec::new();
            match def {
                TypeDef::Record(fs) | TypeDef::Error(fs) => {
                    for (_, t) in fs {
                        names_in(t, &mut out);
                    }
                }
                TypeDef::Alias(t) => names_in(t, &mut out),
                TypeDef::Enum(_) | TypeDef::Extern => {}
            }
            edges.insert(name.clone(), out);
        }
        let reaches = |from: &str, to: &str| -> bool {
            let mut seen: HashSet<&str> = HashSet::new();
            let mut stack = vec![from];
            while let Some(n) = stack.pop() {
                if n == to {
                    return true;
                }
                if seen.insert(n) {
                    stack.extend(edges.get(n).into_iter().flatten().map(String::as_str));
                }
            }
            false
        };
        for (file, d) in self.decls() {
            let params = ast_params(&d.kind);
            if params.is_empty() || self.is_compiled(file) {
                continue;
            }
            // Uses as written, so that a use the checker refused still counts.
            let mut used: HashSet<&str> = HashSet::new();
            match &d.kind {
                DeclKind::Record { fields, .. } => {
                    for f in fields {
                        names_written(&f.ty, &mut used);
                    }
                }
                DeclKind::Alias { target, .. } => names_written(target, &mut used),
                _ => {}
            }
            for p in params {
                if !used.contains(p.name.as_str()) {
                    self.diags.push(
                        Diagnostic::error(
                            codes::UNUSED_TYPE_PARAMETER,
                            p.span,
                            format!(
                                "type parameter `{}` of `{}` is not used",
                                p.name, d.name.name
                            ),
                        )
                        .with_help("use it in a field or the aliased type, or remove it"),
                    );
                }
            }
            let mut uses: Vec<&ast::TypeRef> = Vec::new();
            match &d.kind {
                DeclKind::Record { fields, .. } => {
                    for f in fields {
                        generic_uses(&f.ty, &mut uses);
                    }
                }
                DeclKind::Alias { target, .. } => generic_uses(target, &mut uses),
                _ => {}
            }
            for u in uses {
                let ast::TypeKind::Generic(n, args) = &u.kind else {
                    continue;
                };
                if !reaches(&n.name, &d.name.name) {
                    continue;
                }
                let own = args.len() == params.len()
                    && args.iter().zip(params).all(
                        |(a, p)| matches!(&a.kind, ast::TypeKind::Named(x) if x.name == p.name),
                    );
                if !own {
                    let list: Vec<&str> = params.iter().map(|p| p.name.as_str()).collect();
                    self.diags.push(
                        Diagnostic::error(
                            codes::IRREGULAR_RECURSION,
                            u.span,
                            format!(
                                "`{}` refers back to `{}` with other type arguments than its parameters `<{}>`",
                                ir_type(u),
                                d.name.name,
                                list.join(", ")
                            ),
                        )
                        .with_help("a generic type can refer to itself, directly or through other types, only with its own parameters, in order, so that it has finitely many instances"),
                    );
                }
            }
        }
    }

    /// A declared type named in `file`.
    fn named_type(&mut self, file: usize, n: &Ident) -> Ty {
        let found = self.names.lookup(file, &n.name);
        if n.name == UNANSWERED && matches!(found, Found::Missing) {
            self.diags.push(unanswered_misused(
                n.span,
                "`Unanswered` is not a type: it is the built-in error of a call that is not answered",
            ));
            return Ty::Unknown;
        }
        match found {
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
        let found = self.names.lookup(file, &n.name);
        if n.name == UNANSWERED && matches!(found, Found::Missing) {
            self.diags.push(unanswered_misused(
                n.span,
                "`Unanswered` cannot be declared: every call can fail with it, besides its declared errors",
            ));
            return false;
        }
        match found {
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
            let params = ast_params(&d.kind);
            if !params.is_empty() {
                self.unique(params.iter(), "type parameter");
                for p in params {
                    if self.is_compiled(file) {
                        break;
                    }
                    if let Some(&id) = self.names.global.get(p.name.as_str()) {
                        let other = self.names.decl(id);
                        self.diags.push(
                            Diagnostic::error(
                                codes::DUPLICATE_DECLARATION,
                                p.span,
                                format!(
                                    "type parameter `{}` has the name of {} `{}`",
                                    p.name,
                                    crate::names::kind_phrase(&other.kind),
                                    other.name.name
                                ),
                            )
                            .with_secondary(other.name.span, "declared here")
                            .with_help("a type parameter cannot have the name of a declaration; `T`, `U` and so on are usual"),
                        );
                    }
                }
                let names: Vec<String> = params.iter().map(|p| p.name.clone()).collect();
                self.env.params.insert(d.name.name.clone(), names.clone());
                let symbol = self.decl_symbol(&d.name.name);
                self.type_params = symbol.map(|s| (s, names));
            }
            let def = match &d.kind {
                DeclKind::Record { fields, .. } => {
                    TypeDef::Record(self.fields(file, fields, "field"))
                }
                DeclKind::Error { fields } => TypeDef::Error(self.fields(file, fields, "field")),
                DeclKind::Alias { target, .. } => TypeDef::Alias(self.resolve(file, target)),
                DeclKind::Enum { variants } => {
                    self.unique(variants.iter(), "variant");
                    TypeDef::Enum(variants.iter().map(|v| v.name.clone()).collect())
                }
                DeclKind::Extern => TypeDef::Extern,
                _ => continue,
            };
            self.type_params = None;
            self.env.defs.insert(d.name.name.clone(), def);
        }
        // Break alias cycles, reporting each once, at the alias declared first. An alias is
        // transparent, so one that refers to itself anywhere in its target (through `[]`, `?`,
        // `map` or a type argument) would be an infinite type.
        let aliases: Vec<(usize, &'p ast::Decl)> = self
            .decls()
            .into_iter()
            .filter(|(_, d)| matches!(d.kind, DeclKind::Alias { .. }))
            .collect();
        let alias_edges = |env: &TypeEnv, name: &str| -> Vec<String> {
            let mut out = Vec::new();
            if let Some(TypeDef::Alias(t)) = env.defs.get(name) {
                names_in(t, &mut out);
            }
            out.retain(|n| matches!(env.defs.get(n), Some(TypeDef::Alias(_))));
            out
        };
        let mut broken: HashSet<String> = HashSet::new();
        for (_, d) in aliases {
            let start = d.name.name.clone();
            if broken.contains(&start) {
                continue;
            }
            // Breadth first, so that the path reported is a shortest one.
            let mut parent: HashMap<String, String> = HashMap::new();
            let mut queue: std::collections::VecDeque<String> = alias_edges(&self.env, &start)
                .into_iter()
                .inspect(|n| {
                    parent.entry(n.clone()).or_insert_with(|| start.clone());
                })
                .collect();
            let mut found = false;
            let mut visited: HashSet<String> = HashSet::new();
            while let Some(n) = queue.pop_front() {
                if n == start {
                    found = true;
                    break;
                }
                if !visited.insert(n.clone()) {
                    continue;
                }
                for m in alias_edges(&self.env, &n) {
                    if !parent.contains_key(&m) {
                        parent.insert(m.clone(), n.clone());
                    }
                    queue.push_back(m);
                }
            }
            if !found {
                continue;
            }
            let mut chain = vec![start.clone()];
            let mut cur = parent[&start].clone();
            while cur != start {
                chain.push(cur.clone());
                cur = parent[&cur].clone();
            }
            chain[1..].reverse();
            let path = match (&d.kind, chain.len()) {
                // Directly: say what it is.
                (DeclKind::Alias { target, .. }, 1) => format!("`{start}` = `{}`", ir_type(target)),
                _ => chain
                    .iter()
                    .chain(std::iter::once(&start))
                    .map(|n| format!("`{n}`"))
                    .collect::<Vec<_>>()
                    .join(" = "),
            };
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
            let sig = self.signature(file, &d.name, params, returns, errors);
            self.sigs.insert(d.name.name.clone(), sig);
        }
    }

    /// The resolved signature of a function, action, task, machine or call `name`.
    fn signature(
        &mut self,
        file: usize,
        name: &Ident,
        params: &[Field],
        returns: Option<&ast::TypeRef>,
        errors: &[Ident],
    ) -> Sig {
        let params = self.fields(file, params, "parameter");
        let returns = returns.map(|r| {
            let t = self.resolve(file, r);
            let what = format!("what `{}` returns", name.name);
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
        Sig {
            params,
            returns,
            errors: errs,
            name_span: name.span,
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
                DeclKind::Record { params, fields } => Some(ir::TypeDef::Record {
                    params: params.iter().map(|p| p.name.clone()).collect(),
                    fields: ir_fields(fields),
                }),
                DeclKind::Error { fields } => Some(ir::TypeDef::Error {
                    fields: ir_fields(fields),
                }),
                DeclKind::Alias { params, target } => Some(ir::TypeDef::Alias {
                    params: params.iter().map(|p| p.name.clone()).collect(),
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

/// "`call reserve` is only the type of a machine variable", for a pending call type anywhere
/// else, including inside `?`, `[]` or an alias in a variable's type.
pub(crate) fn call_type_not_allowed(t: &ast::TypeRef, n: &Ident) -> Diagnostic {
    Diagnostic::error(
        codes::CALL_TYPE_NOT_ALLOWED,
        t.span,
        format!(
            "`call {}` is the type of a pending call, which only a machine variable can hold",
            n.name
        ),
    )
    .with_help("a pending call never leaves its run: keep it in a `var` of type `call <name>` (not optional, not a list, not through an alias) and answer it with `reply`")
}

/// The built-in error `Unanswered` where it cannot be.
pub(crate) fn unanswered_misused(span: Span, message: &str) -> Diagnostic {
    Diagnostic::error(codes::UNANSWERED_MISUSED, span, message.to_string()).with_help(
        "`Unanswered` is the error a call fails with when it is not answered; it is handled only in the `fail Unanswered` (or `fail *`) outcome of an `invoke x.call(…)`",
    )
}
