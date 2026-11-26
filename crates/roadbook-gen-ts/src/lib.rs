//! The TypeScript code generator.
//!
//! [`generate`] turns the IR of one entry file into the text of the module `roadbook gen`
//! writes next to it (`checkout.road` -> `checkout.road.ts`), for the runtime
//! `@roadbook/effect`. It reads only the IR. The module holds, for the declarations of the
//! entry file (those whose span is in `ir.sources[0]`):
//!
//! - an Effect `Schema` and a TypeScript type for every type: `Schema.Struct` and an
//!   interface for a record, `Schema.Literals` for an enum, a `Schema.TaggedError` class for
//!   an error, the aliased schema for an alias. Types come in source order, except that a
//!   type follows the types of this file its schema refers to;
//! - for every machine, a `Schema.TaggedStruct` per event, a definition interface (`<Name>Def`)
//!   with its input, output, errors, events, state ids and the signatures of the functions,
//!   actions, tasks and machines it uses, and a handle made by `Roadbook.make` from the
//!   compiled IR, which is inlined once per module.
//!
//! Declarations of other files are imported from their own generated modules, by relative
//! path (`./types.road.js`).
//!
//! # Types the generator maps specially
//!
//! - An `extern type` maps to `Schema.Unknown` and the TypeScript type `unknown`: there is no
//!   way yet to bind it to a host type.
//! - Recursive records and aliases (`type Tree { children: Tree[] }`) are supported: each
//!   reference inside the cycle is `Schema.suspend`ed and the types of the cycle get an
//!   explicit interface. A cycle is rejected with [`GenError::Recursive`] if it spans several
//!   files (the modules would import each other), contains an error type, or reaches an error
//!   type (whose encoded form differs from its type, which `Schema.suspend` cannot express
//!   without spelling out the encoded type).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;

use roadbook_ir::{
    Builtin, Effect, Expr, ExprKind, Field, InvokeKind, Ir, Machine, State, Trigger, TypeDecl,
    TypeDef, TypeRef,
};

/// Lines longer than this are broken, where the generator knows how.
const WIDTH: usize = 100;

/// The runtime the generated module is written for.
const RUNTIME: &str = "@roadbook/effect";

/// Choices about the generated code. There are none yet.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Options {}

/// Why a module could not be generated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GenError {
    /// The IR has no sources, so it has no entry file.
    NoEntry,
    /// The IR names something it does not declare.
    Undeclared {
        /// `type`, `function`, `action`, `task` or `machine`.
        kind: &'static str,
        /// The name.
        name: String,
    },
    /// Recursive types the generator cannot express.
    Recursive {
        /// The types of the cycle, in IR order.
        types: Vec<String>,
        /// Why.
        reason: &'static str,
    },
    /// Two declarations of the module would have the same name.
    Clash {
        /// The name.
        name: String,
    },
}

impl fmt::Display for GenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GenError::NoEntry => write!(f, "the IR has no sources, so there is no entry file"),
            GenError::Undeclared { kind, name } => {
                write!(f, "the IR uses {kind} `{name}` but does not declare it")
            }
            GenError::Recursive { types, reason } => write!(
                f,
                "cannot generate TypeScript for the recursive types {}: {reason}",
                types
                    .iter()
                    .map(|t| format!("`{t}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            GenError::Clash { name } => write!(
                f,
                "the generated module would declare `{name}` twice; rename a declaration \
                 (machine `M` also declares `MDef`; an event declared by two machines of the \
                 file is named `<Machine><Event>`; `Schema`, `Duration` and `Roadbook` are \
                 imported names)"
            ),
        }
    }
}

impl std::error::Error for GenError {}

