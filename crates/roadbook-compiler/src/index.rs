//! The symbol index: every name a compilation declares, and every place a name is used, with
//! their spans. It is built while names are resolved, so it sees exactly what the compiler
//! sees: a use is indexed only where the compiler resolves it, and not at all in a file with
//! syntax errors (whose declarations are left out) or in a file of a compiled IR. Editors use
//! it for go to definition, find references, rename and hover.
//!
//! ```
//! use roadbook_compiler::{FileId, Options, SymbolKind, compile_source};
//!
//! let src = "machine M() {\n  initial A\n  state A { always -> B }\n  final state B {}\n}\n";
//! let out = compile_source("m.road", src, &Options::default());
//! let index = &out.index;
//! // The `B` of `-> B` is a use of the state `B`.
//! let at = src.find("-> B").unwrap() as u32 + 3;
//! let (id, span) = index.at(FileId(0), at).unwrap();
//! assert_eq!(index.symbol(id).kind, SymbolKind::State);
//! assert_eq!(index.symbol(id).signature, "final state M.B");
//! assert_eq!(&src[span.range()], "B");
//! assert_eq!(index.references_to(id).count(), 1);
//! ```

use std::collections::HashMap;

use roadbook_syntax::ast::{Decl, DeclKind, Field, Ident, TypeRef};
use roadbook_syntax::{FileId, Span};

use crate::types::ir_type;

/// Identifies a [`Symbol`] of one [`SymbolIndex`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SymbolId(pub u32);

/// What a symbol is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SymbolKind {
    /// `type Name { fields }`
    Record,
    /// `type Name = T`
    Alias,
    Enum,
    /// A variant of an enum; its parent is the enum.
    Variant,
    Error,
    /// `extern type Name`
    Extern,
    Function,
    Action,
    Task,
    Machine,
    /// A field of a record, an error, an event or an emitted event (its parent).
    Field,
    /// A parameter of a machine; its parent is the machine.
    Parameter,
    /// A `var` of a machine.
    Variable,
    /// An `event` a machine accepts.
    Event,
    /// An event a machine or a task `emits`; its parent is the machine or the task.
    EmittedEvent,
    /// A state; its parent is the state containing it, or the machine.
    State,
    /// The name of `invoke Machine(…) as name` or `invoke task(…) as name`; its parent is the
    /// invoking state.
    InvokeName,
    /// A `call` a machine answers; its parent is the machine, and its parameters are its
    /// members.
    Call,
    /// A type parameter of a generic record or alias (`T` in `type Page<T>`); its parent is
    /// the declaration.
    TypeParameter,
    /// A `children` collection of a machine, of machines or tasks; its parent is the machine.
    Children,
    /// The name of a delayed raise (`raise E after d as name`), declared at the machine's
    /// first raise of it; its other raises and each `cancel name` are its uses. Its parent is
    /// the machine.
    RaiseName,
}

impl SymbolKind {
    /// Whether the symbol is a top-level declaration, whose name is unique among the files
    /// compiled together and can be imported.
    pub fn is_declaration(self) -> bool {
        matches!(
            self,
            SymbolKind::Record
                | SymbolKind::Alias
                | SymbolKind::Enum
                | SymbolKind::Error
                | SymbolKind::Extern
                | SymbolKind::Function
                | SymbolKind::Action
                | SymbolKind::Task
                | SymbolKind::Machine
        )
    }

    /// Whether the grammar requires the name to start with an upper-case letter (types,
    /// errors, events, states and machines) rather than a lower-case one.
    pub fn is_upper_case(self) -> bool {
        matches!(
            self,
            SymbolKind::Record
                | SymbolKind::Alias
                | SymbolKind::Enum
                | SymbolKind::Variant
                | SymbolKind::Error
                | SymbolKind::Extern
                | SymbolKind::Machine
                | SymbolKind::Event
                | SymbolKind::EmittedEvent
                | SymbolKind::State
                | SymbolKind::TypeParameter
        )
    }

    /// How the kind is called in messages: "record type", "emitted event".
    pub fn describe(self) -> &'static str {
        match self {
            SymbolKind::Record => "record type",
            SymbolKind::Alias => "type alias",
            SymbolKind::Enum => "enum",
            SymbolKind::Variant => "enum variant",
            SymbolKind::Error => "error",
            SymbolKind::Extern => "extern type",
            SymbolKind::Function => "function",
            SymbolKind::Action => "action",
            SymbolKind::Task => "task",
            SymbolKind::Machine => "machine",
            SymbolKind::Field => "field",
            SymbolKind::Parameter => "parameter",
            SymbolKind::Variable => "variable",
            SymbolKind::Event => "event",
            SymbolKind::EmittedEvent => "emitted event",
            SymbolKind::State => "state",
            SymbolKind::InvokeName => "invoke's `as` name",
            SymbolKind::Call => "call",
            SymbolKind::TypeParameter => "type parameter",
            SymbolKind::Children => "children collection",
            SymbolKind::RaiseName => "delayed raise",
        }
    }
}

