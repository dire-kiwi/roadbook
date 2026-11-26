//! Machines: variables, events, the state tree, targets, invokes and transitions, checked
//! and lowered to the IR.

use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;
use roadbook_ir as ir;
use roadbook_syntax::ast::{
    self, Body, Branches, Decl, DeclKind, Effect, Ident, Invoke, Item, Pattern, State, StateKind,
    Target, Transition, Trigger,
};
use roadbook_syntax::diagnostic::Code;
use roadbook_syntax::{Diagnostic, Span, expr_text};

use crate::check::{Checker, Sig, SigKind, doc, ir_span};
use crate::codes;
use crate::names::{Found, is_type, kind_phrase, suggest};
use crate::types::{BOOL, DURATION, Ty, ir_type};

/// Source positions of a state's parts, kept for the analyses that run on the IR.
#[derive(Clone, Debug)]
pub(crate) struct StateInfo {
    pub name_span: Span,
    /// `invoke name`, if the state has an invoke.
    pub invoke_span: Option<Span>,
    /// The trigger of each of the state's IR transitions, by index.
    pub triggers: Vec<Span>,
}

#[derive(Clone, Debug)]
pub(crate) struct MachineInfo {
    pub name: String,
    pub file: usize,
    pub states: HashMap<String, StateInfo>,
    /// Declared events with their name spans, in order.
    pub events: Vec<(String, Span)>,
    pub used_events: HashSet<String>,
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
    children: Vec<usize>,
    body: &'p Body,
    /// `None` for the machine's root.
    state: Option<&'p State>,
    depth: usize,
}

pub(crate) struct Var {
    pub ty: Ty,
    pub param: bool,
}