/// The text of the TypeScript module for the entry file of `ir` (`ir.sources[0]`): what
/// `roadbook gen` writes to `<file>.road.ts`. Deterministic: the same IR gives the same text.
pub fn generate(ir: &Ir, options: &Options) -> Result<String, GenError> {
    let _ = options;
    let entry = ir.sources.first().ok_or(GenError::NoEntry)?;
    let mut g = Gen {
        ir,
        imports: BTreeMap::new(),
        schema: false,
        duration: false,
        cycle: cycles(ir)?,
    };
    let mut items: Vec<String> = Vec::new();
    let mut names: Vec<String> = Vec::new();

    let types = g.type_order();
    for (i, name) in types.iter().enumerate() {
        let mut item = g.type_item(name)?;
        if i == 0 {
            item = format!("// ---- types ----\n\n{item}");
        }
        items.push(item);
        names.push(name.to_string());
    }

    let machines: Vec<(&String, &Machine)> = ir
        .machines
        .iter()
        .filter(|(_, m)| m.span.file == 0)
        .collect();
    let mut event_count: HashMap<&str, usize> = HashMap::new();
    for (_, m) in &machines {
        for e in m.events.keys() {
            *event_count.entry(e).or_default() += 1;
        }
    }
    for (i, (name, m)) in machines.iter().enumerate() {
        let mut section = Vec::new();
        let mut events = Vec::new();
        for (event, decl) in &m.events {
            let ident = if event_count[event.as_str()] > 1 {
                format!("{name}{event}")
            } else {
                event.clone()
            };
            g.schema = true;
            let fields = g.schema_fields(&decl.fields, None)?;
            section.push(format!(
                "{}{}\nexport type {ident} = typeof {ident}.Type",
                jsdoc(&decl.doc),
                block(
                    &format!("export const {ident} = Schema.TaggedStruct(\"{event}\", "),
                    &fields,
                    ")"
                )
            ));
            names.push(ident.clone());
            events.push(ident);
        }
        section.push(g.definition(name, m, events)?);
        names.push(format!("{name}Def"));
        if i == 0 {
            section.push(format!("const ir: unknown = {}", ir.to_json().trim_end()));
            let errors = ir
                .types
                .iter()
                .filter(|(_, t)| matches!(t.def, TypeDef::Error { .. }))
                .map(|(n, _)| g.named(n))
                .collect::<Result<Vec<_>, _>>()?;
            section.push(list("const errors = ", &errors, ""));
        }
        section.push(format!(
            "{}export const {name} = Roadbook.make<{name}Def>(ir, \"{name}\", errors)",
            jsdoc(&m.doc)
        ));
        names.push(name.to_string());
        section[0] = format!("// ---- machine: {name} ----\n\n{}", section[0]);
        items.extend(section);
    }

    let mut imports = Vec::new();
    if g.schema {
        imports.push("import { Schema } from \"effect\"".to_owned());
        names.push("Schema".into());
    }
    if g.duration {
        imports.push("import type { Duration } from \"effect\"".to_owned());
        names.push("Duration".into());
    }
    if !machines.is_empty() {
        imports.push(format!("import * as Roadbook from \"{RUNTIME}\""));
        names.push("Roadbook".into());
    }
    for (file, imported) in &g.imports {
        let path = ir
            .sources
            .get(*file as usize)
            .map(|s| s.path.as_str())
            .unwrap_or_default();
        let imported: Vec<&str> = imported.iter().copied().collect();
        names.extend(imported.iter().map(|n| n.to_string()));
        imports.push(list(
            "import ",
            &imported,
            &format!(" from \"{}\"", relative(&entry.path, path)),
        ));
    }

    let mut seen = HashSet::new();
    if let Some(name) = names.into_iter().find(|n| !seen.insert(n.clone())) {
        return Err(GenError::Clash { name });
    }

    let mut parts = vec![format!(
        "// Generated by roadbook gen from {}. Do not edit.",
        entry.path
    )];
    if !imports.is_empty() {
        parts.push(imports.join("\n"));
    }
    parts.extend(items);
    let mut out = parts.join("\n\n");
    out.push('\n');
    Ok(out)
}

struct Gen<'a> {
    ir: &'a Ir,
    /// Names to import, by index of the source declaring them.
    imports: BTreeMap<u32, BTreeSet<&'a str>>,
    /// Whether `Schema` is used.
    schema: bool,
    /// Whether the type `Duration` is used.
    duration: bool,
    /// For each type on a cycle, the index (in `ir.types`) of its cycle's first type.
    cycle: HashMap<&'a str, usize>,
}

