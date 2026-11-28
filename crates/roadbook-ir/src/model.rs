//! The IR document and its declarations, machines and states.

use std::borrow::Cow;

use indexmap::IndexMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::expr::Expr;
use crate::types::TypeRef;

/// One IR document: everything the compiler produced for one entry file and
/// the files it imports.
///
/// The maps are keyed by declaration name and keep declaration order (entry
/// file first, then imports in the order they were loaded, each file's
/// declarations in source order). Declaration names are unique across a
/// project, so a name identifies a declaration without its kind.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Ir {
    /// The IR version, `"MAJOR.MINOR"`. See [`crate::check_version`].
    pub ir_version: String,
    /// Source files. Spans refer to them by index. The entry file is first.
    pub sources: Vec<Source>,
    /// Records, aliases, enums, errors and extern types.
    pub types: IndexMap<String, TypeDecl>,
    /// Pure host functions (`fn`).
    pub functions: IndexMap<String, FunctionDecl>,
    /// Host actions (`action`).
    pub actions: IndexMap<String, ActionDecl>,
    /// Host tasks (`task`).
    pub tasks: IndexMap<String, TaskDecl>,
    /// Machines.
    pub machines: IndexMap<String, Machine>,
}

impl Ir {
    /// `t` with aliases expanded at the top, transitively, a generic alias's
    /// arguments substituted into its target: `Money` for `type Money = int`
    /// is `int`, `S` for `type S = string?` is `string?`, and `Dict<Order>` for
    /// `type Dict<T> = map<string, T>` is `map<string, Order>`. Anything else
    /// (a record, an instance of a generic record, a list) is returned as it
    /// is. The compiler rejects alias cycles; the bound only keeps a
    /// hand-written IR from looping.
    pub fn expand_top<'a>(&'a self, t: &'a TypeRef) -> Cow<'a, TypeRef> {
        let mut t = Cow::Borrowed(t);
        for _ in 0..64 {
            let next = match &*t {
                TypeRef::Named(n) => match self.types.get(n).map(|d| &d.def) {
                    Some(TypeDef::Alias { params, ty }) if params.is_empty() => Cow::Borrowed(ty),
                    _ => break,
                },
                TypeRef::Generic(n, args) => match self.types.get(n).map(|d| &d.def) {
                    Some(TypeDef::Alias { params, ty }) => Cow::Owned(ty.substitute(params, args)),
                    _ => break,
                },
                _ => break,
            };
            t = next;
        }
        t
    }

    /// An alias that refers to itself, directly or through other aliases (anywhere in its
    /// target: in a type argument, a map, a list), if there is one. The compiler never writes
    /// one; a reader refuses an IR that has one, since expanding it would not end.
    pub fn alias_cycle(&self) -> Option<&str> {
        let aliases_in = |name: &str| -> Vec<&str> {
            let mut out = Vec::new();
            if let Some(TypeDef::Alias { ty, .. }) = self.types.get(name).map(|d| &d.def) {
                ty.for_each_name(&mut |n| {
                    if let Some((key, d)) = self.types.get_key_value(n)
                        && matches!(d.def, TypeDef::Alias { .. })
                    {
                        out.push(key.as_str());
                    }
                });
            }
            out
        };
        // Depth first, colouring each alias once: grey while on the path, black when done.
        let mut done: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for start in self.types.keys() {
            if done.contains(start.as_str()) {
                continue;
            }
            let mut path: Vec<(&str, Vec<&str>)> = vec![(start, aliases_in(start))];
            let mut on_path: std::collections::HashSet<&str> = [start.as_str()].into();
            while let Some((name, next)) = path.last_mut() {
                match next.pop() {
                    Some(n) if on_path.contains(n) => return Some(n),
                    Some(n) if !done.contains(n) => {
                        on_path.insert(n);
                        let more = aliases_in(n);
                        path.push((n, more));
                    }
                    Some(_) => {}
                    None => {
                        let name = *name;
                        on_path.remove(name);
                        done.insert(name);
                        path.pop();
                    }
                }
            }
        }
        None
    }