pub(crate) struct Cx<'c, 'p> {
    pub ck: &'c mut Checker<'p>,
    pub file: usize,
    pub machine: String,
    pub vars: HashMap<String, Var>,
    pub events: HashMap<String, Vec<(String, Ty)>>,
    returns: Option<Ty>,
    errors: Vec<String>,
    nodes: Vec<Node<'p>>,
    info: MachineInfo,
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
    let mut cx = Cx {
        ck,
        file,
        machine: name.clone(),
        vars: HashMap::new(),
        events: HashMap::new(),
        returns: sig.as_ref().and_then(|s| s.returns.clone()),
        errors: sig.as_ref().map(|s| s.errors.clone()).unwrap_or_default(),
        nodes: Vec::new(),
        info: MachineInfo {
            name: name.clone(),
            file,
            states: HashMap::new(),
            events: Vec::new(),
            used_events: HashSet::new(),
        },
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
                },
            );
        }
        var_tys.push(ty);
    }

    // Events.
    let mut events = IndexMap::new();
    for e in m.body.events() {
        if events.contains_key(&e.name.name) {
            let first = cx.info.events.iter().find(|(n, _)| *n == e.name.name);
            let mut diag = Diagnostic::error(
                codes::DUPLICATE_NAME,
                e.name.span,
                format!("event `{}` is declared twice", e.name.name),
            );
            if let Some((_, s)) = first {
                diag = diag.with_secondary(*s, "first declared here");
            }
            cx.ck.diags.push(diag);
            continue;
        }
        if let Some(&id) = cx.ck.names.global.get(e.name.name.as_str()) {
            let other = cx.ck.names.decl(id);
            if is_type(&other.kind) || matches!(other.kind, DeclKind::Machine(_)) {
                let path = &cx.ck.names.project.files[cx.ck.names.decls[id.0].file].path;
                cx.ck.diags.push(
                    Diagnostic::error(
                        codes::DUPLICATE_DECLARATION,
                        e.name.span,
                        format!(
                            "event `{}` has the name of {} declared in `{path}`",
                            e.name.name,
                            kind_phrase(&other.kind)
                        ),
                    )
                    .with_secondary(other.name.span, "declared here")
                    .with_help("the IR names an event's type by the event's name, so it must not be the name of a type, error or machine; rename one of them"),
                );
            }
        }
        cx.ck.unique(e.fields.iter().map(|f| &f.name), "field");
        let fields: Vec<(String, Ty)> = e
            .fields
            .iter()
            .map(|f| (f.name.name.clone(), cx.ck.resolve(file, &f.ty)))
            .collect();
        cx.events.insert(e.name.name.clone(), fields);
        cx.info.events.push((e.name.name.clone(), e.name.span));
        events.insert(
            e.name.name.clone(),
            ir::EventDecl {
                fields: e
                    .fields
                    .iter()
                    .map(|f| ir::Field {
                        name: f.name.name.clone(),
                        ty: ir_type(&f.ty),
                        doc: String::new(),
                    })
                    .collect(),
                doc: doc(&e.doc),
                span: ir_span(e.span),
            },
        );
    }

    // Variable initialisers may read parameters and other variables.
    let mut vars = Vec::new();
    for (v, ty) in m.body.vars().zip(var_tys) {
        let init = v.init.as_ref().map(|e| {
            cx.expect(
                e,
                &Scope::none(),
                &ty,
                &format!("the initial value of `{}`", v.name.name),
            )
        });
        vars.push(ir::Var {
            name: v.name.name.clone(),
            ty: ir_type(&v.ty),
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
        body: &m.body,
        state: None,
        depth: 0,
    });
    cx.build_tree(0);
    let mut root = cx.lower_state(0);
    root.doc = doc(&d.doc);
    root.span = ir_span(d.span);

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
                doc: String::new(),
            })
            .collect(),
        returns: m.returns.as_ref().map(ir_type),
        errors: m.errors.iter().map(|e| e.name.clone()).collect(),
        vars,
        events,
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
        for s in body.states() {
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
            self.nodes.push(Node {
                name: s.name.name.clone(),
                name_span: s.name.span,
                id,
                parent: Some(parent),
                children: Vec::new(),
                body: &s.body,
                state: Some(s),
                depth: self.nodes[parent].depth + 1,
            });
            self.nodes[parent].children.push(idx);
            self.build_tree(idx);
        }
    }

    fn child(&self, node: usize, name: &str) -> Option<usize> {
        self.nodes[node]
            .children
            .iter()
            .copied()
            .find(|&c| self.nodes[c].name == name)
    }

    fn lower_state(&mut self, idx: usize) -> ir::State {
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
        let mut group = 0u32;
        let top_final = kind == ir::StateKind::Final && self.nodes[idx].depth == 1;

        for item in &body.items {
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
            match item {
                Item::Entry(h) => entry.extend(self.effects(&h.effects, &Scope::none())),
                Item::Exit(h) => exit.extend(self.effects(&h.effects, &Scope::none())),
                Item::Transition(t) => {
                    self.duplicate_trigger(&mut keys, t, &id, "");
                    let (trigger, scope) = self.state_trigger(&t.trigger);
                    self.branches(
                        idx,
                        t,
                        trigger,
                        &scope,
                        group,
                        &mut transitions,
                        &mut triggers,
                    );
                    group += 1;
                }
                Item::Invoke(inv) => {
                    let lowered =
                        self.invoke(idx, inv, &mut group, &mut transitions, &mut triggers);
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
                    result_seen.get_or_insert(v.span);
                    if is_output {
                        let e = self.output(&v.value);
                        output.get_or_insert(e);
                    } else {
                        let e = self.fail_value(&v.value);
                        fail.get_or_insert(e);
                    }
                }
                Item::Var(_) | Item::Event(_) | Item::Initial(_) | Item::State(_) => {}
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

        if let Some(info) = self.info.states.get_mut(&id) {
            info.triggers = triggers;
        }
        let children = self.nodes[idx].children.clone();
        let states = children.into_iter().map(|c| self.lower_state(c)).collect();
        ir::State {
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
        }
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
                    Item::Var(v) => v.doc.as_ref(),
                    Item::Event(e) => e.doc.as_ref(),
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
            Some(c) => Some(self.nodes[c].id.clone()),
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
    fn state_trigger(&mut self, t: &Trigger) -> (ir::Trigger, Scope) {
        let mut scope = Scope::none();
        let trigger = match t {
            Trigger::On(Pattern::Name(n)) => {
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
                delay: self.expect(e, &Scope::none(), &DURATION, "an `after` delay"),
            },
            Trigger::Always => ir::Trigger::Always,
            // `fail` outcomes only occur inside an invoke; the parser rejects them elsewhere.
            Trigger::Done | Trigger::Fail(_) => ir::Trigger::Done,
        };
        (trigger, scope)
    }

    #[allow(clippy::too_many_arguments)]
    fn branches(
        &mut self,
        idx: usize,
        t: &Transition,
        trigger: ir::Trigger,
        scope: &Scope,
        group: u32,
        out: &mut Vec<ir::Transition>,
        triggers: &mut Vec<Span>,
    ) {
        for b in t.branches.iter() {
            let guard = b
                .guard
                .as_ref()
                .map(|g| self.expect(g, scope, &BOOL, "a guard"));
            let target = b.target.as_ref().and_then(|tg| self.target(idx, tg));
            let effects = self.effects(&b.effects, scope);
            let span = match &t.branches {
                Branches::Single(_) => t.span,
                Branches::Block { .. } => b.span,
            };
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
    fn target(&mut self, from: usize, t: &Target) -> Option<String> {
        let head = &t.path[0];
        let mut scope = self.nodes[from].parent.unwrap_or(from);
        let mut found = None;
        let mut visible: Vec<String> = Vec::new();
        loop {
            if let Some(c) = self.child(scope, &head.name) {
                found = Some(c);
                break;
            }
            for &c in &self.nodes[scope].children {
                if !visible.contains(&self.nodes[c].name) {
                    visible.push(self.nodes[c].name.clone());
                }
            }
            match self.nodes[scope].parent {
                Some(p) => scope = p,
                None => break,
            }
        }
        let written: Vec<&str> = t.path.iter().map(|p| p.name.as_str()).collect();
        let written = written.join(".");
        let Some(mut node) = found else {
            let mut d = Diagnostic::error(
                codes::UNKNOWN_TARGET,
                head.span,
                format!("unknown target state `{}`", head.name),
            );
            if self.child(from, &head.name).is_some() && self.nodes[from].parent.is_some() {
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
            match self.child(node, &seg.name) {
                Some(c) => node = c,
                None => {
                    let parent = self.nodes[node].id.clone();
                    let children: Vec<String> = self.nodes[node]
                        .children
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
        Some(self.nodes[node].id.clone())
    }

    pub(crate) fn effects(&mut self, effects: &[Effect], scope: &Scope) -> Vec<ir::Effect> {
        let mut out = Vec::new();
        for e in effects {
            match e {
                Effect::Set(a) => {
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
                    let value = self.expect(
                        &a.value,
                        scope,
                        &want,
                        &format!("the value assigned to `{}`", a.var.name),
                    );
                    out.push(ir::Effect::Set {
                        var: a.var.name.clone(),
                        value,
                    });
                }
                Effect::Do(c) => {
                    let sig = self.callee(&c.name, SigKind::Action);
                    let args = self.args(&c.name, c.span, &c.args, sig.as_ref(), scope);
                    out.push(ir::Effect::Do {
                        action: c.name.name.clone(),
                        args,
                    });
                }
            }
        }
        out
    }

    /// "unknown variable", with a suggestion among the variables (and parameters, if
    /// `params`) of the machine, or a note that the name is a declaration of another kind.
    pub(crate) fn unknown_variable(&self, n: &Ident, params: bool) -> Diagnostic {
        if let Found::Decl(id) = self.ck.names.lookup(self.file, &n.name) {
            return self.ck.names.wrong_kind(n, id, "a variable");
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
            .filter(|(_, v)| params || !v.param)
            .map(|(k, _)| k.as_str())
            .collect();
        candidates.sort_unstable();
        if let Some(s) = suggest(&n.name, candidates.into_iter()) {
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
                    self.expect(a, scope, &t, &format!("argument `{p}` of `{}`", name.name))
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
    ) -> ir::Invoke {
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
            &Scope::none(),
        );

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
            self.branches(idx, o, trigger, &scope, *group, transitions, triggers);
            *group += 1;
        }

        ir::Invoke {
            kind: if is_machine {
                ir::InvokeKind::Machine
            } else {
                ir::InvokeKind::Task
            },
            src: callee,
            args,
            span: ir_span(inv.span),
        }
    }

    fn output(&mut self, e: &ast::Expr) -> ir::Expr {
        match self.returns.clone() {
            Some(t) => {
                let what = format!("the output of `{}`", self.machine);
                self.expect(e, &Scope::none(), &t, &what)
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