impl<'a> Gen<'a> {
    /// The declaration named `name`, imported if another file declares it.
    fn named(&mut self, name: &str) -> Result<&'a str, GenError> {
        let (key, decl) =
            self.ir
                .types
                .get_key_value(name)
                .ok_or_else(|| GenError::Undeclared {
                    kind: "type",
                    name: name.to_owned(),
                })?;
        if decl.span.file != 0 {
            self.imports.entry(decl.span.file).or_default().insert(key);
        }
        Ok(key)
    }

    /// The TypeScript type of a type reference.
    fn ts(&mut self, t: &TypeRef) -> Result<String, GenError> {
        Ok(match t {
            TypeRef::Builtin(b) => match b {
                Builtin::String => "string".into(),
                Builtin::Int | Builtin::Float => "number".into(),
                Builtin::Bool => "boolean".into(),
                Builtin::Duration => {
                    self.duration = true;
                    "Duration.Duration".into()
                }
                Builtin::Json => {
                    self.schema = true;
                    "Schema.Json".into()
                }
            },
            TypeRef::Named(n) => self.named(n)?.to_owned(),
            TypeRef::List(t) => format!("ReadonlyArray<{}>", self.ts(t)?),
            TypeRef::Optional(t) => format!("{} | null", self.ts(t)?),
        })
    }

    /// The schema of a type reference, in the definition of a type on cycle `cycle`, if any.
    fn schema(&mut self, t: &TypeRef, cycle: Option<usize>) -> Result<String, GenError> {
        self.schema = true;
        Ok(match t {
            TypeRef::Builtin(b) => match b {
                Builtin::String => "Schema.String",
                Builtin::Int => "Schema.Int",
                Builtin::Float => "Schema.Finite",
                Builtin::Bool => "Schema.Boolean",
                Builtin::Duration => "Schema.Duration",
                Builtin::Json => "Schema.Json",
            }
            .to_owned(),
            TypeRef::Named(n) => {
                let n = self.named(n)?;
                if cycle.is_some() && self.cycle.get(n).copied() == cycle {
                    format!("Schema.suspend((): Schema.Codec<{n}> => {n})")
                } else {
                    n.to_owned()
                }
            }
            TypeRef::List(t) => format!("Schema.Array({})", self.schema(t, cycle)?),
            TypeRef::Optional(t) => format!("Schema.NullOr({})", self.schema(t, cycle)?),
        })
    }

    fn schema_fields(
        &mut self,
        fields: &[Field],
        cycle: Option<usize>,
    ) -> Result<Vec<(String, String)>, GenError> {
        fields
            .iter()
            .map(|f| Ok((f.name.clone(), self.schema(&f.ty, cycle)?)))
            .collect()
    }

    fn params(&mut self, fields: &[Field]) -> Result<Ty, GenError> {
        let members = fields
            .iter()
            .map(|f| Ok((f.name.clone(), Ty::Atom(self.ts(&f.ty)?))))
            .collect::<Result<_, GenError>>()?;
        Ok(Ty::Obj(members))
    }

    fn errors(&mut self, errors: &[String]) -> Result<Ty, GenError> {
        let names = errors
            .iter()
            .map(|e| Ok(self.named(e)?.to_owned()))
            .collect::<Result<_, GenError>>()?;
        Ok(Ty::union(names))
    }

    fn returns(&mut self, t: Option<&TypeRef>) -> Result<Ty, GenError> {
        Ok(Ty::Atom(match t {
            Some(t) => self.ts(t)?,
            None => "void".into(),
        }))
    }

    /// The entry file's types, in source order, each after the entry-file types its schema
    /// refers to outside its own cycle.
    fn type_order(&self) -> Vec<&'a str> {
        let ir = self.ir;
        let mut order = Vec::new();
        let mut seen = HashSet::new();
        fn visit<'a>(
            g: &Gen<'a>,
            name: &'a str,
            seen: &mut HashSet<&'a str>,
            order: &mut Vec<&'a str>,
        ) {
            if !seen.insert(name) {
                return;
            }
            let own = g.cycle.get(name);
            for dep in deps(&g.ir.types[name]) {
                let Some((dep, decl)) = g.ir.types.get_key_value(dep) else {
                    continue;
                };
                if decl.span.file == 0 && (own.is_none() || g.cycle.get(dep.as_str()) != own) {
                    visit(g, dep, seen, order);
                }
            }
            order.push(name);
        }
        for (name, decl) in &ir.types {
            if decl.span.file == 0 {
                visit(self, name, &mut seen, &mut order);
            }
        }
        order
    }

    fn type_item(&mut self, name: &'a str) -> Result<String, GenError> {
        let decl = &self.ir.types[name];
        let cycle = self.cycle.get(name).copied();
        let body = match &decl.def {
            TypeDef::Record { fields } => {
                let schema = block(
                    &format!("export const {name} = Schema.Struct("),
                    &self.schema_fields(fields, cycle)?,
                    ")",
                );
                if cycle.is_some() {
                    let head = format!("export interface {name} ");
                    let ty = self.params(fields)?.render(0, head.len());
                    format!("{schema}\n{head}{ty}")
                } else {
                    format!(
                        "{schema}\nexport interface {name} extends Schema.Schema.Type<typeof {name}> {{}}"
                    )
                }
            }
            TypeDef::Alias { ty } => {
                let schema = self.schema(ty, cycle)?;
                let ts = if cycle.is_some() {
                    self.ts(ty)?
                } else {
                    format!("typeof {name}.Type")
                };
                format!("export const {name} = {schema}\nexport type {name} = {ts}")
            }
            TypeDef::Enum { variants } => {
                self.schema = true;
                let quoted: Vec<String> = variants.iter().map(|v| format!("\"{v}\"")).collect();
                let flat = format!(
                    "export const {name} = Schema.Literals([{}])",
                    quoted.join(", ")
                );
                let schema = if flat.len() <= WIDTH {
                    flat
                } else {
                    format!(
                        "export const {name} = Schema.Literals([\n  {}\n])",
                        quoted.join(",\n  ")
                    )
                };
                format!("{schema}\nexport type {name} = typeof {name}.Type")
            }
            TypeDef::Error { fields } => {
                self.schema = true;
                block(
                    &format!(
                        "export class {name} extends Schema.TaggedError<{name}>()(\"{name}\", "
                    ),
                    &self.schema_fields(fields, cycle)?,
                    ") {}",
                )
            }
            TypeDef::Extern => {
                self.schema = true;
                format!("export const {name} = Schema.Unknown\nexport type {name} = unknown")
            }
        };
        Ok(format!("{}{body}", jsdoc(&decl.doc)))
    }

    /// `export interface <Name>Def extends Roadbook.MachineDef { … }`.
    fn definition(
        &mut self,
        name: &str,
        m: &'a Machine,
        events: Vec<String>,
    ) -> Result<String, GenError> {
        let ir = self.ir;
        let mut uses = Uses::default();
        uses.machine(m);
        for (kind, used, declared) in [
            (
                "function",
                &uses.fns,
                ir.functions.keys().collect::<Vec<_>>(),
            ),
            ("action", &uses.actions, ir.actions.keys().collect()),
            ("task", &uses.tasks, ir.tasks.keys().collect()),
            ("machine", &uses.machines, ir.machines.keys().collect()),
        ] {
            if let Some(n) = used.iter().find(|n| !declared.iter().any(|d| d == *n)) {
                return Err(GenError::Undeclared {
                    kind,
                    name: (*n).to_owned(),
                });
            }
        }

        let mut fns = Vec::new();
        for (n, f) in ir
            .functions
            .iter()
            .filter(|(n, _)| uses.fns.contains(n.as_str()))
        {
            let sig = vec![
                ("args".into(), self.params(&f.params)?),
                ("returns".into(), Ty::Atom(self.ts(&f.returns)?)),
            ];
            fns.push((n.clone(), Ty::Obj(sig)));
        }
        let mut actions = Vec::new();
        for (n, a) in ir
            .actions
            .iter()
            .filter(|(n, _)| uses.actions.contains(n.as_str()))
        {
            actions.push((n.clone(), self.params(&a.params)?));
        }
        let mut tasks = Vec::new();
        for (n, t) in ir
            .tasks
            .iter()
            .filter(|(n, _)| uses.tasks.contains(n.as_str()))
        {
            let sig = vec![
                ("args".into(), self.params(&t.params)?),
                ("success".into(), self.returns(t.returns.as_ref())?),
                ("error".into(), self.errors(&t.errors)?),
            ];
            tasks.push((n.clone(), Ty::Obj(sig)));
        }
        let mut machines = Vec::new();
        for (n, c) in ir
            .machines
            .iter()
            .filter(|(n, _)| uses.machines.contains(n.as_str()))
        {
            let sig = vec![
                ("args".into(), self.params(&c.params)?),
                ("success".into(), self.returns(c.returns.as_ref())?),
                ("error".into(), self.errors(&c.errors)?),
            ];
            machines.push((n.clone(), Ty::Obj(sig)));
        }
        let mut states = Vec::new();
        state_ids(&m.root, &mut states);

        let members = vec![
            ("name".into(), Ty::Atom(format!("\"{name}\""))),
            ("input".into(), self.params(&m.params)?),
            ("output".into(), self.returns(m.returns.as_ref())?),
            ("error".into(), self.errors(&m.errors)?),
            ("event".into(), Ty::union(events)),
            ("state".into(), Ty::union(states)),
            ("fns".into(), Ty::Obj(fns)),
            ("actions".into(), Ty::Obj(actions)),
            ("tasks".into(), Ty::Obj(tasks)),
            ("machines".into(), Ty::Obj(machines)),
        ];
        Ok(format!(
            "export interface {name}Def extends Roadbook.MachineDef {}",
            Ty::members(&members, 0)
        ))
    }
}

