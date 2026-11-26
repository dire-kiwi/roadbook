//! Machines: variables, events, the state tree, targets, invokes and transitions, checked
//! and lowered to the IR.

use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;
use roadbook_ir as ir;
use roadbook_syntax::ast::{
    self, Body, Branches, Decl, DeclKind, Effect, Ident, Invoke, Item, Message, Pattern, State,
    StateKind, Target, Transition, Trigger,
};
use roadbook_syntax::diagnostic::Code;
use roadbook_syntax::{Diagnostic, Span, expr_text, message_value};

use crate::check::{Checker, Sig, SigKind, doc, ir_span};
use crate::codes;
use crate::index::{SymbolId, SymbolKind};
use crate::names::{Found, is_type, kind_phrase, suggest};
use crate::sensitive::Dest;
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
    /// The branch: the whole line for a single-branch trigger, from `if` or `else` in a block.
    pub span: Span,
}

/// Source positions of a state's parts, kept for the analyses that run on the IR.
#[derive(Clone, Debug)]
pub(crate) struct StateInfo {
    pub name_span: Span,
    /// `invoke name`, if the state has an invoke.
    pub invoke_span: Option<Span>,
    /// The trigger of each of the state's IR transitions, by index.
    pub triggers: Vec<Span>,
    /// The name in `initial Name`, for a compound state.
    pub initial_span: Option<Span>,
    /// What each IR `entry` and `exit` effect reads, by index.
    pub entry_reads: Vec<Reads>,
    pub exit_reads: Vec<Reads>,
    /// What the invoke's arguments read.
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
    /// The machine's variables, in order.
    pub vars: Vec<VarInfo>,
}

/// Where effects run, for the check that a `send` to an invoked child finds it running.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Place {
    /// The `entry` of a state (by node).
    Entry(usize),
    /// The `exit` of a state.
    Exit(usize),
    /// A transition written in a state, with its target, and whether it is an outcome of the
    /// state's invoke.
    Transition {
        node: usize,
        target: Option<usize>,
        outcome: bool,
    },
}

/// What an implicit name (`event`, `result`, `error`) means at some point.
#[derive(Clone, Debug)]
pub(crate) enum Implicit {
    NotHere,
    Has(Ty),
    /// In scope but unusable, with the diagnostic to give.
    Bad(Code, String),
}

#[derive(Clone, Debug)]
pub(crate) struct Scope {
    pub event: Implicit,
    pub result: Implicit,
    pub error: Implicit,
}

