//! Machines: variables, events, the state tree, targets, invokes and transitions, checked
//! and lowered to the IR.

use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;
use roadbook_ir as ir;
use roadbook_syntax::ast::{
    self, Body, Branches, Decl, DeclKind, Effect, Ident, Invoke, Item, Message, Pattern, Raise,
    Reply, ReplyOutcome, SendKey, State, StateKind, Target, Transition, Trigger,
};
use roadbook_syntax::diagnostic::Code;
use roadbook_syntax::{Diagnostic, Span, expr_text, message_value};

use crate::check::{Checker, Sig, SigKind, UNANSWERED, doc, ir_span, unanswered_misused};
use crate::codes;
use crate::index::{SymbolId, SymbolKind};
use crate::names::{Found, is_type, kind_phrase, suggest};
use crate::sensitive::{Dest, Sensitive};
use crate::types::{BOOL, DURATION, Ty, ir_type};

mod retry;

/// The machine variables (not parameters) an expression or effect reads, each with the span
/// of the read, in the order written. Kept for definite assignment, which runs on the IR.
pub(crate) type Reads = Vec<(String, Span)>;

/// What one IR transition reads, by part.
#[derive(Clone, Debug)]
pub(crate) struct TransitionReads {
    /// The `after` delay of its block: the same for every branch of the block.
    pub delay: Reads,
    pub guard: Reads,
    /// By IR effect index.
    pub effects: Vec<Reads>,
    /// The arguments of the states it enters, evaluated after its effects. A state's local is
    /// named here `<state id>.<name>`.
    pub args: Reads,
    /// The branch: the whole line for a single-branch trigger, from `if` or `else` in a block.
    pub span: Span,
}

/// Source positions of a state's parts, kept for the analyses that run on the IR.
#[derive(Clone, Debug)]
pub(crate) struct StateInfo {
    pub name_span: Span,
    /// `invoke name` of each of the state's invokes, in the order written.
    pub invoke_spans: Vec<Span>,
    /// The trigger of each of the state's IR transitions, by index.
    pub triggers: Vec<Span>,
    /// The name in `initial Name`, for a compound state.
    pub initial_span: Option<Span>,
    /// What each IR `entry` and `exit` effect reads, by index.
    pub entry_reads: Vec<Reads>,
    pub exit_reads: Vec<Reads>,
    /// What the invokes' arguments read: they are all evaluated as the state's work starts.
    pub invoke_reads: Reads,
    /// What `output` or `fail` reads.
    pub result_reads: Reads,
    /// What each IR transition reads, by index.
    pub transition_reads: Vec<TransitionReads>,
    /// The IR transitions a `retry` added: they do not count as a way out, since they stop
    /// once the retries run out.
    pub retry_branches: Vec<usize>,
    /// For a state a `retry` added, the state it was written in, which diagnostics name.
    pub written_as: Option<String>,
    /// The state's parameters, then its variables, in order.
    pub locals: Vec<LocalInfo>,
}

/// A state's parameter or variable, for definite assignment. Reads of it are named
/// `<state id>.<name>` in [`Reads`], which no machine variable's name can be.
#[derive(Clone, Debug)]
pub(crate) struct LocalInfo {
    pub name: String,
    /// The name in its declaration.
    pub span: Span,
    pub param: bool,
    /// Whether it has an initial value.
    pub init: bool,
    /// Whether its type is optional, aliases expanded: it then starts as `null`.
    pub optional: bool,
    /// What its initial value reads.
    pub init_reads: Reads,
}

/// A machine variable, for definite assignment.
#[derive(Clone, Debug)]
pub(crate) struct VarInfo {
    pub name: String,
    /// The variable's name in its declaration.
    pub span: Span,
    /// Whether it has an initial value.
    pub init: bool,
    /// Whether its type is optional, aliases expanded: it then starts as `null`.
    pub optional: bool,
    /// What its initial value reads.
    pub init_reads: Reads,
}

#[derive(Clone, Debug)]
pub(crate) struct MachineInfo {
    pub name: String,
    pub file: usize,
    pub states: HashMap<String, StateInfo>,
    /// Declared events with their name spans, in order.
    pub events: Vec<(String, Span)>,
    pub used_events: HashSet<String>,
    /// Declared emitted events with their name spans, in order.
    pub emits: Vec<(String, Span)>,
    /// The events an `emit` of the machine sends.
    pub emitted: HashSet<String>,
    /// Every `on name.Event`: the event's span, the sending machine and the event.
    pub receives: Vec<(Span, String, String)>,
    /// Declared `children` collections with their name spans, in order.
    pub children: Vec<(String, Span)>,
    /// The collections a `spawn` of the machine fills.
    pub spawned: HashSet<String>,
    /// The machine's variables, in order.
    pub vars: Vec<VarInfo>,
    /// Declared calls with their name spans, in order.
    pub calls: Vec<(String, Span)>,
    /// The calls an `on name` takes.
    pub taken_calls: HashSet<String>,
    /// The variables of type `call name`, with their name spans, in order.
    pub call_vars: Vec<(String, Span)>,
    /// The variables a `reply` answers.
    pub replied: HashSet<String>,
    /// Declared internal events with their name spans, in order. They are in `events` too.
    pub internal_events: Vec<(String, Span)>,
    /// Every `raise`, by its event: the event's span in the `raise`, in the order lowered.
    pub raised: Vec<(String, Span)>,
    /// Whether some state of the machine has an `on *`.
    pub any_handler: bool,
}

/// Where effects run, for the check that a `send` to an invoked child finds it running.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Place {
    /// The `entry` of a state (by node).
    Entry(usize),
    /// The `exit` of a state.
    Exit(usize),
    /// A transition written in a state, with its domain (`None` with no target: it leaves
    /// nothing), and, for an outcome of one of the state's invokes, which one (its position
    /// among the state's invokes).
    Transition {
        node: usize,
        domain: Option<usize>,
        outcome: Option<usize>,
    },
}

/// What an implicit name (`event`, `result`, `error`, `call`, `key`) means at some point.
#[derive(Clone, Debug)]
pub(crate) enum Implicit {
    NotHere,
    Has(Ty),
    /// In scope but unusable, with the diagnostic to give.
    Bad(Code, String),
    /// `event` in `on name` for the machine's call `name`: its arguments, read only through
    /// their fields.
    CallArgs(String),
    /// `event` in `on name.ended`: read only through its field `status`, of the built-in enum
    /// `Ended`.
    Ended,
    /// `event` in `on A | B`: one of these events of the machine's own, read only through the
    /// fields they all have.
    OneOf(Vec<String>),
}

#[derive(Clone, Debug)]
pub(crate) struct Scope {
    pub event: Implicit,
    pub result: Implicit,
    pub error: Implicit,
    pub call: Implicit,
    pub key: Implicit,
}

impl Scope {
    pub fn none() -> Scope {
        Scope {
            event: Implicit::NotHere,
            result: Implicit::NotHere,
            error: Implicit::NotHere,
            call: Implicit::NotHere,
            key: Implicit::NotHere,
        }
    }
}

/// A `children` collection: its machine or task (`None` if it is not a known one, already
/// reported), whether it holds tasks, and its key type.
#[derive(Clone, Debug)]
pub(crate) struct Collection {
    pub src: Option<String>,
    /// Written with a lower-case name: `children uploads: upload by string`.
    pub task: bool,
    pub key: Ty,
    pub symbol: Option<SymbolId>,
}

struct Node<'p> {
    name: String,
    name_span: Span,
    id: String,
    parent: Option<usize>,
    /// The child states, history states not among them.
    children: Vec<usize>,
    /// The history states, in source order.
    histories: Vec<usize>,
    /// Every child, history states among them, in source order: the IR's `states`.
    order: Vec<usize>,
    /// `Some(deep)` for a history state.
    history: Option<&'p ast::History>,
    body: &'p Body,
    /// `None` for the machine's root.
    state: Option<&'p State>,
    depth: usize,
    /// The `as` names of the state's invokes, in the order written.
    aliases: Vec<Alias<'p>>,
    /// The state's symbol (the machine's, for the root).
    symbol: Option<SymbolId>,
}

/// `invoke Machine(…) as name` or `invoke task(…) as name`.
#[derive(Clone)]
struct Alias<'p> {
    name: &'p Ident,
    /// What is invoked, as written: `Upload`, `upload`, or for a call `inventory.reserve`.
    callee: String,
    /// The invoke's position among the state's invokes.
    index: usize,
    /// The symbol of the name.
    symbol: Option<SymbolId>,
}

pub(crate) struct Var {
    pub ty: Ty,
    pub param: bool,
    /// Marked `sensitive`.
    pub sensitive: bool,
}

/// A state's parameter or variable: one of its locals.
#[derive(Clone, Debug)]
pub(crate) struct Local {
    pub name: String,
    /// The name in its declaration.
    pub span: Span,
    pub ty: Ty,
    pub param: bool,
    /// Marked `sensitive`.
    pub sensitive: bool,
    pub symbol: Option<SymbolId>,
}

pub(crate) struct Cx<'c, 'p> {
    pub ck: &'c mut Checker<'p>,
    pub file: usize,
    pub machine: String,
    /// The machine's symbol, which its parameters, variables and events are members of.
    pub symbol: Option<SymbolId>,
    pub vars: HashMap<String, Var>,
    /// The machine's own events, its internal events among them: what `on E` and `raise E`
    /// take.
    pub events: HashMap<String, Vec<(String, Ty)>>,
    /// Which of `events` are internal events.
    pub internal: HashSet<String>,
    pub emits: HashMap<String, Vec<(String, Ty)>>,
    /// The names of the machine's delayed raises (`raise … as name`), with where each is
    /// first written.
    pub raise_names: HashMap<String, Span>,
    /// The calls the machine answers.
    pub calls: IndexMap<String, Sig>,
    pub collections: HashMap<String, Collection>,
    returns: Option<Ty>,
    errors: Vec<String>,
    nodes: Vec<Node<'p>>,
    info: MachineInfo,
    /// The variables read since this was last taken (see [`Reads`]).
    pub reads: Reads,
    /// What the transitions lowered since this was last taken read, in IR order.
    transition_reads: Vec<TransitionReads>,
    /// The counters of the `retry`s written out so far, in the order of their states.
    retry_vars: Vec<(ir::Var, VarInfo)>,
    /// The counter `attempt` reads while a retry's delay is checked: a variable for the
    /// checker, but not one to suggest, since the chart cannot name it.
    counter: Option<String>,
    /// Whether the expression [`Cx::expr`] last returned reads a sensitive value.
    pub sens: bool,
    /// The locals of each node, its parameters then its variables, in order.
    locals: Vec<Vec<Local>>,
    /// The node whose items are being lowered: its locals and its ancestors' are in scope.
    here: usize,
}

/// Checks machine `m` declared in `file` and lowers it. Its hash is left empty.
pub(crate) fn lower<'p>(
    ck: &mut Checker<'p>,
    file: usize,
    d: &'p Decl,
    m: &'p ast::Machine,
) -> ir::Machine {
    let name = d.name.name.clone();
    let sig = ck.sigs.get(&name).cloned();
    let symbol = ck.decl_symbol(&name);
    let mut cx = Cx {
        ck,
        file,
        machine: name.clone(),
        symbol,
        vars: HashMap::new(),
        events: HashMap::new(),
        internal: HashSet::new(),
        emits: HashMap::new(),
        raise_names: HashMap::new(),
        calls: IndexMap::new(),
        collections: HashMap::new(),
        returns: sig.as_ref().and_then(|s| s.returns.clone()),
        errors: sig.as_ref().map(|s| s.errors.clone()).unwrap_or_default(),
        nodes: Vec::new(),
        info: MachineInfo {
            name: name.clone(),
            file,
            states: HashMap::new(),
            events: Vec::new(),
            used_events: HashSet::new(),
            emits: Vec::new(),
            emitted: HashSet::new(),
            receives: Vec::new(),
            children: Vec::new(),
            spawned: HashSet::new(),
            vars: Vec::new(),
            calls: Vec::new(),
            taken_calls: HashSet::new(),
            call_vars: Vec::new(),
            replied: HashSet::new(),
            internal_events: Vec::new(),
            raised: Vec::new(),
            any_handler: false,
        },
        reads: Vec::new(),
        transition_reads: Vec::new(),
        retry_vars: Vec::new(),
        counter: None,
        sens: false,
        locals: Vec::new(),
        here: 0,
    };

    // Parameters and variables share one namespace.
    let mut seen: HashMap<String, Span> = HashMap::new();
    for (p, (_, ty)) in m
        .params
        .iter()
        .zip(sig.iter().flat_map(|s| s.params.iter()))
    {
        seen.entry(p.name.name.clone()).or_insert(p.name.span);
        cx.vars.entry(p.name.name.clone()).or_insert(Var {
            ty: ty.clone(),
            param: true,
            sensitive: p.sensitive,
        });
    }
    // Calls: their signatures are in the interface; a name declared twice keeps the first.
    let iface = cx.ck.interfaces.get(&name).cloned().unwrap_or_default();
    cx.calls = iface.calls.clone();
    let mut calls = IndexMap::new();
    for c in m.body.calls() {
        if calls.contains_key(&c.name.name) {
            continue;
        }
        cx.info.calls.push((c.name.name.clone(), c.name.span));
        calls.insert(
            c.name.name.clone(),
            ir::Call {
                params: c
                    .params
                    .iter()
                    .map(|p| ir::Field {
                        name: p.name.name.clone(),
                        ty: ir_type(&p.ty),
                        sensitive: p.sensitive,
                        doc: String::new(),
                    })
                    .collect(),
                returns: c.returns.as_ref().map(ir_type),
                errors: c.errors.iter().map(|e| e.name.clone()).collect(),
                doc: doc(&c.doc),
                span: ir_span(c.span),
            },
        );
    }

    for p in &m.params {
        cx.key_as_name(&p.name, "a parameter");
    }
    let mut var_tys = Vec::new();
    for v in m.body.vars() {
        cx.key_as_name(&v.name, "a variable");
        let ty = cx.var_type(v);
        if let Some(first) = seen.get(&v.name.name) {
            let what = if cx.vars.get(&v.name.name).is_some_and(|x| x.param) {
                "a parameter"
            } else {
                "a variable"
            };
            cx.ck.diags.push(
                Diagnostic::error(
                    codes::DUPLICATE_NAME,
                    v.name.span,
                    format!("`{}` is already {what} of `{name}`", v.name.name),
                )
                .with_secondary(*first, "first declared here"),
            );
        } else {
            seen.insert(v.name.name.clone(), v.name.span);
            cx.vars.insert(
                v.name.name.clone(),
                Var {
                    ty: ty.clone(),
                    param: false,
                    sensitive: v.sensitive,
                },
            );
        }
        var_tys.push(ty);
    }

    // Events, internal events and emitted events: one namespace. Their field types are in
    // the interface.
    let mut events = IndexMap::new();
    let mut internal_events = IndexMap::new();
    let mut emits = IndexMap::new();
    let mut first_seen: HashMap<String, Span> = HashMap::new();
    for (kind, e) in m.body.items.iter().filter_map(|i| match i {
        Item::Event(e) => Some((0, e)),
        Item::InternalEvent(e) => Some((1, e)),
        Item::Emits(e) => Some((2, e)),
        _ => None,
    }) {
        let is_emits = kind == 2;
        let word = match kind {
            0 => "event",
            1 => "internal event",
            _ => "emitted event",
        };
        if let Some(first) = first_seen.get(&e.name.name) {
            cx.ck.diags.push(
                Diagnostic::error(
                    codes::DUPLICATE_NAME,
                    e.name.span,
                    format!("event `{}` is declared twice", e.name.name),
                )
                .with_secondary(*first, "first declared here")
                .with_help("the events a machine accepts, raises and emits share one set of names"),
            );
            continue;
        }
        first_seen.insert(e.name.name.clone(), e.name.span);
        if let Some(&id) = cx.ck.names.global.get(e.name.name.as_str()) {
            let other = cx.ck.names.decl(id);
            if is_type(&other.kind) || matches!(other.kind, DeclKind::Machine(_)) {
                let path = &cx.ck.names.project.files[cx.ck.names.decls[id.0].file].path;
                cx.ck.diags.push(
                    Diagnostic::error(
                        codes::DUPLICATE_DECLARATION,
                        e.name.span,
                        format!(
                            "{word} `{}` has the name of {} declared in `{path}`",
                            e.name.name,
                            kind_phrase(&other.kind)
                        ),
                    )
                    .with_secondary(other.name.span, "declared here")
                    .with_help("the IR names an event's type by the event's name, so it must not be the name of a type, error or machine; rename one of them"),
                );
            }
        }
        let decl = ir::EventDecl {
            fields: e
                .fields
                .iter()
                .map(|f| ir::Field {
                    name: f.name.name.clone(),
                    ty: ir_type(&f.ty),
                    sensitive: f.sensitive,
                    doc: String::new(),
                })
                .collect(),
            doc: doc(&e.doc),
            span: ir_span(e.span),
        };
        if is_emits {
            let fields = iface.emits.get(&e.name.name).cloned().unwrap_or_default();
            cx.emits.insert(e.name.name.clone(), fields);
            cx.info.emits.push((e.name.name.clone(), e.name.span));
            emits.insert(e.name.name.clone(), decl);
        } else if kind == 1 {
            let fields = iface
                .internal
                .get(&e.name.name)
                .cloned()
                .unwrap_or_default();
            cx.events.insert(e.name.name.clone(), fields);
            cx.internal.insert(e.name.name.clone());
            cx.info.events.push((e.name.name.clone(), e.name.span));
            cx.info
                .internal_events
                .push((e.name.name.clone(), e.name.span));
            internal_events.insert(e.name.name.clone(), decl);
        } else {
            let fields = iface.events.get(&e.name.name).cloned().unwrap_or_default();
            cx.events.insert(e.name.name.clone(), fields);
            cx.info.events.push((e.name.name.clone(), e.name.span));
            events.insert(e.name.name.clone(), decl);
        }
    }

    // Children collections share the namespace of parameters and variables.
    let mut children = IndexMap::new();
    for c in m.body.children() {
        cx.key_as_name(&c.name, "a children collection");
        if let Some(first) = seen.get(&c.name.name) {
            let what = match cx.vars.get(&c.name.name) {
                Some(v) if v.param => "a parameter",
                Some(_) => "a variable",
                None => "a children collection",
            };
            cx.ck.diags.push(
                Diagnostic::error(
                    codes::DUPLICATE_NAME,
                    c.name.span,
                    format!("`{}` is already {what} of `{name}`", c.name.name),
                )
                .with_secondary(*first, "first declared here")
                .with_help(
                    "a collection shares the machine's names with its parameters and variables",
                ),
            );
            continue;
        }
        seen.insert(c.name.name.clone(), c.name.span);
        let task = c.is_task();
        let kind = if task {
            SigKind::Task
        } else {
            SigKind::Machine
        };
        let src = cx.callee(&c.src, kind).map(|_| c.src.name.clone());
        let key = cx.ck.resolve(file, &c.key);
        let fits = match cx.ck.env.expand(&key) {
            Ty::Builtin(ir::Builtin::String | ir::Builtin::Int) | Ty::Unknown => true,
            Ty::Named(n) => matches!(cx.ck.env.defs.get(&n), Some(crate::types::TypeDef::Enum(_))),
            _ => false,
        };
        if !fits {
            cx.ck.diags.push(
                Diagnostic::error(
                    codes::BAD_KEY_TYPE,
                    c.key.span,
                    format!("a collection's key cannot be `{key}`"),
                )
                .with_help("keys are compared by value: use `string`, `int` or an enum (or an alias of one)"),
            );
        }
        let symbol = cx.symbol.and_then(|s| cx.ck.member_symbol(s, &c.name.name));
        cx.collections.insert(
            c.name.name.clone(),
            Collection {
                src,
                task,
                key,
                symbol,
            },
        );
        cx.info.children.push((c.name.name.clone(), c.name.span));
        children.insert(
            c.name.name.clone(),
            ir::ChildrenDecl {
                kind: if task {
                    ir::ChildrenKind::Task
                } else {
                    ir::ChildrenKind::Machine
                },
                src: c.src.name.clone(),
                key: ir_type(&c.key),
                doc: doc(&c.doc),
                span: ir_span(c.span),
            },
        );
    }

    // The names of delayed raises share the namespace too. Several raises may use one name:
    // a raise replaces the pending one of its name.
    for e in m.body.all_effects() {
        let ast::Effect::Raise(Raise { name: Some(n), .. }) = e else {
            continue;
        };
        if cx.raise_names.contains_key(&n.name) {
            continue;
        }
        cx.key_as_name(n, "a delayed raise");
        if let Some(first) = seen.get(&n.name) {
            let what = match cx.vars.get(&n.name) {
                Some(v) if v.param => "a parameter",
                Some(_) => "a variable",
                None => "a children collection",
            };
            cx.ck.diags.push(
                Diagnostic::error(
                    codes::DUPLICATE_NAME,
                    n.span,
                    format!("`{}` is already {what} of `{name}`", n.name),
                )
                .with_secondary(*first, "first declared here")
                .with_help("a delayed raise's name shares the machine's names with its parameters, variables and collections"),
            );
            continue;
        }
        cx.raise_names.insert(n.name.clone(), n.span);
    }

    // Variable initialisers may read parameters and other variables.
    let mut vars = Vec::new();
    for (v, ty) in m.body.vars().zip(var_tys) {
        cx.reads.clear();
        // A pending call has no initial value (reported with its type): nothing to check against.
        let want = match v.ty.kind {
            ast::TypeKind::Call(_) => Ty::Unknown,
            _ => ty.clone(),
        };
        let init = v.init.as_ref().map(|e| {
            cx.expect_to(
                e,
                &Scope::none(),
                &want,
                &format!("the initial value of `{}`", v.name.name),
                dest(v.sensitive, format!("`{}`", v.name.name)),
            )
        });
        let optional = matches!(cx.ck.env.expand(&ty), Ty::Optional(_));
        let init_reads = std::mem::take(&mut cx.reads);
        cx.info.vars.push(VarInfo {
            name: v.name.name.clone(),
            span: v.name.span,
            init: init.is_some(),
            optional,
            init_reads,
        });
        vars.push(ir::Var {
            name: v.name.name.clone(),
            ty: ir_type(&v.ty),
            sensitive: v.sensitive,
            init,
            doc: doc(&v.doc),
            span: ir_span(v.span),
        });
    }

    cx.nodes.push(Node {
        name: name.clone(),
        name_span: d.name.span,
        id: name.clone(),
        parent: None,
        children: Vec::new(),
        histories: Vec::new(),
        order: Vec::new(),
        history: None,
        body: &m.body,
        state: None,
        depth: 0,
        aliases: Vec::new(),
        symbol,
    });
    cx.build_tree(0);
    cx.check_aliases();
    cx.collect_locals(&seen);
    let mut root = cx.lower_state(0);
    root.doc = doc(&d.doc);
    root.span = ir_span(d.span);
    for (v, info) in std::mem::take(&mut cx.retry_vars) {
        vars.push(v);
        cx.info.vars.push(info);
    }
    let symbols: HashMap<String, SymbolId> = cx
        .nodes
        .iter()
        .filter_map(|n| Some((n.id.clone(), n.symbol?)))
        .collect();
    crate::checks::resolve(cx.ck, &name, &m.body, &root, &symbols);

    let info = cx.info;
    ck.machines.push(info);
    ir::Machine {
        doc: doc(&d.doc),
        span: ir_span(d.span),
        strict: m.strict,
        params: m
            .params
            .iter()
            .map(|p| ir::Field {
                name: p.name.name.clone(),
                ty: ir_type(&p.ty),
                sensitive: p.sensitive,
                doc: String::new(),
            })
            .collect(),
        returns: m.returns.as_ref().map(ir_type),
        errors: m.errors.iter().map(|e| e.name.clone()).collect(),
        vars,
        events,
        internal_events,
        emits,
        calls,
        children,
        root,
        hash: String::new(),
    }
}