/// Every state id under `s`, `s` included, in document order.
fn state_ids(s: &State, out: &mut Vec<String>) {
    out.push(format!("\"{}\"", s.id));
    for c in &s.states {
        state_ids(c, out);
    }
}

/// What a machine uses directly.
#[derive(Default)]
struct Uses<'a> {
    fns: BTreeSet<&'a str>,
    actions: BTreeSet<&'a str>,
    tasks: BTreeSet<&'a str>,
    machines: BTreeSet<&'a str>,
}

impl<'a> Uses<'a> {
    fn machine(&mut self, m: &'a Machine) {
        for v in &m.vars {
            if let Some(e) = &v.init {
                self.expr(e);
            }
        }
        self.state(&m.root);
    }

    fn state(&mut self, s: &'a State) {
        for e in s.entry.iter().chain(&s.exit) {
            self.effect(e);
        }
        if let Some(inv) = &s.invoke {
            match inv.kind {
                InvokeKind::Task => self.tasks.insert(&inv.src),
                InvokeKind::Machine => self.machines.insert(&inv.src),
            };
            for a in &inv.args {
                self.expr(a);
            }
        }
        for t in &s.transitions {
            if let Trigger::After { delay } = &t.trigger {
                self.expr(delay);
            }
            if let Some(g) = &t.guard {
                self.expr(g);
            }
            for e in &t.effects {
                self.effect(e);
            }
        }
        for e in s.output.iter().chain(&s.fail) {
            self.expr(e);
        }
        for c in &s.states {
            self.state(c);
        }
    }

