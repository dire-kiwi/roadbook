//! The TypeScript code generator.
//!
//! [`generate`] turns the IR of one entry file into the text of the module `roadbook gen`
//! writes next to it (`checkout.road` -> `checkout.road.ts`), for the runtime
//! `@dire-kiwi/roadbook-effect`. It reads only the IR. The module holds, for the declarations of the
//! entry file (those whose span is in `ir.sources[0]`):
//!
//! - an Effect `Schema` and a TypeScript type for every type: `Schema.Struct` and an
//!   interface for a record, `Schema.Literals` for an enum, a `Schema.TaggedError` class for
//!   an error, the aliased schema for an alias. Types come in source order, except that a
//!   type follows the types of this file its schema refers to;
//! - for every task that emits, a `Schema.TaggedStruct` per event it emits, which its
//!   signature in a definition interface names as its `emits`;
//! - for every machine, a `Schema.TaggedStruct` per event it accepts and per event it emits, a
//!   definition interface (`<Name>Def`) with its input, output, errors, events, emitted events,
//!   state ids and the signatures of the functions, actions, tasks and machines it uses, and a
//!   handle made by `Roadbook.make` from the compiled IR, which is inlined once per module,
//!   and the module's error classes: those of the files its declarations refer to, directly
//!   or not (see `file_refs`).
//!
//! Declarations of other files are imported from their own generated modules: a file of the
//! project by relative path (`./types.road.js`), a file of another package as
//! [`Options::modules`] says (`roadbook gen` gives a relative path to where the file was found
//! for a package of the `[packages]` table of `roadbook.toml`, and for one in `node_modules`
//! the specifier its `exports` maps to the generated module), else by the package specifier
//! `<package>/<path>.js` (`inventory/inventory.road.js`). A name that is only a type (a
//! machine's `<Name>Def`, an alias of a type with a `ref` in it) is imported with `type`, and
//! with `import type` if the module imports nothing else from that one, so that it is not
//! loaded for it.
//!
//! Charts may import each other, and so may their modules. In an import cycle the module that
//! is loaded first runs after the others, so a module reads what it imports from a module that
//! may import it back, directly or not, only when it is first needed: a schema as
//! `Schema.suspend(() => T)`, an error class through a getter of the errors map that
//! `Roadbook.make` is given. Which modules may import which is read from the IR, which holds
//! every file the entry file imports, directly or not (see `file_refs`).
//!
//! # Types the generator maps specially
//!
//! - An `extern type` maps to `Schema.Unknown` and the TypeScript type `unknown`: there is no
//!   way yet to bind it to a host type.
//! - `ref M` maps to `Roadbook.Handle<MDef>`, the handle of a running `M`, with `MDef` imported
//!   (as a type) from `M`'s module when another file declares it. A handle has no schema: refs
//!   appear only in machine inputs, variables and the parameters of host code, never in a
//!   record, error or event, so an alias of a type with a `ref` in it is a type alias only.
//! - Recursive records and aliases (`type Tree { children: Tree[] }`) are supported: each
//!   reference inside the cycle is `Schema.suspend`ed and the types of the cycle get an
//!   explicit interface. A cycle is rejected with [`GenError::Recursive`] if it spans several
//!   files (the interfaces would have to be written across modules), contains an error type,
//!   or reaches an error type (whose encoded form differs from its type, which
//!   `Schema.suspend` cannot express without spelling out the encoded type).
//! - An `error`'s class extends JavaScript's `Error`, so some field names are taken: see
//!   [`error_field_conflict`]. Such a field is rejected with [`GenError::ErrorField`].

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;

use roadbook_ir::{
    Builtin, ChildrenKind, Effect, Expr, ExprKind, Field, InvokeKind, Ir, Machine, State, Trigger,
    TypeDecl, TypeDef, TypeRef,
};

/// Lines longer than this are broken, where the generator knows how.
const WIDTH: usize = 100;

/// The runtime the generated module is written for.
const RUNTIME: &str = "@dire-kiwi/roadbook-effect";

/// Choices about the generated code.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Options {
    /// How a module imports the generated module of a file of another package, by the file's
    /// package and path as in `ir.sources`. A file of a package not listed here is imported
    /// by the specifier `<package>/<path>.js`.
    pub modules: BTreeMap<(String, String), PackageModule>,
}