/// The key that makes two triggers "the same trigger": the trigger as the formatter writes
/// it, so two `after`s are the same only if their delays have the same text.
fn trigger_key(t: &Trigger) -> String {
    let pattern = |p: &Pattern| match p {
        Pattern::Name(n) => n.name.clone(),
        Pattern::Any { .. } => "*".to_string(),
    };
    match t {
        Trigger::On(p) => format!("on {}", pattern(p)),
        Trigger::OneOf(names) => {
            let names: Vec<&str> = names.iter().map(|n| n.name.as_str()).collect();
            format!("on {}", names.join(" | "))
        }
        Trigger::Receive { from, event } => format!("on {}.{}", from.name, event.name),
        Trigger::Call(n) => format!("on {}", n.name),
        Trigger::ChildDone { from } => format!("on {}.done", from.name),
        Trigger::ChildFail { from, error } => format!("on {}.fail {}", from.name, pattern(error)),
        Trigger::Ended { from } => format!("on {}.ended", from.name),
        Trigger::After(e) => format!("after {}", expr_text(e)),
        Trigger::Always => "always".into(),
        Trigger::Done => "done".into(),
        Trigger::Fail(p) => format!("fail {}", pattern(p)),
    }
}

impl<'p> Cx<'_, 'p> {
    /// The type of a machine variable. `call name` is resolved here, where the machine's calls
    /// are known: it is the one place such a type can be.
    fn var_type(&mut self, v: &ast::Var) -> Ty {
        let ast::TypeKind::Call(n) = &v.ty.kind else {
            return self.ck.resolve(self.file, &v.ty);
        };
        if let Some(init) = &v.init {
            self.ck.diags.push(
                Diagnostic::error(
                    codes::CALL_TYPE_NOT_ALLOWED,
                    init.span,
                    format!(
                        "`{}` holds a pending call, so it has no initial value",
                        v.name.name
                    ),
                )
                .with_help(format!(
                    "it starts unassigned; `set {} = call` in an `on {}` transition gives it one",
                    v.name.name, n.name
                )),
            );
        }
        self.info.call_vars.push((v.name.name.clone(), v.name.span));
        if let Some(id) = self.ck.call_symbol(self.symbol, &n.name) {
            if !self.ck.is_compiled(self.file) {
                self.ck.index.refer(id, n.span);
            }
        }
        if self.calls.contains_key(&n.name) {
            Ty::Call(n.name.clone())
        } else {
            self.unknown_call(&self.machine.clone(), n, None);
            Ty::Unknown
        }
    }

    /// "`M` has no call `name`", for `machine` (the callee, or this machine with `via` `None`).
    fn unknown_call(&mut self, machine: &str, n: &Ident, via: Option<&str>) {
        let calls: Vec<String> = match via {
            None => self.calls.keys().cloned().collect(),
            Some(_) => self
                .ck
                .interfaces
                .get(machine)
                .map(|i| i.calls.keys().cloned().collect())
                .unwrap_or_default(),
        };
        let declared: Vec<String> = calls.iter().map(|c| format!("`{c}`")).collect();
        let mut d = Diagnostic::error(
            codes::UNKNOWN_CALL,
            n.span,
            format!("`{machine}` has no call `{}`", n.name),
        )
        .with_label(if declared.is_empty() {
            format!("`{machine}` declares no calls")
        } else {
            format!("`{machine}` declares {}", declared.join(", "))
        });
        let upper: String = {
            let mut c = n.name.chars();
            c.next()
                .map(|f| f.to_uppercase().chain(c).collect())
                .unwrap_or_default()
        };
        if via.is_none() && self.events.contains_key(&upper) {
            d = d
                .with_help(format!(
                    "`{upper}` is an event of `{machine}`; events are upper-case: `on {upper}`"
                ))
                .with_fix(format!("replace with `{upper}`"), n.span, upper);
        } else if let Some(s) = suggest(&n.name, calls.iter().map(String::as_str)) {
            d = d.with_help(format!("did you mean `{s}`?")).with_fix(
                format!("replace with `{s}`"),
                n.span,
                s,
            );
        } else if via.is_none() {
            d = d.with_help(format!(
                "declare it in the machine body: `call {}(…)`",
                n.name
            ));
        } else {
            d = d.with_help(format!("declare it in `{machine}`: `call {}(…)`", n.name));
        }
        self.ck.diags.push(d);
    }

    fn build_tree(&mut self, parent: usize) {
        let body = self.nodes[parent].body;
        let mut seen: HashMap<&str, Span> = HashMap::new();
        for item in &body.items {
            let s = match item {
                Item::State(s) => s,
                Item::History(h) => {
                    self.history_node(parent, h, &mut seen);
                    continue;
                }
                _ => continue,
            };
            if let Some(first) = seen.get(s.name.name.as_str()) {
                let parent_name = self.nodes[parent].id.clone();
                self.ck.diags.push(
                    Diagnostic::error(
                        codes::DUPLICATE_NAME,
                        s.name.span,
                        format!(
                            "state `{}` is declared twice in `{parent_name}`",
                            s.name.name
                        ),
                    )
                    .with_secondary(*first, "first declared here")
                    .with_help("sibling states need different names"),
                );
            } else {
                seen.insert(&s.name.name, s.name.span);
            }
            let idx = self.nodes.len();
            let id = format!("{}.{}", self.nodes[parent].id, s.name.name);
            let word = match s.kind {
                StateKind::Normal => "state",
                StateKind::Final => "final state",
                StateKind::Parallel => "parallel state",
            };
            let symbol = self.ck.define(
                SymbolKind::State,
                &s.name,
                s.span,
                self.nodes[parent].symbol,
                &s.doc,
                format!("{word} {id}"),
            );
            let mut aliases: Vec<Alias<'p>> = Vec::new();
            for (index, inv) in s.body.invokes().enumerate() {
                let Some(a) = inv.alias.as_ref() else {
                    continue;
                };
                if let Some(first) = aliases.iter().find(|x| x.name.name == a.name) {
                    self.ck.diags.push(
                        Diagnostic::error(
                            codes::DUPLICATE_NAME,
                            a.span,
                            format!("`{}` already names an invoke of `{id}`", a.name),
                        )
                        .with_secondary(first.name.span, "named here")
                        .with_help(
                            "each invoke of a state needs a name of its own; choose another",
                        ),
                    );
                    continue;
                }
                let callee = match &inv.to {
                    Some(to) => format!("{}.{}", to.name, inv.callee.name),
                    None => inv.callee.name.clone(),
                };
                let symbol = self.ck.define(
                    SymbolKind::InvokeName,
                    a,
                    a.span,
                    Some(symbol),
                    &None,
                    format!("invoke {}(…) as {}", inv.callee.name, a.name),
                );
                aliases.push(Alias {
                    name: a,
                    callee,
                    index,
                    symbol: Some(symbol),
                });
            }
            self.nodes.push(Node {
                name: s.name.name.clone(),
                name_span: s.name.span,
                id,
                parent: Some(parent),
                children: Vec::new(),
                histories: Vec::new(),
                order: Vec::new(),
                history: None,
                body: &s.body,
                state: Some(s),
                depth: self.nodes[parent].depth + 1,
                aliases,
                symbol: Some(symbol),
            });
            self.nodes[parent].children.push(idx);
            self.nodes[parent].order.push(idx);
            self.build_tree(idx);
        }
    }

    /// Reports `key` as the name of a parameter, variable, `as` name or collection: inside the
    /// transitions a spawned child's input takes, `key` is that child's key.
    fn key_as_name(&mut self, name: &Ident, what: &str) {
        if name.name == "key" {
            self.ck.diags.push(
                Diagnostic::error(
                    codes::KEY_AS_NAME,
                    name.span,
                    format!("`key` cannot name {what}"),
                )
                .with_help("`key` is the implicit name of a spawned child's key, like `event`; choose another name"),
            );
        }
    }

    /// Adds history state `h` of `parent` to the tree.
    fn history_node(
        &mut self,
        parent: usize,
        h: &'p ast::History,
        seen: &mut HashMap<&'p str, Span>,
    ) {
        let parent_id = self.nodes[parent].id.clone();
        if let Some(first) = seen.get(h.name.name.as_str()) {
            self.ck.diags.push(
                Diagnostic::error(
                    codes::DUPLICATE_NAME,
                    h.name.span,
                    format!("state `{}` is declared twice in `{parent_id}`", h.name.name),
                )
                .with_secondary(*first, "first declared here")
                .with_help("a history state shares the names of its sibling states"),
            );
        } else {
            seen.insert(&h.name.name, h.name.span);
        }
        let idx = self.nodes.len();
        let id = format!("{parent_id}.{}", h.name.name);
        let word = if h.deep { "deep history" } else { "history" };
        let symbol = self.ck.define(
            SymbolKind::State,
            &h.name,
            h.span,
            self.nodes[parent].symbol,
            &h.doc,
            format!("{word} {id}"),
        );
        self.nodes.push(Node {
            name: h.name.name.clone(),
            name_span: h.name.span,
            id,
            parent: Some(parent),
            children: Vec::new(),
            histories: Vec::new(),
            order: Vec::new(),
            history: Some(h),
            body: self.nodes[parent].body,
            state: None,
            depth: self.nodes[parent].depth + 1,
            aliases: Vec::new(),
            symbol: Some(symbol),
        });
        self.nodes[parent].histories.push(idx);
        self.nodes[parent].order.push(idx);
    }

    /// Lowers history state `idx`, checking that its parent can have one.
    fn lower_history(&mut self, idx: usize, h: &ast::History) -> ir::State {
        let parent = self.nodes[idx]
            .parent
            .expect("a history state has a parent");
        let parent_kind = self.nodes[parent].state.map(|s| s.kind);
        let id = self.nodes[idx].id.clone();
        let word = if h.deep { "deep history" } else { "history" };
        let why = match parent_kind {
            Some(StateKind::Final) => Some("is a final state"),
            _ if self.nodes[parent].children.is_empty() => Some("has no child states"),
            _ => None,
        };
        if let Some(why) = why {
            let parent_id = &self.nodes[parent].id;
            self.ck.diags.push(
                Diagnostic::error(
                    codes::HISTORY_WITHOUT_CHILDREN,
                    h.span,
                    format!("`{word} {}`: `{parent_id}` {why}", h.name.name),
                )
                .with_help("a history state returns to the child states its parent was in when it was last left; put it in a state that has child states"),
            );
        }
        let default = self.history_default(idx, parent, h);
        self.info.states.insert(
            id.clone(),
            StateInfo {
                name_span: h.name.span,
                invoke_spans: Vec::new(),
                triggers: Vec::new(),
                initial_span: None,
                entry_reads: Vec::new(),
                exit_reads: Vec::new(),
                invoke_reads: Vec::new(),
                result_reads: Vec::new(),
                transition_reads: Vec::new(),
                retry_branches: Vec::new(),
                written_as: None,
                locals: Vec::new(),
            },
        );
        ir::State {
            name: self.nodes[idx].name.clone(),
            id,
            kind: if h.deep {
                ir::StateKind::DeepHistory
            } else {
                ir::StateKind::History
            },
            doc: doc(&h.doc),
            span: ir_span(h.span),
            params: Vec::new(),
            vars: Vec::new(),
            initial: None,
            default,
            states: Vec::new(),
            entry: Vec::new(),
            exit: Vec::new(),
            invokes: Vec::new(),
            transitions: Vec::new(),
            output: None,
            fail: None,
        }
    }

    /// Resolves and checks the default targets of history state `idx` of `parent`, `-> X, …`,
    /// as a transition's targets written in the history state: among its siblings, then
    /// outward. Each is a proper descendant of `parent` and not a history state (`RB0226`), no
    /// state from just below `parent` down to it has parameters (`RB0223`), and several go
    /// together as a transition's targets do (`RB0222`). The ids of those that pass.
    fn history_default(&mut self, idx: usize, parent: usize, h: &ast::History) -> Vec<String> {
        let word = if h.deep { "deep history" } else { "history" };
        let parent_id = self.nodes[parent].id.clone();
        let mut nodes = Vec::new();
        let mut ok = true;
        for t in &h.default {
            let Some(n) = self.target(idx, t) else {
                ok = false;
                continue;
            };
            let path: Vec<&str> = t.path.iter().map(|p| p.name.as_str()).collect();
            let written = format!("{word} {} -> {}", h.name.name, path.join("."));
            let problem = if n == parent {
                Some((
                    format!(
                        "`{written}`: `{parent_id}` is the history state's parent, not a state inside it"
                    ),
                    format!(
                        "a history state's default is where `{parent_id}` starts when it has nothing to return to: one of its child states, or a state below one"
                    ),
                ))
            } else if !self.inside(n, parent) {
                Some((
                    format!(
                        "`{written}`: `{}` is not inside `{parent_id}`",
                        self.nodes[n].id
                    ),
                    format!(
                        "a history state's default is where `{parent_id}` starts when it has nothing to return to: one of its child states, or a state below one"
                    ),
                ))
            } else if self.nodes[n].history.is_some() {
                Some((
                    format!("`{written}`: `{}` is a history state", self.nodes[n].id),
                    "a default names the states to enter; a history state is only a transition's target".to_string(),
                ))
            } else {
                // Step 5 (choice) adds: a choice state is not a default either.
                None
            };
            if let Some((message, help)) = problem {
                self.ck.diags.push(
                    Diagnostic::error(codes::BAD_HISTORY_DEFAULT, t.span, message)
                        .with_secondary(h.name.span, format!("the history state of `{parent_id}`"))
                        .with_help(help),
                );
                ok = false;
                continue;
            }
            // The states entered on the way, from just below the parent: none may take
            // parameters, since nothing passes them arguments.
            let mut x = n;
            while x != parent {
                let first = self.locals[x].iter().find(|l| l.param).map(|l| l.span);
                if let Some(first) = first {
                    let xid = self.nodes[x].id.clone();
                    self.ck.diags.push(
                        Diagnostic::error(
                            codes::PARAMETERS_ON_DEFAULT_ENTRY,
                            t.span,
                            format!("`{written}`: `{xid}` takes parameters, so the default of a history state cannot enter it"),
                        )
                        .with_secondary(first, "a parameter")
                        .with_help("a history state's default is entered with no transition to pass arguments; make the parameters variables of the state, or choose a default that does not enter it"),
                    );
                    ok = false;
                    break;
                }
                x = self.nodes[x].parent.unwrap_or(parent);
            }
            nodes.push(n);
        }
        if ok && nodes.len() > 1 {
            let before = self.ck.diags.len();
            self.check_targets_go_together(&h.default, &nodes);
            ok = self.ck.diags.len() == before;
        }
        if !ok {
            return Vec::new();
        }
        nodes.iter().map(|&n| self.nodes[n].id.clone()).collect()
    }

    /// Reports `as` names that cannot be used: on a task, the name of a parameter or variable,
    /// or the name of an invoke around it. Such a name is not in scope.
    fn check_aliases(&mut self) {
        for i in 0..self.nodes.len() {
            // In a state with several invokes, `as` names each of them, whatever it runs.
            let several = self.nodes[i].body.invokes().nth(1).is_some();
            for Alias {
                name: alias,
                callee,
                index,
                ..
            } in self.nodes[i].aliases.clone()
            {
                let word = callee_word(&callee);
                self.key_as_name(alias, &format!("an invoked {word}"));
                let mut bad = None;
                // In a state with several invokes, `as` on a call or on a task that emits
                // nothing is only a label, reported where the name is used to talk to it; it
                // still shares the machine's namespace.
                if !several && callee.contains('.') {
                    bad = Some(
                    Diagnostic::error(
                        codes::ALIAS_ON_TASK,
                        alias.span,
                        format!(
                            "`{callee}` is a call, so `as {}` gives it a name nothing can use",
                            alias.name
                        ),
                    )
                    .with_help("a call is answered once, with its outcome; talk to the machine through its `ref` instead; remove `as …`"),
                );
                } else if !several && word == "task" && self.task_emits(&callee) == Some(false) {
                    let declared = self
                        .ck
                        .sigs
                        .get(&callee)
                        .map(|s| s.name_span)
                        .filter(|_| !self.ck.is_compiled(self.file));
                    let mut d = Diagnostic::error(
                    codes::ALIAS_ON_TASK,
                    alias.span,
                    format!(
                        "`{callee}` is a task that declares no `emits`, so `as {}` gives it a name nothing can use",
                        alias.name
                    ),
                )
                .with_help(
                    "a task's `as` name receives what it emits: declare `emits Event` on the task, or remove `as …`",
                );
                    if let Some(span) = declared {
                        d = d.with_secondary(span, "declared here");
                    }
                    bad = Some(d);
                } else if let Some(v) = self.vars.get(&alias.name) {
                    let what = if v.param { "a parameter" } else { "a variable" };
                    bad = Some(
                    Diagnostic::error(
                        codes::DUPLICATE_NAME,
                        alias.span,
                        format!("`{}` is already {what} of `{}`", alias.name, self.machine),
                    )
                    .with_help("an `as` name shares the machine's names with its parameters and variables; choose another"),
                );
                } else if self.collections.contains_key(&alias.name) {
                    bad = Some(
                    Diagnostic::error(
                        codes::DUPLICATE_NAME,
                        alias.span,
                        format!(
                            "`{}` is already a children collection of `{}`",
                            alias.name, self.machine
                        ),
                    )
                    .with_help("an `as` name shares the machine's names with its collections; choose another"),
                );
                } else if let Some(&raise) = self.raise_names.get(&alias.name) {
                    bad = Some(
                        Diagnostic::error(
                            codes::DUPLICATE_NAME,
                            alias.span,
                            format!(
                                "`{}` is already the name of a delayed raise of `{}`",
                                alias.name, self.machine
                            ),
                        )
                        .with_secondary(raise, "named here")
                        .with_help("an `as` name shares the machine's names with its delayed raises; choose another"),
                    );
                } else {
                    let mut up = self.nodes[i].parent;
                    while let Some(a) = up {
                        if let Some(outer) = self.nodes[a]
                            .aliases
                            .iter()
                            .find(|x| x.name.name == alias.name)
                        {
                            bad = Some(
                            Diagnostic::error(
                                codes::DUPLICATE_NAME,
                                alias.span,
                                format!(
                                    "`{}` already names the {} invoked by `{}`, which contains this state",
                                    alias.name,
                                    callee_word(&outer.callee),
                                    self.nodes[a].id
                                ),
                            )
                            .with_secondary(outer.name.span, "named here")
                            .with_help("choose another name: both invokes run at once here"),
                        );
                            break;
                        }
                        up = self.nodes[a].parent;
                    }
                }
                if let Some(d) = bad {
                    self.ck.diags.push(d);
                    self.nodes[i].aliases.retain(|x| x.index != index);
                }
            }
        }
    }

    /// Gathers every state's locals, its parameters then its variables, checking their names
    /// and types: a name is unique in its state (`RB0108`), is not `key` (`RB0122`), is not
    /// anything else in scope there or a local of a state inside it (`RB0127`), and a local
    /// has no `ref` or `call` type (`RB0328`). `machine` holds the machine's parameters,
    /// variables and collections. Each is defined as a member of its state's symbol.
    fn collect_locals(&mut self, machine: &HashMap<String, Span>) {
        self.locals = vec![Vec::new(); self.nodes.len()];
        for i in 0..self.nodes.len() {
            let Some(s) = self.nodes[i].state else {
                continue;
            };
            let id = self.nodes[i].id.clone();
            let decls = s
                .params
                .iter()
                .map(|p| (&p.name, &p.ty, true, p.sensitive, p.span, &None, None))
                .chain(
                    s.body
                        .vars()
                        .map(|v| (&v.name, &v.ty, false, v.sensitive, v.span, &v.doc, Some(v))),
                );
            let mut mine: Vec<Local> = Vec::new();
            for (name, tref, param, sensitive, span, doc, var) in decls {
                let what = if param {
                    "a state parameter"
                } else {
                    "a state variable"
                };
                self.key_as_name(name, what);
                let ty = if let ast::TypeKind::Call(_) = tref.kind {
                    self.local_type_not_allowed(name, tref.span, "a pending call", &id);
                    Ty::Unknown
                } else {
                    let t = self.ck.resolve(self.file, tref);
                    if self.ck.env.has_ref(&t) {
                        self.local_type_not_allowed(name, tref.span, "a `ref`", &id);
                        Ty::Unknown
                    } else {
                        t
                    }
                };
                if let Some(first) = mine.iter().find(|l| l.name == name.name) {
                    let kind = if first.param {
                        "a parameter"
                    } else {
                        "a variable"
                    };
                    self.ck.diags.push(
                        Diagnostic::error(
                            codes::DUPLICATE_NAME,
                            name.span,
                            format!("`{}` is already {kind} of `{id}`", name.name),
                        )
                        .with_secondary(first.span, "first declared here")
                        .with_help("a state's parameters and variables share one set of names"),
                    );
                    continue;
                }
                self.local_clash(i, name, machine);
                let signature = match var {
                    None => crate::index::field_text(&ast::Field {
                        sensitive,
                        name: name.clone(),
                        ty: tref.clone(),
                        span,
                    }),
                    Some(v) => {
                        let mut sig = format!(
                            "{}var {}",
                            if v.sensitive { "sensitive " } else { "" },
                            crate::index::field_text_of(&v.name, &v.ty)
                        );
                        if let Some(init) = &v.init {
                            sig.push_str(&format!(" = {}", expr_text(init)));
                        }
                        sig
                    }
                };
                let kind = if param {
                    SymbolKind::Parameter
                } else {
                    SymbolKind::Variable
                };
                let symbol = self.nodes[i]
                    .symbol
                    .map(|st| self.ck.member(st, kind, name, span, doc, signature));
                mine.push(Local {
                    name: name.name.clone(),
                    span: name.span,
                    ty,
                    param,
                    sensitive,
                    symbol,
                });
            }
            self.locals[i] = mine;
        }
    }

    fn local_type_not_allowed(&mut self, name: &Ident, span: Span, what: &str, id: &str) {
        self.ck.diags.push(
            Diagnostic::error(
                codes::LOCAL_TYPE_NOT_ALLOWED,
                span,
                format!("`{}`, a local of `{id}`, cannot hold {what}", name.name),
            )
            .with_help("a handle to a peer and a pending call belong to the machine: declare it as a machine parameter or variable"),
        );
    }

    /// Reports local `name` of node `i` if its name is already in scope there or is an `as`
    /// name of a state inside it (`RB0127`). A clash with a local of a state inside it is
    /// reported at that local, when its turn comes.
    fn local_clash(&mut self, i: usize, name: &Ident, machine: &HashMap<String, Span>) {
        let id = self.nodes[i].id.clone();
        let clash: Option<(String, Option<Span>)> = if let Some(v) = self.vars.get(&name.name) {
            let what = if v.param { "a parameter" } else { "a variable" };
            Some((
                format!("{what} of machine `{}`", self.machine),
                machine.get(&name.name).copied(),
            ))
        } else if self.collections.contains_key(&name.name) {
            Some((
                format!("a children collection of `{}`", self.machine),
                machine.get(&name.name).copied(),
            ))
        } else if let Some(span) = self.raise_names.get(&name.name) {
            Some((
                format!("the name of a delayed raise of `{}`", self.machine),
                Some(*span),
            ))
        } else if let Some((owner, local)) = self.ancestors(i).find_map(|a| {
            self.locals[a]
                .iter()
                .find(|l| l.name == name.name)
                .map(|l| (a, l.span))
        }) {
            Some((
                format!(
                    "a local of `{}`, which contains `{id}`",
                    self.nodes[owner].id
                ),
                Some(local),
            ))
        } else {
            // An `as` name of the state, a state around it, or a state inside it.
            (0..self.nodes.len())
                .filter(|&n| n == i || self.inside(n, i) || self.inside(i, n))
                .find_map(|n| {
                    let a = self.alias_of(n, &name.name)?;
                    Some((
                        format!("the `as` name of an invoke of `{}`", self.nodes[n].id),
                        Some(a.name.span),
                    ))
                })
        };
        let Some((what, first)) = clash else {
            return;
        };
        let mut d = Diagnostic::error(
            codes::LOCAL_SHADOWS,
            name.span,
            format!("`{}` is already {what}", name.name),
        )
        .with_help("a state's locals cannot have the name of anything else in scope in the state, or of a local of a state inside it; choose another name");
        if let Some(span) = first {
            d = d.with_secondary(span, "declared here");
        }
        self.ck.diags.push(d);
    }

    /// The proper ancestors of node `i`, innermost first.
    fn ancestors(&self, i: usize) -> impl Iterator<Item = usize> + '_ {
        std::iter::successors(self.nodes[i].parent, |&p| self.nodes[p].parent)
    }

    /// The local `name` in scope where the items of [`Cx::here`] are lowered: of that state or
    /// of a state around it. With the node it belongs to.
    pub(crate) fn local(&self, name: &str) -> Option<(usize, &Local)> {
        if self.locals.is_empty() {
            return None;
        }
        std::iter::once(self.here)
            .chain(self.ancestors(self.here))
            .find_map(|n| {
                self.locals[n]
                    .iter()
                    .find(|l| l.name == name)
                    .map(|l| (n, l))
            })
    }

    /// The id of node `n`.
    pub(crate) fn node_id(&self, n: usize) -> &str {
        &self.nodes[n].id
    }

    /// A state's local named `name` anywhere in the machine, for the diagnostic of a read
    /// outside its scope.
    pub(crate) fn local_anywhere(&self, name: &str) -> Option<&str> {
        self.locals
            .iter()
            .position(|ls| ls.iter().any(|l| l.name == name))
            .map(|n| self.nodes[n].id.as_str())
    }

    /// The `as` name `name` of one of node `a`'s invokes.
    fn alias_of(&self, a: usize, name: &str) -> Option<&Alias<'p>> {
        self.nodes[a].aliases.iter().find(|x| x.name.name == name)
    }

    /// The first state with an invoke named `name`, wherever it is, and the alias.
    fn alias_anywhere(&self, name: &str) -> Option<(&Node<'p>, &Alias<'p>)> {
        self.nodes
            .iter()
            .find_map(|n| Some((n, n.aliases.iter().find(|x| x.name.name == name)?)))
    }

    /// For a declared task, whether it declares any `emits`; `None` for anything else.
    pub(crate) fn task_emits(&self, name: &str) -> Option<bool> {
        match self.ck.names.lookup(self.file, name) {
            Found::Decl(id) => match &self.ck.names.decl(id).kind {
                DeclKind::Task { emits, .. } => Some(!emits.is_empty()),
                _ => None,
            },
            _ => None,
        }
    }

    /// Whether `a` is a proper descendant of `of`.
    fn inside(&self, a: usize, of: usize) -> bool {
        let mut up = self.nodes[a].parent;
        while let Some(p) = up {
            if p == of {
                return true;
            }
            up = self.nodes[p].parent;
        }
        false
    }

    /// Resolves the `name` of `send name.E` or `on name.E` written in `node`: the innermost
    /// `invoke … as name` of the state or a state containing it, else a parameter or variable
    /// of type `ref`. Returns the address and, for a child, the invoking state's node.
    fn address(&mut self, node: usize, name: &Ident) -> Option<(ir::Address, Option<usize>)> {
        let mut up = Some(node);
        while let Some(a) = up {
            if let Some(Alias {
                callee,
                symbol,
                name: alias,
                ..
            }) = self.alias_of(a, &name.name).cloned()
            {
                let callee = &callee;
                if let Some(symbol) = symbol {
                    self.ck.index.refer(symbol, name.span);
                }
                if callee.contains('.') {
                    // `as` on a call names it only among its state's several invokes.
                    self.ck.diags.push(
                        Diagnostic::error(
                            codes::UNKNOWN_ADDRESS,
                            name.span,
                            format!(
                                "`{}` names the call `{callee}` that `{}` invokes, which takes no events and emits none",
                                name.name, self.nodes[a].id
                            ),
                        )
                        .with_secondary(alias.span, "named here")
                        .with_help("a call is answered once, with its outcome; talk to the machine through its `ref` instead"),
                    );
                    return None;
                }
                // An invoke of a machine or task that is not declared (already reported) names
                // a child with no known events: the name is in scope, but nothing is checked
                // against it.
                let task = callee_word(callee) == "task";
                let known = matches!(
                    self.ck.names.lookup(self.file, callee),
                    Found::Decl(id) if match self.ck.names.decl(id).kind {
                        DeclKind::Machine(_) => !task,
                        DeclKind::Task { .. } => task,
                        _ => false,
                    }
                );
                if !known {
                    return None;
                }
                let (kind, machine, task) = if task {
                    (ir::AddressKind::Task, None, Some(callee.clone()))
                } else {
                    (ir::AddressKind::Child, Some(callee.clone()), None)
                };
                let address = ir::Address {
                    kind,
                    name: name.name.clone(),
                    machine,
                    task,
                    state: Some(self.nodes[a].id.clone()),
                };
                return Some((address, Some(a)));
            }
            up = self.nodes[a].parent;
        }
        if let Some(c) = self.collections.get(&name.name).cloned() {
            if let Some(symbol) = c.symbol {
                self.ck.index.refer(symbol, name.span);
            }
            let src = c.src?;
            let (machine, task) = if c.task {
                (None, Some(src))
            } else {
                (Some(src), None)
            };
            let address = ir::Address {
                kind: ir::AddressKind::Children,
                name: name.name.clone(),
                machine,
                task,
                state: None,
            };
            return Some((address, None));
        }
        if let Some(v) = self.vars.get(&name.name) {
            let t = v.ty.clone();
            self.ck.refer_member(self.file, self.symbol, name);
            let why = match self.ck.env.expand(&t) {
                Ty::Ref(machine) => {
                    let address = ir::Address {
                        kind: ir::AddressKind::Ref,
                        name: name.name.clone(),
                        machine: Some(machine),
                        task: None,
                        state: None,
                    };
                    return Some((address, None));
                }
                Ty::Unknown => return None,
                Ty::Optional(_) if self.ck.env.has_ref(&t) => {
                    format!("`{}` is `{t}`, which may be absent", name.name)
                }
                Ty::List(_) if self.ck.env.has_ref(&t) => {
                    format!("`{}` is `{t}`, a list of handles, not one", name.name)
                }
                _ => format!("`{}` is `{t}`, not a `ref`", name.name),
            };
            self.ck.diags.push(
                Diagnostic::error(codes::NOT_A_REF, name.span, why)
                    .with_help("events are sent to, and received from, an `invoke … as name` or a parameter or variable of type `ref Machine`"),
            );
            return None;
        }
        let elsewhere = self
            .alias_anywhere(&name.name)
            .map(|(n, a)| (n.id.clone(), callee_word(&a.callee)));
        let mut d = match &elsewhere {
            Some((state, word)) => Diagnostic::error(
                codes::UNKNOWN_ADDRESS,
                name.span,
                format!("`{}` is not in scope here: it names the {word} `{state}` invokes", name.name),
            )
            .with_help(format!(
                "an `as` name is in scope only in the invoking state and the states inside it, while the {} runs; move this into `{state}`",
                if *word == "task" { "task" } else { "child" }
            )),
            None => Diagnostic::error(
                codes::UNKNOWN_ADDRESS,
                name.span,
                format!("unknown machine `{}` to talk to", name.name),
            ),
        };
        if elsewhere.is_none() {
            let mut candidates: Vec<String> = Vec::new();
            let mut up = Some(node);
            while let Some(a) = up {
                candidates.extend(self.nodes[a].aliases.iter().map(|x| x.name.name.clone()));
                up = self.nodes[a].parent;
            }
            for (n, v) in &self.vars {
                if matches!(self.ck.env.expand(&v.ty), Ty::Ref(_)) {
                    candidates.push(n.clone());
                }
            }
            candidates.extend(self.collections.keys().cloned());
            candidates.sort();
            if let Some(s) = suggest(&name.name, candidates.iter().map(String::as_str)) {
                d = d.with_help(format!("did you mean `{s}`?")).with_fix(
                    format!("replace with `{s}`"),
                    name.span,
                    s,
                );
            } else {
                d = d.with_help("name an invoked machine with `invoke Machine(…) as name`, or take a parameter of type `ref Machine`");
            }
        }
        self.ck.diags.push(d);
        None
    }

    /// Resolves the `name` of `on name.ended` written in `node`: a parameter or variable of type
    /// `ref M`, `ref M?` or `ref M[]`, through aliases, as an address of kind `ref`. Reports an
    /// invoked child or a collection, whose ends are their outcomes, and every other name.
    fn peer(&mut self, node: usize, name: &Ident) -> Option<ir::Address> {
        let child = |state: &str| {
            format!(
                "an invoke's end is its outcome: handle it with `done` and `fail` inside the `invoke {{ }}` of `{state}`"
            )
        };
        let mut up = Some(node);
        while let Some(a) = up {
            if let Some(Alias {
                name: alias,
                callee,
                symbol,
                ..
            }) = self.alias_of(a, &name.name).cloned()
            {
                let (span, state) = (alias.span, self.nodes[a].id.clone());
                let word = callee_word(&callee);
                if let Some(symbol) = symbol {
                    self.ck.index.refer(symbol, name.span);
                }
                self.ck.diags.push(
                    Diagnostic::error(
                        codes::ENDED_NOT_A_PEER,
                        name.span,
                        format!(
                            "`{}` names the {word} `{state}` invokes, not a `ref`, so it has no `ended`",
                            name.name
                        ),
                    )
                    .with_secondary(span, "invoked here")
                    .with_help(child(&state)),
                );
                return None;
            }
            up = self.nodes[a].parent;
        }
        if let Some(c) = self.collections.get(&name.name).cloned() {
            if let Some(symbol) = c.symbol {
                self.ck.index.refer(symbol, name.span);
            }
            self.ck.diags.push(
                Diagnostic::error(
                    codes::ENDED_NOT_A_PEER,
                    name.span,
                    format!(
                        "`{}` is a children collection, not a `ref`, so it has no `ended`",
                        name.name
                    ),
                )
                .with_help(format!(
                    "a spawned child's end is its outcome: take it with `on {0}.done` and `on {0}.fail E` (or `on {0}.fail *`)",
                    name.name
                )),
            );
            return None;
        }
        if let Some(v) = self.vars.get(&name.name) {
            let t = v.ty.clone();
            self.ck.refer_member(self.file, self.symbol, name);
            let machine = match self.ck.env.expand(&t) {
                Ty::Ref(m) => Some(m),
                Ty::Optional(e) | Ty::List(e) => match *e {
                    Ty::Ref(m) => Some(m),
                    _ => None,
                },
                Ty::Unknown => return None,
                _ => None,
            };
            if let Some(machine) = machine {
                return Some(ir::Address {
                    kind: ir::AddressKind::Ref,
                    name: name.name.clone(),
                    machine: Some(machine),
                    task: None,
                    state: None,
                });
            }
            self.ck.diags.push(
                Diagnostic::error(
                    codes::NOT_A_REF,
                    name.span,
                    format!("`{}` is `{t}`, not a `ref`", name.name),
                )
                .with_help("`on name.ended` is taken when the run that a parameter or variable of type `ref Machine`, `ref Machine?` or `ref Machine[]` holds ends"),
            );
            return None;
        }
        let elsewhere = self
            .alias_anywhere(&name.name)
            .map(|(n, a)| (n.id.clone(), Some(a.name.span), callee_word(&a.callee)));
        if let Some((state, span, word)) = elsewhere {
            let mut d = Diagnostic::error(
                codes::ENDED_NOT_A_PEER,
                name.span,
                format!(
                    "`{}` names the {word} `{state}` invokes, not a `ref`, so it has no `ended`",
                    name.name
                ),
            )
            .with_help(child(&state));
            if let Some(span) = span {
                d = d.with_secondary(span, "invoked here");
            }
            self.ck.diags.push(d);
            return None;
        }
        let candidates: Vec<String> = self
            .vars
            .iter()
            .filter(|(_, v)| {
                matches!(
                    self.ck.env.expand(&v.ty),
                    Ty::Ref(_) | Ty::Optional(_) | Ty::List(_)
                ) && self.ck.env.has_ref(&v.ty)
            })
            .map(|(n, _)| n.clone())
            .collect();
        let mut d = Diagnostic::error(
            codes::UNKNOWN_ADDRESS,
            name.span,
            format!("unknown run `{}` to watch", name.name),
        );
        d = match suggest(&name.name, candidates.iter().map(String::as_str)) {
            Some(s) => d.with_help(format!("did you mean `{s}`?")).with_fix(
                format!("replace with `{s}`"),
                name.span,
                s,
            ),
            None => d.with_help(
                "`on name.ended` watches the run that a parameter or variable of type `ref Machine` holds",
            ),
        };
        self.ck.diags.push(d);
        None
    }

    /// Reports a `send` to the child invoked by `invoking` from a place where it is not
    /// running: the child starts after the invoking state's `entry`, stops before its `exit`
    /// and has ended when an outcome is taken, and a transition that leaves the invoking state
    /// stops it before its effects run.
    fn check_running(&mut self, place: Place, invoking: usize, name: &Ident) {
        let state = self.nodes[invoking].id.clone();
        let invoke = self.alias_of(invoking, &name.name).map(|a| a.index);
        let why = match place {
            Place::Entry(n) if n == invoking => Some(format!(
                "in the `entry` of `{state}`, its child has not started yet: it starts once the step that enters `{state}` has settled"
            )),
            Place::Exit(n) if n == invoking => Some(format!(
                "in the `exit` of `{state}`, its child has already been stopped"
            )),
            Place::Transition {
                node,
                outcome: Some(k),
                ..
            } if node == invoking && Some(k) == invoke => {
                Some(if self.nodes[invoking].body.invokes().nth(1).is_some() {
                    format!("in an outcome of `{}`, it has already ended", name.name)
                } else {
                    format!("in an outcome of the invoke of `{state}`, its child has already ended")
                })
            }
            // A transition exits the states below its domain (for an internal one written in a
            // parallel state, below some of its regions): the invoking state is the state the
            // transition is written in or one around it, so it is exited when it is below the
            // domain.
            Place::Transition {
                domain: Some(d), ..
            } if self.inside(invoking, d) => Some(format!(
                "this transition leaves `{state}`, which stops its child before the transition's effects run"
            )),
            _ => None,
        };
        if let Some(why) = why {
            self.ck.diags.push(
                Diagnostic::error(
                    codes::CHILD_NOT_RUNNING,
                    name.span,
                    format!("`{}` is not running here: {why}", name.name),
                )
                .with_help("send to an invoked child from a transition that stays inside the invoking state (an internal one, `-> .X`, written in it among them), or from the states inside it"),
            );
        }
    }

    /// Checks and lowers the event value of a `send` (`to` is `Some`) or an `emit`.
    fn message(&mut self, m: &Message, scope: &Scope, place: Place) -> Option<ir::Effect> {
        let current = match place {
            Place::Entry(n) | Place::Exit(n) | Place::Transition { node: n, .. } => n,
        };
        let event = &m.event;
        match &m.to {
            Some(to) => {
                let Some((address, invoking)) = self.address(current, to) else {
                    // The key is still checked, for its own mistakes.
                    if let Some(SendKey::Key(k)) = &m.key {
                        self.expr(k, scope);
                    }
                    return None;
                };
                if let Some(task) = &address.task {
                    if let Some(SendKey::Key(k)) = &m.key {
                        self.expr(k, scope);
                    }
                    let what = if address.kind == ir::AddressKind::Children {
                        format!("`{}` holds `{task}` tasks, which take no events", to.name)
                    } else {
                        format!(
                            "`{}` names the task `{task}`, which takes no events",
                            to.name
                        )
                    };
                    self.ck.diags.push(
                        Diagnostic::error(
                            codes::NOT_ACCEPTED,
                            event.span,
                            format!("{what}: it cannot be sent `{}`", event.name),
                        )
                        .with_help(format!(
                            "a task only emits, as `on {}.Event`; host code decides how it reacts, so pass what it needs as an argument when it starts",
                            to.name
                        )),
                    );
                    return None;
                }
                let is_collection = address.kind == ir::AddressKind::Children;
                let key = match (&m.key, is_collection) {
                    (Some(SendKey::Key(k)), true) => {
                        let want = self.collections[&to.name].key.clone();
                        Some(self.expect(k, scope, &want, &format!("the key of `{}`", to.name)))
                    }
                    (Some(SendKey::All { .. }), true) => None,
                    (None, true) => {
                        self.ck.diags.push(
                            Diagnostic::error(
                                codes::NOT_A_COLLECTION,
                                to.span,
                                format!("`{}` is a children collection: say which child to send to", to.name),
                            )
                            .with_help(format!(
                                "send to one child with `send {0}[key].{1}`, or to every child with `send {0}.*.{1}`",
                                to.name, event.name
                            )),
                        );
                        None
                    }
                    (Some(k), false) => {
                        let span = match k {
                            SendKey::Key(k) => {
                                self.expr(k, scope);
                                k.span
                            }
                            SendKey::All { span } => *span,
                        };
                        self.ck.diags.push(
                            Diagnostic::error(
                                codes::NOT_A_COLLECTION,
                                span,
                                format!(
                                    "`{}` is not a children collection, so it takes no key",
                                    to.name
                                ),
                            )
                            .with_help(format!(
                                "send to it with `send {}.{}`",
                                to.name, event.name
                            )),
                        );
                        None
                    }
                    (None, false) => None,
                };
                // A send to a `ref` variable reads it.
                if address.kind == ir::AddressKind::Ref
                    && self.vars.get(&to.name).is_some_and(|v| !v.param)
                {
                    self.reads.push((to.name.clone(), to.span));
                }
                if let Some(i) = invoking {
                    self.check_running(place, i, to);
                }
                let receiver_name = address.peer().to_owned();
                let iface = self
                    .ck
                    .interfaces
                    .get(&receiver_name)
                    .cloned()
                    .unwrap_or_default();
                let receiver = self.ck.decl_symbol(&receiver_name);
                self.ck.refer_member(self.file, receiver, event);
                let fields = iface.events.get(&event.name).cloned();
                if fields.is_none() {
                    self.not_declared(
                        codes::NOT_ACCEPTED,
                        &receiver_name,
                        &iface,
                        event,
                        false,
                        &to.name,
                    );
                }
                let ty = Ty::Event(format!("{receiver_name}.{}", event.name));
                let value = self.message_value(m, &ty, fields.as_deref(), scope);
                Some(ir::Effect::Send {
                    to: address,
                    key,
                    event: event.name.clone(),
                    value,
                })
            }
            None => {
                self.ck.refer_member(self.file, self.symbol, event);
                let fields = self.emits.get(&event.name).cloned();
                if fields.is_some() {
                    self.info.emitted.insert(event.name.clone());
                } else {
                    let iface = self
                        .ck
                        .interfaces
                        .get(&self.machine)
                        .cloned()
                        .unwrap_or_default();
                    let machine = self.machine.clone();
                    self.not_declared(codes::NOT_EMITTED, &machine, &iface, event, true, "");
                }
                let ty = Ty::Event(event.name.clone());
                let value = self.message_value(m, &ty, fields.as_deref(), scope);
                Some(ir::Effect::Emit {
                    event: event.name.clone(),
                    value,
                })
            }
        }
    }

    /// "`Upload` does not accept `Stop`" (`emitted` false: a `send`) or "… does not emit"
    /// (an `emit`, or `on name.E` with `via` the name).
    fn not_declared(
        &mut self,
        code: Code,
        machine: &str,
        iface: &crate::check::Interface,
        event: &Ident,
        emitted: bool,
        via: &str,
    ) {
        let (verb, list, other, keyword) = if emitted {
            ("emit", &iface.emits, &iface.events, "emits")
        } else {
            ("accept", &iface.events, &iface.emits, "event")
        };
        let own = machine == self.machine && via.is_empty();
        let mut d = Diagnostic::error(
            code,
            event.span,
            format!("`{machine}` does not {verb} the event `{}`", event.name),
        );
        let declared: Vec<String> = list.keys().map(|k| format!("`{k}`")).collect();
        d = d.with_label(if declared.is_empty() {
            format!("`{machine}` declares no `{keyword}`")
        } else {
            format!("`{machine}` {verb}s {}", declared.join(", "))
        });
        if other.contains_key(&event.name) {
            let help = if emitted {
                format!(
                    "`{}` is an event `{machine}` accepts, not one it emits",
                    event.name
                )
            } else {
                format!(
                    "`{}` is an event `{machine}` emits: receive it with `on {via}.{}`",
                    event.name, event.name
                )
            };
            d = d.with_help(help);
        } else if iface.internal.contains_key(&event.name) {
            let help = if emitted {
                format!(
                    "`{}` is an internal event of `{machine}`, which only it raises",
                    event.name
                )
            } else {
                format!(
                    "`{}` is an internal event of `{machine}`: only `{machine}` raises it, so no other machine can send it",
                    event.name
                )
            };
            d = d.with_help(help);
        } else if let Some(s) = suggest(&event.name, list.keys().map(String::as_str)) {
            d = d.with_help(format!("did you mean `{s}`?")).with_fix(
                format!("replace with `{s}`"),
                event.span,
                s,
            );
        } else if own {
            d = d.with_help(format!(
                "declare it in the machine body: `{keyword} {}`",
                event.name
            ));
        } else {
            d = d.with_help(format!(
                "declare it in `{machine}`: `{keyword} {}`",
                event.name
            ));
        }
        self.ck.diags.push(d);
    }

    fn message_value(
        &mut self,
        m: &Message,
        ty: &Ty,
        fields: Option<&[(String, Ty)]>,
        scope: &Scope,
    ) -> ir::Expr {
        let shown = m.event.name.clone();
        let out = self.record_fields(&shown, ty, fields, &m.fields, m.value_span, scope);
        ir::Expr {
            kind: ir::ExprKind::Record { fields: out },
            ty: ty.to_ir(),
            text: message_value(m),
        }
    }

    /// The collection `name` names, recording the use; reports a name that is not one.
    /// `written` is the construct, for the message: `spawn sessions`, `on sessions.done`.
    fn collection(&mut self, name: &Ident, written: &str) -> Option<Collection> {
        if let Some(c) = self.collections.get(&name.name).cloned() {
            if let Some(symbol) = c.symbol {
                self.ck.index.refer(symbol, name.span);
            }
            return Some(c);
        }
        let alias = self.alias_anywhere(&name.name).is_some();
        let what = if self.vars.contains_key(&name.name) {
            format!(
                "`{}` is a parameter or variable, not a children collection",
                name.name
            )
        } else if alias {
            format!(
                "`{}` names an invoke's machine or task, not a children collection",
                name.name
            )
        } else {
            format!("unknown children collection `{}`", name.name)
        };
        let mut d = Diagnostic::error(codes::NOT_A_COLLECTION, name.span, what);
        if alias {
            d = d.with_help(format!(
                "what an invoke runs is reached with `on {0}.Event` (and a machine with `send {0}.Event`), and its outcome is handled inside its `invoke {{ }}`",
                name.name
            ));
        } else if let Some(s) = suggest(&name.name, self.collections.keys().map(String::as_str)) {
            d = d.with_help(format!("did you mean `{s}`?")).with_fix(
                format!("replace with `{s}`"),
                name.span,
                s,
            );
        } else {
            d = d.with_help(format!(
                "`{written}` needs a collection declared in the machine body: `children {}: Machine by Key` or `children {0}: task by Key`",
                name.name
            ));
        }
        self.ck.diags.push(d);
        None
    }

    fn child(&self, node: usize, name: &str) -> Option<usize> {
        self.nodes[node]
            .children
            .iter()
            .copied()
            .find(|&c| self.nodes[c].name == name)
    }

    fn history_child(&self, node: usize, name: &str) -> Option<usize> {
        self.nodes[node]
            .histories
            .iter()
            .copied()
            .find(|&c| self.nodes[c].name == name)
    }

    /// A child state or history state named `name`: what a target names.
    fn target_child(&self, node: usize, name: &str) -> Option<usize> {
        self.child(node, name)
            .or_else(|| self.history_child(node, name))
    }

    fn lower_state(&mut self, idx: usize) -> ir::State {
        if let Some(h) = self.nodes[idx].history {
            return self.lower_history(idx, h);
        }
        let body = self.nodes[idx].body;
        let state = self.nodes[idx].state;
        let has_children = !self.nodes[idx].children.is_empty();
        let kind = match state.map(|s| s.kind) {
            None => ir::StateKind::Compound,
            Some(StateKind::Final) => ir::StateKind::Final,
            Some(StateKind::Parallel) => ir::StateKind::Parallel,
            Some(StateKind::Normal) if has_children => ir::StateKind::Compound,
            Some(StateKind::Normal) => ir::StateKind::Atomic,
        };
        let id = self.nodes[idx].id.clone();
        let name_span = self.nodes[idx].name_span;
        if kind == ir::StateKind::Final && has_children {
            self.ck.diags.push(
                Diagnostic::error(
                    codes::STATE_KIND_MISMATCH,
                    name_span,
                    format!("final state `{id}` has child states"),
                )
                .with_help("a final state ends its parent; it cannot contain states"),
            );
        }
        if kind == ir::StateKind::Parallel && !has_children {
            self.ck.diags.push(
                Diagnostic::error(
                    codes::STATE_KIND_MISMATCH,
                    name_span,
                    format!("parallel state `{id}` has no child states"),
                )
                .with_help("the children of a parallel state are its regions; add at least one, or drop `parallel`"),
            );
        }

        let initial = self.initial(idx, kind);
        if kind == ir::StateKind::Parallel {
            for c in self.nodes[idx].children.clone() {
                self.default_entered_with_parameters(c, None, &id);
            }
        }
        self.here = idx;
        let (params, vars, locals) = self.lower_locals(idx);
        self.info.states.insert(
            id.clone(),
            StateInfo {
                name_span,
                invoke_spans: Vec::new(),
                triggers: Vec::new(),
                initial_span: if kind == ir::StateKind::Compound {
                    body.initials().next().map(|n| n.span)
                } else {
                    None
                },
                entry_reads: Vec::new(),
                exit_reads: Vec::new(),
                invoke_reads: Vec::new(),
                result_reads: Vec::new(),
                transition_reads: Vec::new(),
                retry_branches: Vec::new(),
                written_as: None,
                locals,
            },
        );

        let mut entry = Vec::new();
        let mut exit = Vec::new();
        let mut invokes: Vec<ir::Invoke> = Vec::new();
        let several = body.invokes().nth(1).is_some();
        let mut transitions = Vec::new();
        let mut triggers: Vec<Span> = Vec::new();
        let mut keys: HashMap<String, Span> = HashMap::new();
        let mut output = None;
        let mut fail = None;
        let mut result_seen: Option<Span> = None;
        let mut entry_reads = Vec::new();
        let mut exit_reads = Vec::new();
        let mut invoke_reads = Vec::new();
        let mut result_reads = Vec::new();
        self.transition_reads.clear();
        let mut group = 0u32;
        let top_final = kind == ir::StateKind::Final && self.nodes[idx].depth == 1;
        let mut retry: Option<retry::Plan<'p>> = None;

        for item in &body.items {
            self.reads.clear();
            if kind == ir::StateKind::Final
                && let Item::Transition(_) | Item::Invoke(_) = item
            {
                let what = if let Item::Invoke(_) = item {
                    "an `invoke`"
                } else {
                    "a transition"
                };
                let span = match item {
                    Item::Transition(t) => t.trigger_span,
                    Item::Invoke(inv) => invoke_head(inv),
                    _ => item.span(),
                };
                self.ck.diags.push(
                    Diagnostic::error(
                        codes::ACTIVITY_IN_FINAL_STATE,
                        span,
                        format!("final state `{id}` has {what}, which could never run"),
                    )
                    .with_help("a final state ends its parent as soon as it is entered; only `entry`, `exit`, `output` and `fail` belong in it"),
                );
            }
            if kind == ir::StateKind::Atomic
                && let Item::Transition(t) = item
                && matches!(t.trigger, Trigger::Done)
            {
                self.ck.diags.push(
                    Diagnostic::error(
                        codes::DONE_WITHOUT_CHILDREN,
                        t.trigger_span,
                        format!("`{id}` has no child states, so its `done` can never be taken"),
                    )
                    .with_help("`done` is taken when a compound state reaches a final child, or every region of a parallel state does; to leave when an `invoke` finishes, put `done` inside the `invoke { }`"),
                );
            }
            match item {
                Item::Entry(h) => {
                    let (fx, reads) = self.effects(&h.effects, &Scope::none(), Place::Entry(idx));
                    entry.extend(fx);
                    entry_reads.extend(reads);
                }
                Item::Exit(h) => {
                    let (fx, reads) = self.effects(&h.effects, &Scope::none(), Place::Exit(idx));
                    exit.extend(fx);
                    exit_reads.extend(reads);
                }
                Item::Transition(t) => {
                    self.duplicate_trigger(&mut keys, t, &id, "");
                    let (trigger, scope) = self.state_trigger(idx, &t.trigger);
                    let first = transitions.len();
                    self.branches(
                        idx,
                        t,
                        trigger,
                        &scope,
                        (group, None),
                        &mut transitions,
                        &mut triggers,
                    );
                    if let Trigger::OneOf(names) = &t.trigger {
                        // One copy of the branches per other event, sharing their group and
                        // spans: checked once, taken as if written as separate `on`s.
                        let count = transitions.len() - first;
                        let reads = self.transition_reads.len() - count;
                        for n in &names[1..] {
                            for k in 0..count {
                                let mut copy = transitions[first + k].clone();
                                copy.trigger = ir::Trigger::Event {
                                    event: n.name.clone(),
                                };
                                transitions.push(copy);
                                triggers.push(triggers[first + k]);
                                let r = self.transition_reads[reads + k].clone();
                                self.transition_reads.push(r);
                            }
                        }
                    }
                    group += 1;
                }
                Item::Invoke(inv) => {
                    let first = transitions.len();
                    let index = invokes.len();
                    let (lowered, reads, declared) = self.invoke(
                        idx,
                        inv,
                        (index, several),
                        &mut group,
                        &mut transitions,
                        &mut triggers,
                    );
                    if let Some(r) = &inv.retry {
                        if several {
                            self.ck.diags.push(
                                Diagnostic::error(
                                    codes::RETRY_WITH_CHILDREN,
                                    r.span,
                                    format!("`{id}` has other invokes, so this one cannot `retry`"),
                                )
                                .with_help("`retry` gives the state the children `Attempt` and `Backoff`, which hold its one invoke; move the retried invoke into a state of its own, such as a region of a parallel state"),
                            );
                        } else {
                            retry = Some(retry::Plan {
                                inv,
                                retry: r,
                                outcomes: first..transitions.len(),
                                declared,
                            });
                        }
                    }
                    if several && inv.alias.is_none() {
                        self.ck.diags.push(
                            Diagnostic::error(
                                codes::SECOND_INVOKE,
                                invoke_head(inv),
                                format!("`{id}` has several invokes, so this one needs a name"),
                            )
                            .with_help("each invoke of a state with several says which one its outcomes belong to by its `as` name: add `as <name>`"),
                        );
                    }
                    invokes.push(lowered);
                    invoke_reads.extend(reads);
                    if let Some(info) = self.info.states.get_mut(&id) {
                        info.invoke_spans.push(invoke_head(inv));
                    }
                }
                Item::Output(v) | Item::Fail(v) => {
                    let is_output = matches!(item, Item::Output(_));
                    let word = if is_output { "output" } else { "fail" };
                    if !top_final {
                        self.ck.diags.push(
                            Diagnostic::error(
                                codes::MISPLACED_RESULT,
                                v.span,
                                format!("`{word}` is only allowed in a final state at the top level of a machine"),
                            )
                            .with_help("a nested final state ends its parent state, not the machine"),
                        );
                    } else if let Some(first) = result_seen {
                        self.ck.diags.push(
                            Diagnostic::error(
                                codes::MISPLACED_RESULT,
                                v.span,
                                "a final state has one `output` or one `fail`",
                            )
                            .with_secondary(first, "already given here"),
                        );
                    }
                    let first = result_seen.is_none();
                    result_seen.get_or_insert(v.span);
                    if is_output {
                        let e = self.output(&v.value);
                        output.get_or_insert(e);
                    } else {
                        let e = self.fail_value(&v.value);
                        fail.get_or_insert(e);
                    }
                    if first {
                        result_reads = std::mem::take(&mut self.reads);
                    }
                }
                Item::Var(_)
                | Item::Event(_)
                | Item::InternalEvent(_)
                | Item::Emits(_)
                | Item::Call(_)
                | Item::Children(_)
                | Item::Initial(_)
                | Item::History(_)
                // Resolved once the machine is lowered, in `checks.rs`.
                | Item::Check(_)
                | Item::State(_) => {}
            }
        }

        if top_final
            && result_seen.is_none()
            && let Some(ret) = &self.returns
        {
            let machine = self.machine.clone();
            self.ck.diags.push(
                Diagnostic::error(
                    codes::MISSING_RESULT,
                    name_span,
                    format!(
                        "final state `{}` does not say what `{machine}` returns",
                        self.nodes[idx].name
                    ),
                )
                .with_label(format!("`{machine}` returns `{ret}`"))
                .with_help(
                    "add `output <value>`, or `fail <error>` for one of its declared errors",
                ),
            );
        }

        let transition_reads = std::mem::take(&mut self.transition_reads);
        if let Some(info) = self.info.states.get_mut(&id) {
            info.triggers = triggers;
            info.entry_reads = entry_reads;
            info.exit_reads = exit_reads;
            info.invoke_reads = invoke_reads;
            info.result_reads = result_reads;
            info.transition_reads = transition_reads;
        }
        let retry = retry.and_then(|plan| self.check_retry(idx, kind, plan));
        let children = self.nodes[idx].order.clone();
        let states = children.into_iter().map(|c| self.lower_state(c)).collect();
        self.here = idx;
        let mut lowered = ir::State {
            name: self.nodes[idx].name.clone(),
            id,
            kind,
            doc: state.map(|s| doc(&s.doc)).unwrap_or_default(),
            span: state
                .map(|s| ir_span(s.span))
                .unwrap_or_else(|| ir_span(name_span)),
            params,
            vars,
            initial,
            default: Vec::new(),
            states,
            entry,
            exit,
            invokes,
            transitions,
            output,
            fail,
        };
        if let Some(r) = retry {
            self.write_out_retry(&mut lowered, r);
        }
        lowered
    }

    /// Reports `RB0223` if node `c`, the `initial` of `parent` (written at `initial`) or one of
    /// its regions (`initial` `None`), takes parameters: it is entered by default, with no
    /// transition to pass them.
    fn default_entered_with_parameters(&mut self, c: usize, initial: Option<Span>, parent: &str) {
        let Some(first) = self
            .locals
            .get(c)
            .and_then(|ls| ls.iter().find(|l| l.param))
        else {
            return;
        };
        let first = first.span;
        let cid = self.nodes[c].id.clone();
        let (span, message) = match initial {
            Some(span) => (
                span,
                format!("`{cid}` takes parameters, so it cannot be the `initial` of `{parent}`"),
            ),
            None => (
                self.nodes[c].name_span,
                format!(
                    "`{cid}` takes parameters, so it cannot be a region of the parallel state `{parent}`"
                ),
            ),
        };
        self.ck.diags.push(
            Diagnostic::error(codes::PARAMETERS_ON_DEFAULT_ENTRY, span, message)
                .with_secondary(first, "a parameter")
                .with_help("a state's `initial` and the regions of a parallel state are entered with no transition to pass arguments; make the parameters variables of the state, or enter it from another state with a transition that passes them"),
        );
    }

    /// Lowers node `idx`'s parameters and variables, checking its variables' initial values
    /// (with [`Cx::here`] at `idx`).
    fn lower_locals(&mut self, idx: usize) -> (Vec<ir::Field>, Vec<ir::Var>, Vec<LocalInfo>) {
        let mut params = Vec::new();
        let mut vars = Vec::new();
        let mut infos = Vec::new();
        let Some(s) = self.nodes[idx].state else {
            return (params, vars, infos);
        };
        let mut seen: HashSet<&str> = HashSet::new();
        for p in &s.params {
            if !seen.insert(&p.name.name) {
                continue;
            }
            params.push(ir::Field {
                name: p.name.name.clone(),
                ty: ir_type(&p.ty),
                sensitive: p.sensitive,
                doc: String::new(),
            });
            infos.push(LocalInfo {
                name: p.name.name.clone(),
                span: p.name.span,
                param: true,
                init: true,
                optional: false,
                init_reads: Vec::new(),
            });
        }
        for v in s.body.vars() {
            if !seen.insert(&v.name.name) {
                continue;
            }
            let ty = self.locals[idx]
                .iter()
                .find(|l| l.name == v.name.name)
                .map_or(Ty::Unknown, |l| l.ty.clone());
            self.reads.clear();
            let init = v.init.as_ref().map(|e| {
                self.expect_to(
                    e,
                    &Scope::none(),
                    &ty,
                    &format!("the initial value of `{}`", v.name.name),
                    dest(v.sensitive, format!("`{}`", v.name.name)),
                )
            });
            let optional = matches!(self.ck.env.expand(&ty), Ty::Optional(_));
            infos.push(LocalInfo {
                name: v.name.name.clone(),
                span: v.name.span,
                param: false,
                init: init.is_some(),
                optional,
                init_reads: std::mem::take(&mut self.reads),
            });
            vars.push(ir::Var {
                name: v.name.name.clone(),
                ty: ir_type(&v.ty),
                sensitive: v.sensitive,
                init,
                doc: doc(&v.doc),
                span: ir_span(v.span),
            });
        }
        (params, vars, infos)
    }

    fn initial(&mut self, idx: usize, kind: ir::StateKind) -> Option<String> {
        let body = self.nodes[idx].body;
        let initials: Vec<&Ident> = body.initials().collect();
        let id = self.nodes[idx].id.clone();
        let is_root = self.nodes[idx].parent.is_none();
        for extra in initials.iter().skip(1) {
            self.ck.diags.push(
                Diagnostic::error(
                    codes::BAD_INITIAL,
                    extra.span,
                    format!("`{id}` has more than one `initial`"),
                )
                .with_secondary(initials[0].span, "the first is here"),
            );
        }
        if kind != ir::StateKind::Compound {
            if let Some(first) = initials.first() {
                let why = match kind {
                    ir::StateKind::Parallel => "is parallel: all of its children are entered",
                    ir::StateKind::Final => "is final",
                    _ => "has no child states",
                };
                self.ck.diags.push(Diagnostic::error(
                    codes::BAD_INITIAL,
                    first.span,
                    format!("`initial {}`: `{id}` {why}", first.name),
                ));
            }
            return None;
        }
        let children = self.nodes[idx].children.clone();
        if is_root && children.is_empty() && !initials.is_empty() {
            self.ck.diags.push(
                Diagnostic::error(
                    codes::MISSING_INITIAL,
                    self.nodes[idx].name_span,
                    format!("machine `{id}` has no states"),
                )
                .with_secondary(initials[0].span, "its `initial` names no state")
                .with_help(format!(
                    "declare the state: `final state {} {{}}`",
                    initials[0].name
                )),
            );
            return None;
        }
        let Some(first) = initials.first() else {
            if children.is_empty() {
                self.ck.diags.push(
                    Diagnostic::error(
                        codes::MISSING_INITIAL,
                        self.nodes[idx].name_span,
                        format!("machine `{id}` has no states"),
                    )
                    .with_help(
                        "add an `initial` state, such as `initial Done` and `final state Done {}`",
                    ),
                );
                return None;
            }
            let first_child = self.nodes[children[0]].name.clone();
            let what = if is_root { "machine" } else { "state" };
            let mut d = Diagnostic::error(
                codes::MISSING_INITIAL,
                self.nodes[idx].name_span,
                format!("{what} `{id}` has child states but no `initial`"),
            )
            .with_help(format!(
                "add `initial {first_child}` to say which child is entered first"
            ));
            if let Some(item) = body.items.first() {
                // Before the item's doc comment, which must stay with the item.
                let doc = match item {
                    Item::State(s) => s.doc.as_ref(),
                    Item::History(h) => h.doc.as_ref(),
                    Item::Var(v) => v.doc.as_ref(),
                    Item::Event(e) | Item::InternalEvent(e) | Item::Emits(e) => e.doc.as_ref(),
                    Item::Check(c) => c.doc.as_ref(),
                    _ => None,
                };
                let at = doc.map_or(item.span(), |d| d.span).shrink_to_start();
                let indent = " ".repeat(at.start.col.saturating_sub(1) as usize);
                d = d.with_fix(
                    format!("add `initial {first_child}`"),
                    at,
                    format!("initial {first_child}\n{indent}"),
                );
            }
            self.ck.diags.push(d);
            return None;
        };
        match self.child(idx, &first.name) {
            Some(c) => {
                self.refer_state(c, first);
                self.default_entered_with_parameters(c, Some(first.span), &id);
                Some(self.nodes[c].id.clone())
            }
            None if let Some(h) = self.history_child(idx, &first.name) => {
                self.refer_state(h, first);
                self.ck.diags.push(
                    Diagnostic::error(
                        codes::BAD_INITIAL,
                        first.span,
                        format!("`initial {}`: `{}` is a history state", first.name, self.nodes[h].id),
                    )
                    .with_help(format!(
                        "`initial` names the child state entered when `{id}` is entered with no history; a history state is only a transition's target"
                    )),
                );
                None
            }
            None => {
                let names: Vec<String> = children
                    .iter()
                    .map(|&c| self.nodes[c].name.clone())
                    .collect();
                let mut d = Diagnostic::error(
                    codes::BAD_INITIAL,
                    first.span,
                    format!(
                        "`initial {}`: `{id}` has no child state `{}`",
                        first.name, first.name
                    ),
                )
                .with_label(format!("children: {}", names.join(", ")));
                if let Some(s) = suggest(&first.name, names.iter().map(String::as_str)) {
                    d = d.with_help(format!("did you mean `{s}`?")).with_fix(
                        format!("replace with `{s}`"),
                        first.span,
                        s,
                    );
                }
                self.ck.diags.push(d);
                None
            }
        }
    }

    fn duplicate_trigger(
        &mut self,
        keys: &mut HashMap<String, Span>,
        t: &Transition,
        owner: &str,
        prefix: &str,
    ) {
        if let Trigger::OneOf(names) = &t.trigger {
            // Each event is a trigger of its own: `on A | B` and `on B` both handle `B`.
            for (i, n) in names.iter().enumerate() {
                if names[..i].iter().any(|m| m.name == n.name) {
                    let all = trigger_key(&t.trigger);
                    self.ck.diags.push(
                        Diagnostic::error(
                            codes::DUPLICATE_TRIGGER,
                            n.span,
                            format!("event `{}` is listed twice in `{all}`", n.name),
                        )
                        .with_help("list each event once"),
                    );
                    continue;
                }
                self.duplicate_key(keys, format!("on {}", n.name), n.span, owner, prefix);
            }
            return;
        }
        let key = trigger_key(&t.trigger);
        self.duplicate_key(keys, key, t.trigger_span, owner, prefix);
    }

    fn duplicate_key(
        &mut self,
        keys: &mut HashMap<String, Span>,
        key: String,
        span: Span,
        owner: &str,
        prefix: &str,
    ) {
        if let Some(first) = keys.get(&key) {
            let what = if prefix.is_empty() {
                format!("trigger `{key}` is written twice in `{owner}`")
            } else {
                format!("outcome `{key}` is written twice in {prefix}")
            };
            self.ck.diags.push(
                Diagnostic::error(codes::DUPLICATE_TRIGGER, span, what)
                    .with_secondary(*first, "first written here")
                    .with_help(format!(
                        "put every branch in one block, where their order is explicit: `{key} {{ if … -> A  else -> B }}`"
                    )),
            );
        } else {
            keys.insert(key, span);
        }
    }

    /// Records a use of `n`, one of the machine's own events in an `on`, for references and
    /// the unused-event warning; reports it if the machine does not declare it.
    fn own_event(&mut self, n: &Ident) -> bool {
        self.ck.refer_member(self.file, self.symbol, n);
        if self.events.contains_key(&n.name) {
            self.info.used_events.insert(n.name.clone());
            return true;
        }
        let declared: Vec<String> = self
            .info
            .events
            .iter()
            .map(|(e, _)| format!("`{e}`"))
            .collect();
        let mut d = Diagnostic::error(
            codes::UNKNOWN_EVENT,
            n.span,
            format!("unknown event `{}`", n.name),
        )
        .with_label(if declared.is_empty() {
            format!("`{}` declares no events", self.machine)
        } else {
            format!("`{}` declares {}", self.machine, declared.join(", "))
        });
        if let Some(s) = suggest(&n.name, self.info.events.iter().map(|(e, _)| e.as_str())) {
            d = d.with_help(format!("did you mean `{s}`?")).with_fix(
                format!("replace with `{s}`"),
                n.span,
                s,
            );
        } else {
            d = d.with_help(format!(
                "declare it in the machine body: `event {}`",
                n.name
            ));
        }
        self.ck.diags.push(d);
        false
    }

    /// The IR trigger and the implicit names in scope for an `on`, `after`, `always` or `done`
    /// written in a state or the machine body.
    fn state_trigger(&mut self, idx: usize, t: &Trigger) -> (ir::Trigger, Scope) {
        let mut scope = Scope::none();
        let trigger = match t {
            Trigger::Receive { from, event } => {
                scope.event = Implicit::Has(Ty::Unknown);
                if self.collections.contains_key(&from.name) {
                    // Also when its machine is unknown (reported): `key` is still in scope.
                    scope.key = Implicit::Has(Ty::Unknown);
                }
                let Some((address, _)) = self.address(idx, from) else {
                    // Reported; the trigger is not lowered further.
                    return (
                        ir::Trigger::Event {
                            event: event.name.clone(),
                        },
                        scope,
                    );
                };
                if address.kind == ir::AddressKind::Children {
                    scope.key = Implicit::Has(self.collections[&from.name].key.clone());
                }
                // The machine or task that emits it: their names never clash.
                let sender_name = address.peer().to_owned();
                let sender = self.ck.decl_symbol(&sender_name);
                self.ck.refer_member(self.file, sender, event);
                let iface = self
                    .ck
                    .interfaces
                    .get(&sender_name)
                    .cloned()
                    .unwrap_or_default();
                if iface.emits.contains_key(&event.name) {
                    scope.event = Implicit::Has(Ty::Event(format!("{sender_name}.{}", event.name)));
                    self.ck
                        .received
                        .insert((sender_name.clone(), event.name.clone()));
                    self.info
                        .receives
                        .push((event.span, sender_name.clone(), event.name.clone()));
                } else {
                    let machine = sender_name.clone();
                    self.not_declared(
                        codes::NOT_EMITTED,
                        &machine,
                        &iface,
                        event,
                        true,
                        &from.name,
                    );
                }
                ir::Trigger::Emitted {
                    from: address,
                    event: event.name.clone(),
                }
            }
            Trigger::On(Pattern::Name(n)) => {
                scope.event = if self.own_event(n) {
                    Implicit::Has(Ty::Event(n.name.clone()))
                } else {
                    Implicit::Has(Ty::Unknown)
                };
                ir::Trigger::Event {
                    event: n.name.clone(),
                }
            }
            Trigger::OneOf(names) => {
                let mut known = true;
                for n in names {
                    known &= self.own_event(n);
                }
                scope.event = if known {
                    Implicit::OneOf(names.iter().map(|n| n.name.clone()).collect())
                } else {
                    Implicit::Has(Ty::Unknown)
                };
                // The first event's: the caller copies the transition for each other one.
                ir::Trigger::Event {
                    event: names[0].name.clone(),
                }
            }
            Trigger::Ended { from } => {
                scope.event = Implicit::Ended;
                match self.peer(idx, from) {
                    Some(address) => ir::Trigger::Ended { from: address },
                    // Reported; the trigger is not lowered further.
                    None => ir::Trigger::Event {
                        event: from.name.clone(),
                    },
                }
            }
            Trigger::Call(n) => {
                if let Some(id) = self.ck.call_symbol(self.symbol, &n.name)
                    && !self.ck.is_compiled(self.file)
                {
                    self.ck.index.refer(id, n.span);
                }
                if self.calls.contains_key(&n.name) {
                    self.info.taken_calls.insert(n.name.clone());
                    scope.event = Implicit::CallArgs(n.name.clone());
                    scope.call = Implicit::Has(Ty::Call(n.name.clone()));
                } else {
                    let machine = self.machine.clone();
                    self.unknown_call(&machine, n, None);
                    scope.event = Implicit::Has(Ty::Unknown);
                    scope.call = Implicit::Has(Ty::Unknown);
                }
                ir::Trigger::Call {
                    call: n.name.clone(),
                }
            }
            Trigger::On(Pattern::Any { .. }) => {
                self.info.any_handler = true;
                scope.event = Implicit::Bad(
                    codes::UNTYPED_EVENT,
                    "`event` has no type in `on *`, which matches events of different types".into(),
                );
                ir::Trigger::Any
            }
            Trigger::ChildDone { from } => {
                scope.key = Implicit::Has(Ty::Unknown);
                scope.result = Implicit::Has(Ty::Unknown);
                if let Some(c) = self.collection(from, &format!("on {}.done", from.name)) {
                    scope.key = Implicit::Has(c.key.clone());
                    if let Some(sig) = c.src.as_ref().and_then(|m| self.ck.sigs.get(m)) {
                        scope.result = match &sig.returns {
                            Some(t) => Implicit::Has(t.clone()),
                            None => Implicit::Bad(
                                codes::NO_RESULT,
                                format!(
                                    "`{}` returns nothing, so `on {}.done` has no `result`",
                                    c.src.as_deref().unwrap_or_default(),
                                    from.name
                                ),
                            ),
                        };
                    }
                }
                ir::Trigger::SpawnDone {
                    children: from.name.clone(),
                }
            }
            Trigger::ChildFail { from, error } => {
                scope.key = Implicit::Has(Ty::Unknown);
                scope.error = Implicit::Has(Ty::Unknown);
                let shown = format!("on {}.fail", from.name);
                if let Some(c) = self.collection(from, &shown) {
                    scope.key = Implicit::Has(c.key.clone());
                    let machine = c.src.clone().unwrap_or_default();
                    let sig = c.src.as_ref().and_then(|m| self.ck.sigs.get(m)).cloned();
                    // The errors the state lists in its own `on name.fail` triggers.
                    let listed: Vec<String> = self.nodes[idx]
                        .body
                        .transitions()
                        .filter_map(|t| match &t.trigger {
                            Trigger::ChildFail {
                                from: f,
                                error: Pattern::Name(n),
                            } if f.name == from.name => Some(n.name.clone()),
                            _ => None,
                        })
                        .collect();
                    match (error, &sig) {
                        (Pattern::Name(n), Some(sig)) => {
                            match self.ck.decl_symbol(&n.name) {
                                Some(e) if sig.errors.contains(&n.name) => {
                                    self.ck.index.refer(e, n.span);
                                }
                                _ => self.ck.refer_decl(self.file, n),
                            }
                            if sig.errors.contains(&n.name) {
                                scope.error = Implicit::Has(Ty::Named(n.name.clone()));
                            } else {
                                let mut d = Diagnostic::error(
                                    codes::UNDECLARED_ERROR_OUTCOME,
                                    n.span,
                                    format!("`{machine}` does not declare the error `{}`", n.name),
                                )
                                .with_secondary(
                                    sig.name_span,
                                    format!("declared as `{}`", sig.result_text()),
                                );
                                if let Some(s) =
                                    suggest(&n.name, sig.errors.iter().map(String::as_str))
                                {
                                    d = d.with_help(format!("did you mean `{s}`?")).with_fix(
                                        format!("replace with `{s}`"),
                                        n.span,
                                        s,
                                    );
                                }
                                self.ck.diags.push(d);
                            }
                        }
                        (Pattern::Any { .. }, Some(sig)) => {
                            let rest: Vec<&String> =
                                sig.errors.iter().filter(|e| !listed.contains(e)).collect();
                            scope.error = match rest.as_slice() {
                                [one] => Implicit::Has(Ty::Named((*one).clone())),
                                [] => Implicit::Bad(
                                    codes::AMBIGUOUS_ERROR,
                                    format!(
                                        "`error` has no type here: `{shown} *` covers none of `{machine}`'s errors"
                                    ),
                                ),
                                more => Implicit::Bad(
                                    codes::AMBIGUOUS_ERROR,
                                    format!(
                                        "`error` has no single type here: `{shown} *` covers {}",
                                        more.iter()
                                            .map(|e| format!("`{e}`"))
                                            .collect::<Vec<_>>()
                                            .join(", ")
                                    ),
                                ),
                            };
                        }
                        (Pattern::Name(n), None) => self.ck.refer_decl(self.file, n),
                        _ => {}
                    }
                }
                ir::Trigger::SpawnFail {
                    children: from.name.clone(),
                    error: match error {
                        Pattern::Name(n) => n.name.clone(),
                        Pattern::Any { .. } => "*".into(),
                    },
                }
            }
            Trigger::After(e) => ir::Trigger::After {
                delay: self.expect_to(
                    e,
                    &Scope::none(),
                    &DURATION,
                    "an `after` delay",
                    Dest::Shown,
                ),
            },
            Trigger::Always => ir::Trigger::Always,
            // `fail` outcomes only occur inside an invoke; the parser rejects them elsewhere.
            Trigger::Done | Trigger::Fail(_) => ir::Trigger::Done,
        };
        (trigger, scope)
    }

    /// The branches of a transition written in node `idx`; `(group, outcome)` is its group
    /// and, for an outcome of one of the state's invokes, which one.
    #[allow(clippy::too_many_arguments)]
    fn branches(
        &mut self,
        idx: usize,
        t: &Transition,
        trigger: ir::Trigger,
        scope: &Scope,
        (group, outcome): (u32, Option<usize>),
        out: &mut Vec<ir::Transition>,
        triggers: &mut Vec<Span>,
    ) {
        // What the trigger read: an `after` delay.
        let delay = std::mem::take(&mut self.reads);
        for b in t.branches.iter() {
            let guard = b
                .guard
                .as_ref()
                .map(|g| self.expect(g, scope, &BOOL, "a guard"));
            let guard_reads = std::mem::take(&mut self.reads);
            let (target_nodes, internal, resolved) = self.targets(idx, &b.targets);
            let targets = target_nodes
                .iter()
                .map(|&n| self.nodes[n].id.clone())
                .collect();
            let domain =
                (!target_nodes.is_empty()).then(|| self.domain(idx, &target_nodes, internal));
            let place = Place::Transition {
                node: idx,
                domain,
                outcome,
            };
            let (effects, effect_reads) = self.effects(&b.effects, scope, place);
            self.reads.clear();
            let args = self.state_args(idx, &b.targets, &resolved, &target_nodes, domain, scope);
            let arg_reads = std::mem::take(&mut self.reads);
            let span = match &t.branches {
                Branches::Single(_) => t.span,
                Branches::Block { .. } => b.span,
            };
            self.transition_reads.push(TransitionReads {
                delay: delay.clone(),
                guard: guard_reads,
                effects: effect_reads,
                args: arg_reads,
                span,
            });
            out.push(ir::Transition {
                trigger: trigger.clone(),
                group,
                guard,
                targets,
                internal,
                args,
                effects,
                span: ir_span(span),
            });
            triggers.push(t.trigger_span);
        }
    }

    /// Resolves the targets of a branch written in node `from`, and checks that they go
    /// together: every one with the leading dot or none (`RB0221`), and each in a different
    /// region of a parallel state (`RB0222`). With whether the transition is internal: written
    /// with the dot, outside the machine body (where `-> .X` is `-> X`). A target that does not
    /// resolve is left out, with its diagnostic.
    fn targets(
        &mut self,
        from: usize,
        written: &[Target],
    ) -> (Vec<usize>, bool, Vec<Option<usize>>) {
        let dot = written.first().is_some_and(|t| t.dot);
        let resolved: Vec<Option<usize>> = written.iter().map(|t| self.target(from, t)).collect();
        let nodes: Vec<usize> = resolved.iter().flatten().copied().collect();
        if let Some(odd) = written.iter().find(|t| t.dot != dot) {
            let own = self.nodes[from].id.clone();
            let mut d = Diagnostic::error(
                codes::MIXED_TARGETS,
                odd.span,
                if dot {
                    "this target has no leading dot, and the first one does: either every target is written `.X` or none is".to_string()
                } else {
                    "this target has a leading dot, and the first one does not: either every target is written `.X` or none is".to_string()
                },
            )
            .with_secondary(written[0].span, "the first target")
            .with_help(format!(
                "the dot belongs to the transition: with it, the transition stays in `{own}` and every target is resolved among `{own}`'s children; without it, the transition leaves `{own}`"
            ));
            // With the dot: each target as a path from `from`, if every one is below it.
            let dotted: Option<Vec<(Span, String)>> = written
                .iter()
                .zip(&resolved)
                .filter(|(t, _)| !t.dot)
                .map(|(t, n)| {
                    let n = n.filter(|&n| self.inside(n, from))?;
                    Some((t.span, format!(".{}", self.path_below(from, n))))
                })
                .collect();
            if let Some(edits) = dotted {
                d = d.with_fix_edits("write every target with the dot", edits);
            }
            // Without it: each dotted target through the state's own name.
            let name = &self.nodes[from].name;
            let undotted: Vec<(Span, String)> = written
                .iter()
                .filter(|t| t.dot)
                .map(|t| match self.nodes[from].parent {
                    Some(_) => (dot_span(t), format!("{name}.")),
                    None => (dot_span(t), String::new()),
                })
                .collect();
            d = d.with_fix_edits("write every target without the dot", undotted);
            self.ck.diags.push(d);
        } else if nodes.len() == written.len() && nodes.len() > 1 {
            self.check_targets_go_together(written, &nodes);
        }
        let internal = dot && self.nodes[from].parent.is_some();
        (nodes, internal, resolved)
    }

    /// Checks and lowers the arguments written in the targets of a branch written in `from`,
    /// whose targets resolved to `resolved` (by written target) and `nodes`, with `domain`
    /// (`None` with no target). Every state the transition enters that takes parameters gets
    /// arguments, once (`RB0224`); no other state does (`RB0225`). In document order, which
    /// is the order the states are entered.
    fn state_args(
        &mut self,
        from: usize,
        written: &[Target],
        resolved: &[Option<usize>],
        nodes: &[usize],
        domain: Option<usize>,
        scope: &Scope,
    ) -> Vec<ir::StateArgs> {
        // The states entered on the way to each target, from just below the domain. Default
        // entry never enters a state with parameters (`RB0223`).
        let mut entered: Vec<usize> = Vec::new();
        if let Some(d) = domain {
            for &r in nodes {
                let mut x = match (self.nodes[r].history, self.nodes[r].parent) {
                    (Some(_), Some(p)) => p,
                    _ => r,
                };
                while x != d && self.inside(x, d) {
                    if !entered.contains(&x) {
                        entered.push(x);
                    }
                    x = self.nodes[x].parent.unwrap_or(d);
                }
            }
        }
        let mut given: Vec<(usize, Span, ir::StateArgs)> = Vec::new();
        for (t, r) in written.iter().zip(resolved) {
            if !t.has_args() {
                continue;
            }
            // The node each segment names: the target, its parent, and so on up.
            let Some(r) = *r else {
                // Unresolved, already reported: still check the arguments' expressions.
                for a in t.args.iter().flatten() {
                    for e in &a.args {
                        self.expr(e, scope);
                    }
                }
                continue;
            };
            let mut segs = vec![r; t.path.len()];
            for k in (0..t.path.len().saturating_sub(1)).rev() {
                segs[k] = self.nodes[segs[k + 1]].parent.unwrap_or(segs[k + 1]);
            }
            for (k, seg) in segs.iter().copied().enumerate() {
                let Some(a) = t.args_of(k) else { continue };
                let sid = self.nodes[seg].id.clone();
                let takes = self
                    .locals
                    .get(seg)
                    .is_some_and(|ls| ls.iter().any(|l| l.param));
                let problem = if !takes {
                    Some((
                        format!("`{sid}` takes no parameters"),
                        "remove the arguments".to_string(),
                    ))
                } else if !entered.contains(&seg) {
                    Some((
                        format!("this transition does not enter `{sid}`, so it takes no arguments"),
                        format!(
                            "`{sid}` stays active and keeps its locals; remove the arguments, or leave and enter it again with a target written from outside it"
                        ),
                    ))
                } else if let Some((_, first, _)) = given.iter().find(|(n, _, _)| *n == seg) {
                    let first = *first;
                    self.ck.diags.push(
                        Diagnostic::error(
                            codes::UNEXPECTED_STATE_ARGUMENTS,
                            a.span,
                            format!("the arguments of `{sid}` are already given"),
                        )
                        .with_secondary(first, "given here")
                        .with_help("a state on the paths of several targets gets its arguments once, on one of them"),
                    );
                    continue;
                } else {
                    None
                };
                if let Some((message, help)) = problem {
                    let d = Diagnostic::error(codes::UNEXPECTED_STATE_ARGUMENTS, a.span, message)
                        .with_help(help)
                        .with_fix("remove the arguments", a.span, String::new());
                    self.ck.diags.push(d);
                    for e in &a.args {
                        self.expr(e, scope);
                    }
                    continue;
                }
                let values = self.state_arg_values(seg, &t.path[k], a, scope);
                given.push((seg, a.span, ir::StateArgs { state: sid, values }));
            }
        }
        for &n in &entered {
            let takes = self
                .locals
                .get(n)
                .is_some_and(|ls| ls.iter().any(|l| l.param));
            if !takes || given.iter().any(|(g, _, _)| *g == n) {
                continue;
            }
            let Some(i) = resolved
                .iter()
                .position(|r| r.is_some_and(|r| r == n || self.inside(r, n)))
            else {
                continue;
            };
            let nid = self.nodes[n].id.clone();
            let params: Vec<String> = self.locals[n]
                .iter()
                .filter(|l| l.param)
                .map(|l| format!("{}: {}", l.name, l.ty))
                .collect();
            let r = resolved[i].unwrap_or(n);
            let d = domain.unwrap_or(from);
            let mut names = Vec::new();
            let mut x = r;
            while x != d {
                let takes = self.locals[x].iter().any(|l| l.param);
                names.push(format!(
                    "{}{}",
                    self.nodes[x].name,
                    if takes { "(…)" } else { "" }
                ));
                x = self.nodes[x].parent.unwrap_or(d);
            }
            names.reverse();
            let dot = if written[i].dot { "." } else { "" };
            self.ck.diags.push(
                Diagnostic::error(
                    codes::MISSING_STATE_ARGUMENTS,
                    written[i].span,
                    format!(
                        "this transition enters `{nid}`, which takes `{}`, without passing its arguments",
                        params.join(", ")
                    ),
                )
                .with_secondary(self.nodes[n].name_span, "declared here")
                .with_help(format!(
                    "write the arguments after the state's name in the path: `-> {dot}{}`",
                    names.join(".")
                )),
            );
        }
        given.sort_by_key(|(n, _, _)| *n);
        given.into_iter().map(|(_, _, a)| a).collect()
    }

    /// Checks the arguments `a` written after `seg`, the name of node `n`, against its
    /// parameters, and lowers them.
    fn state_arg_values(
        &mut self,
        n: usize,
        seg: &Ident,
        a: &ast::TargetArgs,
        scope: &Scope,
    ) -> Vec<ir::Expr> {
        let params: Vec<Local> = self.locals[n].iter().filter(|l| l.param).cloned().collect();
        let id = self.nodes[n].id.clone();
        if params.len() != a.args.len() {
            let plural = |n: usize| if n == 1 { "" } else { "s" };
            let shown: Vec<String> = params
                .iter()
                .map(|l| format!("{}: {}", l.name, l.ty))
                .collect();
            self.ck.diags.push(
                Diagnostic::error(
                    codes::ARGUMENT_COUNT,
                    a.span,
                    format!(
                        "`{}` takes {} argument{} but {} {} given",
                        seg.name,
                        params.len(),
                        plural(params.len()),
                        a.args.len(),
                        if a.args.len() == 1 { "was" } else { "were" }
                    ),
                )
                .with_secondary(
                    self.nodes[n].name_span,
                    format!("declared as `{}({})`", seg.name, shown.join(", ")),
                ),
            );
        }
        a.args
            .iter()
            .enumerate()
            .map(|(i, e)| match params.get(i) {
                Some(p) => {
                    let what = format!("argument `{}` of `{id}`", p.name);
                    let to = dest(p.sensitive, format!("parameter `{}` of `{id}`", p.name));
                    self.expect_to(e, scope, &p.ty, &what, to)
                }
                None => self.expr(e, scope).0,
            })
            .collect()
    }

    /// Reports two targets that are not in different regions of a parallel state: one is the
    /// other or inside it, or they meet in a compound state. A history state stands for its
    /// parent's child, or for a parallel parent itself, since it restores every region.
    fn check_targets_go_together(&mut self, written: &[Target], nodes: &[usize]) {
        let stand_in = |cx: &Self, n: usize| match (cx.nodes[n].history, cx.nodes[n].parent) {
            (Some(_), Some(p)) if cx.is_parallel(p) => p,
            _ => n,
        };
        for j in 1..nodes.len() {
            for i in 0..j {
                let (x, y) = (nodes[i], nodes[j]);
                let (a, b) = (stand_in(self, x), stand_in(self, y));
                let id = |n: usize| self.nodes[n].id.clone();
                let why = if x == y {
                    format!("`{}` is targeted twice", id(x))
                } else if a != x || b != y {
                    // A history state of a parallel state, with another target in it.
                    let (h, other) = if a != x { (x, y) } else { (y, x) };
                    let p = stand_in(self, h);
                    if other != p && !self.inside(other, p) {
                        if !self.is_parallel(self.common_ancestor(a, b)) {
                            format!(
                                "both are in `{}`, which is not a parallel state, so only one of its children can be active",
                                id(self.common_ancestor(a, b))
                            )
                        } else {
                            continue;
                        }
                    } else {
                        format!(
                            "`{}` restores every region of `{}`, so the other target's region is entered twice",
                            id(h),
                            id(p)
                        )
                    }
                } else if self.inside(b, a) || self.inside(a, b) {
                    let (outer, inner) = if self.inside(b, a) { (a, b) } else { (b, a) };
                    format!(
                        "`{}` is inside `{}`, which the other target enters",
                        id(inner),
                        id(outer)
                    )
                } else {
                    let meet = self.common_ancestor(a, b);
                    if self.is_parallel(meet) {
                        continue;
                    }
                    format!(
                        "both are in `{}`, which is not a parallel state, so only one of its children can be active",
                        id(meet)
                    )
                };
                self.ck.diags.push(
                    Diagnostic::error(
                        codes::TARGETS_CLASH,
                        written[j].span,
                        format!("these targets can't be entered together: {why}"),
                    )
                    .with_secondary(written[i].span, "the other target")
                    .with_help("several targets enter a parallel state with a chosen state in more than one region: each target must be in a different region of the same parallel state"),
                );
                return;
            }
        }
    }

    /// The names from just below `from` down to `n`, a state below it: `A.B`.
    fn path_below(&self, from: usize, n: usize) -> String {
        let mut names = Vec::new();
        let mut x = n;
        while x != from {
            names.push(self.nodes[x].name.as_str());
            x = self.nodes[x].parent.unwrap_or(from);
        }
        names.reverse();
        names.join(".")
    }

    fn is_parallel(&self, n: usize) -> bool {
        self.nodes[n]
            .state
            .is_some_and(|s| s.kind == StateKind::Parallel)
    }

    /// The nearest state containing both `a` and `b` (or either, if one contains the other).
    fn common_ancestor(&self, a: usize, b: usize) -> usize {
        let mut up = Some(a);
        while let Some(x) = up {
            if x == b || self.inside(b, x) {
                return x;
            }
            up = self.nodes[x].parent;
        }
        0
    }

    /// The domain of a transition written in `from` to `targets`, as `spec/execution.md`
    /// defines it: the root for the machine body's, `from` itself for an internal one, else
    /// the nearest proper ancestor of `from` that is not a parallel state and contains every
    /// target.
    pub(crate) fn domain(&self, from: usize, targets: &[usize], internal: bool) -> usize {
        if self.nodes[from].parent.is_none() || internal {
            return from;
        }
        let mut up = self.nodes[from].parent;
        while let Some(x) = up {
            if !self.is_parallel(x) && targets.iter().all(|&t| self.inside(t, x)) {
                return x;
            }
            up = self.nodes[x].parent;
        }
        0
    }

    /// Resolves a target. Written `.X`, the first segment among the children of the state the
    /// transition is written in. Otherwise among the siblings of that state, then among its
    /// parent's siblings, and so on outward (for the machine body, among the top-level
    /// states). Each further segment names a child of the one before.
    fn target(&mut self, from: usize, t: &Target) -> Option<usize> {
        let head = &t.path[0];
        if t.dot {
            return match self.target_child(from, &head.name) {
                Some(n) => {
                    self.refer_state(n, head);
                    self.descend(n, t)
                }
                None => {
                    self.unknown_child(from, t);
                    None
                }
            };
        }
        let start = self.nodes[from].parent.unwrap_or(from);
        let mut scope = Some(start);
        let mut found = None;
        while let Some(s) = scope {
            if let Some(c) = self.target_child(s, &head.name) {
                found = Some(c);
                break;
            }
            scope = self.nodes[s].parent;
        }
        let written: Vec<&str> = t.path.iter().map(|p| p.name.as_str()).collect();
        let written = written.join(".");
        if let Some(n) = found {
            self.refer_state(n, head);
        }
        let Some(node) = found else {
            // The states in scope, nearest first, for the message.
            let mut visible: Vec<String> = Vec::new();
            let mut scope = Some(start);
            while let Some(s) = scope {
                for &c in &self.nodes[s].order {
                    if !visible.contains(&self.nodes[c].name) {
                        visible.push(self.nodes[c].name.clone());
                    }
                }
                scope = self.nodes[s].parent;
            }
            let mut d = Diagnostic::error(
                codes::UNKNOWN_TARGET,
                head.span,
                format!("unknown target state `{}`", head.name),
            );
            if self.target_child(from, &head.name).is_some() && self.nodes[from].parent.is_some() {
                let own = &self.nodes[from].name;
                d = d
                    .with_help(format!(
                        "`{}` is a child of `{own}`: write `.{written}` to go to it without leaving `{own}`, or `{own}.{written}` to leave `{own}` and enter it again",
                        head.name
                    ))
                    .with_fix(format!("write `.{written}`"), t.span, format!(".{written}"))
                    .with_fix(format!("write `{own}.{written}`"), t.span, format!("{own}.{written}"));
            } else if let Some(s) = suggest(&head.name, visible.iter().map(String::as_str)) {
                d = d.with_help(format!("did you mean `{s}`?")).with_fix(
                    format!("replace with `{s}`"),
                    head.span,
                    s,
                );
            } else {
                d = d.with_help(format!("states in scope here: {}", visible.join(", ")));
            }
            self.ck.diags.push(d);
            return None;
        };
        self.descend(node, t)
    }

    /// `.X` written in `from`, where `from` has no child `X`.
    fn unknown_child(&mut self, from: usize, t: &Target) {
        let head = &t.path[0];
        let own = self.nodes[from].id.clone();
        let children: Vec<String> = self.nodes[from]
            .order
            .iter()
            .map(|&c| self.nodes[c].name.clone())
            .collect();
        let mut d = Diagnostic::error(
            codes::UNKNOWN_TARGET,
            head.span,
            format!(
                "unknown target state `.{}`: `{own}` has no child `{}`",
                head.name, head.name
            ),
        );
        let retried = self.nodes[from]
            .body
            .invokes()
            .any(|inv| inv.retry.is_some());
        // Where the name is without the dot: among the siblings, then outward.
        let mut scope = Some(self.nodes[from].parent.unwrap_or(from));
        let mut outward = None;
        while let Some(sc) = scope {
            if let Some(c) = self.target_child(sc, &head.name) {
                outward = Some(c);
                break;
            }
            scope = self.nodes[sc].parent;
        }
        if let Some(s) = suggest(&head.name, children.iter().map(String::as_str)) {
            d = d.with_help(format!("did you mean `.{s}`?")).with_fix(
                format!("replace with `{s}`"),
                head.span,
                s,
            );
        } else if retried {
            d = d.with_help(format!(
                "`.X` names a child of `{own}`; a state with `retry` has none of its own, and the `Attempt` and `Backoff` states the retry compiles to can't be named"
            ));
        } else if let Some(c) = outward {
            d = d
                .with_help(format!(
                    "`.X` names a child of the state the transition is written in; without the dot, `{}` is `{}`, and the transition leaves `{own}`",
                    head.name,
                    self.nodes[c].id
                ))
                .with_fix("remove the dot", dot_span(t), String::new());
        } else if children.is_empty() {
            d = d.with_help(format!(
                "`.X` names a child of the state the transition is written in, and `{own}` has no child states"
            ));
        } else {
            d = d.with_help(format!(
                "the children of `{own}` are {}",
                children.join(", ")
            ));
        }
        self.ck.diags.push(d);
    }

    /// The rest of a target's path, below the state its first segment resolved to.
    fn descend(&mut self, mut node: usize, t: &Target) -> Option<usize> {
        let written: Vec<&str> = t.path.iter().map(|p| p.name.as_str()).collect();
        let written = format!("{}{}", if t.dot { "." } else { "" }, written.join("."));
        for seg in &t.path[1..] {
            if self.nodes[node].history.is_some() {
                let through = self.nodes[node].id.clone();
                self.ck.diags.push(
                    Diagnostic::error(
                        codes::UNKNOWN_TARGET,
                        seg.span,
                        format!("unknown target state `{written}`: `{through}` is a history state, which has no children"),
                    )
                    .with_help(format!("target the history state itself, `-> {}`, to return to where its parent was", t.path.iter().take_while(|p| !std::ptr::eq(*p, seg)).map(|p| p.name.as_str()).collect::<Vec<_>>().join("."))),
                );
                return None;
            }
            match self.target_child(node, &seg.name) {
                Some(c) => {
                    self.refer_state(c, seg);
                    node = c;
                }
                None => {
                    let parent = self.nodes[node].id.clone();
                    let children: Vec<String> = self.nodes[node]
                        .order
                        .iter()
                        .map(|&c| self.nodes[c].name.clone())
                        .collect();
                    let mut d = Diagnostic::error(
                        codes::UNKNOWN_TARGET,
                        seg.span,
                        format!(
                            "unknown target state `{written}`: `{parent}` has no child `{}`",
                            seg.name
                        ),
                    );
                    if let Some(s) = suggest(&seg.name, children.iter().map(String::as_str)) {
                        d = d.with_help(format!("did you mean `{s}`?")).with_fix(
                            format!("replace with `{s}`"),
                            seg.span,
                            s,
                        );
                    } else if children.is_empty() {
                        d = d.with_help(format!("`{parent}` has no child states"));
                    } else {
                        d = d.with_help(format!("its children are {}", children.join(", ")));
                    }
                    self.ck.diags.push(d);
                    return None;
                }
            }
        }
        Some(node)
    }

    fn refer_state(&mut self, node: usize, name: &Ident) {
        if let Some(symbol) = self.nodes[node].symbol {
            self.ck.index.refer(symbol, name.span);
        }
    }

    /// Checks and lowers effects: every `set`, then the `do`, `send` and `emit` effects, each
    /// in the order written, which is the order they run in. With each effect, what it reads.
    pub(crate) fn effects(
        &mut self,
        effects: &[Effect],
        scope: &Scope,
        place: Place,
    ) -> (Vec<ir::Effect>, Vec<Reads>) {
        let mut out = Vec::new();
        let mut reads = Vec::new();
        let sets = effects.iter().filter(|e| matches!(e, Effect::Set(_)));
        let rest = effects.iter().filter(|e| !matches!(e, Effect::Set(_)));
        for e in sets.chain(rest) {
            self.reads.clear();
            let before = out.len();
            match e {
                Effect::Set(a) => {
                    let local = self
                        .local(&a.var.name)
                        .map(|(owner, l)| (owner, l.ty.clone(), l.param, l.sensitive, l.symbol));
                    let (want, sensitive, state) = if let Some((owner, ty, param, sensitive, sym)) =
                        local
                    {
                        let id = self.nodes[owner].id.clone();
                        if let Some(sym) = sym
                            && !self.ck.is_compiled(self.file)
                        {
                            self.ck.index.refer(sym, a.var.span);
                        }
                        if param {
                            self.ck.diags.push(
                                Diagnostic::error(
                                    codes::ASSIGN_TO_PARAMETER,
                                    a.var.span,
                                    format!("`{}` is a parameter of `{id}`; parameters are read-only", a.var.name),
                                )
                                .with_help(format!(
                                    "declare a variable of the state to hold the changing value: `var {}: <type>`, or enter the state again with a new argument",
                                    a.var.name
                                )),
                            );
                        } else if let Place::Transition {
                            domain: Some(d), ..
                        } = place
                            && self.inside(owner, d)
                        {
                            self.ck.diags.push(
                                Diagnostic::warning(
                                    codes::LOCAL_SET_LOST,
                                    a.var.span,
                                    format!(
                                        "this assigns `{}`, a variable of `{id}`, in a transition that leaves `{id}`, so the value is discarded with it",
                                        a.var.name
                                    ),
                                )
                                .with_help(format!(
                                    "a state's variables get their initial values again each time it is entered; to keep the value, assign it in a transition with no target, pass it as an argument of a parameter (`-> {}(…)`), or use a machine variable",
                                    self.nodes[owner].name
                                )),
                            );
                        }
                        (ty, sensitive, Some(id))
                    } else {
                        self.ck.refer_member(self.file, self.symbol, &a.var);
                        let sensitive = self.vars.get(&a.var.name).is_some_and(|v| v.sensitive);
                        let want = match self.vars.get(&a.var.name) {
                            Some(v) if v.param => {
                                self.ck.diags.push(
                                    Diagnostic::error(
                                        codes::ASSIGN_TO_PARAMETER,
                                        a.var.span,
                                        format!("`{}` is a parameter of `{}`; parameters are read-only", a.var.name, self.machine),
                                    )
                                    .with_help(format!(
                                        "declare a variable to hold the changing value: `var {} : <type>`",
                                        a.var.name
                                    )),
                                );
                                v.ty.clone()
                            }
                            Some(v) => v.ty.clone(),
                            None => {
                                let d = self.unknown_variable(&a.var, false);
                                self.ck.diags.push(d);
                                Ty::Unknown
                            }
                        };
                        (want, sensitive, None)
                    };
                    let to = dest(sensitive, format!("`{}`", a.var.name));
                    let value = self.expect_to(
                        &a.value,
                        scope,
                        &want,
                        &format!("the value assigned to `{}`", a.var.name),
                        to,
                    );
                    out.push(ir::Effect::Set {
                        var: a.var.name.clone(),
                        state,
                        value,
                    });
                }
                Effect::Do(c) => {
                    let sig = self.callee(&c.name, SigKind::Action);
                    let args = self.args(
                        &c.name,
                        c.span,
                        &c.args,
                        sig.as_ref(),
                        SigKind::Action,
                        scope,
                        None,
                    );
                    out.push(ir::Effect::Do {
                        action: c.name.name.clone(),
                        args,
                    });
                }
                Effect::Send(m) | Effect::Emit(m) => {
                    if let Some(e) = self.message(m, scope, place) {
                        out.push(e);
                    }
                }
                Effect::Reply(r) => out.push(self.reply(r, scope)),
                Effect::Spawn(sp) => {
                    let written = format!("spawn {}", sp.children.name);
                    let c = self.collection(&sp.children, &written);
                    let want = c.as_ref().map_or(Ty::Unknown, |c| c.key.clone());
                    // `spawn.started` and `spawn.stopped` records show the key as it is.
                    let key = self.expect_to(
                        &sp.key,
                        scope,
                        &want,
                        &format!("the key of `{}`", sp.children.name),
                        Dest::Shown,
                    );
                    // A lower-case name is a task, an upper-case one a machine.
                    let kind = if callee_word(&sp.src.name) == "task" {
                        SigKind::Task
                    } else {
                        SigKind::Machine
                    };
                    let sig = self.callee(&sp.src, kind);
                    if let Some(c) = &c {
                        self.info.spawned.insert(sp.children.name.clone());
                        if let Some(m) = &c.src
                            && sig.is_some()
                            && *m != sp.src.name
                        {
                            let held = if c.task { "tasks" } else { "machines" };
                            self.ck.diags.push(
                                Diagnostic::error(
                                    codes::SPAWN_MACHINE_MISMATCH,
                                    sp.src.span,
                                    format!(
                                        "`{}` holds `{m}` {held}, not `{}`",
                                        sp.children.name, sp.src.name
                                    ),
                                )
                                .with_help(format!(
                                    "spawn a `{m}`, or declare another collection for `{}`",
                                    sp.src.name
                                )),
                            );
                        }
                    }
                    let span = sp.src.span.to(sp.span);
                    let args = self.args(&sp.src, span, &sp.args, sig.as_ref(), kind, scope, None);
                    if c.is_some() {
                        out.push(ir::Effect::Spawn {
                            children: sp.children.name.clone(),
                            src: sp.src.name.clone(),
                            key,
                            args,
                        });
                    }
                }
                Effect::Raise(r) => {
                    if let Some(e) = self.raise(r, scope) {
                        out.push(e);
                    }
                }
                Effect::Cancel(c) => {
                    if self.raise_names.contains_key(&c.name.name) {
                        out.push(ir::Effect::Cancel {
                            name: c.name.name.clone(),
                        });
                    } else {
                        let mut d = Diagnostic::error(
                            codes::UNKNOWN_RAISE_NAME,
                            c.name.span,
                            format!(
                                "no delayed raise of `{}` is named `{}`",
                                self.machine, c.name.name
                            ),
                        );
                        let names: Vec<&str> =
                            self.raise_names.keys().map(String::as_str).collect();
                        if let Some(s) = suggest(&c.name.name, names.iter().copied()) {
                            d = d.with_help(format!("did you mean `{s}`?")).with_fix(
                                format!("replace with `{s}`"),
                                c.name.span,
                                s,
                            );
                        } else {
                            d = d.with_help(format!(
                                "name the raise to cancel: `raise Event after 5s as {}`",
                                c.name.name
                            ));
                        }
                        self.ck.diags.push(d);
                    }
                }
                Effect::Stop(st) => {
                    let written = format!("stop {}", st.children.name);
                    let c = self.collection(&st.children, &written);
                    let want = c.as_ref().map_or(Ty::Unknown, |c| c.key.clone());
                    let key = self.expect(
                        &st.key,
                        scope,
                        &want,
                        &format!("the key of `{}`", st.children.name),
                    );
                    if c.is_some() {
                        out.push(ir::Effect::Stop {
                            children: st.children.name.clone(),
                            key,
                        });
                    }
                }
            }
            if out.len() > before {
                reads.push(std::mem::take(&mut self.reads));
            }
        }
        (out, reads)
    }

    /// Checks and lowers `raise E [{ fields }] [after d [as name]]`: `E` is one of the
    /// machine's own events, internal or not.
    fn raise(&mut self, r: &Raise, scope: &Scope) -> Option<ir::Effect> {
        let event = &r.message.event;
        self.ck.refer_member(self.file, self.symbol, event);
        let fields = self.events.get(&event.name).cloned();
        if fields.is_some() {
            self.info.raised.push((event.name.clone(), event.span));
        } else {
            let mut d = Diagnostic::error(
                codes::NOT_RAISABLE,
                event.span,
                format!("`{}` has no event `{}` to raise", self.machine, event.name),
            );
            if self.emits.contains_key(&event.name) {
                d = d.with_help(format!(
                    "`{}` is an event `{}` emits for others; a machine raises only its own events (`event` or `internal event`): `emit {}` sends it",
                    event.name, self.machine, event.name
                ));
            } else if let Some(s) = suggest(
                &event.name,
                self.info.events.iter().map(|(e, _)| e.as_str()),
            ) {
                d = d.with_help(format!("did you mean `{s}`?")).with_fix(
                    format!("replace with `{s}`"),
                    event.span,
                    s,
                );
            } else {
                d = d.with_help(format!(
                    "declare it in the machine body: `internal event {}` for an event only the machine raises, or `event {}`",
                    event.name, event.name
                ));
            }
            self.ck.diags.push(d);
        }
        let ty = Ty::Event(event.name.clone());
        let delay = r
            .delay
            .as_ref()
            .map(|d| self.expect_to(d, scope, &DURATION, "a `raise` delay", Dest::Shown));
        let value = self.message_value(&r.message, &ty, fields.as_deref(), scope);
        fields.map(|_| ir::Effect::Raise {
            event: event.name.clone(),
            value,
            delay,
            name: r.name.as_ref().map(|n| n.name.clone()),
        })
    }

    /// Checks and lowers `reply x done [value]` or `reply x fail error`.
    fn reply(&mut self, r: &Reply, scope: &Scope) -> ir::Effect {
        let target = ast::Expr {
            kind: ast::ExprKind::Var(r.call.clone()),
            span: r.call.span,
        };
        let (call, t) = self.expr(&target, scope);
        let name = &r.call.name;
        let sig = match self.ck.env.expand(&t) {
            Ty::Call(c) => {
                if name != "call" {
                    self.info.replied.insert(name.clone());
                }
                self.calls.get(&c).cloned().map(|s| (c, s))
            }
            Ty::Unknown => None,
            other => {
                let what = if self.vars.get(name).is_some_and(|v| v.param) {
                    "a parameter"
                } else {
                    "a variable"
                };
                self.ck.diags.push(
                    Diagnostic::error(
                        codes::NOT_A_PENDING_CALL,
                        r.call.span,
                        format!("`{name}` is {what} of type `{other}`, not a pending call"),
                    )
                    .with_help("reply to `call` in an `on name` transition, or to a variable of type `call name` that holds it"),
                );
                None
            }
        };
        let value = match (r.outcome, &r.value, &sig) {
            (ReplyOutcome::Done, Some(v), Some((c, s))) => match &s.returns {
                Some(want) => {
                    // A `replied` record shows the value, redacted by the result's type.
                    let what = format!("the result of call `{c}`");
                    Some(self.expect_to(v, scope, &want.clone(), &what, Dest::Shown))
                }
                None => {
                    self.ck.diags.push(
                        Diagnostic::error(
                            codes::BAD_REPLY,
                            v.span,
                            format!("call `{c}` returns nothing, so its `reply … done` has no value"),
                        )
                        .with_secondary(s.name_span, "declared here")
                        .with_help(format!("write `reply {name} done`, or declare what it returns: `call {c}(…) -> <type>`")),
                    );
                    Some(self.expr(v, scope).0)
                }
            },
            (ReplyOutcome::Done, None, Some((c, s))) => {
                if let Some(want) = &s.returns {
                    self.ck.diags.push(
                        Diagnostic::error(
                            codes::BAD_REPLY,
                            r.span,
                            format!("call `{c}` returns `{want}`, so `reply … done` needs a value"),
                        )
                        .with_secondary(s.name_span, "declared here")
                        .with_help(format!("write `reply {name} done <{want}>`")),
                    );
                }
                None
            }
            (ReplyOutcome::Fail, Some(v), Some((c, s))) => {
                let (ie, t) = self.expr(v, scope);
                // A `replied` record shows the error, redacting only its sensitive fields.
                let what = format!("the error of call `{c}`");
                self.check_dest(v, &ie.text.clone(), &t, &t, &what, &Dest::Shown);
                let ok = match self.ck.env.expand(&t) {
                    Ty::Unknown => true,
                    Ty::Named(n) => s.errors.contains(&n),
                    _ => false,
                };
                if !ok {
                    let mut d = Diagnostic::error(
                        codes::BAD_REPLY,
                        v.span,
                        if s.errors.is_empty() {
                            format!("call `{c}` declares no errors, so it cannot be answered with `fail`")
                        } else {
                            format!(
                                "`reply … fail` needs one of the errors call `{c}` declares, found `{t}`"
                            )
                        },
                    )
                    .with_secondary(s.name_span, "declared here");
                    if self.ck.env.expand(&t) == Ty::Named(UNANSWERED.into()) {
                        d = d.with_help("a call is answered `Unanswered` only when it is not answered; reply with one of its declared errors");
                    } else if !s.errors.is_empty() {
                        d = d.with_label(format!("`{c}` declares `! {}`", s.errors.join(" | ")));
                    }
                    self.ck.diags.push(d);
                }
                Some(ie)
            }
            (_, Some(v), None) => Some(self.expr(v, scope).0),
            (_, None, _) => None,
        };
        ir::Effect::Reply {
            call,
            outcome: match r.outcome {
                ReplyOutcome::Done => ir::ReplyOutcome::Done,
                ReplyOutcome::Fail => ir::ReplyOutcome::Fail,
            },
            value,
        }
    }

    /// "unknown variable", with a suggestion among the variables (and parameters, if
    /// `params`) of the machine, or a note that the name is a declaration of another kind.
    pub(crate) fn unknown_variable(&self, n: &Ident, params: bool) -> Diagnostic {
        if let Found::Decl(id) = self.ck.names.lookup(self.file, &n.name) {
            return self.ck.names.wrong_kind(n, id, "a variable");
        }
        if let Some((state, _)) = self.alias_anywhere(&n.name) {
            return Diagnostic::error(
                codes::UNKNOWN_VARIABLE,
                n.span,
                format!(
                    "`{}` names the machine `{}` invokes, not a value",
                    n.name, state.id
                ),
            )
            .with_help(format!(
                "an `as` name is used only in `send {0}.Event` and `on {0}.Event`",
                n.name
            ));
        }
        if self.collections.contains_key(&n.name) {
            return Diagnostic::error(
                codes::UNKNOWN_VARIABLE,
                n.span,
                format!("`{}` is a children collection, not a value", n.name),
            )
            .with_help(format!(
                "a collection is used only in `spawn {0}[key]`, `stop {0}[key]`, `send {0}[key].Event` and `on {0}.Event`",
                n.name
            ));
        }
        if let Some(owner) = self.local_anywhere(&n.name) {
            let here = &self.nodes[self.here].id;
            return Diagnostic::error(
                codes::UNKNOWN_VARIABLE,
                n.span,
                format!(
                    "`{}` is a local of `{owner}`, which is not in scope in `{here}`",
                    n.name
                ),
            )
            .with_help(format!(
                "a state's parameters and variables exist only in that state and the states inside it; pass the value on as an argument, or declare `{}` in a state around both, or in the machine",
                n.name
            ));
        }
        let mut d = Diagnostic::error(
            codes::UNKNOWN_VARIABLE,
            n.span,
            format!(
                "unknown variable `{}` in machine `{}`",
                n.name, self.machine
            ),
        );
        let in_scope: Vec<&str> = if self.locals.is_empty() {
            Vec::new()
        } else {
            std::iter::once(self.here)
                .chain(self.ancestors(self.here))
                .flat_map(|a| self.locals[a].iter())
                .filter(|l| params || !l.param)
                .map(|l| l.name.as_str())
                .collect()
        };
        let mut candidates: Vec<&str> = self
            .vars
            .iter()
            .filter(|(k, v)| (params || !v.param) && self.counter.as_ref() != Some(*k))
            .map(|(k, _)| k.as_str())
            .chain(in_scope)
            .collect();
        candidates.sort_unstable();
        if n.name == "attempt" {
            d = d.with_help("`attempt` counts failed attempts only in a retry's delay: `invoke f() retry 3 after backoff(attempt) { … }`");
        } else if let Some(s) = suggest(&n.name, candidates.into_iter()) {
            d = d.with_help(format!("did you mean `{s}`?")).with_fix(
                format!("replace with `{s}`"),
                n.span,
                s,
            );
        } else if !params && !self.vars.contains_key(&n.name) {
            d = d.with_help(format!(
                "declare it in the machine body: `var {}: <type>`",
                n.name
            ));
        }
        d
    }

    /// Looks up a function, action, task or machine by the name used in a call, `do` or
    /// `invoke`, reporting it if it is missing or of another kind, and records the use.
    pub(crate) fn callee(&mut self, n: &Ident, kind: SigKind) -> Option<Sig> {
        let (code, what, wanted) = match kind {
            SigKind::Fn => (codes::UNKNOWN_FUNCTION, "function", "a function"),
            SigKind::Action => (codes::UNKNOWN_ACTION, "action", "an action"),
            SigKind::Task => (codes::UNKNOWN_INVOKE, "task", "a task"),
            SigKind::Machine => (codes::UNKNOWN_INVOKE, "machine", "a machine"),
        };
        let accept = move |k: &DeclKind| match kind {
            SigKind::Fn => matches!(k, DeclKind::Fn { .. }),
            SigKind::Action => matches!(k, DeclKind::Action { .. }),
            SigKind::Task => matches!(k, DeclKind::Task { .. }),
            SigKind::Machine => matches!(k, DeclKind::Machine(_)),
        };
        self.ck.refer_decl(self.file, n);
        match self.ck.names.lookup(self.file, &n.name) {
            Found::Decl(id) if accept(&self.ck.names.decl(id).kind) => {
                self.ck.used.insert(n.name.clone());
                self.ck.sigs.get(&n.name).cloned()
            }
            Found::Decl(id) => {
                let mut d = self.ck.names.wrong_kind(n, id, wanted);
                d = d.with_help(match self.ck.names.decl(id).kind {
                    DeclKind::Fn { .. } => "functions are called in expressions: `f(x)`",
                    DeclKind::Action { .. } => "actions are run with `do`: `do audit(x)`",
                    DeclKind::Task { .. } | DeclKind::Machine(_) => {
                        "tasks and machines are run with `invoke` in a state"
                    }
                    _ => "types are not called",
                });
                self.ck.diags.push(d);
                None
            }
            Found::Poisoned => None,
            Found::Missing => {
                let d = self.ck.names.unknown(self.file, n, code, what, accept);
                self.ck.diags.push(d);
                None
            }
        }
    }

    /// Checks arguments against a signature (if known) and lowers them. `call` is, for a call
    /// invoke `peer.c(…)`, the `ref` it goes through and that ref's machine, whose call `c`'s
    /// parameters the arguments go to.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn args(
        &mut self,
        name: &Ident,
        span: Span,
        args: &[ast::Expr],
        sig: Option<&Sig>,
        kind: SigKind,
        scope: &Scope,
        call: Option<(&str, &str)>,
    ) -> Vec<ir::Expr> {
        let (owner, shown) = match call {
            Some((to, machine)) => (
                Sensitive::call(machine, &name.name),
                format!("{to}.{}", name.name),
            ),
            None => (name.name.clone(), name.name.clone()),
        };
        if let Some(sig) = sig
            && sig.params.len() != args.len()
        {
            let n = sig.params.len();
            let plural = |n: usize| if n == 1 { "" } else { "s" };
            let params: Vec<String> = sig
                .params
                .iter()
                .map(|(p, t)| format!("{p}: {t}"))
                .collect();
            self.ck.diags.push(
                Diagnostic::error(
                    codes::ARGUMENT_COUNT,
                    span,
                    format!(
                        "`{}` takes {n} argument{} but {} {} given",
                        name.name,
                        plural(n),
                        args.len(),
                        if args.len() == 1 { "was" } else { "were" }
                    ),
                )
                .with_secondary(
                    sig.name_span,
                    format!("declared as `{}({})`", name.name, params.join(", ")),
                ),
            );
        }
        args.iter()
            .enumerate()
            .map(|(i, a)| match sig.and_then(|s| s.params.get(i)) {
                Some((p, t)) => {
                    let t = t.clone();
                    let what = format!("argument `{p}` of `{shown}`");
                    if kind == SigKind::Fn {
                        // Function calls are not traced.
                        self.expect(a, scope, &t, &what)
                    } else {
                        let to = dest(
                            self.ck.sensitive.is(&owner, p),
                            format!("parameter `{p}` of `{shown}`"),
                        );
                        self.expect_to(a, scope, &t, &what, to)
                    }
                }
                None => self.expr(a, scope).0,
            })
            .collect()
    }

    /// Checks and lowers an invoke of node `idx`, the `index`th of its state's, and its
    /// outcomes; `several` when the state has more than one.
    fn invoke(
        &mut self,
        idx: usize,
        inv: &Invoke,
        (index, several): (usize, bool),
        group: &mut u32,
        transitions: &mut Vec<ir::Transition>,
        triggers: &mut Vec<Span>,
    ) -> (ir::Invoke, Reads, Option<Vec<String>>) {
        let is_machine = inv.is_machine();
        let is_call = inv.is_call();
        let (to, sig) = match &inv.to {
            Some(to) => self.call_target(to, &inv.callee),
            None => {
                let kind = if is_machine {
                    SigKind::Machine
                } else {
                    SigKind::Task
                };
                (None, self.callee(&inv.callee, kind))
            }
        };
        let args = self.args(
            &inv.callee,
            invoke_head(inv),
            &inv.args,
            sig.as_ref(),
            if is_machine {
                SigKind::Machine
            } else {
                SigKind::Task
            },
            &Scope::none(),
            to.as_ref().map(|a| (a.name.as_str(), a.peer())),
        );
        let arg_reads = std::mem::take(&mut self.reads);
        // The name the IR gives the invoke, and its outcomes' triggers: on a call, only in a
        // state with several invokes, where it says which one an outcome belongs to.
        let alias = inv
            .alias
            .as_ref()
            .filter(|_| several || !is_call)
            .map(|a| a.name.clone());
        // What the invoke can fail with: a call also with `Unanswered`.
        let errors: Option<Vec<String>> = sig.as_ref().map(|s| {
            let mut e = s.errors.clone();
            if is_call {
                e.push(UNANSWERED.to_string());
            }
            e
        });

        let listed: Vec<&Ident> = inv
            .outcomes
            .iter()
            .filter_map(|o| match &o.trigger {
                Trigger::Fail(Pattern::Name(n)) => Some(n),
                _ => None,
            })
            .collect();
        let has_any = inv
            .outcomes
            .iter()
            .any(|o| matches!(o.trigger, Trigger::Fail(Pattern::Any { .. })));
        let callee = match &inv.to {
            Some(to) => format!("{}.{}", to.name, inv.callee.name),
            None => inv.callee.name.clone(),
        };
        let declared_as = |sig: &Sig| {
            let text = sig.result_text();
            if text.is_empty() {
                "declared with no result and no errors".to_string()
            } else {
                format!("declared as `{text}`")
            }
        };

        if let (Some(sig), Some(errors)) = (&sig, &errors)
            && !has_any
        {
            for e in errors {
                if !listed.iter().any(|n| n.name == *e) {
                    let label = if e == UNANSWERED {
                        "a call fails with `Unanswered` when the callee does not answer it"
                            .to_string()
                    } else {
                        declared_as(sig)
                    };
                    self.ck.diags.push(
                        Diagnostic::error(
                            codes::UNHANDLED_ERROR,
                            invoke_head(inv),
                            format!("`invoke {callee}` does not handle `{e}`"),
                        )
                        .with_label(label)
                        .with_help(format!("add `fail {e} -> <State>` or `fail * -> <State>`")),
                    );
                }
            }
        }

        let owner = format!("`invoke {callee}`");
        let mut keys: HashMap<String, Span> = HashMap::new();
        for o in &inv.outcomes {
            self.duplicate_trigger(&mut keys, o, "", &owner);
            let mut scope = Scope::none();
            let trigger = match &o.trigger {
                Trigger::Done => {
                    scope.result = match &sig {
                        Some(s) => match &s.returns {
                            Some(t) => Implicit::Has(t.clone()),
                            None => Implicit::Bad(
                                codes::NO_RESULT,
                                format!(
                                    "`{callee}` returns nothing, so its `done` has no `result`"
                                ),
                            ),
                        },
                        None => Implicit::Has(Ty::Unknown),
                    };
                    ir::Trigger::InvokeDone {
                        alias: alias.clone(),
                    }
                }
                Trigger::Fail(Pattern::Name(n)) if n.name == UNANSWERED && !is_call => {
                    scope.error = Implicit::Has(Ty::Unknown);
                    self.ck.diags.push(unanswered_misused(
                        n.span,
                        &format!("`{callee}` is not a call, so it is never `Unanswered`"),
                    ));
                    ir::Trigger::InvokeFail {
                        alias: alias.clone(),
                        error: n.name.clone(),
                    }
                }
                Trigger::Fail(Pattern::Name(n)) => {
                    // The error the callee declares by that name, imported here or not.
                    match self.ck.decl_symbol(&n.name) {
                        Some(e) if sig.as_ref().is_some_and(|s| s.errors.contains(&n.name)) => {
                            self.ck.index.refer(e, n.span);
                        }
                        _ => self.ck.refer_decl(self.file, n),
                    }
                    scope.error = Implicit::Has(Ty::Unknown);
                    if let (Some(sig), Some(errors)) = (&sig, &errors) {
                        if errors.contains(&n.name) {
                            scope.error = Implicit::Has(Ty::Named(n.name.clone()));
                        } else {
                            let mut d = Diagnostic::error(
                                codes::UNDECLARED_ERROR_OUTCOME,
                                n.span,
                                format!("`{callee}` does not declare the error `{}`", n.name),
                            )
                            .with_secondary(sig.name_span, declared_as(sig));
                            let unlisted: Vec<&str> = errors
                                .iter()
                                .filter(|e| !listed.iter().any(|l| l.name == **e))
                                .map(String::as_str)
                                .collect();
                            if let Some(s) = suggest(&n.name, unlisted.iter().copied()) {
                                d = d.with_help(format!("did you mean `{s}`?")).with_fix(
                                    format!("replace with `{s}`"),
                                    n.span,
                                    s,
                                );
                            } else {
                                d = d.with_help(
                                    "remove this outcome, or add the error to the declaration",
                                );
                            }
                            self.ck.diags.push(d);
                        }
                    }
                    ir::Trigger::InvokeFail {
                        alias: alias.clone(),
                        error: n.name.clone(),
                    }
                }
                Trigger::Fail(Pattern::Any { .. }) => {
                    scope.error = match &errors {
                        Some(errors) => {
                            let rest: Vec<&String> = errors
                                .iter()
                                .filter(|e| !listed.iter().any(|l| l.name == **e))
                                .collect();
                            match rest.as_slice() {
                                [one] => Implicit::Has(Ty::Named((*one).clone())),
                                [] => Implicit::Bad(
                                    codes::AMBIGUOUS_ERROR,
                                    format!(
                                        "`error` has no type here: `fail *` covers none of `{callee}`'s errors"
                                    ),
                                ),
                                more => Implicit::Bad(
                                    codes::AMBIGUOUS_ERROR,
                                    format!(
                                        "`error` has no single type here: `fail *` covers {}",
                                        more.iter()
                                            .map(|e| format!("`{e}`"))
                                            .collect::<Vec<_>>()
                                            .join(", ")
                                    ),
                                ),
                            }
                        }
                        None => Implicit::Has(Ty::Unknown),
                    };
                    ir::Trigger::InvokeFail {
                        alias: alias.clone(),
                        error: "*".into(),
                    }
                }
                // The parser accepts only `done` and `fail` here.
                _ => ir::Trigger::InvokeDone {
                    alias: alias.clone(),
                },
            };
            self.branches(
                idx,
                o,
                trigger,
                &scope,
                (*group, Some(index)),
                transitions,
                triggers,
            );
            *group += 1;
        }

        let lowered = ir::Invoke {
            kind: if is_call {
                ir::InvokeKind::Call
            } else if is_machine {
                ir::InvokeKind::Machine
            } else {
                ir::InvokeKind::Task
            },
            src: inv.callee.name.clone(),
            to,
            alias,
            args,
            span: ir_span(inv.span),
        };
        (lowered, arg_reads, sig.map(|s| s.errors))
    }

    /// Resolves `x` and `name` of `invoke x.name(…)`: `x` must be a parameter or variable of
    /// type `ref M`, and `name` a call of `M`. Returns the callee's address and the call's
    /// signature, each if known. A read of a variable `x` is recorded.
    fn call_target(&mut self, to: &Ident, name: &Ident) -> (Option<ir::Address>, Option<Sig>) {
        let Some(v) = self.vars.get(&to.name) else {
            let mut d = Diagnostic::error(
                codes::UNKNOWN_ADDRESS,
                to.span,
                format!("unknown machine `{}` to call", to.name),
            );
            let mut candidates: Vec<&str> = self
                .vars
                .iter()
                .filter(|(_, v)| matches!(self.ck.env.expand(&v.ty), Ty::Ref(_)))
                .map(|(n, _)| n.as_str())
                .collect();
            candidates.sort_unstable();
            if let Some(s) = suggest(&to.name, candidates.into_iter()) {
                d = d.with_help(format!("did you mean `{s}`?")).with_fix(
                    format!("replace with `{s}`"),
                    to.span,
                    s,
                );
            } else {
                d = d.with_help("a call goes to the run that a parameter or variable of type `ref Machine` holds");
            }
            self.ck.diags.push(d);
            return (None, None);
        };
        let t = v.ty.clone();
        let param = v.param;
        self.ck.refer_member(self.file, self.symbol, to);
        let why = match self.ck.env.expand(&t) {
            Ty::Ref(machine) => {
                if !param {
                    self.reads.push((to.name.clone(), to.span));
                }
                let address = ir::Address {
                    kind: ir::AddressKind::Ref,
                    name: to.name.clone(),
                    machine: Some(machine.clone()),
                    task: None,
                    state: None,
                };
                let callee = self.ck.decl_symbol(&machine);
                if let Some(id) = self.ck.call_symbol(callee, &name.name)
                    && !self.ck.is_compiled(self.file)
                {
                    self.ck.index.refer(id, name.span);
                }
                let sig = self
                    .ck
                    .interfaces
                    .get(&machine)
                    .and_then(|i| i.calls.get(&name.name))
                    .cloned();
                if sig.is_none() {
                    self.unknown_call(&machine, name, Some(&to.name));
                }
                return (Some(address), sig);
            }
            Ty::Unknown => return (None, None),
            Ty::Optional(_) if self.ck.env.has_ref(&t) => {
                format!("`{}` is `{t}`, which may be absent", to.name)
            }
            Ty::List(_) if self.ck.env.has_ref(&t) => {
                format!("`{}` is `{t}`, a list of handles, not one", to.name)
            }
            _ => format!("`{}` is `{t}`, not a `ref`", to.name),
        };
        self.ck
            .diags
            .push(Diagnostic::error(codes::NOT_A_REF, to.span, why).with_help(
                "a call goes to the run that a parameter or variable of type `ref Machine` holds",
            ));
        (None, None)
    }

    fn output(&mut self, e: &ast::Expr) -> ir::Expr {
        match self.returns.clone() {
            Some(t) => {
                let what = format!("the output of `{}`", self.machine);
                self.expect_to(e, &Scope::none(), &t, &what, Dest::Shown)
            }
            None => {
                let m = self.machine.clone();
                self.ck.diags.push(
                    Diagnostic::error(
                        codes::OUTPUT_WITHOUT_RETURN,
                        e.span,
                        format!("`{m}` returns nothing, so it has no `output`"),
                    )
                    .with_help(format!(
                        "declare what it returns: `machine {m}(…) -> <type>`"
                    )),
                );
                self.expr(e, &Scope::none()).0
            }
        }
    }

    fn fail_value(&mut self, e: &ast::Expr) -> ir::Expr {
        let (ie, t) = self.expr(e, &Scope::none());
        let m = self.machine.clone();
        if self.sens {
            self.ck.diags.push(
                Diagnostic::error(
                    codes::SENSITIVE_VALUE,
                    e.span,
                    format!("`{}` is sensitive, and the failure of `{m}` is not", ie.text),
                )
                .with_label("a trace would show this value")
                .with_help("traces show a machine's failure as it is, redacting only the error's sensitive fields; fail with an error built with the value in a `sensitive` field"),
            );
        }
        let ok = match self.ck.env.expand(&t) {
            Ty::Unknown => true,
            Ty::Named(n) => self.errors.contains(&n),
            _ => false,
        };
        if !ok {
            let d = if self.errors.is_empty() {
                Diagnostic::error(
                    codes::FAIL_NOT_DECLARED,
                    e.span,
                    format!("`{m}` declares no errors, so it cannot `fail`"),
                )
                .with_help(format!(
                    "declare the errors it can fail with: `machine {m}(…) ! {t}`"
                ))
            } else {
                Diagnostic::error(
                    codes::FAIL_NOT_DECLARED,
                    e.span,
                    format!("`fail` needs one of the errors `{m}` declares, found `{t}`"),
                )
                .with_label(format!("`{m}` declares `! {}`", self.errors.join(" | ")))
            };
            self.ck.diags.push(d);
        }
        ie
    }
}

/// `invoke name`: where diagnostics about an invoke point.
/// "task" for a lower-case callee (`upload`), "machine" for an upper-case one, by the rule that
/// tells them apart in `invoke`, `children` and `spawn`.
pub(crate) fn callee_word(callee: &str) -> &'static str {
    if callee.starts_with(|c: char| c.is_ascii_lowercase()) {
        "task"
    } else {
        "machine"
    }
}

fn invoke_head(inv: &Invoke) -> Span {
    inv.span.to(inv.callee.span)
}

/// Where a value goes: `place`, which is marked `sensitive` or not.
fn dest(sensitive: bool, place: String) -> Dest {
    if sensitive {
        Dest::Sensitive
    } else {
        Dest::Plain(place)
    }
}

/// The leading dot of a target written `.X`.
fn dot_span(t: &Target) -> Span {
    let mut end = t.span.start;
    end.offset += 1;
    end.col += 1;
    Span { end, ..t.span }
}