    fn effect(&mut self, e: &'a Effect) {
        match e {
            Effect::Set { value, .. } => self.expr(value),
            Effect::Do { action, args } => {
                self.actions.insert(action);
                for a in args {
                    self.expr(a);
                }
            }
        }
    }

    fn expr(&mut self, e: &'a Expr) {
        match &e.kind {
            ExprKind::Call { func, args } => {
                self.fns.insert(func);
                for a in args {
                    self.expr(a);
                }
            }
            ExprKind::Op { args, .. } => {
                for a in args {
                    self.expr(a);
                }
            }
            ExprKind::Field { of, .. } => self.expr(of),
            ExprKind::Record { fields } => {
                for f in fields {
                    self.expr(&f.value);
                }
            }
            ExprKind::Lit(_) | ExprKind::Ref { .. } | ExprKind::Enum { .. } => {}
        }
    }
}

/// The names of the types a type's schema refers to.
fn deps(decl: &TypeDecl) -> Vec<&str> {
    match &decl.def {
        TypeDef::Record { fields } | TypeDef::Error { fields } => {
            fields.iter().filter_map(|f| f.ty.named_base()).collect()
        }
        TypeDef::Alias { ty } => ty.named_base().into_iter().collect(),
        TypeDef::Enum { .. } | TypeDef::Extern => Vec::new(),
    }
}