    /// The fields of `t` if it is (through aliases) a record or error type,
    /// with the arguments of an instance of a generic record substituted:
    /// its name, whether it is an error, and the fields.
    pub fn fields_of<'a>(&'a self, t: &'a TypeRef) -> Option<(String, bool, Cow<'a, [Field]>)> {
        let (name, args): (String, Vec<TypeRef>) = match self.expand_top(t).into_owned() {
            TypeRef::Named(n) => (n, Vec::new()),
            TypeRef::Generic(n, args) => (n, args),
            _ => return None,
        };
        match &self.types.get(&name)?.def {
            TypeDef::Record { params, fields } if params.is_empty() => {
                Some((name, false, Cow::Borrowed(fields)))
            }
            TypeDef::Record { params, fields } => {
                let fields = fields
                    .iter()
                    .map(|f| Field {
                        name: f.name.clone(),
                        ty: f.ty.substitute(params, &args),
                        sensitive: f.sensitive,
                        doc: f.doc.clone(),
                    })
                    .collect();
                Some((name, false, Cow::Owned(fields)))
            }
            TypeDef::Error { fields } => Some((name, true, Cow::Borrowed(fields))),
            _ => None,
        }
    }
}

/// A source file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    /// With `/` separators: relative to the project root for a file of the
    /// project, relative to the package's root for a file of another package.
    pub path: String,
    /// The package the file belongs to (`inventory`, `@shop/inventory`), or
    /// `None` for a file of the project itself. Every declaration whose span
    /// is in such a file comes from that package.
    pub package: Option<String>,
}

/// A source range. Lines and columns are 1-based; columns count Unicode
/// scalar values (a tab is one column). The end is exclusive: it is the
/// position just after the last character of the range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Span {
    /// Index into [`Ir::sources`].
    pub file: u32,
    /// First line.
    pub line: u32,
    /// First column.
    pub col: u32,
    /// Line of the end position.
    pub end_line: u32,
    /// Column of the end position (exclusive).
    pub end_col: u32,
}

/// A named, typed slot: a record or error field, an event field or a
/// parameter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Field {
    /// The name (lower case).
    pub name: String,
    /// Its type.
    #[serde(rename = "type")]
    pub ty: TypeRef,
    /// Written `sensitive`: its value is redacted in trace records.
    pub sensitive: bool,
    /// Doc comment, `""` if none.
    pub doc: String,
}

/// A declaration in `types`.
#[derive(Debug, Clone, PartialEq)]
pub struct TypeDecl {
    /// What kind of type it is.
    pub def: TypeDef,
    /// Doc comment, `""` if none.
    pub doc: String,
    /// The whole declaration.
    pub span: Span,
}

/// The kinds of type declaration. In JSON the kind is the `kind` member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeDef {
    /// `type Name<params> { fields }`:
    /// `{"kind": "record", "params": [string], "fields": [Field]}`.
    Record {
        /// Type parameters, in source order. Empty unless the type is generic.
        params: Vec<String>,
        /// Fields, in source order. Their types may name the parameters.
        fields: Vec<Field>,
    },
    /// `type Name<params> = T`: `{"kind": "alias", "params": [string], "type": TypeRef}`.
    Alias {
        /// Type parameters, in source order. Empty unless the alias is generic.
        params: Vec<String>,
        /// The aliased type. It may name the parameters.
        ty: TypeRef,
    },
    /// `enum Name { A, B }`: `{"kind": "enum", "variants": [string]}`.
    Enum {
        /// Variant names, in source order.
        variants: Vec<String>,
    },
    /// `error Name { fields }`: `{"kind": "error", "fields": [Field]}`.
    Error {
        /// Fields, in source order. Empty for a bare `error Name`.
        fields: Vec<Field>,
    },
    /// `extern type Name`: `{"kind": "extern"}`.
    Extern,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum TypeDeclSer<'a> {
    Record {
        params: &'a [String],
        fields: &'a [Field],
        doc: &'a str,
        span: &'a Span,
    },
    Alias {
        params: &'a [String],
        #[serde(rename = "type")]
        ty: &'a TypeRef,
        doc: &'a str,
        span: &'a Span,
    },
    Enum {
        variants: &'a [String],
        doc: &'a str,
        span: &'a Span,
    },
    Error {
        fields: &'a [Field],
        doc: &'a str,
        span: &'a Span,
    },
    Extern {
        doc: &'a str,
        span: &'a Span,
    },
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
enum TypeDeclDe {
    Record {
        params: Vec<String>,
        fields: Vec<Field>,
        doc: String,
        span: Span,
    },
    Alias {
        params: Vec<String>,
        #[serde(rename = "type")]
        ty: TypeRef,
        doc: String,
        span: Span,
    },
    Enum {
        variants: Vec<String>,
        doc: String,
        span: Span,
    },
    Error {
        fields: Vec<Field>,
        doc: String,
        span: Span,
    },
    Extern {
        doc: String,
        span: Span,
    },
}

