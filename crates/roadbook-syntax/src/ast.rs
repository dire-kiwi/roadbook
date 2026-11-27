//! The syntax tree.
//!
//! Every node has a [`Span`]. The tree is close to the grammar in `docs/design/language.md`
//! with two simplifications: a machine body and a state body are both a [`Body`] of [`Item`]s
//! (the parser rejects the items that do not belong in each), and parentheses are not kept
//! (the formatter puts back the ones precedence needs).
//!
//! `Serialize` gives a structural form with every span left out. It is meant for tests and
//! tools that compare trees, not as an interchange format: that is the IR's job.

// Nodes are built once and read many times; boxing variants would only add noise.
#![allow(clippy::large_enum_variant)]

use serde::{Serialize, Serializer};

use crate::lexer::Comment;
use crate::span::Span;

/// A name with its span. Serialized as the bare name.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Ident {
    pub name: String,
    pub span: Span,
}

impl Serialize for Ident {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.name)
    }
}

/// The text of the `///` comments before a node, each line without `///` and one following
/// space, joined with `\n`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Doc {
    pub text: String,
    #[serde(skip)]
    pub span: Span,
}

/// A parsed file. After a syntax error the tree is partial: the items the parser could not
/// make sense of are left out.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct File {
    pub items: Vec<TopItem>,
    /// Every comment, doc comments included, in source order.
    pub comments: Vec<Comment>,
    #[serde(skip)]
    pub span: Span,
}

impl File {
    pub fn imports(&self) -> impl Iterator<Item = &Import> {
        self.items.iter().filter_map(|i| match i {
            TopItem::Import(i) => Some(i),
            TopItem::Decl(_) => None,
        })
    }

    pub fn decls(&self) -> impl Iterator<Item = &Decl> {
        self.items.iter().filter_map(|i| match i {
            TopItem::Decl(d) => Some(d),
            TopItem::Import(_) => None,
        })
    }