/// For each entry-file type on a cycle, the index of the first type of its cycle. Fails if a
/// cycle cannot be generated (see the crate documentation).
fn cycles(ir: &Ir) -> Result<HashMap<&str, usize>, GenError> {
    let n = ir.types.len();
    let edges: Vec<Vec<usize>> = ir
        .types
        .values()
        .map(|d| {
            deps(d)
                .into_iter()
                .filter_map(|t| ir.types.get_index_of(t))
                .collect()
        })
        .collect();
    // What each type reaches through one or more references.
    let reach: Vec<Vec<bool>> = (0..n)
        .map(|start| {
            let mut seen = vec![false; n];
            let mut stack = edges[start].clone();
            while let Some(i) = stack.pop() {
                if !std::mem::replace(&mut seen[i], true) {
                    stack.extend(&edges[i]);
                }
            }
            seen
        })
        .collect();
    let is_error = |i: usize| matches!(ir.types[i].def, TypeDef::Error { .. });
    let mut out = HashMap::new();
    for (i, (name, decl)) in ir.types.iter().enumerate() {
        if decl.span.file != 0 || !reach[i][i] {
            continue;
        }
        let members: Vec<usize> = (0..n).filter(|&j| reach[i][j] && reach[j][i]).collect();
        let fail = |reason| GenError::Recursive {
            types: members
                .iter()
                .map(|&j| {
                    ir.types
                        .get_index(j)
                        .map(|(n, _)| n.clone())
                        .unwrap_or_default()
                })
                .collect(),
            reason,
        };
        // Not reached for a type on a cycle: a non-empty cycle contains `i`.
        let first = members.first().copied().unwrap_or(i);
        if members.iter().any(|&j| ir.types[j].span.file != 0) {
            return Err(fail(
                "the cycle spans several files, whose modules would import each other",
            ));
        }
        if members.iter().any(|&j| is_error(j)) {
            return Err(fail("an error type cannot be part of a cycle"));
        }
        if (0..n).any(|j| reach[i][j] && is_error(j)) {
            return Err(fail("a recursive type cannot contain an error type"));
        }
        out.insert(name.as_str(), first);
    }
    Ok(out)
}

/// A TypeScript type in a definition interface, laid out to fit [`WIDTH`] where it can.
enum Ty {
    Atom(String),
    /// `{ readonly a: A; readonly b: B }`.
    Obj(Vec<(String, Ty)>),
    /// `A | B`, at least two members.
    Union(Vec<String>),
}

impl Ty {
    fn union(mut items: Vec<String>) -> Ty {
        match items.len() {
            0 => Ty::Atom("never".into()),
            1 => Ty::Atom(items.remove(0)),
            _ => Ty::Union(items),
        }
    }

    fn flat(&self) -> String {
        match self {
            Ty::Atom(s) => s.clone(),
            Ty::Obj(m) if m.is_empty() => "{}".into(),
            Ty::Obj(m) => {
                let m: Vec<String> = m
                    .iter()
                    .map(|(k, v)| format!("readonly {k}: {}", v.flat()))
                    .collect();
                format!("{{ {} }}", m.join("; "))
            }
            Ty::Union(items) => items.join(" | "),
        }
    }