/// Where the generated module of a file of another package is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageModule {
    /// Next to the file, which is at this project path (relative to the project root,
    /// `/`-separated, starting with `..` if it is outside): imported by a relative path.
    Path(String),
    /// Imported by this specifier, as it is: `@shop/inventory/inventory.road.js`.
    Specifier(String),
}

impl Options {
    /// These options with `modules` as the modules of other packages' files.
    pub fn with_modules(mut self, modules: BTreeMap<(String, String), PackageModule>) -> Options {
        self.modules = modules;
        self
    }
}

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
    /// A field of an `error` that its class cannot have: see [`error_field_conflict`].
    ErrorField {
        /// The file declaring the error, as in `ir.sources`.
        file: String,
        /// The line of the error's declaration.
        line: u32,
        /// The error.
        error: String,
        /// The field.
        field: String,
        /// Its type, as written in Roadbook.
        ty: String,
        /// Why it cannot have it.
        reason: &'static str,
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
                "the generated module would declare `{name}` twice, or hide a name it uses; \
                 rename a declaration or type parameter (machine `M` also declares `MDef`; an \
                 event declared by two machines or tasks of the file is named `<Machine><Event>`, \
                 or `<Task><Event>` with the task's name capitalised; \
                 `Schema`, `Duration` and `Roadbook` are imported names, and `Record`, \
                 `Readonly` and `ReadonlyArray` are TypeScript's)"
            ),
            GenError::ErrorField {
                file,
                line,
                error,
                field,
                ty,
                reason,
            } => write!(
                f,
                "{file}:{line}: the error type `{error}` cannot have the field `{field}: {ty}` in \
                 TypeScript: {reason}"
            ),
        }
    }
}

impl std::error::Error for GenError {}