    pub fn machines(&self) -> impl Iterator<Item = (&Decl, &Machine)> {
        self.decls().filter_map(|d| match &d.kind {
            DeclKind::Machine(m) => Some((d, m)),
            _ => None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TopItem {
    Import(Import),
    Decl(Decl),
}

/// `import { A, b } from "path"`
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Import {
    pub names: Vec<Ident>,
    pub path: StringLit,
    #[serde(skip)]
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct StringLit {
    pub value: String,
    #[serde(skip)]
    pub span: Span,
}

/// A top-level declaration.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Decl {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc: Option<Doc>,
    pub name: Ident,
    pub kind: DeclKind,
    /// From the keyword to the end, doc comment excluded.
    #[serde(skip)]
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DeclKind {
    /// `type Name { fields }`
    Record {
        fields: Vec<Field>,
    },
    /// `type Name = T`
    Alias {
        target: TypeRef,
    },
    /// `enum Name { A, B }`
    Enum {
        variants: Vec<Ident>,
    },
    /// `error Name [{ fields }]`. `error E {}` and `error E` are the same.
    Error {
        fields: Vec<Field>,
    },
    /// `extern type Name`
    Extern,
    /// `fn name(params) -> T`
    Fn {
        params: Vec<Field>,
        returns: TypeRef,
    },
    /// `action name(params)`
    Action {
        params: Vec<Field>,
    },
    /// `task name(params) [-> T] [! E | F]`
    Task {
        params: Vec<Field>,
        returns: Option<TypeRef>,
        errors: Vec<Ident>,
    },
    Machine(Machine),
}

impl DeclKind {
    /// The declaration keyword, as written.
    pub fn keyword(&self) -> &'static str {
        match self {
            DeclKind::Record { .. } | DeclKind::Alias { .. } => "type",
            DeclKind::Enum { .. } => "enum",
            DeclKind::Error { .. } => "error",
            DeclKind::Extern => "extern type",
            DeclKind::Fn { .. } => "fn",
            DeclKind::Action { .. } => "action",
            DeclKind::Task { .. } => "task",
            DeclKind::Machine(_) => "machine",
        }
    }
}

/// `name: T`, in records, errors, events and parameter lists.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Field {
    pub name: Ident,
    pub ty: TypeRef,
    #[serde(skip)]
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct TypeRef {
    pub kind: TypeKind,
    #[serde(skip)]
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TypeKind {
    Builtin(Builtin),
    /// A declared type: record, alias, enum, error or extern.
    Named(Ident),
    /// `T[]`
    List(Box<TypeRef>),
    /// `T?`
    Optional(Box<TypeRef>),
    /// `ref Machine`: a handle to a running machine.
    Ref(Ident),
    /// `call name`: a pending call of the machine's call `name`.
    Call(Ident),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Builtin {
    String,
    Int,
    Float,
    Bool,
    Duration,
    Json,
}

impl Builtin {
    pub const ALL: [Builtin; 6] = [
        Builtin::String,
        Builtin::Int,
        Builtin::Float,
        Builtin::Bool,
        Builtin::Duration,
        Builtin::Json,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Builtin::String => "string",
            Builtin::Int => "int",
            Builtin::Float => "float",
            Builtin::Bool => "bool",
            Builtin::Duration => "duration",
            Builtin::Json => "json",
        }
    }

    pub fn lookup(name: &str) -> Option<Builtin> {
        Builtin::ALL.into_iter().find(|b| b.as_str() == name)
    }
}

/// `machine Name(params) [-> T] [! E | F] { body }`. The name and doc are on the [`Decl`].
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Machine {
    pub params: Vec<Field>,
    pub returns: Option<TypeRef>,
    pub errors: Vec<Ident>,
    pub body: Body,
}

/// The items between `{` and `}` of a machine or a state, in source order.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Body {
    pub items: Vec<Item>,
    /// From `{` to `}`.
    #[serde(skip)]
    pub span: Span,
}

impl Body {
    pub fn vars(&self) -> impl Iterator<Item = &Var> {
        self.items
            .iter()
            .filter_map(|i| if let Item::Var(v) = i { Some(v) } else { None })
    }
    pub fn events(&self) -> impl Iterator<Item = &EventDecl> {
        self.items.iter().filter_map(|i| {
            if let Item::Event(e) = i {
                Some(e)
            } else {
                None
            }
        })
    }
    pub fn calls(&self) -> impl Iterator<Item = &CallDecl> {
        self.items
            .iter()
            .filter_map(|i| if let Item::Call(c) = i { Some(c) } else { None })
    }
    pub fn emits(&self) -> impl Iterator<Item = &EventDecl> {
        self.items.iter().filter_map(|i| {
            if let Item::Emits(e) = i {
                Some(e)
            } else {
                None
            }
        })
    }
    pub fn initials(&self) -> impl Iterator<Item = &Ident> {
        self.items.iter().filter_map(|i| {
            if let Item::Initial(n) = i {
                Some(n)
            } else {
                None
            }
        })
    }
    pub fn states(&self) -> impl Iterator<Item = &State> {
        self.items.iter().filter_map(|i| {
            if let Item::State(s) = i {
                Some(s)
            } else {
                None
            }
        })
    }
    /// `on`, `after`, `always` and `done` transitions, not invoke outcomes.
    pub fn transitions(&self) -> impl Iterator<Item = &Transition> {
        self.items.iter().filter_map(|i| {
            if let Item::Transition(t) = i {
                Some(t)
            } else {
                None
            }
        })
    }
    pub fn invokes(&self) -> impl Iterator<Item = &Invoke> {
        self.items.iter().filter_map(|i| {
            if let Item::Invoke(v) = i {
                Some(v)
            } else {
                None
            }
        })
    }
    pub fn entry(&self) -> impl Iterator<Item = &Hook> {
        self.items.iter().filter_map(|i| {
            if let Item::Entry(h) = i {
                Some(h)
            } else {
                None
            }
        })
    }
    pub fn exit(&self) -> impl Iterator<Item = &Hook> {
        self.items
            .iter()
            .filter_map(|i| if let Item::Exit(h) = i { Some(h) } else { None })
    }
}

/// An item of a machine or state body. `Var`, `Event`, `Emits` and `Call` occur only in a
/// machine body; `Invoke`, `Output`, `Fail` and `done` transitions only in a state.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Item {
    Var(Var),
    Event(EventDecl),
    /// `emits Name [{ fields }]`
    Emits(EventDecl),
    /// `call name(params) [-> T] [! E | F]`
    Call(CallDecl),
    /// `initial Name`
    Initial(Ident),
    State(State),
    Entry(Hook),
    Exit(Hook),
    Transition(Transition),
    Invoke(Invoke),
    /// `output expr`
    Output(FinalValue),
    /// `fail expr`
    Fail(FinalValue),
}

impl Item {
    pub fn span(&self) -> Span {
        match self {
            Item::Var(v) => v.span,
            Item::Event(e) | Item::Emits(e) => e.span,
            Item::Call(c) => c.span,
            Item::Initial(n) => n.span,
            Item::State(s) => s.span,
            Item::Entry(h) | Item::Exit(h) => h.span,
            Item::Transition(t) => t.span,
            Item::Invoke(i) => i.span,
            Item::Output(v) | Item::Fail(v) => v.span,
        }
    }
}

/// `var name: T [= expr]`
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Var {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc: Option<Doc>,
    pub name: Ident,
    pub ty: TypeRef,
    pub init: Option<Expr>,
    #[serde(skip)]
    pub span: Span,
}

/// `event Name [{ fields }]`, or `emits Name [{ fields }]`
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EventDecl {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc: Option<Doc>,
    pub name: Ident,
    pub fields: Vec<Field>,
    #[serde(skip)]
    pub span: Span,
}

/// `call name(params) [-> T] [! E | F]`: a request other runs make of the machine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CallDecl {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc: Option<Doc>,
    pub name: Ident,
    pub params: Vec<Field>,
    pub returns: Option<TypeRef>,
    pub errors: Vec<Ident>,
    /// From `call` to the end, doc comment excluded.
    #[serde(skip)]
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum StateKind {
    /// `state`: atomic or compound, depending on whether it has child states.
    Normal,
    Final,
    Parallel,
}

/// `[final | parallel] state Name { body }`
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct State {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc: Option<Doc>,
    pub kind: StateKind,
    pub name: Ident,
    pub body: Body,
    #[serde(skip)]
    pub span: Span,
}

/// The effects of `entry` or `exit`.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Hook {
    pub effects: Vec<Effect>,
    #[serde(skip)]
    pub span: Span,
}

/// `output expr` or `fail expr` in a final state.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(transparent)]
pub struct FinalValue {
    pub value: Expr,
    #[serde(skip)]
    pub span: Span,
}

/// A trigger and what happens when it fires: a state's `on`, `after`, `always` and `done`, and
/// an invoke's `done` and `fail` outcomes.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Transition {
    pub trigger: Trigger,
    pub branches: Branches,
    /// The trigger as written: `on Cancel`, `after 30s`, `fail *`.
    #[serde(skip)]
    pub trigger_span: Span,
    #[serde(skip)]
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Trigger {
    /// `on Event` or `on *`
    On(Pattern),
    /// `on name.Event`: an event emitted by an invoked child or a `ref`.
    Receive {
        from: Ident,
        event: Ident,
    },
    /// `on name`: a call of the machine's call `name` (a lower-case name, no dot).
    Call(Ident),
    /// `after expr`
    After(Expr),
    Always,
    /// A state's `done`, or an invoke's `done` outcome.
    Done,
    /// An invoke's `fail Error` or `fail *` outcome. Never directly in a [`Body`].
    Fail(Pattern),
}

/// A name or `*` (any).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Pattern {
    Name(Ident),
    Any {
        #[serde(skip)]
        span: Span,
    },
}

impl Pattern {
    pub fn span(&self) -> Span {
        match self {
            Pattern::Name(n) => n.span,
            Pattern::Any { span } => *span,
        }
    }
}

/// One branch, or a block of guarded branches tried in order.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Branches {
    /// `[if guard] action`
    Single(Branch),
    /// `{ if g1 action  if g2 action  [else action] }`. Every branch but an `else` has a
    /// guard, and an `else` (a branch with no guard) can only be last.
    Block {
        branches: Vec<Branch>,
        #[serde(skip)]
        span: Span,
    },
}