    /// The type on a line indented by `indent`, starting at column `column`.
    fn render(&self, indent: usize, column: usize) -> String {
        let flat = self.flat();
        if column + flat.len() <= WIDTH {
            return flat;
        }
        match self {
            Ty::Atom(_) => flat,
            Ty::Obj(m) if m.is_empty() => flat,
            Ty::Obj(m) => Ty::members(m, indent),
            Ty::Union(items) => items
                .iter()
                .map(|t| format!("\n{}| {t}", " ".repeat(indent + 2)))
                .collect(),
        }
    }

    /// An object type with one member per line.
    fn members(m: &[(String, Ty)], indent: usize) -> String {
        let pad = " ".repeat(indent + 2);
        let mut s = String::from("{\n");
        for (k, v) in m {
            let lead = format!("{pad}readonly {k}: ");
            let v = v.render(indent + 2, lead.len());
            s.push_str(if v.starts_with('\n') {
                lead.trim_end()
            } else {
                &lead
            });
            s.push_str(&v);
            s.push('\n');
        }
        s.push_str(&" ".repeat(indent));
        s.push('}');
        s
    }
}

/// `head{ a: A, b: B }tail`, or one entry per line if that is too long.
fn block(head: &str, entries: &[(String, String)], tail: &str) -> String {
    let entries: Vec<String> = entries.iter().map(|(k, v)| format!("{k}: {v}")).collect();
    list(head, &entries, tail)
}

/// `head{ a, b }tail`, or one item per line if that is too long.
fn list<S: AsRef<str>>(head: &str, items: &[S], tail: &str) -> String {
    let items: Vec<&str> = items.iter().map(AsRef::as_ref).collect();
    if items.is_empty() {
        return format!("{head}{{}}{tail}");
    }
    let flat = format!("{head}{{ {} }}{tail}", items.join(", "));
    if flat.len() <= WIDTH {
        return flat;
    }
    format!("{head}{{\n  {}\n}}{tail}", items.join(",\n  "))
}

/// A doc comment as JSDoc, with a final newline, or nothing.
fn jsdoc(doc: &str) -> String {
    if doc.is_empty() {
        return String::new();
    }
    let doc = doc.replace("*/", "*\\/");
    let lines: Vec<&str> = doc.lines().collect();
    if let [line] = lines[..] {
        return format!("/** {line} */\n");
    }
    let mut s = String::from("/**\n");
    for line in lines {
        s.push_str(format!(" * {line}").trim_end());
        s.push('\n');
    }
    s.push_str(" */\n");
    s
}

/// The import specifier of the module generated for project file `to`, from the module of
/// project file `from`: `./types.road.js`, `../common/types.road.js`.
fn relative(from: &str, to: &str) -> String {
    let from: Vec<&str> = from.split('/').collect();
    let to: Vec<&str> = to.split('/').collect();
    let from_dir = &from[..from.len() - 1];
    let common = from_dir
        .iter()
        .zip(&to[..to.len() - 1])
        .take_while(|(a, b)| a == b)
        .count();
    let mut parts = vec![".."; from_dir.len() - common];
    if parts.is_empty() {
        parts.push(".");
    }
    parts.extend(&to[common..]);
    format!("{}.js", parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths() {
        assert_eq!(relative("a/b.road", "a/c.road"), "./c.road.js");
        assert_eq!(relative("b.road", "c.road"), "./c.road.js");
        assert_eq!(relative("a/b.road", "c.road"), "../c.road.js");
        assert_eq!(relative("a/x/b.road", "a/y/c.road"), "../y/c.road.js");
        assert_eq!(relative("b.road", "a/c.road"), "./a/c.road.js");
    }

    #[test]
    fn docs() {
        assert_eq!(jsdoc(""), "");
        assert_eq!(jsdoc("One."), "/** One. */\n");
        assert_eq!(jsdoc("A\n\nb */"), "/**\n * A\n *\n * b *\\/\n */\n");
    }
}