/// A declared name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Symbol {
    pub kind: SymbolKind,
    pub name: String,
    /// The name in its declaration.
    pub span: Span,
    /// The whole declaration, doc comment excluded.
    pub decl_span: Span,
    /// What the symbol is declared in: the record, error or event of a field, the enum of a
    /// variant, the machine of a parameter, variable or event, the state or machine
    /// containing a state, the invoking state of an `as` name, the machine of a call, the call
    /// of a call's parameter. `None` for a top-level
    /// declaration.
    pub parent: Option<SymbolId>,
    /// Its `///` comment, without the slashes.
    pub doc: Option<String>,
    /// The declaration in one line, as the formatter would write it without its body:
    /// `task chargeCard(order: Order) -> Receipt ! CardDeclined`, `var attempts: int`,
    /// `final state Checkout.Charged`.
    pub signature: String,
}

/// A use of a symbol.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Reference {
    pub symbol: SymbolId,
    /// The name as written at the use: one segment of a dotted target, the name in an
    /// import list.
    pub span: Span,
}

/// The symbols of a compilation and their uses. Spans use the compilation's file ids
/// ([`Compilation::files`](crate::Compilation::files)).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SymbolIndex {
    symbols: Vec<Symbol>,
    references: Vec<Reference>,
}

impl SymbolIndex {
    /// Every symbol, in the order the compiler met them.
    pub fn symbols(&self) -> &[Symbol] {
        &self.symbols
    }

    pub fn symbol(&self, id: SymbolId) -> &Symbol {
        &self.symbols[id.0 as usize]
    }

    /// The symbols with their ids.
    pub fn iter(&self) -> impl Iterator<Item = (SymbolId, &Symbol)> {
        self.symbols
            .iter()
            .enumerate()
            .map(|(i, s)| (SymbolId(i as u32), s))
    }

    /// Every use, by file and position.
    pub fn references(&self) -> &[Reference] {
        &self.references
    }

    /// The uses of a symbol, not counting its declaration.
    pub fn references_to(&self, id: SymbolId) -> impl Iterator<Item = Span> + '_ {
        self.references
            .iter()
            .filter(move |r| r.symbol == id)
            .map(|r| r.span)
    }

    /// The symbol whose declared name or use covers byte `offset` of `file`, with the span of
    /// that name. The end of a name counts as inside it, so a cursor just after a name finds
    /// it.
    pub fn at(&self, file: FileId, offset: u32) -> Option<(SymbolId, Span)> {
        let covers =
            |s: &Span| s.file == file && s.start.offset <= offset && offset <= s.end.offset;
        // A use is preferred: the names of a compiled IR's declarations share one span.
        self.references
            .iter()
            .find(|r| covers(&r.span))
            .map(|r| (r.symbol, r.span))
            .or_else(|| {
                self.iter()
                    .find(|(_, s)| covers(&s.span) && s.span.start.offset < s.span.end.offset)
                    .map(|(id, s)| (id, s.span))
            })
    }

    /// The name with its parents', dot-separated: `Checkout.Charging`, `Order.id`,
    /// `Upload.Progress.percent`.
    pub fn qualified_name(&self, id: SymbolId) -> String {
        let s = self.symbol(id);
        match s.parent {
            Some(p) => format!("{}.{}", self.qualified_name(p), s.name),
            None => s.name.clone(),
        }
    }

    pub(crate) fn define(&mut self, symbol: Symbol) -> SymbolId {
        self.symbols.push(symbol);
        SymbolId(self.symbols.len() as u32 - 1)
    }

    pub(crate) fn refer(&mut self, symbol: SymbolId, span: Span) {
        self.references.push(Reference { symbol, span });
    }

    /// Removes repeated uses (a type resolved twice) and sorts them by position.
    pub(crate) fn finish(&mut self) {
        self.references.sort_by_key(|r| {
            (
                r.span.file,
                r.span.start.offset,
                r.span.end.offset,
                r.symbol,
            )
        });
        self.references.dedup();
    }

    /// Adds the symbols and uses of another compilation, whose file ids `map` turns into this
    /// one's. A symbol both have (one declared in a file both compiled) is kept once.
    pub(crate) fn merge(&mut self, other: SymbolIndex, map: &[FileId]) {
        type Key = (SymbolKind, String, FileId, u32, u32, u32);
        let key = |s: &Symbol| -> Key {
            (
                s.kind,
                s.name.clone(),
                s.span.file,
                s.span.start.offset,
                s.span.start.line,
                s.span.start.col,
            )
        };
        let mut known: HashMap<Key, SymbolId> = self.iter().map(|(id, s)| (key(s), id)).collect();
        let remap = |s: &mut Span| s.file = map[s.file.0 as usize];
        let mut ids: Vec<SymbolId> = Vec::with_capacity(other.symbols.len());
        for mut s in other.symbols {
            remap(&mut s.span);
            remap(&mut s.decl_span);
            // A parent comes before its children, so it is already mapped.
            s.parent = s.parent.map(|p| ids[p.0 as usize]);
            let id = match known.get(&key(&s)) {
                Some(&id) => id,
                None => {
                    let k = key(&s);
                    let id = self.define(s);
                    known.insert(k, id);
                    id
                }
            };
            ids.push(id);
        }
        for mut r in other.references {
            remap(&mut r.span);
            r.symbol = ids[r.symbol.0 as usize];
            self.references.push(r);
        }
        self.finish();
    }
}