impl Serialize for TypeDecl {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let (doc, span) = (self.doc.as_str(), &self.span);
        match &self.def {
            TypeDef::Record { params, fields } => TypeDeclSer::Record {
                params,
                fields,
                doc,
                span,
            },
            TypeDef::Alias { params, ty } => TypeDeclSer::Alias {
                params,
                ty,
                doc,
                span,
            },
            TypeDef::Enum { variants } => TypeDeclSer::Enum {
                variants,
                doc,
                span,
            },
            TypeDef::Error { fields } => TypeDeclSer::Error { fields, doc, span },
            TypeDef::Extern => TypeDeclSer::Extern { doc, span },
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for TypeDecl {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<TypeDecl, D::Error> {
        let (def, doc, span) = match TypeDeclDe::deserialize(deserializer)? {
            TypeDeclDe::Record {
                params,
                fields,
                doc,
                span,
            } => (TypeDef::Record { params, fields }, doc, span),
            TypeDeclDe::Alias {
                params,
                ty,
                doc,
                span,
            } => (TypeDef::Alias { params, ty }, doc, span),
            TypeDeclDe::Enum {
                variants,
                doc,
                span,
            } => (TypeDef::Enum { variants }, doc, span),
            TypeDeclDe::Error { fields, doc, span } => (TypeDef::Error { fields }, doc, span),
            TypeDeclDe::Extern { doc, span } => (TypeDef::Extern, doc, span),
        };
        Ok(TypeDecl { def, doc, span })
    }
}

/// `fn name(params) -> T`: a pure, synchronous host function.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FunctionDecl {
    /// Parameters, in order.
    pub params: Vec<Field>,
    /// Result type.
    pub returns: TypeRef,
    /// Doc comment, `""` if none.
    pub doc: String,
    /// The whole declaration.
    pub span: Span,
}

/// `action name(params)`: a host side effect that cannot fail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionDecl {
    /// Parameters, in order.
    pub params: Vec<Field>,
    /// Doc comment, `""` if none.
    pub doc: String,
    /// The whole declaration.
    pub span: Span,
}

/// `task name(params) [-> T] [! E | …] [emits E { fields } …]`: effectful, fallible,
/// cancellable host work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskDecl {
    /// Parameters, in order.
    pub params: Vec<Field>,
    /// Result type, `null` if the task returns nothing.
    pub returns: Option<TypeRef>,
    /// Declared errors (names of `error` types), in source order.
    pub errors: Vec<String>,
    /// Events the task emits while it runs (`emits Name { fields }`), keyed by name, in
    /// source order, `{}` if none. They are in the task's own namespace: their type is
    /// `task.Name`.
    pub emits: IndexMap<String, EventDecl>,
    /// Doc comment, `""` if none.
    pub doc: String,
    /// The whole declaration.
    pub span: Span,
}

/// A machine: a statechart with a task's signature.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Machine {
    /// Doc comment, `""` if none.
    pub doc: String,
    /// The whole declaration, from `machine` (or `strict`) to the closing brace.
    pub span: Span,
    /// Written `strict machine`: one of its own events or messages that
    /// enables no transition is an `unhandled` defect instead of being dropped.
    pub strict: bool,
    /// Parameters, in order. They are read-only variables.
    pub params: Vec<Field>,
    /// Result type, `null` if the machine returns nothing.
    pub returns: Option<TypeRef>,
    /// Declared errors, in source order.
    pub errors: Vec<String>,
    /// Machine variables, in source order.
    pub vars: Vec<Var>,
    /// Events the machine accepts, keyed by name, in source order.
    pub events: IndexMap<String, EventDecl>,
    /// Events only the machine itself raises (`internal event Name { fields }`), keyed by
    /// name, in source order. Host code and other runs cannot send them, and they are not
    /// part of the machine's interface. No name is in two of `events`, `internal_events`
    /// and `emits`.
    #[serde(rename = "internalEvents")]
    pub internal_events: IndexMap<String, EventDecl>,
    /// Events the machine emits (`emits Name { fields }`), keyed by name, in
    /// source order. No name is both in `events` and in `emits`.
    pub emits: IndexMap<String, EventDecl>,
    /// Calls the machine answers (`call name(params) -> T ! E`), keyed by their
    /// lower-case name, in source order.
    pub calls: IndexMap<String, Call>,
    /// The keyed collections of spawned children (`children name: Machine by
    /// Key`), keyed by name, in source order.
    pub children: IndexMap<String, ChildrenDecl>,
    /// The root state. Its `id` and `name` are the machine's name, it is
    /// `compound`, and it holds the `entry`, `exit`, `on`, `after` and
    /// `always` items written directly in the machine body.
    pub root: State,
    /// `"sha256:"` and 64 lowercase hex digits. See [`crate::machine_hash`].
    pub hash: String,
}