impl Branches {
    /// The branches in the order they are tried.
    pub fn iter(&self) -> std::slice::Iter<'_, Branch> {
        match self {
            Branches::Single(b) => std::slice::from_ref(b).iter(),
            Branches::Block { branches, .. } => branches.iter(),
        }
    }
}

/// `[if guard] [-> Target] [effects]`, at least one of the target and the effects.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Branch {
    pub guard: Option<Expr>,
    pub target: Option<Target>,
    pub effects: Vec<Effect>,
    #[serde(skip)]
    pub span: Span,
}

/// `Name { . Name }`
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Target {
    pub path: Vec<Ident>,
    #[serde(skip)]
    pub span: Span,
}

/// One effect. `set a = 1, b = 2` is two `Set` effects; order is as written.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Effect {
    Set(Assign),
    Do(Call),
    /// `send name.Event [{ fields }]`
    Send(Message),
    /// `emit Event [{ fields }]`
    Emit(Message),
    /// `reply name done [value]` or `reply name fail error`
    Reply(Reply),
}

impl Effect {
    pub fn span(&self) -> Span {
        match self {
            Effect::Set(a) => a.span,
            Effect::Do(c) => c.span,
            Effect::Send(m) | Effect::Emit(m) => m.span,
            Effect::Reply(r) => r.span,
        }
    }
}