/// `name: T`, as in a parameter list or a record.
pub(crate) fn field_text(f: &Field) -> String {
    let text = field_text_of(&f.name, &f.ty);
    if f.sensitive {
        format!("sensitive {text}")
    } else {
        text
    }
}

pub(crate) fn field_text_of(name: &Ident, ty: &TypeRef) -> String {
    format!("{}: {}", name.name, type_text(ty))
}

/// A type as the formatter writes it: `Order[]`, `ref Upload`, `string?`.
pub fn type_text(t: &TypeRef) -> String {
    ir_type(t).to_string()
}

pub(crate) fn list(fields: &[Field]) -> String {
    fields.iter().map(field_text).collect::<Vec<_>>().join(", ")
}

/// The braces of a record, error or event: ` { a: T, b: U }`, or nothing without fields.
pub(crate) fn braces(fields: &[Field]) -> String {
    if fields.is_empty() {
        String::new()
    } else {
        format!(" {{ {} }}", list(fields))
    }
}

pub(crate) fn result_text(returns: Option<&TypeRef>, errors: &[Ident]) -> String {
    let mut s = String::new();
    if let Some(r) = returns {
        s.push_str(&format!(" -> {}", type_text(r)));
    }
    if !errors.is_empty() {
        let names: Vec<&str> = errors.iter().map(|e| e.name.as_str()).collect();
        s.push_str(&format!(" ! {}", names.join(" | ")));
    }
    s
}

/// `<T, U>`, or nothing for a declaration that is not generic.
fn type_params(params: &[Ident]) -> String {
    if params.is_empty() {
        return String::new();
    }
    let names: Vec<&str> = params.iter().map(|p| p.name.as_str()).collect();
    format!("<{}>", names.join(", "))
}

/// The kind and one-line signature of a top-level declaration, as in [`Symbol::signature`].
pub fn declaration(d: &Decl) -> (SymbolKind, String) {
    let n = &d.name.name;
    match &d.kind {
        DeclKind::Record { params, fields } => (
            SymbolKind::Record,
            format!("type {n}{}{}", type_params(params), braces(fields)),
        ),
        DeclKind::Alias { params, target } => (
            SymbolKind::Alias,
            format!("type {n}{} = {}", type_params(params), type_text(target)),
        ),
        DeclKind::Enum { variants } => {
            let vs: Vec<&str> = variants.iter().map(|v| v.name.as_str()).collect();
            (
                SymbolKind::Enum,
                format!("enum {n} {{ {} }}", vs.join(", ")),
            )
        }
        DeclKind::Error { fields } => (SymbolKind::Error, format!("error {n}{}", braces(fields))),
        DeclKind::Extern => (SymbolKind::Extern, format!("extern type {n}")),
        DeclKind::Fn { params, returns } => (
            SymbolKind::Function,
            format!("fn {n}({}) -> {}", list(params), type_text(returns)),
        ),
        DeclKind::Action { params } => {
            (SymbolKind::Action, format!("action {n}({})", list(params)))
        }
        DeclKind::Task {
            params,
            returns,
            errors,
            emits,
        } => {
            // As the formatter writes it: one `emits` on the signature's line, several on
            // lines of their own.
            let emits: Vec<String> = emits
                .iter()
                .map(|e| format!("emits {}{}", e.name.name, braces(&e.fields)))
                .collect();
            let emits = match emits.as_slice() {
                [] => String::new(),
                [one] => format!(" {one}"),
                several => several.iter().map(|e| format!("\n  {e}")).collect(),
            };
            (
                SymbolKind::Task,
                format!(
                    "task {n}({}){}{emits}",
                    list(params),
                    result_text(returns.as_ref(), errors)
                ),
            )
        }
        DeclKind::Machine(m) => (
            SymbolKind::Machine,
            format!(
                "{}machine {n}({}){}",
                if m.strict { "strict " } else { "" },
                list(&m.params),
                result_text(m.returns.as_ref(), &m.errors)
            ),
        ),
    }
}