/// `[sensitive] var name: T [= expr]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Var {
    /// The name.
    pub name: String,
    /// Its type.
    #[serde(rename = "type")]
    pub ty: TypeRef,
    /// Written `sensitive var`: its value is redacted in trace records.
    pub sensitive: bool,
    /// Initial value, `null` if the variable starts unassigned.
    pub init: Option<Expr>,
    /// Doc comment, `""` if none.
    pub doc: String,
    /// The whole declaration.
    pub span: Span,
}

/// `event Name [{ fields }]`, or `emits Name [{ fields }]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventDecl {
    /// The event's payload fields, in source order.
    pub fields: Vec<Field>,
    /// Doc comment, `""` if none.
    pub doc: String,
    /// The whole declaration.
    pub span: Span,
}

/// `call name(params) [-> T] [! E | …]`: a request another run makes of the
/// machine with `invoke x.name(…)`, taken with `on name` and answered with
/// `reply`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Call {
    /// Parameters, in order: the fields of `event` in `on name`.
    pub params: Vec<Field>,
    /// Result type, `null` if the call returns nothing.
    pub returns: Option<TypeRef>,
    /// Declared errors (names of `error` types), in source order. Never
    /// [`UNANSWERED`], which every call can fail with.
    pub errors: Vec<String>,
    /// Doc comment, `""` if none.
    pub doc: String,
    /// The whole declaration, from `call` to its end.
    pub span: Span,
}

/// The built-in error every call can fail with besides its declared ones: the
/// callee ended, or left the call unanswered. It has no fields and is never a
/// key of [`Ir::types`].
pub const UNANSWERED: &str = "Unanswered";

/// The built-in enum of how a peer's run ended, the type of `event.status`
/// in an `on name.ended` transition. Unlike [`UNANSWERED`] it is a key of
/// [`Ir::types`], an ordinary `enum` with [`ENDED_VARIANTS`], whenever the
/// document names it, so readers need nothing special for it.
pub const ENDED: &str = "Ended";

/// The variants of [`ENDED`], in order.
pub const ENDED_VARIANTS: [&str; 5] = ["Done", "Failed", "Cancelled", "Defect", "Suspended"];

/// What a `children` collection holds: machines or tasks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChildrenKind {
    /// Child machines (`children name: Machine by Key`).
    Machine,
    /// Host tasks (`children name: task by Key`).
    Task,
}

/// `children name: Machine by Key` or `children name: task by Key`: a keyed collection of
/// spawned children, machines or tasks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildrenDecl {
    /// Whether the children are machines or tasks.
    pub kind: ChildrenKind,
    /// What the children run: a key of [`Ir::machines`] or of [`Ir::tasks`], by `kind`.
    pub src: String,
    /// The key type: `string`, `int` or an enum, or an alias of one.
    pub key: TypeRef,
    /// Doc comment, `""` if none.
    pub doc: String,
    /// From `children` to the end of the key type.
    pub span: Span,
}

/// The kinds of state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StateKind {
    /// No children.
    Atomic,
    /// Children, one active at a time, entered at `initial`.
    Compound,
    /// Children (regions), all active at once.
    Parallel,
    /// `final state`.
    Final,
    /// `history Name`: a shallow history pseudo-state of its parent.
    History,
    /// `deep history Name`: a deep history pseudo-state of its parent.
    #[serde(rename = "deepHistory")]
    DeepHistory,
}