/// `reply name done [value]` or `reply name fail error`: answer a pending call.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Reply {
    /// The pending call: the keyword `call` (as a name `call`) or a variable.
    pub call: Ident,
    pub outcome: ReplyOutcome,
    /// The result for `done`, if given; the error for `fail`, always given.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Expr>,
    /// After `reply`, to the end.
    #[serde(skip)]
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ReplyOutcome {
    Done,
    Fail,
}

impl ReplyOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            ReplyOutcome::Done => "done",
            ReplyOutcome::Fail => "fail",
        }
    }
}

/// The event of a `send` or `emit`: `upload.Pause`, `Progress { percent: p }`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Message {
    /// The receiver of a `send`; `None` for an `emit`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<Ident>,
    pub event: Ident,
    /// The fields of the record body, in the order written. Empty without a body.
    pub fields: Vec<FieldValue>,
    /// From the event's name to the end of the body: the event value.
    #[serde(skip)]
    pub value_span: Span,
    /// After `send` or `emit`, to the end.
    #[serde(skip)]
    pub span: Span,
}

/// `var = expr`
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Assign {
    pub var: Ident,
    pub value: Expr,
    #[serde(skip)]
    pub span: Span,
}

/// `name(args)`: a function call in an expression, or an action in `do`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Call {
    pub name: Ident,
    pub args: Vec<Expr>,
    #[serde(skip)]
    pub span: Span,
}

/// `invoke name(args) { outcomes }`. A lower-case name is a task, an upper-case one a machine;
/// `invoke x.name(args)` calls the call `name` of the run that `x` holds.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Invoke {
    /// `x` in `invoke x.name(…)`: the callee of a call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<Ident>,
    pub callee: Ident,
    pub args: Vec<Expr>,
    /// `as name`: the name the state uses to talk to the invoked machine.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias: Option<Ident>,
    /// `done` and `fail` transitions, in source order.
    pub outcomes: Vec<Transition>,
    #[serde(skip)]
    pub span: Span,
}

impl Invoke {
    /// Whether the callee is a machine (upper-case name) rather than a task or a call.
    pub fn is_machine(&self) -> bool {
        self.to.is_none()
            && self
                .callee
                .name
                .starts_with(|c: char| c.is_ascii_uppercase())
    }