impl Scope {
    pub fn none() -> Scope {
        Scope {
            event: Implicit::NotHere,
            result: Implicit::NotHere,
            error: Implicit::NotHere,
        }
    }
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
    /// `invoke Machine(…) as name`: the name and the machine.
    alias: Option<(&'p Ident, String)>,
    /// The state's symbol (the machine's, for the root).
    symbol: Option<SymbolId>,
    /// The symbol of the `as` name.
    alias_symbol: Option<SymbolId>,
}

pub(crate) struct Var {
    pub ty: Ty,
    pub param: bool,
    /// Marked `sensitive`.
    pub sensitive: bool,
}

pub(crate) struct Cx<'c, 'p> {
    pub ck: &'c mut Checker<'p>,
    pub file: usize,
    pub machine: String,
    /// The machine's symbol, which its parameters, variables and events are members of.
    pub symbol: Option<SymbolId>,
    pub vars: HashMap<String, Var>,
    pub events: HashMap<String, Vec<(String, Ty)>>,
    pub emits: HashMap<String, Vec<(String, Ty)>>,
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
        emits: HashMap::new(),
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
            vars: Vec::new(),
        },
        reads: Vec::new(),
        transition_reads: Vec::new(),
        retry_vars: Vec::new(),
        counter: None,
        sens: false,
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
    let mut var_tys = Vec::new();
    for v in m.body.vars() {
        let ty = cx.ck.resolve(file, &v.ty);
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

    // Events and emitted events: one namespace. Their field types are in the interface.
    let iface = cx.ck.interfaces.get(&name).cloned().unwrap_or_default();
    let mut events = IndexMap::new();
    let mut emits = IndexMap::new();
    let mut first_seen: HashMap<String, Span> = HashMap::new();
    for (is_emits, e) in m.body.items.iter().filter_map(|i| match i {
        Item::Event(e) => Some((false, e)),
        Item::Emits(e) => Some((true, e)),
        _ => None,
    }) {
        let word = if is_emits { "emitted event" } else { "event" };
        if let Some(first) = first_seen.get(&e.name.name) {
            cx.ck.diags.push(
                Diagnostic::error(
                    codes::DUPLICATE_NAME,
                    e.name.span,
                    format!("event `{}` is declared twice", e.name.name),
                )
                .with_secondary(*first, "first declared here")
                .with_help(
                    "the events a machine accepts and those it emits share one set of names",
                ),
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
        } else {
            let fields = iface.events.get(&e.name.name).cloned().unwrap_or_default();
            cx.events.insert(e.name.name.clone(), fields);
            cx.info.events.push((e.name.name.clone(), e.name.span));
            events.insert(e.name.name.clone(), decl);
        }
    }

    // Variable initialisers may read parameters and other variables.
    let mut vars = Vec::new();
    for (v, ty) in m.body.vars().zip(var_tys) {
        cx.reads.clear();
        let init = v.init.as_ref().map(|e| {
            cx.expect_to(
                e,
                &Scope::none(),
                &ty,
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
        alias: None,
        symbol,
        alias_symbol: None,
    });
    cx.build_tree(0);
    cx.check_aliases();
    let mut root = cx.lower_state(0);
    root.doc = doc(&d.doc);
    root.span = ir_span(d.span);
    for (v, info) in std::mem::take(&mut cx.retry_vars) {
        vars.push(v);
        cx.info.vars.push(info);
    }

    let info = cx.info;
    ck.machines.push(info);
    ir::Machine {
        doc: doc(&d.doc),
        span: ir_span(d.span),
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
        emits,
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
        Trigger::Receive { from, event } => format!("on {}.{}", from.name, event.name),
        Trigger::After(e) => format!("after {}", expr_text(e)),
        Trigger::Always => "always".into(),
        Trigger::Done => "done".into(),
        Trigger::Fail(p) => format!("fail {}", pattern(p)),
    }
}

impl<'p> Cx<'_, 'p> {
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
            let invoke = s.body.invokes().next();
            let alias = invoke.and_then(|inv| {
                let a = inv.alias.as_ref()?;
                Some((a, inv.callee.name.clone()))
            });
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
            let alias_symbol = invoke.and_then(|inv| {
                let a = inv.alias.as_ref()?;
                Some(self.ck.define(
                    SymbolKind::InvokeName,
                    a,
                    a.span,
                    Some(symbol),
                    &None,
                    format!("invoke {}(…) as {}", inv.callee.name, a.name),
                ))
            });
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
                alias,
                symbol: Some(symbol),
                alias_symbol,
            });
            self.nodes[parent].children.push(idx);
            self.nodes[parent].order.push(idx);
            self.build_tree(idx);
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
            alias: None,
            symbol: Some(symbol),
            alias_symbol: None,
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
        self.info.states.insert(
            id.clone(),
            StateInfo {
                name_span: h.name.span,
                invoke_span: None,
                triggers: Vec::new(),
                initial_span: None,
                entry_reads: Vec::new(),
                exit_reads: Vec::new(),
                invoke_reads: Vec::new(),
                result_reads: Vec::new(),
                transition_reads: Vec::new(),
                retry_branches: Vec::new(),
                written_as: None,
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
            initial: None,
            states: Vec::new(),
            entry: Vec::new(),
            exit: Vec::new(),
            invoke: None,
            transitions: Vec::new(),
            output: None,
            fail: None,
        }
    }

    /// Reports `as` names that cannot be used: on a task, the name of a parameter or variable,
    /// or the name of an invoke around it. Such a name is not in scope.
    fn check_aliases(&mut self) {
        for i in 0..self.nodes.len() {
            let Some((alias, callee)) = self.nodes[i].alias.clone() else {
                continue;
            };
            let mut bad = None;
            if !callee.starts_with(|c: char| c.is_ascii_uppercase()) {
                bad = Some(
                    Diagnostic::error(
                        codes::ALIAS_ON_TASK,
                        alias.span,
                        format!(
                            "`{callee}` is a task, so `as {}` gives it a name nothing can use",
                            alias.name
                        ),
                    )
                    .with_help(
                        "only an invoked machine can be sent events and emit them; remove `as …`",
                    ),
                );
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
            } else {
                let mut up = self.nodes[i].parent;
                while let Some(a) = up {
                    if let Some((outer, _)) = &self.nodes[a].alias
                        && outer.name == alias.name
                    {
                        bad = Some(
                            Diagnostic::error(
                                codes::DUPLICATE_NAME,
                                alias.span,
                                format!(
                                    "`{}` already names the machine invoked by `{}`, which contains this state",
                                    alias.name, self.nodes[a].id
                                ),
                            )
                            .with_secondary(outer.span, "named here")
                            .with_help("choose another name: both children run at once here"),
                        );
                        break;
                    }
                    up = self.nodes[a].parent;
                }
            }
            if let Some(d) = bad {
                self.ck.diags.push(d);
                self.nodes[i].alias = None;
            }
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
            if let Some((alias, machine)) = &self.nodes[a].alias
                && alias.name == name.name
            {
                if let Some(symbol) = self.nodes[a].alias_symbol {
                    self.ck.index.refer(symbol, name.span);
                }
                // An invoke of a machine that is not declared (already reported) names a child
                // with no known events: the name is in scope, but nothing is checked against it.
                let known = matches!(
                    self.ck.names.lookup(self.file, machine),
                    Found::Decl(id) if matches!(self.ck.names.decl(id).kind, DeclKind::Machine(_))
                );
                if !known {
                    return None;
                }
                let address = ir::Address {
                    kind: ir::AddressKind::Child,
                    name: name.name.clone(),
                    machine: machine.clone(),
                    state: Some(self.nodes[a].id.clone()),
                };
                return Some((address, Some(a)));
            }
            up = self.nodes[a].parent;
        }
        if let Some(v) = self.vars.get(&name.name) {
            let t = v.ty.clone();
            self.ck.refer_member(self.file, self.symbol, name);
            let why = match self.ck.env.expand(&t) {
                Ty::Ref(machine) => {
                    let address = ir::Address {
                        kind: ir::AddressKind::Ref,
                        name: name.name.clone(),
                        machine,
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
            .nodes
            .iter()
            .find(|n| n.alias.as_ref().is_some_and(|(a, _)| a.name == name.name))
            .map(|n| n.id.clone());
        let mut d = match &elsewhere {
            Some(state) => Diagnostic::error(
                codes::UNKNOWN_ADDRESS,
                name.span,
                format!("`{}` is not in scope here: it names the machine `{state}` invokes", name.name),
            )
            .with_help(format!(
                "an `as` name is in scope only in the invoking state and the states inside it, while the child runs; move this into `{state}`"
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
                if let Some((alias, _)) = &self.nodes[a].alias {
                    candidates.push(alias.name.clone());
                }
                up = self.nodes[a].parent;
            }
            for (n, v) in &self.vars {
                if matches!(self.ck.env.expand(&v.ty), Ty::Ref(_)) {
                    candidates.push(n.clone());
                }
            }
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

    /// Reports a `send` to the child invoked by `invoking` from a place where it is not
    /// running: the child starts after the invoking state's `entry`, stops before its `exit`
    /// and has ended when an outcome is taken, and a transition that leaves the invoking state
    /// stops it before its effects run.
    fn check_running(&mut self, place: Place, invoking: usize, name: &Ident) {
        let state = self.nodes[invoking].id.clone();
        let why = match place {
            Place::Entry(n) if n == invoking => Some(format!(
                "in the `entry` of `{state}`, its child has not started yet: it starts once the step that enters `{state}` has settled"
            )),
            Place::Exit(n) if n == invoking => Some(format!(
                "in the `exit` of `{state}`, its child has already been stopped"
            )),
            Place::Transition {
                node,
                outcome: true,
                ..
            } if node == invoking => Some(format!(
                "in an outcome of the invoke of `{state}`, its child has already ended"
            )),
            Place::Transition {
                node,
                target: Some(t),
                ..
            } if !(self.inside(node, invoking) && self.inside(t, invoking)) => Some(format!(
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
                .with_help("send to an invoked child from a transition that stays inside the invoking state, or from the states inside it"),
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
                let (address, invoking) = self.address(current, to)?;
                // A send to a `ref` variable reads it.
                if address.kind == ir::AddressKind::Ref
                    && self.vars.get(&to.name).is_some_and(|v| !v.param)
                {
                    self.reads.push((to.name.clone(), to.span));
                }
                if let Some(i) = invoking {
                    self.check_running(place, i, to);
                }
                let iface = self
                    .ck
                    .interfaces
                    .get(&address.machine)
                    .cloned()
                    .unwrap_or_default();
                let receiver = self.ck.decl_symbol(&address.machine);
                self.ck.refer_member(self.file, receiver, event);
                let fields = iface.events.get(&event.name).cloned();
                if fields.is_none() {
                    self.not_declared(
                        codes::NOT_ACCEPTED,
                        &address.machine,
                        &iface,
                        event,
                        false,
                        &to.name,
                    );
                }
                let ty = Ty::Event(format!("{}.{}", address.machine, event.name));
                let value = self.message_value(m, &ty, fields.as_deref(), scope);
                Some(ir::Effect::Send {
                    to: address,
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
        self.info.states.insert(
            id.clone(),
            StateInfo {
                name_span,
                invoke_span: None,
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
            },
        );

        let mut entry = Vec::new();
        let mut exit = Vec::new();
        let mut invoke = None;
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
                    self.branches(
                        idx,
                        t,
                        trigger,
                        &scope,
                        (group, false),
                        &mut transitions,
                        &mut triggers,
                    );
                    group += 1;
                }
                Item::Invoke(inv) => {
                    let first = transitions.len();
                    let (lowered, reads, declared) =
                        self.invoke(idx, inv, &mut group, &mut transitions, &mut triggers);
                    if invoke.is_none()
                        && let Some(r) = &inv.retry
                    {
                        retry = Some(retry::Plan {
                            inv,
                            retry: r,
                            outcomes: first..transitions.len(),
                            declared,
                        });
                    }
                    if invoke.is_some() {
                        self.ck.diags.push(
                            Diagnostic::error(
                                codes::SECOND_INVOKE,
                                invoke_head(inv),
                                format!("`{id}` already has an `invoke`"),
                            )
                            .with_help("a state runs at most one task or machine; put the second in a state of its own"),
                        );
                    } else {
                        invoke = Some(lowered);
                        invoke_reads = reads;
                        if let Some(info) = self.info.states.get_mut(&id) {
                            info.invoke_span = Some(invoke_head(inv));
                        }
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
                | Item::Emits(_)
                | Item::Initial(_)
                | Item::History(_)
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
        let mut lowered = ir::State {
            name: self.nodes[idx].name.clone(),
            id,
            kind,
            doc: state.map(|s| doc(&s.doc)).unwrap_or_default(),
            span: state
                .map(|s| ir_span(s.span))
                .unwrap_or_else(|| ir_span(name_span)),
            initial,
            states,
            entry,
            exit,
            invoke,
            transitions,
            output,
            fail,
        };
        if let Some(r) = retry {
            self.write_out_retry(&mut lowered, r);
        }
        lowered
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
                    Item::Event(e) | Item::Emits(e) => e.doc.as_ref(),
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
        let key = trigger_key(&t.trigger);
        if let Some(first) = keys.get(&key) {
            let what = if prefix.is_empty() {
                format!("trigger `{key}` is written twice in `{owner}`")
            } else {
                format!("outcome `{key}` is written twice in {prefix}")
            };
            self.ck.diags.push(
                Diagnostic::error(codes::DUPLICATE_TRIGGER, t.trigger_span, what)
                    .with_secondary(*first, "first written here")
                    .with_help(format!(
                        "put every branch in one block, where their order is explicit: `{key} {{ if … -> A  else -> B }}`"
                    )),
            );
        } else {
            keys.insert(key, t.trigger_span);
        }
    }

    /// The IR trigger and the implicit names in scope for an `on`, `after`, `always` or `done`
    /// written in a state or the machine body.
    fn state_trigger(&mut self, idx: usize, t: &Trigger) -> (ir::Trigger, Scope) {
        let mut scope = Scope::none();
        let trigger = match t {
            Trigger::Receive { from, event } => {
                scope.event = Implicit::Has(Ty::Unknown);
                let Some((address, _)) = self.address(idx, from) else {
                    // Reported; the trigger is not lowered further.
                    return (
                        ir::Trigger::Event {
                            event: event.name.clone(),
                        },
                        scope,
                    );
                };
                let sender = self.ck.decl_symbol(&address.machine);
                self.ck.refer_member(self.file, sender, event);
                let iface = self
                    .ck
                    .interfaces
                    .get(&address.machine)
                    .cloned()
                    .unwrap_or_default();
                if iface.emits.contains_key(&event.name) {
                    scope.event =
                        Implicit::Has(Ty::Event(format!("{}.{}", address.machine, event.name)));
                    self.ck
                        .received
                        .insert((address.machine.clone(), event.name.clone()));
                    self.info.receives.push((
                        event.span,
                        address.machine.clone(),
                        event.name.clone(),
                    ));
                } else {
                    let machine = address.machine.clone();
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
                self.ck.refer_member(self.file, self.symbol, n);
                if self.events.contains_key(&n.name) {
                    self.info.used_events.insert(n.name.clone());
                    scope.event = Implicit::Has(Ty::Event(n.name.clone()));
                } else {
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
                    if let Some(s) =
                        suggest(&n.name, self.info.events.iter().map(|(e, _)| e.as_str()))
                    {
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
                    scope.event = Implicit::Has(Ty::Unknown);
                }
                ir::Trigger::Event {
                    event: n.name.clone(),
                }
            }
            Trigger::On(Pattern::Any { .. }) => {
                scope.event = Implicit::Bad(
                    codes::UNTYPED_EVENT,
                    "`event` has no type in `on *`, which matches events of different types".into(),
                );
                ir::Trigger::Any
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
    /// and whether it is an outcome of the state's invoke.
    #[allow(clippy::too_many_arguments)]
    fn branches(
        &mut self,
        idx: usize,
        t: &Transition,
        trigger: ir::Trigger,
        scope: &Scope,
        (group, outcome): (u32, bool),
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
            let target_node = b.target.as_ref().and_then(|tg| self.target(idx, tg));
            let target = target_node.map(|n| self.nodes[n].id.clone());
            let place = Place::Transition {
                node: idx,
                target: target_node,
                outcome,
            };
            let (effects, effect_reads) = self.effects(&b.effects, scope, place);
            let span = match &t.branches {
                Branches::Single(_) => t.span,
                Branches::Block { .. } => b.span,
            };
            self.transition_reads.push(TransitionReads {
                delay: delay.clone(),
                guard: guard_reads,
                effects: effect_reads,
                span,
            });
            out.push(ir::Transition {
                trigger: trigger.clone(),
                group,
                guard,
                target,
                effects,
                span: ir_span(span),
            });
            triggers.push(t.trigger_span);
        }
    }

    /// Resolves a target: the first segment among the siblings of the state the transition is
    /// written in, then among its parent's siblings, and so on outward (for the machine body,
    /// among the top-level states); each further segment names a child of the one before.
    fn target(&mut self, from: usize, t: &Target) -> Option<usize> {
        let head = &t.path[0];
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
        let Some(mut node) = found else {
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
                        "`{}` is a child of `{own}`; a state names its own children through its name: `{own}.{written}`",
                        head.name
                    ))
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
                    self.ck.refer_member(self.file, self.symbol, &a.var);
                    let to = dest(
                        self.vars.get(&a.var.name).is_some_and(|v| v.sensitive),
                        format!("`{}`", a.var.name),
                    );
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
                    let value = self.expect_to(
                        &a.value,
                        scope,
                        &want,
                        &format!("the value assigned to `{}`", a.var.name),
                        to,
                    );
                    out.push(ir::Effect::Set {
                        var: a.var.name.clone(),
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
            }
            if out.len() > before {
                reads.push(std::mem::take(&mut self.reads));
            }
        }
        (out, reads)
    }

    /// "unknown variable", with a suggestion among the variables (and parameters, if
    /// `params`) of the machine, or a note that the name is a declaration of another kind.
    pub(crate) fn unknown_variable(&self, n: &Ident, params: bool) -> Diagnostic {
        if let Found::Decl(id) = self.ck.names.lookup(self.file, &n.name) {
            return self.ck.names.wrong_kind(n, id, "a variable");
        }
        if let Some(state) = self
            .nodes
            .iter()
            .find(|x| x.alias.as_ref().is_some_and(|(a, _)| a.name == n.name))
        {
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
        let mut d = Diagnostic::error(
            codes::UNKNOWN_VARIABLE,
            n.span,
            format!(
                "unknown variable `{}` in machine `{}`",
                n.name, self.machine
            ),
        );
        let mut candidates: Vec<&str> = self
            .vars
            .iter()
            .filter(|(k, v)| (params || !v.param) && self.counter.as_ref() != Some(*k))
            .map(|(k, _)| k.as_str())
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

    /// Checks arguments against a signature (if known) and lowers them.
    pub(crate) fn args(
        &mut self,
        name: &Ident,
        span: Span,
        args: &[ast::Expr],
        sig: Option<&Sig>,
        kind: SigKind,
        scope: &Scope,
    ) -> Vec<ir::Expr> {
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
                    let what = format!("argument `{p}` of `{}`", name.name);
                    if kind == SigKind::Fn {
                        // Function calls are not traced.
                        self.expect(a, scope, &t, &what)
                    } else {
                        let to = dest(
                            self.ck.sensitive.is(&name.name, p),
                            format!("parameter `{p}` of `{}`", name.name),
                        );
                        self.expect_to(a, scope, &t, &what, to)
                    }
                }
                None => self.expr(a, scope).0,
            })
            .collect()
    }

    fn invoke(
        &mut self,
        idx: usize,
        inv: &Invoke,
        group: &mut u32,
        transitions: &mut Vec<ir::Transition>,
        triggers: &mut Vec<Span>,
    ) -> (ir::Invoke, Reads, Option<Vec<String>>) {
        let is_machine = inv.is_machine();
        let sig = self.callee(
            &inv.callee,
            if is_machine {
                SigKind::Machine
            } else {
                SigKind::Task
            },
        );
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
        );
        let arg_reads = std::mem::take(&mut self.reads);

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
        let callee = inv.callee.name.clone();
        let declared_as = |sig: &Sig| {
            let text = sig.result_text();
            if text.is_empty() {
                "declared with no result and no errors".to_string()
            } else {
                format!("declared as `{text}`")
            }
        };

        if let Some(sig) = &sig
            && !has_any
        {
            for e in &sig.errors {
                if !listed.iter().any(|n| n.name == *e) {
                    self.ck.diags.push(
                        Diagnostic::error(
                            codes::UNHANDLED_ERROR,
                            invoke_head(inv),
                            format!("`invoke {callee}` does not handle `{e}`"),
                        )
                        .with_label(declared_as(sig))
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
                    ir::Trigger::InvokeDone
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
                    if let Some(sig) = &sig {
                        if sig.errors.contains(&n.name) {
                            scope.error = Implicit::Has(Ty::Named(n.name.clone()));
                        } else {
                            let mut d = Diagnostic::error(
                                codes::UNDECLARED_ERROR_OUTCOME,
                                n.span,
                                format!("`{callee}` does not declare the error `{}`", n.name),
                            )
                            .with_secondary(sig.name_span, declared_as(sig));
                            let unlisted: Vec<&str> = sig
                                .errors
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
                        error: n.name.clone(),
                    }
                }
                Trigger::Fail(Pattern::Any { .. }) => {
                    scope.error = match &sig {
                        Some(sig) => {
                            let rest: Vec<&String> = sig
                                .errors
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
                    ir::Trigger::InvokeFail { error: "*".into() }
                }
                // The parser accepts only `done` and `fail` here.
                _ => ir::Trigger::InvokeDone,
            };
            self.branches(
                idx,
                o,
                trigger,
                &scope,
                (*group, true),
                transitions,
                triggers,
            );
            *group += 1;
        }

        let lowered = ir::Invoke {
            kind: if is_machine {
                ir::InvokeKind::Machine
            } else {
                ir::InvokeKind::Task
            },
            src: callee,
            alias: inv
                .alias
                .as_ref()
                .filter(|_| is_machine)
                .map(|a| a.name.clone()),
            args,
            span: ir_span(inv.span),
        };
        (lowered, arg_reads, sig.map(|s| s.errors))
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