impl StateKind {
    /// Whether this is a history pseudo-state, `history` or `deepHistory`: never active, and
    /// never one of its parent's children in the sense of `spec/execution.md`.
    pub fn is_history(self) -> bool {
        matches!(self, StateKind::History | StateKind::DeepHistory)
    }
}

/// A state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    /// Absolute dotted path from the machine: `PlaceOrder.Processing.Paying`.
    pub id: String,
    /// The last segment of `id`.
    pub name: String,
    /// What kind of state it is.
    pub kind: StateKind,
    /// Doc comment, `""` if none.
    pub doc: String,
    /// The whole state, from its first keyword to the closing brace.
    pub span: Span,
    /// Id of the child entered first; set exactly when `kind` is `compound`.
    pub initial: Option<String>,
    /// Child states, in source order.
    pub states: Vec<State>,
    /// Effects run on entry, in source order.
    pub entry: Vec<Effect>,
    /// Effects run on exit, in source order.
    pub exit: Vec<Effect>,
    /// The tasks and machines run while the state is active, in the order written. When
    /// there are several, each has an `as`, and no two the same.
    pub invokes: Vec<Invoke>,
    /// Transitions, in document order. Traces refer to them by index.
    pub transitions: Vec<Transition>,
    /// A top-level final state's result, if it has one.
    pub output: Option<Expr>,
    /// A top-level final state's failure, if it has one.
    pub fail: Option<Expr>,
}

impl State {
    /// The invoke whose `as` is `alias`: what an `invoke.done` or `invoke.fail` trigger's
    /// `as`, or a `child` or `task` address's `name`, names. `None` names a state's one
    /// invoke when it has no `as`.
    pub fn invoke_named(&self, alias: Option<&str>) -> Option<&Invoke> {
        self.invokes.iter().find(|i| i.alias.as_deref() == alias)
    }

    /// Whether the state has more than one invoke, so that its snapshot entries and
    /// `invoke.started` records carry `as`.
    pub fn has_several_invokes(&self) -> bool {
        self.invokes.len() > 1
    }
}

/// Whether an invoke runs a task or a machine, or calls a peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InvokeKind {
    /// A host task, a key of [`Ir::tasks`].
    Task,
    /// A child machine, a key of [`Ir::machines`].
    Machine,
    /// A call of the run a `ref` holds: a key of the [`Machine::calls`] of
    /// [`Invoke::to`]'s machine.
    Call,
}

/// `invoke src(args) { outcomes }`. The outcomes are transitions of the
/// state, with `invoke.done` and `invoke.fail` triggers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Invoke {
    /// Task or machine.
    pub kind: InvokeKind,
    /// Its name: the task's, the machine's or the call's.
    pub src: String,
    /// For a call, the callee: an address of kind `ref`. `None` for a task or
    /// a machine.
    pub to: Option<Address>,
    /// The name given with `invoke Machine(…) as name` or `invoke task(…) as name`,
    /// through which the state and its descendants send to the child and receive what
    /// it or the task emits, and by which its outcomes' triggers name it. `None` without
    /// `as`. In a state with one invoke, always `None` for a call and for a task that
    /// declares no `emits`; in a state with several, always set.
    #[serde(rename = "as")]
    pub alias: Option<String>,
    /// Arguments, positional, in parameter order.
    pub args: Vec<Expr>,
    /// From `invoke` to the closing brace.
    pub span: Span,
}

impl Invoke {
    /// What is invoked, as written after `invoke`: the task's or machine's name, or for a
    /// call `inventory.reserve`.
    pub fn callee(&self) -> String {
        match &self.to {
            Some(to) => format!("{}.{}", to.name, self.src),
            None => self.src.clone(),
        }
    }
}

/// Whether an [`Address`] names an invoked child, a `ref` value, a children collection or
/// an invoked task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AddressKind {
    /// The child machine of an `invoke … as name`.
    Child,
    /// A parameter or variable of type `ref Machine`.
    Ref,
    /// The spawned children of a `children` collection, machines or tasks.
    Children,
    /// The task of an `invoke task(…) as name`. Only in `emitted` triggers.
    Task,
}