/// The text of the TypeScript module for the entry file of `ir` (`ir.sources[0]`): what
/// `roadbook gen` writes to `<file>.road.ts`. Deterministic: the same IR gives the same text.
pub fn generate(ir: &Ir, options: &Options) -> Result<String, GenError> {
    let entry = ir.sources.first().ok_or(GenError::NoEntry)?;
    let refs = file_refs(ir);
    let reached = reach(&refs, 0);
    let mut g = Gen {
        ir,
        imports: BTreeMap::new(),
        schema: false,
        duration: false,
        roadbook: false,
        globals: BTreeSet::new(),
        cycle: cycles(ir)?,
        lazy: (1..ir.sources.len())
            .filter(|&f| reached[f] && reach(&refs, f)[0])
            .map(|f| f as u32)
            .collect(),
        params: &[],
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
    let event_count = event_counts(ir, 0);
    // The events each task of the file emits, a schema each, before the machines that hear them.
    for (task, decl) in ir
        .tasks
        .iter()
        .filter(|(_, t)| t.span.file == 0 && !t.emits.is_empty())
    {
        let mut section = Vec::new();
        for (event, e) in &decl.emits {
            let ident = event_ident(&event_count, &pascal(task), event);
            g.schema = true;
            let fields = g.schema_fields(&e.fields, None)?;
            section.push(format!(
                "{}{}\nexport type {ident} = typeof {ident}.Type",
                jsdoc(&e.doc),
                block(
                    &format!("export const {ident} = Schema.TaggedStruct(\"{event}\", "),
                    &fields,
                    ")"
                )
            ));
            names.push(ident);
        }
        section[0] = format!("// ---- task: {task} ----\n\n{}", section[0]);
        items.extend(section);
    }
    for (i, (name, m)) in machines.iter().enumerate() {
        let mut section = Vec::new();
        let mut events = Vec::new();
        let mut emitted = Vec::new();
        let declared = m.events.iter().map(|e| (e, false));
        for ((event, decl), emits) in declared.chain(m.emits.iter().map(|e| (e, true))) {
            let ident = event_ident(&event_count, name, event);
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
            if emits {
                emitted.push(ident);
            } else {
                events.push(ident);
            }
        }
        section.push(g.definition(name, m, events, emitted)?);
        names.push(format!("{name}Def"));
        if i == 0 {
            section.push(format!("const ir: unknown = {}", ir.to_json().trim_end()));
            // The error classes of the files this one refers to, directly or not: those its
            // machines can meet. One of a module that imports this one is read when it is
            // first needed, as the modules may not have run yet when this one runs.
            let errors = ir
                .types
                .iter()
                .filter(|(_, t)| {
                    matches!(t.def, TypeDef::Error { .. }) && reached[t.span.file as usize]
                })
                .map(|(n, t)| {
                    let n = g.named(n)?;
                    Ok(if g.lazy.contains(&t.span.file) {
                        format!("get {n}() {{ return {n} }}")
                    } else {
                        n.to_owned()
                    })
                })
                .collect::<Result<Vec<_>, GenError>>()?;
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
    if !machines.is_empty() || g.roadbook {
        imports.push(format!("import * as Roadbook from \"{RUNTIME}\""));
        names.push("Roadbook".into());
    }
    for (file, imported) in &g.imports {
        let source = ir.sources.get(*file as usize);
        let path = source.map(|s| s.path.as_str()).unwrap_or_default();
        let specifier = match source.and_then(|s| s.package.as_deref()) {
            None => relative(&entry.path, path),
            Some(package) => match options.modules.get(&(package.to_owned(), path.to_owned())) {
                Some(PackageModule::Path(at)) => relative(&entry.path, at),
                Some(PackageModule::Specifier(s)) => s.clone(),
                None => format!("{package}/{path}.js"),
            },
        };
        names.extend(imported.iter().map(|i| i.name.clone()));
        // Names that exist only as types are imported with `type`: under
        // `verbatimModuleSyntax` a value import of one fails, and at run time the module has
        // no such export. A module only types are imported from is not loaded at all.
        let (head, imported): (&str, Vec<String>) = if imported.iter().all(|i| i.type_only) {
            (
                "import type ",
                imported.iter().map(|i| i.name.clone()).collect(),
            )
        } else {
            ("import ", imported.iter().map(Import::render).collect())
        };
        imports.push(list(head, &imported, &format!(" from \"{specifier}\"")));
    }

    names.extend(g.globals.iter().map(|n| n.to_string()));
    // A type parameter hides, inside its declaration, the names the declaration's code uses.
    for (_, decl) in ir.types.iter().filter(|(_, d)| d.span.file == 0) {
        if let TypeDef::Record { params, .. } | TypeDef::Alias { params, .. } = &decl.def
            && let Some(p) = params.iter().find(|p| {
                [
                    "Schema",
                    "Duration",
                    "Roadbook",
                    "Record",
                    "Readonly",
                    "ReadonlyArray",
                ]
                .contains(&p.as_str())
            })
        {
            return Err(GenError::Clash { name: p.clone() });
        }
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

/// Why an `error` cannot have `field` in TypeScript, if it cannot. Its class extends
/// `Schema.TaggedError`, and so JavaScript's `Error`, and its instance type is the fields
/// intersected with that of an `Error`. Checked against `tsc` and `effect@4.1.5`:
///
/// - `name`, `message` and `stack` are `string`s on every `Error`. A field with one of these
///   names must be a `string` or an enum (directly or through aliases): any other type makes
///   the field's type wrong, and a `bool` makes the whole class `never`, which `tsc` rejects.
/// - `toString`, `toJSON` and `pipe` are methods of every error instance (Effect's
///   `Inspectable` and `Pipeable`): a field would get the wrong type and replace the method,
///   so `String(error)`, `JSON.stringify(error)` or `error.pipe(…)` would break.
///
/// Other names, `cause` and `constructor` among them, give the field its declared type.
pub fn error_field_conflict(ir: &Ir, field: &Field) -> Option<&'static str> {
    fn stringish(ir: &Ir, t: &TypeRef) -> bool {
        match &*ir.expand_top(t) {
            TypeRef::Builtin(b) => *b == Builtin::String,
            TypeRef::Named(n) => {
                matches!(ir.types.get(n).map(|d| &d.def), Some(TypeDef::Enum { .. }))
            }
            _ => false,
        }
    }
    match field.name.as_str() {
        "name" | "message" | "stack" if !stringish(ir, &field.ty) => Some(
            "every error is a JavaScript `Error`, whose `name`, `message` and `stack` are \
             strings; give the field the type `string` (or an enum), or rename it",
        ),
        "toString" | "toJSON" | "pipe" => Some(
            "every error instance has the methods `toString`, `toJSON` and `pipe`, which the \
             field would replace; rename it",
        ),
        _ => None,
    }
}

/// A name a module imports from another module.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Import {
    name: String,
    /// Whether it is only a type (a machine's `<Name>Def`, an alias of a type with a `ref` in
    /// it), imported with `type`.
    type_only: bool,
}

impl Import {
    fn render(&self) -> String {
        if self.type_only {
            format!("type {}", self.name)
        } else {
            self.name.clone()
        }
    }
}

struct Gen<'a> {
    ir: &'a Ir,
    /// Names to import, by index of the source declaring them, sorted by name.
    imports: BTreeMap<u32, BTreeSet<Import>>,
    /// Whether `Schema` is used.
    schema: bool,
    /// Whether the type `Duration` is used.
    duration: bool,
    /// Whether `Roadbook` is used outside a machine's section (a `ref` in a type).
    roadbook: bool,
    /// The TypeScript globals the module's types use (`ReadonlyArray`, `Record`, `Readonly`),
    /// which a declaration of the same name would hide.
    globals: BTreeSet<&'static str>,
    /// For each type on a cycle, the index (in `ir.types`) of its cycle's first type.
    cycle: HashMap<&'a str, usize>,
    /// The files whose modules may import this one, directly or not: what this module imports
    /// from them it reads only when first needed (`Schema.suspend`, a getter), since in an
    /// import cycle the module that is loaded first runs after the others.
    lazy: HashSet<u32>,
    /// The type parameters of the generic declaration being generated, which its schema
    /// function takes as arguments of the same names.
    params: &'a [String],
}

impl<'a> Gen<'a> {
    /// The declaration named `name`, imported if another file declares it.
    fn named(&mut self, name: &str) -> Result<&'a str, GenError> {
        if let Some(p) = self.params.iter().find(|p| *p == name) {
            return Ok(p);
        }
        let (key, decl) =
            self.ir
                .types
                .get_key_value(name)
                .ok_or_else(|| GenError::Undeclared {
                    kind: "type",
                    name: name.to_owned(),
                })?;
        if decl.span.file != 0 {
            let type_only = matches!(&decl.def, TypeDef::Alias { ty, .. } if self.has_ref(ty));
            self.imports
                .entry(decl.span.file)
                .or_default()
                .insert(Import {
                    name: key.clone(),
                    type_only,
                });
        }
        Ok(key)
    }

    /// `Roadbook.Handle<MDef>`, the type of `ref M`, importing `MDef` if another file
    /// declares `M`.
    fn handle(&mut self, machine: &str) -> Result<String, GenError> {
        let (name, decl) =
            self.ir
                .machines
                .get_key_value(machine)
                .ok_or_else(|| GenError::Undeclared {
                    kind: "machine",
                    name: machine.to_owned(),
                })?;
        let def = format!("{name}Def");
        if decl.span.file != 0 {
            self.imports
                .entry(decl.span.file)
                .or_default()
                .insert(Import {
                    name: def.clone(),
                    type_only: true,
                });
        }
        self.roadbook = true;
        Ok(format!("Roadbook.Handle<{def}>"))
    }

    /// Whether a type has a `ref` in it, through aliases.
    fn has_ref(&self, t: &TypeRef) -> bool {
        has_ref(self.ir, t, 0)
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
            TypeRef::Generic(n, args) => {
                let n = self.named(n)?;
                let args = args
                    .iter()
                    .map(|a| self.ts(a))
                    .collect::<Result<Vec<_>, _>>()?;
                format!("{n}<{}>", args.join(", "))
            }
            TypeRef::Map(k, v) => {
                self.globals.extend(["Readonly", "Record"]);
                format!("Readonly<Record<{}, {}>>", self.ts(k)?, self.ts(v)?)
            }
            TypeRef::List(t) => {
                self.globals.insert("ReadonlyArray");
                format!("ReadonlyArray<{}>", self.ts(t)?)
            }
            TypeRef::Optional(t) => format!("{} | null", self.ts(t)?),
            TypeRef::Ref(m) => self.handle(m)?,
            // A pending call stays inside its run: the runtime holds it, and no generated
            // signature names it yet.
            TypeRef::Call(_) => "unknown".into(),
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
            TypeRef::Named(n) if self.params.contains(n) => n.clone(),
            TypeRef::Named(n) => {
                let n = self.named(n)?;
                if cycle.is_some() && self.cycle.get(n).copied() == cycle {
                    format!("Schema.suspend((): Schema.Codec<{n}> => {n})")
                } else if self.lazy.contains(&self.ir.types[n].span.file) {
                    format!("Schema.suspend(() => {n})")
                } else {
                    n.to_owned()
                }
            }
            TypeRef::Generic(n, args) => {
                let n = self.named(n)?;
                let args = args
                    .iter()
                    .map(|a| self.schema(a, cycle))
                    .collect::<Result<Vec<_>, _>>()?;
                let call = format!("{n}({})", args.join(", "));
                // A generic type on a cycle has an annotated schema function, so a call of it
                // needs no annotation of its own.
                if (cycle.is_some() && self.cycle.get(n).copied() == cycle)
                    || self.lazy.contains(&self.ir.types[n].span.file)
                {
                    format!("Schema.suspend(() => {call})")
                } else {
                    call
                }
            }
            // The keys are strings whatever alias names them, and a key schema must be a
            // string schema itself: a `Schema.suspend` of one is refused when the module loads.
            TypeRef::Map(_, v) => {
                format!("Schema.Record(Schema.String, {})", self.schema(v, cycle)?)
            }
            TypeRef::List(t) => format!("Schema.Array({})", self.schema(t, cycle)?),
            TypeRef::Optional(t) => format!("Schema.NullOr({})", self.schema(t, cycle)?),
            // The compiler allows a `ref` in no record, error or event field, and an alias of
            // one has no schema (see `type_item`), so no schema refers to a handle.
            TypeRef::Ref(m) => {
                return Err(GenError::Undeclared {
                    kind: "schema for the type",
                    name: format!("ref {m}"),
                });
            }
            // Only a machine variable has a pending call's type, and variables have no schema.
            TypeRef::Call(c) => {
                return Err(GenError::Undeclared {
                    kind: "schema for the type",
                    name: format!("call {c}"),
                });
            }
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
            TypeDef::Record { params, fields } if !params.is_empty() => {
                self.params = params;
                let schema = block("Schema.Struct(", &self.schema_fields(fields, cycle)?, ")");
                let head = format!("export interface {name}<{}> ", params.join(", "));
                let ty = self.params(fields)?.render(0, head.len());
                self.params = &[];
                format!(
                    "{}
{head}{ty}",
                    generic_schema(name, params, cycle.is_some(), &schema)
                )
            }
            TypeDef::Alias { params, ty } if !params.is_empty() => {
                self.params = params;
                let schema = self.schema(ty, cycle)?;
                let ts = self.ts(ty)?;
                self.params = &[];
                format!(
                    "{}
export type {name}<{}> = {ts}",
                    generic_schema(name, params, cycle.is_some(), &schema),
                    params.join(", ")
                )
            }
            TypeDef::Record { fields, .. } => {
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
            TypeDef::Alias { ty, .. } if self.has_ref(ty) => {
                format!("export type {name} = {}", self.ts(ty)?)
            }
            TypeDef::Alias { ty, .. } => {
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
                for field in fields {
                    if let Some(reason) = error_field_conflict(self.ir, field) {
                        return Err(GenError::ErrorField {
                            file: self
                                .ir
                                .sources
                                .get(decl.span.file as usize)
                                .map(|s| s.path.clone())
                                .unwrap_or_default(),
                            line: decl.span.line,
                            error: name.to_owned(),
                            field: field.name.clone(),
                            ty: field.ty.to_string(),
                            reason,
                        });
                    }
                }
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

    /// The union of the schemas of the events task `name` emits, imported if another file
    /// declares the task: named there as that file's own module names them.
    fn task_emits(&mut self, name: &str, t: &'a roadbook_ir::TaskDecl) -> Ty {
        let file = t.span.file;
        let counts = event_counts(self.ir, file);
        let idents: Vec<String> = t
            .emits
            .keys()
            .map(|e| event_ident(&counts, &pascal(name), e))
            .collect();
        if file != 0 {
            let imports = self.imports.entry(file).or_default();
            for ident in &idents {
                imports.insert(Import {
                    name: ident.clone(),
                    type_only: true,
                });
            }
        }
        Ty::union(idents)
    }

    /// `export interface <Name>Def extends Roadbook.MachineDef { … }`.
    fn definition(
        &mut self,
        name: &str,
        m: &'a Machine,
        events: Vec<String>,
        emitted: Vec<String>,
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
            let mut sig = vec![
                ("args".into(), self.params(&t.params)?),
                ("success".into(), self.returns(t.returns.as_ref())?),
                ("error".into(), self.errors(&t.errors)?),
            ];
            // What the task's emitter takes. A task that emits nothing has no `emits`: its
            // emitter's parameter is then `never`.
            if !t.emits.is_empty() {
                sig.push(("emits".into(), self.task_emits(n, t)));
            }
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
            ("emits".into(), Ty::union(emitted)),
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

/// How many times each event name is declared in file `file`: by its machines, as an event or an
/// emitted event, and by its tasks' `emits`. A name declared more than once is prefixed, in each
/// module that names it, by its machine's or task's name (see [`event_ident`]).
fn event_counts(ir: &Ir, file: u32) -> HashMap<&str, usize> {
    let mut count: HashMap<&str, usize> = HashMap::new();
    for m in ir.machines.values().filter(|m| m.span.file == file) {
        for e in m.events.keys().chain(m.emits.keys()) {
            *count.entry(e).or_default() += 1;
        }
    }
    for t in ir.tasks.values().filter(|t| t.span.file == file) {
        for e in t.emits.keys() {
            *count.entry(e).or_default() += 1;
        }
    }
    count
}

/// The name the module of its file gives event `event` of `owner` (a machine, or a task's name
/// capitalised): the event's own, or `<Owner><Event>` when the file declares the name twice.
fn event_ident(counts: &HashMap<&str, usize>, owner: &str, event: &str) -> String {
    if counts.get(event).copied().unwrap_or_default() > 1 {
        format!("{owner}{event}")
    } else {
        event.to_owned()
    }
}

/// A task's name with its first letter in upper case: `signIn` gives `SignIn`.
fn pascal(task: &str) -> String {
    let mut chars = task.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Every state id under `s`, `s` included, in document order. History states are left out:
/// they are never active, so host code never sees one.
fn state_ids(s: &State, out: &mut Vec<String>) {
    if s.kind.is_history() {
        return;
    }
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
        // A collection's children run its machine or task, through the same implementation as
        // an invoke of it.
        for c in m.children.values() {
            match c.kind {
                ChildrenKind::Machine => self.machines.insert(&c.src),
                ChildrenKind::Task => self.tasks.insert(&c.src),
            };
        }
        self.state(&m.root);
    }

    fn state(&mut self, s: &'a State) {
        for v in &s.vars {
            if let Some(e) = &v.init {
                self.expr(e);
            }
        }
        for e in s.entry.iter().chain(&s.exit) {
            self.effect(e);
        }
        if let Some(inv) = &s.invoke {
            match inv.kind {
                InvokeKind::Task => {
                    self.tasks.insert(&inv.src);
                }
                InvokeKind::Machine => {
                    self.machines.insert(&inv.src);
                }
                // A call runs nothing here: the callee is the run a `ref` holds.
                InvokeKind::Call => {}
            }
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
            for a in t.args.iter().flat_map(|a| &a.values) {
                self.expr(a);
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
            Effect::Send { key, value, .. } => {
                if let Some(k) = key {
                    self.expr(k);
                }
                self.expr(value);
            }
            Effect::Emit { value, .. } => self.expr(value),
            Effect::Spawn { key, args, .. } => {
                self.expr(key);
                for a in args {
                    self.expr(a);
                }
            }
            Effect::Stop { key, .. } => self.expr(key),
            Effect::Raise { value, delay, .. } => {
                self.expr(value);
                if let Some(d) = delay {
                    self.expr(d);
                }
            }
            Effect::Cancel { .. } => {}
            Effect::Reply { call, value, .. } => {
                self.expr(call);
                if let Some(v) = value {
                    self.expr(v);
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
    let mut out = Vec::new();
    match &decl.def {
        TypeDef::Record { params, fields } => {
            for f in fields {
                f.ty.for_each_name(&mut |n| out.push(n));
            }
            out.retain(|n| !params.iter().any(|p| p == n));
        }
        TypeDef::Error { fields } => {
            for f in fields {
                f.ty.for_each_name(&mut |n| out.push(n));
            }
        }
        TypeDef::Alias { params, ty } => {
            ty.for_each_name(&mut |n| out.push(n));
            out.retain(|n| !params.iter().any(|p| p == n));
        }
        TypeDef::Enum { .. } | TypeDef::Extern => {}
    }
    out
}

/// Whether a type has a `ref` in it, through aliases.
fn has_ref(ir: &Ir, t: &TypeRef, depth: usize) -> bool {
    if t.mentions_ref() {
        return true;
    }
    let mut found = false;
    t.for_each_name(&mut |n| {
        if let Some(TypeDef::Alias { ty, .. }) = ir.types.get(n).map(|d| &d.def) {
            found |= depth < 64 && has_ref(ir, ty, depth + 1);
        }
    });
    found
}

/// The schema of a generic record or alias: a function from the schemas of its type arguments,
/// named as its parameters. One on a cycle has its result annotated, since it calls itself, and
/// its body cast to that type: `tsc` cannot relate a struct of generic fields to the
/// recursive interface it builds, so the annotation, which callers are checked against, is the
/// schema's type.
fn generic_schema(name: &str, params: &[String], cycle: bool, body: &str) -> String {
    let args: Vec<String> = params.iter().map(|p| format!("{p}: {p}")).collect();
    let bounds: Vec<String> = params
        .iter()
        .map(|p| format!("{p} extends Schema.Top"))
        .collect();
    let annotation = if cycle {
        let view = |member: &str| {
            let v: Vec<String> = params
                .iter()
                .map(|p| format!("{p}[\"{member}\"]"))
                .collect();
            v
        };
        let services = |member: &str| view(member).join(" | ");
        format!(
            ": Schema.Codec<{name}<{}>, {name}<{}>, {}, {}>",
            view("Type").join(", "),
            view("Encoded").join(", "),
            services("DecodingServices"),
            services("EncodingServices")
        )
    } else {
        String::new()
    };
    let cast = if cycle { " as never" } else { "" };
    format!(
        "export const {name} = <{}>({}){annotation} =>\n  {}{cast}",
        bounds.join(", "),
        args.join(", "),
        body.replace('\n', "\n  ")
    )
}

/// For each source, the sources its declarations refer to: by a type or a `ref`, through the
/// signature of a function, action or task, or by a machine using or invoking one. The module
/// generated for a file imports only from the files it reaches through these, and its errors
/// map holds the error classes of exactly those files, so this graph tells which modules may
/// import which, and reads the same from the IR of any file that imports this one, directly or
/// not, as from its own. (A chart's imports are not in the IR, but an import nothing uses gives
/// the module nothing to import.)
fn file_refs(ir: &Ir) -> Vec<BTreeSet<u32>> {
    let mut out = vec![BTreeSet::new(); ir.sources.len()];
    let ty = |t: &TypeRef| {
        let mut files = Vec::new();
        t.for_each_name(&mut |n| files.extend(ir.types.get(n).map(|d| d.span.file)));
        refs_in(t, &mut |m| {
            files.extend(ir.machines.get(m).map(|d| d.span.file))
        });
        files
    };
    let fields = |fields: &[Field]| fields.iter().flat_map(|f| ty(&f.ty)).collect::<Vec<_>>();
    let errors = |errors: &[String]| {
        errors
            .iter()
            .filter_map(|e| ir.types.get(e).map(|d| d.span.file))
            .collect::<Vec<_>>()
    };
    let mut add = |file: u32, to: Vec<u32>| {
        if let Some(set) = out.get_mut(file as usize) {
            set.extend(to.into_iter().filter(|&t| t != file));
        }
    };
    for d in ir.types.values() {
        let to = match &d.def {
            TypeDef::Record { fields: f, .. } | TypeDef::Error { fields: f } => fields(f),
            TypeDef::Alias { ty: t, .. } => ty(t),
            TypeDef::Enum { .. } | TypeDef::Extern => Vec::new(),
        };
        add(d.span.file, to);
    }
    for d in ir.functions.values() {
        add(d.span.file, fields(&d.params));
        add(d.span.file, ty(&d.returns));
    }
    for d in ir.actions.values() {
        add(d.span.file, fields(&d.params));
    }
    for d in ir.tasks.values() {
        add(d.span.file, fields(&d.params));
        add(d.span.file, d.returns.iter().flat_map(ty).collect());
        add(d.span.file, errors(&d.errors));
        for e in d.emits.values() {
            add(d.span.file, fields(&e.fields));
        }
    }
    for m in ir.machines.values() {
        let file = m.span.file;
        add(file, fields(&m.params));
        add(file, m.returns.iter().flat_map(ty).collect());
        add(file, errors(&m.errors));
        add(file, m.vars.iter().flat_map(|v| ty(&v.ty)).collect());
        let mut locals = Vec::new();
        state_locals(&m.root, &mut |s| {
            locals.extend(fields(&s.params));
            locals.extend(s.vars.iter().flat_map(|v| ty(&v.ty)));
        });
        add(file, locals);
        for e in m.events.values().chain(m.emits.values()) {
            add(file, fields(&e.fields));
        }
        for c in m.calls.values() {
            add(file, fields(&c.params));
            add(file, c.returns.iter().flat_map(ty).collect());
            add(file, errors(&c.errors));
        }
        let mut uses = Uses::default();
        uses.machine(m);
        let used = (uses.fns.iter())
            .filter_map(|n| ir.functions.get(*n).map(|d| d.span.file))
            .chain(
                uses.actions
                    .iter()
                    .filter_map(|n| ir.actions.get(*n).map(|d| d.span.file)),
            )
            .chain(
                uses.tasks
                    .iter()
                    .filter_map(|n| ir.tasks.get(*n).map(|d| d.span.file)),
            )
            .chain(
                uses.machines
                    .iter()
                    .filter_map(|n| ir.machines.get(*n).map(|d| d.span.file)),
            );
        add(file, used.collect());
    }
    out
}

/// Calls `f` with `s` and every state below it.
fn state_locals<'a>(s: &'a State, f: &mut impl FnMut(&'a State)) {
    f(s);
    for c in &s.states {
        state_locals(c, f);
    }
}

/// Calls `f` with the machine of every `ref` in `t`.
fn refs_in(t: &TypeRef, f: &mut impl FnMut(&str)) {
    match t {
        TypeRef::Ref(m) => f(m),
        TypeRef::Generic(_, args) => args.iter().for_each(|a| refs_in(a, f)),
        TypeRef::Map(k, v) => {
            refs_in(k, f);
            refs_in(v, f);
        }
        TypeRef::List(e) | TypeRef::Optional(e) => refs_in(e, f),
        TypeRef::Builtin(_) | TypeRef::Named(_) | TypeRef::Call(_) => {}
    }
}

/// Which sources `from` reaches through `refs`, itself included.
fn reach(refs: &[BTreeSet<u32>], from: usize) -> Vec<bool> {
    let mut seen = vec![false; refs.len()];
    let mut stack = vec![from];
    while let Some(i) = stack.pop() {
        if i < seen.len() && !std::mem::replace(&mut seen[i], true) {
            stack.extend(refs[i].iter().map(|&j| j as usize));
        }
    }
    seen
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
                "the cycle spans several files; declare its types in one file",
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
    parts.extend(&to[common..]);
    if parts[0] != ".." {
        parts.insert(0, ".");
    }
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
        assert_eq!(relative("a/b.road", "../x/c.road"), "../../x/c.road.js");
        assert_eq!(relative("b.road", "../../x/c.road"), "../../x/c.road.js");
    }

    #[test]
    fn docs() {
        assert_eq!(jsdoc(""), "");
        assert_eq!(jsdoc("One."), "/** One. */\n");
        assert_eq!(jsdoc("A\n\nb */"), "/**\n * A\n *\n * b *\\/\n */\n");
    }
}
