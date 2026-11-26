//! The IR document and its declarations, machines and states.

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
    /// `type Name { fields }`: `{"kind": "record", "fields": [Field]}`.
    Record {
        /// Fields, in source order.
        fields: Vec<Field>,
    },
    /// `type Name = T`: `{"kind": "alias", "type": TypeRef}`.
    Alias {
        /// The aliased type.
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
        fields: &'a [Field],
        doc: &'a str,
        span: &'a Span,
    },
    Alias {
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
        fields: Vec<Field>,
        doc: String,
        span: Span,
    },
    Alias {
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
            TypeDef::Record { fields } => TypeDeclSer::Record { fields, doc, span },
            TypeDef::Alias { ty } => TypeDeclSer::Alias { ty, doc, span },
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
            TypeDeclDe::Record { fields, doc, span } => (TypeDef::Record { fields }, doc, span),
            TypeDeclDe::Alias { ty, doc, span } => (TypeDef::Alias { ty }, doc, span),
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

/// `task name(params) [-> T] [! E | …]`: effectful, fallible, cancellable
/// host work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskDecl {
    /// Parameters, in order.
    pub params: Vec<Field>,
    /// Result type, `null` if the task returns nothing.
    pub returns: Option<TypeRef>,
    /// Declared errors (names of `error` types), in source order.
    pub errors: Vec<String>,
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
    /// The whole declaration, from `machine` to the closing brace.
    pub span: Span,
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
    /// Events the machine emits (`emits Name { fields }`), keyed by name, in
    /// source order. No name is both in `events` and in `emits`.
    pub emits: IndexMap<String, EventDecl>,
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
    /// The task or machine run while the state is active, if any.
    pub invoke: Option<Invoke>,
    /// Transitions, in document order. Traces refer to them by index.
    pub transitions: Vec<Transition>,
    /// A top-level final state's result, if it has one.
    pub output: Option<Expr>,
    /// A top-level final state's failure, if it has one.
    pub fail: Option<Expr>,
}

/// Whether an invoke runs a task or a machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InvokeKind {
    /// A host task, a key of [`Ir::tasks`].
    Task,
    /// A child machine, a key of [`Ir::machines`].
    Machine,
}

/// `invoke src(args) { outcomes }`. The outcomes are transitions of the
/// state, with `invoke.done` and `invoke.fail` triggers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Invoke {
    /// Task or machine.
    pub kind: InvokeKind,
    /// Its name.
    pub src: String,
    /// The name given with `invoke Machine(…) as name`, through which the
    /// state and its descendants send to the child and receive what it
    /// emits. `None` without `as`; always `None` for a task.
    #[serde(rename = "as")]
    pub alias: Option<String>,
    /// Arguments, positional, in parameter order.
    pub args: Vec<Expr>,
    /// From `invoke` to the closing brace.
    pub span: Span,
}

/// Whether an [`Address`] names an invoked child or a `ref` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AddressKind {
    /// The child machine of an `invoke … as name`.
    Child,
    /// A parameter or variable of type `ref Machine`.
    Ref,
}

/// Another running machine, as a `send` names its receiver and an
/// `on name.Event` trigger its sender.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Address {
    /// A child or a ref.
    pub kind: AddressKind,
    /// The `as` name, or the parameter or variable.
    pub name: String,
    /// The machine at the other end, a key of [`Ir::machines`].
    pub machine: String,
    /// For a child, the id of the state whose invoke is named `name`: the
    /// innermost such state containing the item. `None` for a ref.
    pub state: Option<String>,
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
    /// Target state id, `null` for an internal transition.
    pub target: Option<String>,
    /// Effects: every `set` assignment, then the `do`, `send` and `emit`
    /// effects in the order written.
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
    /// `on name.Event`: an event emitted by an invoked child or by the
    /// machine a `ref` holds.
    #[serde(rename = "emitted")]
    Emitted {
        /// The sender.
        from: Address,
        /// The event's name, a key of the sender's [`Machine::emits`].
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
    InvokeDone,
    /// `fail E` inside `invoke { }`: it failed with error `E`.
    #[serde(rename = "invoke.fail")]
    InvokeFail {
        /// The error's name, or `"*"` for the declared errors not listed.
        error: String,
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
}

impl Machine {
    /// The first use of messaging in the machine, described for a "not supported" message:
    /// an `emits`, a `ref` type, an `invoke … as`, an `on name.Event`, a `send` or an `emit`.
    /// `None` if it uses none. A reader that does not implement messaging refuses a machine
    /// for which this is `Some`.
    pub fn messaging(&self) -> Option<String> {
        if let Some(e) = self.emits.keys().next() {
            return Some(format!("`emits {e}`"));
        }
        let has_ref = |t: &TypeRef| t.ref_base().is_some();
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
    }) = &s.invoke
    {
        return Some(format!("`invoke {src}(…) as {a}` in state `{}`", s.id));
    }
    for t in &s.transitions {
        if let Trigger::Emitted { from, event } = &t.trigger {
            return Some(format!("`on {}.{event}` in state `{}`", from.name, s.id));
        }
        if let Some(found) = effects(&t.effects) {
            return Some(format!("{found} in state `{}`", s.id));
        }
    }
    s.states.iter().find_map(state_messaging)
}