/// Another running machine or a running task, as a `send` names its receiver, an
/// `on name.Event` trigger its sender and a call invoke its callee.
///
/// It names its other end by `machine` (kinds `child`, `ref`, and `children` of a machine
/// collection) or by `task` (kind `task`, and `children` of a task collection): exactly one
/// of the two is set, and in JSON the other member is absent.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields, try_from = "AddressDe")]
pub struct Address {
    /// A child, a ref, a collection or a task.
    pub kind: AddressKind,
    /// The `as` name, the parameter or variable, or the collection.
    pub name: String,
    /// The machine at the other end, a key of [`Ir::machines`], or `None` for a task.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
    /// The task at the other end, a key of [`Ir::tasks`], or `None` for a machine.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// For a child or a task, the id of the state whose invoke is named `name`: the
    /// innermost such state containing the item. `None` for a ref or a
    /// collection.
    pub state: Option<String>,
}

impl Address {
    /// The name of the machine or task at the other end.
    pub fn peer(&self) -> &str {
        self.machine
            .as_deref()
            .or(self.task.as_deref())
            .unwrap_or_default()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddressDe {
    kind: AddressKind,
    name: String,
    #[serde(default)]
    machine: Option<String>,
    #[serde(default)]
    task: Option<String>,
    state: Option<String>,
}

impl TryFrom<AddressDe> for Address {
    type Error = String;

    fn try_from(a: AddressDe) -> Result<Address, String> {
        let task_kind = a.kind == AddressKind::Task;
        match (&a.machine, &a.task) {
            (Some(_), None) if !task_kind => {}
            (None, Some(_)) if task_kind || a.kind == AddressKind::Children => {}
            _ => {
                return Err(format!(
                    "address `{}` of kind `{}` must name exactly one of `machine` and `task`, by its kind",
                    a.name,
                    match a.kind {
                        AddressKind::Child => "child",
                        AddressKind::Ref => "ref",
                        AddressKind::Children => "children",
                        AddressKind::Task => "task",
                    }
                ));
            }
        }
        Ok(Address {
            kind: a.kind,
            name: a.name,
            machine: a.machine,
            task: a.task,
            state: a.state,
        })
    }
}

/// A transition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transition {
    /// What enables it.
    pub trigger: Trigger,
    /// Index of the trigger block it was written in, among the blocks of
    /// its state, counting from 0. Branches of one block share it.
    pub group: u32,
    /// The guard, `null` if unguarded (including `else`).
    pub guard: Option<Expr>,
    /// Target state ids, in the order written: `[]` for a transition with no
    /// target, more than one for a transition into several regions of a
    /// parallel state. With no target and no effects, the transition is an
    /// `ignore`.
    pub targets: Vec<String>,
    /// Whether the transition is internal (`-> .X`): its domain is its own
    /// state, which it does not leave. `false` for one with no target.
    pub internal: bool,
    /// Effects: every `set` assignment, then the `do`, `send`, `emit` and
    /// `reply` effects in the order written.
    pub effects: Vec<Effect>,
    /// The branch: the whole line for a single-branch trigger, from `if` or
    /// `else` for a branch in a block.
    pub span: Span,
}

/// What enables a transition. In JSON the kind is the `kind` member.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum Trigger {
    /// `on Name`: an external event declared by the machine.
    #[serde(rename = "event")]
    Event {
        /// The event's name, a key of [`Machine::events`].
        event: String,
    },
    /// `on name.Event`: an event emitted by an invoked child, by the
    /// machine a `ref` holds, by a spawned child or by an invoked or spawned task.
    #[serde(rename = "emitted")]
    Emitted {
        /// The sender.
        from: Address,
        /// The event's name, a key of the sender's [`Machine::emits`] or
        /// [`TaskDecl::emits`].
        event: String,
    },
    /// `on *`: any event the state has no `on` for. Only the machine's own
    /// events: never an emitted one.
    #[serde(rename = "any")]
    Any,
    /// `after delay`: the state has been active for `delay`.
    #[serde(rename = "after")]
    After {
        /// A `duration` expression, evaluated on entry.
        delay: Expr,
    },
    /// `always`: no event; taken as soon as the guard holds.
    #[serde(rename = "always")]
    Always,
    /// `done`: the compound state reached a final child, or every region of
    /// the parallel state did.
    #[serde(rename = "done")]
    Done,
    /// `done` inside `invoke { }`: the invoked task or machine succeeded.
    #[serde(rename = "invoke.done")]
    InvokeDone {
        /// The `as` of the invoke it belongs to, one of the state's `invokes`; `None` for
        /// a state's one invoke when it has no `as`.
        #[serde(rename = "as")]
        alias: Option<String>,
    },
    /// `fail E` inside `invoke { }`: it failed with error `E`.
    #[serde(rename = "invoke.fail")]
    InvokeFail {
        /// The `as` of the invoke it belongs to, as for [`Trigger::InvokeDone`].
        #[serde(rename = "as")]
        alias: Option<String>,
        /// The error's name, or `"*"` for the declared errors not listed. For
        /// a call, it may be [`UNANSWERED`], and `"*"` covers it too.
        error: String,
    },
    /// `on name`: a call of the machine's own call `name`, from another run.
    #[serde(rename = "call")]
    Call {
        /// The call's name, a key of [`Machine::calls`].
        call: String,
    },
    /// `on name.done`: a spawned child (or task) of the collection ended `done`.
    #[serde(rename = "spawn.done")]
    SpawnDone {
        /// The collection, a key of [`Machine::children`].
        children: String,
    },
    /// `on name.fail E`: a spawned child of the collection failed with `E`.
    #[serde(rename = "spawn.fail")]
    SpawnFail {
        /// The collection, a key of [`Machine::children`].
        children: String,
        /// The error's name, or `"*"` for the declared errors the state has
        /// no `on name.fail` for.
        error: String,
    },
    /// `on name.ended`: the run a `ref` parameter or variable holds closed.
    /// `event.status` is in scope, of the built-in enum [`ENDED`].
    #[serde(rename = "ended")]
    Ended {
        /// The peer, an address of kind `ref`.
        from: Address,
    },
}