    /// Whether it calls a peer: `invoke x.name(…)`.
    pub fn is_call(&self) -> bool {
        self.to.is_some()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Expr {
    pub kind: ExprKind,
    #[serde(skip)]
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ExprKind {
    Int(i64),
    Float(f64),
    Duration(Duration),
    String(String),
    Bool(bool),
    /// A variable, parameter, or one of the implicit names `event`, `error`, `result` and
    /// `call`.
    Var(Ident),
    /// `expr.field`
    Field(Box<Expr>, Ident),
    Call(Call),
    /// `Type.Variant`
    Enum(Ident, Ident),
    /// `Type { field: expr, … }`, or a bare `Type` (no fields, such as an error value).
    Record {
        ty: Ident,
        fields: Vec<FieldValue>,
    },
    Unary(UnaryOp, Box<Expr>),
    Binary(BinaryOp, Box<Expr>, Box<Expr>),
}

/// `field: expr` in a record value.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FieldValue {
    pub name: Ident,
    pub value: Expr,
    #[serde(skip)]
    pub span: Span,
}

/// A duration literal, kept in the unit it was written in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct Duration {
    pub value: u64,
    pub unit: DurationUnit,
}

impl Duration {
    pub fn millis(self) -> u64 {
        let factor = match self.unit {
            DurationUnit::Ms => 1,
            DurationUnit::S => 1_000,
            DurationUnit::M => 60_000,
            DurationUnit::H => 3_600_000,
        };
        self.value.saturating_mul(factor)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DurationUnit {
    Ms,
    S,
    M,
    H,
}

impl DurationUnit {
    pub fn as_str(self) -> &'static str {
        match self {
            DurationUnit::Ms => "ms",
            DurationUnit::S => "s",
            DurationUnit::M => "m",
            DurationUnit::H => "h",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum UnaryOp {
    Not,
    Neg,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum BinaryOp {
    #[serde(rename = "or")]
    Or,
    #[serde(rename = "and")]
    And,
    #[serde(rename = "==")]
    Eq,
    #[serde(rename = "!=")]
    Ne,
    #[serde(rename = "<")]
    Lt,
    #[serde(rename = "<=")]
    Le,
    #[serde(rename = ">")]
    Gt,
    #[serde(rename = ">=")]
    Ge,
    #[serde(rename = "+")]
    Add,
    #[serde(rename = "-")]
    Sub,
    #[serde(rename = "*")]
    Mul,
}

impl BinaryOp {
    pub fn as_str(self) -> &'static str {
        match self {
            BinaryOp::Or => "or",
            BinaryOp::And => "and",
            BinaryOp::Eq => "==",
            BinaryOp::Ne => "!=",
            BinaryOp::Lt => "<",
            BinaryOp::Le => "<=",
            BinaryOp::Gt => ">",
            BinaryOp::Ge => ">=",
            BinaryOp::Add => "+",
            BinaryOp::Sub => "-",
            BinaryOp::Mul => "*",
        }
    }

    /// Binding strength: `or` 1, `and` 2, comparisons 4, `+ -` 5, `*` 6. (`not` is 3 and unary
    /// minus 7.)
    pub fn precedence(self) -> u8 {
        match self {
            BinaryOp::Or => 1,
            BinaryOp::And => 2,
            BinaryOp::Eq
            | BinaryOp::Ne
            | BinaryOp::Lt
            | BinaryOp::Le
            | BinaryOp::Gt
            | BinaryOp::Ge => 4,
            BinaryOp::Add | BinaryOp::Sub => 5,
            BinaryOp::Mul => 6,
        }
    }
}

impl Expr {
    /// Call `f` on this expression and every expression inside it, outermost first.
    pub fn walk<'a>(&'a self, f: &mut impl FnMut(&'a Expr)) {
        f(self);
        match &self.kind {
            ExprKind::Field(e, _) | ExprKind::Unary(_, e) => e.walk(f),
            ExprKind::Call(c) => c.args.iter().for_each(|a| a.walk(f)),
            ExprKind::Record { fields, .. } => fields.iter().for_each(|fv| fv.value.walk(f)),
            ExprKind::Binary(_, a, b) => {
                a.walk(f);
                b.walk(f);
            }
            ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::Duration(_)
            | ExprKind::String(_)
            | ExprKind::Bool(_)
            | ExprKind::Var(_)
            | ExprKind::Enum(..) => {}
        }
    }
}