/// An effect, run by an `entry`, an `exit` or a transition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Effect {
    /// `set var = value`.
    Set {
        /// A machine variable (not a parameter).
        var: String,
        /// Its new value.
        value: Expr,
    },
    /// `do action(args)`.
    Do {
        /// A key of [`Ir::actions`].
        action: String,
        /// Arguments, positional, in parameter order.
        args: Vec<Expr>,
    },
    /// `send name.Event [{ fields }]`: queue an event for another machine.
    Send {
        /// The receiver.
        to: Address,
        /// For a collection, the key of the child (`send name[key].Event`), or
        /// `None` for every running child (`send name.*.Event`). `None` for
        /// the other kinds of address.
        key: Option<Expr>,
        /// The event's name, a key of the receiver's [`Machine::events`].
        event: String,
        /// The event's value: a `record` expression whose type is
        /// `Machine.Event`.
        value: Expr,
    },
    /// `emit Event [{ fields }]`: emit one of the machine's own `emits`.
    Emit {
        /// The event's name, a key of the machine's [`Machine::emits`].
        event: String,
        /// The event's value: a `record` expression whose type is the
        /// event's bare name.
        value: Expr,
    },
    /// `reply name done [value]` or `reply name fail error`: answer a pending
    /// call.
    Reply {
        /// The pending call: a `ref` expression of type `call c`, the implicit
        /// `call` or a variable.
        call: Expr,
        /// Done or fail.
        outcome: ReplyOutcome,
        /// For `done`, the result, of the call's `returns` type, or `None` when
        /// it returns nothing; for `fail`, one of its declared errors.
        value: Option<Expr>,
    },
    /// `spawn name[key] = Machine(args)` or `spawn name[key] = task(args)`: start a child
    /// or a task in a collection.
    Spawn {
        /// The collection, a key of [`Machine::children`].
        children: String,
        /// Its machine or task, the collection's `src`.
        src: String,
        /// The child's key.
        key: Expr,
        /// Arguments, positional, in parameter order.
        args: Vec<Expr>,
    },
    /// `stop name[key]`: stop a spawned child.
    Stop {
        /// The collection, a key of [`Machine::children`].
        children: String,
        /// The child's key.
        key: Expr,
    },
    /// `raise Event [{ fields }]`, on the run's internal queue, or
    /// `raise Event [{ fields }] after delay [as name]`, a delayed raise: a timer of the run.
    Raise {
        /// The event's name, a key of the machine's [`Machine::events`] or
        /// [`Machine::internal_events`].
        event: String,
        /// The event's value: a `record` expression whose type is the event's bare name.
        value: Expr,
        /// The delay, a `duration` expression, or `None` for an immediate raise.
        delay: Option<Expr>,
        /// The delayed raise's name, which `cancel` names it by, or `None`. Always `None`
        /// when `delay` is.
        name: Option<String>,
    },
    /// `cancel name`: cancel the pending delayed raise of that name, if there is one.
    Cancel {
        /// The name of one of the machine's delayed raises.
        name: String,
    },
}

impl Machine {
    /// The machine's own event `name`: one of its `events` or `internal_events`.
    pub fn own_event(&self, name: &str) -> Option<&EventDecl> {
        self.events
            .get(name)
            .or_else(|| self.internal_events.get(name))
    }

    /// The names of the machine's delayed raises (`raise … after … as name`), each once, in
    /// document order.
    pub fn raise_names(&self) -> Vec<&str> {
        fn walk<'a>(s: &'a State, out: &mut Vec<&'a str>) {
            let effects = s
                .entry
                .iter()
                .chain(&s.exit)
                .chain(s.transitions.iter().flat_map(|t| &t.effects));
            for e in effects {
                if let Effect::Raise { name: Some(n), .. } = e
                    && !out.contains(&n.as_str())
                {
                    out.push(n);
                }
            }
            s.states.iter().for_each(|c| walk(c, out));
        }
        let mut out = Vec::new();
        walk(&self.root, &mut out);
        out
    }
}

/// How a `reply` answers a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReplyOutcome {
    /// `reply … done [value]`: the call succeeded.
    Done,
    /// `reply … fail error`: the call failed with one of its declared errors.
    Fail,
}

impl Machine {
    /// The first use of messaging in the machine, described for a "not supported" message:
    /// an `emits`, a `children` collection, a `ref` type, an `invoke … as`, an `on name.Event`,
    /// a `send`, an `emit`, a `spawn` or a `stop`.
    /// `None` if it uses none. A reader that does not implement messaging refuses a machine
    /// for which this is `Some`.
    pub fn messaging(&self) -> Option<String> {
        if let Some(e) = self.emits.keys().next() {
            return Some(format!("`emits {e}`"));
        }
        if let Some(c) = self.children.keys().next() {
            return Some(format!("`children {c}`"));
        }
        let has_ref = |t: &TypeRef| t.mentions_ref();
        if let Some(p) = self.params.iter().find(|p| has_ref(&p.ty)) {
            return Some(format!("the parameter `{}: {}`", p.name, p.ty));
        }
        if let Some(v) = self.vars.iter().find(|v| has_ref(&v.ty)) {
            return Some(format!("the variable `{}: {}`", v.name, v.ty));
        }
        state_messaging(&self.root)
    }
}

fn state_messaging(s: &State) -> Option<String> {
    let effects = |effects: &[Effect]| {
        effects.iter().find_map(|e| match e {
            Effect::Send { to, event, .. } => Some(format!("`send {}.{event}`", to.name)),
            Effect::Emit { event, .. } => Some(format!("`emit {event}`")),
            Effect::Spawn { children, .. } => Some(format!("`spawn {children}[…]`")),
            Effect::Stop { children, .. } => Some(format!("`stop {children}[…]`")),
            _ => None,
        })
    };
    if let Some(found) = effects(&s.entry).or_else(|| effects(&s.exit)) {
        return Some(format!("{found} in state `{}`", s.id));
    }
    if let Some(Invoke {
        alias: Some(a),
        src,
        ..
    }) = s.invokes.iter().find(|i| i.alias.is_some())
    {
        return Some(format!("`invoke {src}(…) as {a}` in state `{}`", s.id));
    }
    for t in &s.transitions {
        if let Trigger::Emitted { from, event } = &t.trigger {
            return Some(format!("`on {}.{event}` in state `{}`", from.name, s.id));
        }
        if let Trigger::Ended { from } = &t.trigger {
            return Some(format!("`on {}.ended` in state `{}`", from.name, s.id));
        }
        if let Some(found) = effects(&t.effects) {
            return Some(format!("{found} in state `{}`", s.id));
        }
    }
    s.states.iter().find_map(state_messaging)
}
