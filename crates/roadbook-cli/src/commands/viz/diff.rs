//! The diff model of `roadbook viz --diff`: two versions of an entry chart's IR, drawn as one.
//!
//! [`diff`] takes the IR of a chart before and after (either may be missing: the chart was added
//! or removed) and gives one merged IR, which the viewer draws as any other, and what changed in
//! it: each machine, state and transition is added, removed, changed or unchanged, and a changed
//! one lists its changes with the text before and after.
//!
//! **Matching.** Machines are matched by name (the caller, `changes.rs`, puts the machine
//! each is matched with across the whole project into the before version; one declared in
//! another file than before has the change `file`, and is moved), states by id (the path of names from the
//! machine: `Checkout.Charging`), so a state renamed or moved to another parent is a removed
//! state and an added one. A transition is matched by its source state and its trigger (the
//! event, the sender and event of an emitted one, the error of a `fail`; `after` by the keyword
//! alone, so a changed delay is a change of the trigger), the n-th block with that trigger in
//! the state, and then, among the branches of a guarded block, first by the guard's text and
//! then by position. Everything else is compared by its canonical text (as the formatter writes
//! it), so formatting, comments and source positions never count as changes.
//!
//! **The merged IR.** It is the after version with what was removed put back: a removed state
//! (with everything inside it) goes where it was among its siblings, a removed transition after
//! the state's other transitions, and declarations and machines only the before version has are
//! added. Spans of what comes from the before version point into copies of its source files
//! named `path @ <revision>`, appended to `sources`, so the details panel names the line in the
//! version it is from.

use std::collections::{HashMap, HashSet};

use roadbook_compiler::Ir;
use roadbook_compiler::statespace::plain_trigger_text;
use roadbook_ir::{
    Effect, Invoke, InvokeKind, Machine, Source, State, StateKind, Transition, Trigger, TypeDef,
};
use serde_json::{Map, Value, json};

use super::page;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Added,
    Removed,
    Changed,
    Unchanged,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Added => "added",
            Status::Removed => "removed",
            Status::Changed => "changed",
            Status::Unchanged => "unchanged",
        }
    }
}

/// One difference: what it is about (`guard`, `target`, `var attempts`), and its text before and
/// after (`None`: there was none).
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub what: String,
    pub before: Option<String>,
    pub after: Option<String>,
}

/// A state, transition or declaration that is not unchanged.
#[derive(Clone, Debug, PartialEq)]
pub struct Element {
    pub status: Status,
    /// For a changed one, what changed.
    pub changes: Vec<Change>,
}

/// Added, removed and changed, of one kind of thing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub added: usize,
    pub removed: usize,
    pub changed: usize,
}

impl Counts {
    fn count(&mut self, s: Status) {
        match s {
            Status::Added => self.added += 1,
            Status::Removed => self.removed += 1,
            Status::Changed => self.changed += 1,
            Status::Unchanged => {}
        }
    }

    pub fn add(&mut self, o: Counts) {
        self.added += o.added;
        self.removed += o.removed;
        self.changed += o.changed;
    }

    pub fn is_empty(&self) -> bool {
        self.added + self.removed + self.changed == 0
    }

    pub fn to_json(self) -> Value {
        json!({ "added": self.added, "removed": self.removed, "changed": self.changed })
    }
}

#[derive(Clone, Debug)]
pub struct MachineDiff {
    pub status: Status,
    /// Changes to the machine itself: its signature, doc comment, variables, events and calls, the
    /// file it is declared in, and what its body has as a state would (`initial`, `entry`,
    /// `exit`). Its transitions are with the others, under the machine's id.
    pub changes: Vec<Change>,
    /// The file it was declared in and is declared in now, if they differ.
    pub moved: Option<(String, String)>,
    /// The states that are not unchanged, by id, in document order of the merged machine.
    pub states: Vec<(String, Element)>,
    /// The transitions that are not unchanged, by source state id and index in the merged
    /// state's `transitions`, in document order.
    pub transitions: Vec<((String, usize), Element)>,
    /// The ids among `states` of history states (in either version): counted with the states,
    /// and again on their own.
    pub history: HashSet<String>,
}

impl MachineDiff {
    /// Whether it changed only by moving to another file.
    pub fn only_moved(&self) -> bool {
        self.moved.is_some()
            && self.changes.len() == 1
            && self.states.is_empty()
            && self.transitions.is_empty()
    }

    pub fn state_counts(&self) -> Counts {
        let mut c = Counts::default();
        for (_, e) in &self.states {
            c.count(e.status);
        }
        c
    }

    /// The history states among [`state_counts`](Self::state_counts).
    pub fn history_counts(&self) -> Counts {
        let mut c = Counts::default();
        for (id, e) in &self.states {
            if self.history.contains(id) {
                c.count(e.status);
            }
        }
        c
    }

    pub fn transition_counts(&self) -> Counts {
        let mut c = Counts::default();
        for (_, e) in &self.transitions {
            c.count(e.status);
        }
        c
    }
}

pub struct ChartDiff {
    /// The merged IR.
    pub ir: Ir,
    /// Every machine of the merged IR, in its order.
    pub machines: Vec<(String, MachineDiff)>,
    /// The declarations (types, functions, actions, tasks) that are not unchanged.
    pub decls: Vec<(String, Element)>,
}

impl ChartDiff {
    /// Whether anything differs.
    pub fn changed(&self) -> bool {
        !self.decls.is_empty()
            || self
                .machines
                .iter()
                .any(|(_, m)| m.status != Status::Unchanged)
    }

    /// Machines, states and transitions added, removed and changed.
    #[cfg(test)]
    pub fn counts(&self) -> (Counts, Counts, Counts) {
        let (mut m, mut s, mut t) = (Counts::default(), Counts::default(), Counts::default());
        for (_, d) in &self.machines {
            m.count(d.status);
            s.add(d.state_counts());
            t.add(d.transition_counts());
        }
        (m, s, t)
    }

    /// The `diff` member of the page data (`ChartDiff` in `packages/viewer/src/diff.ts`), less
    /// the members the caller adds: `from`, `to`, `status` and `errors`.
    pub fn to_json(&self) -> Map<String, Value> {
        let element = |e: &Element| {
            json!({
                "status": e.status.as_str(),
                "changes": e.changes.iter().map(change_json).collect::<Vec<_>>(),
            })
        };
        let mut machines = Map::new();
        for (name, d) in &self.machines {
            let mut states = Map::new();
            for (id, e) in &d.states {
                states.insert(id.clone(), element(e));
            }
            let mut transitions = Map::new();
            for ((id, i), e) in &d.transitions {
                transitions.insert(format!("{id}#{i}"), element(e));
            }
            machines.insert(
                name.clone(),
                json!({
                    "status": d.status.as_str(),
                    "changes": d.changes.iter().map(change_json).collect::<Vec<_>>(),
                    "states": states,
                    "transitions": transitions,
                    "counts": {
                        "states": d.state_counts().to_json(),
                        "transitions": d.transition_counts().to_json(),
                        "history": d.history_counts().to_json(),
                    },
                }),
            );
        }
        let mut out = Map::new();
        out.insert(
            "decls".into(),
            Value::Array(
                self.decls
                    .iter()
                    .map(|(n, e)| {
                        let mut v = element(e);
                        v["name"] = Value::from(n.as_str());
                        v
                    })
                    .collect(),
            ),
        );
        out.insert("machines".into(), Value::Object(machines));
        out
    }
}

fn change_json(c: &Change) -> Value {
    json!({ "what": c.what, "before": c.before, "after": c.after })
}

/// The diff of two versions of a chart. `before_label` names the before version in the source
/// paths of what only it has (`examples/shop/checkout.road @ main`).
pub fn diff(before: Option<&Ir>, after: Option<&Ir>, before_label: &str) -> ChartDiff {
    let empty = empty_ir();
    let b_ir = before.unwrap_or(&empty);
    let a_ir = after.unwrap_or(&empty);
    let mut ir = a_ir.clone();
    // What comes from the before version points into copies of its sources.
    let offset = ir.sources.len() as u32;
    for s in &b_ir.sources {
        ir.sources.push(Source {
            path: format!("{} @ {before_label}", s.path),
            package: s.package.clone(),
        });
    }
    let shift = |span: &mut roadbook_ir::Span| span.file += offset;

    let mut decls = Vec::new();
    // The built-in enum `Ended` is in `types` whenever a chart names it: it comes and goes with
    // its uses, which the machines' changes show, and is never a declaration added or removed.
    let builtin = |name: &str| name == "Ended";
    macro_rules! decls {
        ($map:ident, $text:expr) => {
            for (name, a) in &a_ir.$map {
                match b_ir.$map.get(name) {
                    None if builtin(name) => {}
                    None => decls.push((name.clone(), added())),
                    Some(b) => {
                        let (tb, ta) = ($text(name, b), $text(name, a));
                        let mut changes = Vec::new();
                        if tb != ta {
                            changes.push(change("declaration", tb, ta));
                        }
                        if b.doc != a.doc {
                            changes.push(change("doc", b.doc.clone(), a.doc.clone()));
                        }
                        if !changes.is_empty() {
                            decls.push((name.clone(), changed(changes)));
                        }
                    }
                }
            }
            for (name, b) in &b_ir.$map {
                if !a_ir.$map.contains_key(name) {
                    let mut b = b.clone();
                    shift(&mut b.span);
                    ir.$map.insert(name.clone(), b);
                    if !builtin(name) {
                        decls.push((name.clone(), removed()));
                    }
                }
            }
        };
    }
    decls!(types, type_text);
    decls!(functions, |n: &str, f: &roadbook_ir::FunctionDecl| format!(
        "fn {n}({}) -> {}",
        fields_text(&f.params),
        f.returns
    ));
    decls!(actions, |n: &str, f: &roadbook_ir::ActionDecl| format!(
        "action {n}({})",
        fields_text(&f.params)
    ));
    decls!(tasks, task_text);

    let mut machines = Vec::new();
    let names: Vec<String> = a_ir.machines.keys().cloned().collect();
    for name in &names {
        let a = &a_ir.machines[name];
        let mut d = MachineDiff {
            status: Status::Unchanged,
            changes: Vec::new(),
            moved: None,
            states: Vec::new(),
            transitions: Vec::new(),
            history: HashSet::new(),
        };
        let merged = match b_ir.machines.get(name) {
            None => {
                d.status = Status::Added;
                mark_all(&a.root, Status::Added, &mut d);
                d.states.retain(|(id, _)| id != name); // the machine, not one of its states
                a.root.clone()
            }
            Some(b) => {
                let (fb, fa) = (
                    source_text(b_ir, b.span.file),
                    source_text(a_ir, a.span.file),
                );
                if fb != fa {
                    d.changes.push(change("file", fb.clone(), fa.clone()));
                    d.moved = Some((fb, fa));
                }
                d.changes.extend(machine_changes(name, b, a));
                let mut m = Merge {
                    d: &mut d,
                    offset,
                    root: &a.root.id,
                };
                let root = m.state(&a.root, Some(&b.root));
                if !d.changes.is_empty() || !d.states.is_empty() || !d.transitions.is_empty() {
                    d.status = Status::Changed;
                }
                root
            }
        };
        if let Some(m) = ir.machines.get_mut(name) {
            m.root = merged;
        }
        machines.push((name.clone(), d));
    }
    for (name, b) in &b_ir.machines {
        if a_ir.machines.contains_key(name) {
            continue;
        }
        let mut d = MachineDiff {
            status: Status::Removed,
            changes: Vec::new(),
            moved: None,
            states: Vec::new(),
            transitions: Vec::new(),
            history: HashSet::new(),
        };
        mark_all(&b.root, Status::Removed, &mut d);
        d.states.retain(|(id, _)| id != name);
        let mut m = b.clone();
        shift_machine(&mut m, offset);
        ir.machines.insert(name.clone(), m);
        machines.push((name.clone(), d));
    }
    // Every element in document order of the merged machine.
    for (name, d) in &mut machines {
        let order = document_order(&ir.machines[name.as_str()].root);
        d.states
            .sort_by_key(|(id, _)| order.get(id.as_str()).copied());
        d.transitions
            .sort_by_key(|((id, i), _)| (order.get(id.as_str()).copied(), *i));
    }
    ChartDiff {
        ir,
        machines,
        decls,
    }
}

fn document_order(root: &State) -> HashMap<&str, usize> {
    fn go<'a>(s: &'a State, out: &mut HashMap<&'a str, usize>) {
        let n = out.len();
        out.insert(&s.id, n);
        for c in &s.states {
            go(c, out);
        }
    }
    let mut out = HashMap::new();
    go(root, &mut out);
    out
}

/// A source file as the summary names it: `checkout.road`, `inventory/inventory.road` for a
/// file of the package `inventory`.
pub fn source_text(ir: &Ir, file: u32) -> String {
    match ir.sources.get(file as usize) {
        Some(Source {
            path,
            package: Some(p),
        }) => format!("{p}/{path}"),
        Some(s) => s.path.clone(),
        None => String::new(),
    }
}

/// Points every span of `m` at the sources `offset` further on, for a machine moved into an
/// IR whose sources have others before its own.
pub fn shift_machine(m: &mut Machine, offset: u32) {
    m.span.file += offset;
    for v in &mut m.vars {
        v.span.file += offset;
    }
    for e in m.events.values_mut().chain(m.emits.values_mut()) {
        e.span.file += offset;
    }
    for c in m.calls.values_mut() {
        c.span.file += offset;
    }
    for c in m.children.values_mut() {
        c.span.file += offset;
    }
    shift_state(&mut m.root, offset);
}

/// The machines of `ir` that use the declaration `name` (a type, function, action or task):
/// name it in a type, an invoke, a `do`, an error or an expression, or use a declaration that
/// does, in the IR's order.
pub fn users(ir: &Ir, name: &str) -> Vec<String> {
    let json = |x: Result<Value, serde_json::Error>| names_in(&x.unwrap_or(Value::Null));
    // The declarations that use it, directly or through one another.
    let mut via: HashSet<String> = HashSet::from([name.to_string()]);
    let decls: Vec<(&String, HashSet<String>)> = ir
        .types
        .iter()
        .map(|(n, d)| (n, json(serde_json::to_value(d))))
        .chain(
            ir.functions
                .iter()
                .map(|(n, d)| (n, json(serde_json::to_value(d)))),
        )
        .chain(
            ir.actions
                .iter()
                .map(|(n, d)| (n, json(serde_json::to_value(d)))),
        )
        .chain(
            ir.tasks
                .iter()
                .map(|(n, d)| (n, json(serde_json::to_value(d)))),
        )
        .collect();
    loop {
        let known = via.len();
        for (n, names) in &decls {
            if !via.contains(n.as_str()) && names.iter().any(|x| via.contains(x)) {
                via.insert(n.to_string());
            }
        }
        if via.len() == known {
            break;
        }
    }
    ir.machines
        .iter()
        .filter(|(_, m)| {
            json(serde_json::to_value(m))
                .iter()
                .any(|x| via.contains(x))
        })
        .map(|(n, _)| n.clone())
        .collect()
}

/// The identifiers in the text of an IR value that can name a declaration: those in its
/// types, expressions, invokes, actions and errors, not in state names, targets, event names,
/// the call of an `on c` or a `reply`, or doc comments.
fn names_in(v: &Value) -> HashSet<String> {
    const NOT: &[&str] = &[
        "id", "name", "doc", "target", "initial", "hash", "event", "call", "kind", "span", "path",
    ];
    fn walk(v: &Value, out: &mut HashSet<String>) {
        match v {
            Value::String(s) => {
                for w in s.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
                    if !w.is_empty() {
                        out.insert(w.to_string());
                    }
                }
            }
            Value::Array(list) => list.iter().for_each(|x| walk(x, out)),
            Value::Object(map) => {
                for (k, x) in map {
                    if !NOT.contains(&k.as_str()) {
                        walk(x, out);
                    }
                }
            }
            _ => {}
        }
    }
    let mut out = HashSet::new();
    walk(v, &mut out);
    out
}

pub fn empty_ir() -> Ir {
    Ir {
        ir_version: roadbook_ir::IR_VERSION.to_string(),
        sources: Vec::new(),
        types: Default::default(),
        functions: Default::default(),
        actions: Default::default(),
        tasks: Default::default(),
        machines: Default::default(),
    }
}

fn added() -> Element {
    Element {
        status: Status::Added,
        changes: Vec::new(),
    }
}

fn removed() -> Element {
    Element {
        status: Status::Removed,
        changes: Vec::new(),
    }
}

fn changed(changes: Vec<Change>) -> Element {
    Element {
        status: Status::Changed,
        changes,
    }
}

fn change(
    what: &str,
    before: impl Into<Option<String>>,
    after: impl Into<Option<String>>,
) -> Change {
    Change {
        what: what.to_string(),
        before: before.into(),
        after: after.into(),
    }
}

/// Every state and transition of `s` and inside it, with status `status`.
fn mark_all(s: &State, status: Status, d: &mut MachineDiff) {
    if s.kind.is_history() {
        d.history.insert(s.id.clone());
    }
    d.states.push((
        s.id.clone(),
        Element {
            status,
            changes: Vec::new(),
        },
    ));
    for i in 0..s.transitions.len() {
        d.transitions.push((
            (s.id.clone(), i),
            Element {
                status,
                changes: Vec::new(),
            },
        ));
    }
    for c in &s.states {
        mark_all(c, status, d);
    }
}

fn shift_state(s: &mut State, offset: u32) {
    s.span.file += offset;
    for inv in &mut s.invokes {
        inv.span.file += offset;
    }
    for t in &mut s.transitions {
        t.span.file += offset;
    }
    for c in &mut s.states {
        shift_state(c, offset);
    }
}

struct Merge<'a> {
    d: &'a mut MachineDiff,
    offset: u32,
    /// The machine's root state's id: what differs in it is the machine's.
    root: &'a str,
}

impl Merge<'_> {
    /// The merged state of `a` (after) and `b` (before, with the same id), recording what differs.
    fn state(&mut self, a: &State, b: Option<&State>) -> State {
        let Some(b) = b else {
            mark_all(a, Status::Added, self.d);
            return a.clone();
        };
        let changes = state_changes(a, b);
        if a.id == self.root {
            // The machine's body: its doc comment is the machine's, compared already.
            self.d
                .changes
                .extend(changes.into_iter().filter(|c| c.what != "doc"));
        } else if !changes.is_empty() {
            // `history` to `deep history` is a change of kind, as any.
            if a.kind.is_history() || b.kind.is_history() {
                self.d.history.insert(a.id.clone());
            }
            self.d.states.push((a.id.clone(), changed(changes)));
        }
        let mut out = a.clone();
        // Children: the after version's, merged, with the removed ones put back where they were.
        let mut states: Vec<State> = a
            .states
            .iter()
            .map(|c| self.state(c, b.states.iter().find(|x| x.id == c.id)))
            .collect();
        let mut previous: Option<&str> = None;
        for bc in &b.states {
            if a.states.iter().any(|x| x.id == bc.id) {
                previous = Some(&bc.id);
                continue;
            }
            mark_all(bc, Status::Removed, self.d);
            let mut r = bc.clone();
            shift_state(&mut r, self.offset);
            let at = previous
                .and_then(|p| states.iter().position(|s| s.id == p))
                .map_or(0, |i| i + 1);
            previous = Some(&bc.id);
            states.insert(at, r);
        }
        if !states.is_empty() && out.states.is_empty() {
            // Every child was removed: draw it as the container it was.
            if matches!(out.kind, StateKind::Atomic | StateKind::Final) {
                out.kind = b.kind;
            }
            if out.initial.is_none() {
                out.initial = b.initial.clone();
            }
        }
        out.states = states;
        out.transitions = self.transitions(a, b);
        out
    }

    /// The merged transitions of a state: the after version's, then the removed ones.
    fn transitions(&mut self, a: &State, b: &State) -> Vec<Transition> {
        let ab = blocks(&a.transitions);
        let bb = blocks(&b.transitions);
        let mut pairs: Vec<(usize, usize)> = Vec::new(); // (after index, before index)
        let mut block_of_before: HashMap<usize, u32> = HashMap::new(); // before index -> after group
        let mut used_blocks: HashSet<usize> = HashSet::new();
        for blk in &ab {
            let Some((bi, bblk)) = bb
                .iter()
                .enumerate()
                .find(|(i, x)| !used_blocks.contains(i) && x.key == blk.key && x.nth == blk.nth)
            else {
                continue;
            };
            used_blocks.insert(bi);
            let group = a.transitions[blk.members[0]].group;
            for &m in &bblk.members {
                block_of_before.insert(m, group);
            }
            // Branches: by guard text first, then by position.
            let guard = |t: &Transition| t.guard.as_ref().map(|g| g.text.clone());
            let mut free_a: Vec<usize> = blk.members.clone();
            let mut free_b: Vec<usize> = bblk.members.clone();
            free_a.retain(|&ai| {
                let g = guard(&a.transitions[ai]);
                match free_b.iter().position(|&bi| guard(&b.transitions[bi]) == g) {
                    Some(p) => {
                        pairs.push((ai, free_b.remove(p)));
                        false
                    }
                    None => true,
                }
            });
            for (ai, bi) in free_a.into_iter().zip(free_b) {
                pairs.push((ai, bi));
            }
        }
        let mut out = a.transitions.clone();
        for i in 0..a.transitions.len() {
            match pairs.iter().find(|(ai, _)| *ai == i) {
                None => self.d.transitions.push(((a.id.clone(), i), added())),
                Some(&(_, bi)) => {
                    let changes = transition_changes(a, i, b, bi);
                    if !changes.is_empty() {
                        self.d
                            .transitions
                            .push(((a.id.clone(), i), changed(changes)));
                    }
                }
            }
        }
        // Removed transitions: in a matched block they join its group, else a group of their own.
        let mut next_group = a.transitions.iter().map(|t| t.group + 1).max().unwrap_or(0);
        let mut fresh: HashMap<u32, u32> = HashMap::new();
        for (bi, t) in b.transitions.iter().enumerate() {
            if pairs.iter().any(|(_, x)| *x == bi) {
                continue;
            }
            let mut r = t.clone();
            r.span.file += self.offset;
            r.group = match block_of_before.get(&bi) {
                Some(&g) => g,
                None => *fresh.entry(t.group).or_insert_with(|| {
                    next_group += 1;
                    next_group - 1
                }),
            };
            self.d
                .transitions
                .push(((a.id.clone(), out.len()), removed()));
            out.push(r);
        }
        out
    }
}

/// A block of transitions (the branches of one trigger), and which of its kind it is in the state.
struct Block {
    key: String,
    nth: usize,
    members: Vec<usize>,
}

fn blocks(ts: &[Transition]) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::new();
    let mut groups: HashMap<u32, usize> = HashMap::new();
    for (i, t) in ts.iter().enumerate() {
        if let Some(&b) = groups.get(&t.group) {
            out[b].members.push(i);
            continue;
        }
        let key = trigger_key(&t.trigger);
        let nth = out.iter().filter(|b| b.key == key).count();
        groups.insert(t.group, out.len());
        out.push(Block {
            key,
            nth,
            members: vec![i],
        });
    }
    out
}

/// What identifies a trigger for matching: `after` without its delay, and an invoke's outcome
/// with the `as` of its invoke, so that the outcomes of a state's several invokes stay apart.
fn trigger_key(t: &Trigger) -> String {
    match t {
        Trigger::After { .. } => "after".into(),
        Trigger::InvokeDone { alias: Some(a) } | Trigger::InvokeFail { alias: Some(a), .. } => {
            format!("{a}: {}", plain_trigger_text(t))
        }
        other => plain_trigger_text(other),
    }
}

/// The trigger of state `s` as the viewer labels it (`triggerText` in
/// `packages/viewer/src/model.ts`).
pub fn trigger_text(s: &State, t: &Trigger) -> String {
    roadbook_compiler::statespace::trigger_text(s, t)
}

/// `effectText` in `packages/viewer/src/model.ts`.
pub fn effect_text(e: &Effect) -> String {
    match e {
        Effect::Set { var, value } => format!("set {var} = {}", value.text),
        Effect::Do { action, args } => format!(
            "do {action}({})",
            args.iter()
                .map(|a| a.text.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Effect::Send { to, key, value, .. } => {
            let which = match (to.kind, key) {
                (roadbook_ir::AddressKind::Children, Some(k)) => format!("[{}]", k.text),
                (roadbook_ir::AddressKind::Children, None) => ".*".into(),
                _ => String::new(),
            };
            format!("send {}{which}.{}", to.name, value.text)
        }
        Effect::Emit { value, .. } => format!("emit {}", value.text),
        Effect::Reply {
            call,
            outcome,
            value,
        } => {
            let outcome = match outcome {
                roadbook_ir::ReplyOutcome::Done => "done",
                roadbook_ir::ReplyOutcome::Fail => "fail",
            };
            let value = value
                .as_ref()
                .map_or(String::new(), |v| format!(" {}", v.text));
            format!("reply {} {outcome}{value}", call.text)
        }
        Effect::Spawn {
            children,
            src,
            key,
            args,
        } => format!(
            "spawn {children}[{}] = {src}({})",
            key.text,
            args.iter()
                .map(|a| a.text.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Effect::Stop { children, key } => format!("stop {children}[{}]", key.text),
    }
}

fn effects_text(list: &[Effect]) -> Option<String> {
    (!list.is_empty()).then(|| list.iter().map(effect_text).collect::<Vec<_>>().join("\n"))
}

/// A state's invokes, one a line.
fn invokes_text(list: &[Invoke]) -> Option<String> {
    (!list.is_empty()).then(|| list.iter().map(invoke_text).collect::<Vec<_>>().join("\n"))
}

fn invoke_text(inv: &Invoke) -> String {
    let args: Vec<&str> = inv.args.iter().map(|a| a.text.as_str()).collect();
    format!(
        "{}{}({}){}",
        if inv.kind == InvokeKind::Machine {
            "machine "
        } else {
            ""
        },
        inv.callee(),
        args.join(", "),
        inv.alias
            .as_ref()
            .map(|a| format!(" as {a}"))
            .unwrap_or_default()
    )
}

fn kind_text(k: StateKind) -> String {
    match k {
        StateKind::Atomic => "atomic",
        StateKind::Compound => "compound",
        StateKind::Parallel => "parallel",
        StateKind::Final => "final",
        StateKind::History => "history",
        StateKind::DeepHistory => "deep history",
    }
    .into()
}

/// A state id relative to its machine: `Processing.Paying`.
fn short(id: &str) -> String {
    id.split_once('.').map_or(id, |(_, rest)| rest).to_string()
}

fn doc(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_string())
}

/// What differs in a state itself, not counting the states inside it or its transitions.
fn state_changes(a: &State, b: &State) -> Vec<Change> {
    let mut out = Vec::new();
    let mut cmp = |what: &str, before: Option<String>, after: Option<String>| {
        if before != after {
            out.push(change(what, before, after));
        }
    };
    cmp("kind", Some(kind_text(b.kind)), Some(kind_text(a.kind)));
    cmp("doc", doc(&b.doc), doc(&a.doc));
    cmp(
        "initial",
        b.initial.as_deref().map(short),
        a.initial.as_deref().map(short),
    );
    cmp("invoke", invokes_text(&b.invokes), invokes_text(&a.invokes));
    cmp("entry", effects_text(&b.entry), effects_text(&a.entry));
    cmp("exit", effects_text(&b.exit), effects_text(&a.exit));
    cmp(
        "output",
        b.output.as_ref().map(|e| e.text.clone()),
        a.output.as_ref().map(|e| e.text.clone()),
    );
    cmp(
        "fail",
        b.fail.as_ref().map(|e| e.text.clone()),
        a.fail.as_ref().map(|e| e.text.clone()),
    );
    out
}

fn target_text(t: &Transition) -> String {
    if t.targets.is_empty() {
        return "(no target)".to_string();
    }
    let targets: Vec<String> = t.targets.iter().map(|x| short(x)).collect();
    let targets = targets.join(", ");
    if t.internal {
        format!("{targets} (internal: its state is not left)")
    } else {
        targets
    }
}

fn transition_changes(a: &State, ai: usize, b: &State, bi: usize) -> Vec<Change> {
    let (ta, tb) = (&a.transitions[ai], &b.transitions[bi]);
    let mut out = Vec::new();
    let mut cmp = |what: &str, before: Option<String>, after: Option<String>| {
        if before != after {
            out.push(change(what, before, after));
        }
    };
    cmp(
        "trigger",
        Some(trigger_text(b, &tb.trigger)),
        Some(trigger_text(a, &ta.trigger)),
    );
    cmp(
        "guard",
        Some(guard_text(b, bi)).filter(|g| !g.is_empty()),
        Some(guard_text(a, ai)).filter(|g| !g.is_empty()),
    );
    cmp("target", Some(target_text(tb)), Some(target_text(ta)));
    cmp(
        "effects",
        effects_text(&tb.effects),
        effects_text(&ta.effects),
    );
    out
}

/// `[guard]`, `[else]` for the unguarded last branch of a block, `""` otherwise (`guardText` in
/// `packages/viewer/src/model.ts`).
fn guard_text(s: &State, i: usize) -> String {
    let t = &s.transitions[i];
    if let Some(g) = &t.guard {
        return format!("[{}]", g.text);
    }
    let first = s.transitions.iter().position(|o| o.group == t.group);
    if first != Some(i) {
        "[else]".into()
    } else {
        String::new()
    }
}

fn fields_text(fields: &[roadbook_ir::Field]) -> String {
    fields
        .iter()
        .map(page::field_text)
        .collect::<Vec<_>>()
        .join(", ")
}

fn type_text(name: &str, t: &roadbook_ir::TypeDecl) -> String {
    let fields = |fs: &[roadbook_ir::Field]| {
        if fs.is_empty() {
            String::new()
        } else {
            format!(" {{ {} }}", fields_text(fs))
        }
    };
    let generic = |params: &[String]| {
        if params.is_empty() {
            String::new()
        } else {
            format!("<{}>", params.join(", "))
        }
    };
    match &t.def {
        TypeDef::Record { params, fields: fs } => {
            format!("type {name}{}{}", generic(params), fields(fs))
        }
        TypeDef::Error { fields: fs } => format!("error {name}{}", fields(fs)),
        TypeDef::Enum { variants } => format!("enum {name} {{ {} }}", variants.join(", ")),
        TypeDef::Alias { params, ty } => format!("type {name}{} = {ty}", generic(params)),
        TypeDef::Extern => format!("extern type {name}"),
    }
}

/// Changes to a machine itself: signature, doc comment, variables, events, emitted events and
/// calls.
fn machine_changes(name: &str, b: &Machine, a: &Machine) -> Vec<Change> {
    let mut out = Vec::new();
    let (sb, sa) = (page::signature(name, b), page::signature(name, a));
    if sb != sa {
        out.push(change("signature", sb, sa));
    }
    if b.doc != a.doc {
        out.push(change("doc", doc(&b.doc), doc(&a.doc)));
    }
    let var = |v: &roadbook_ir::Var| {
        let init = v
            .init
            .as_ref()
            .map(|e| format!(" = {}", e.text))
            .unwrap_or_default();
        let sensitive = if v.sensitive { "sensitive " } else { "" };
        format!("{sensitive}var {}: {}{init}", v.name, v.ty)
    };
    keyed(
        &mut out,
        "var",
        b.vars.iter().map(|v| (v.name.clone(), var(v))),
        a.vars.iter().map(|v| (v.name.clone(), var(v))),
    );
    let event = |kw: &str, n: &str, e: &roadbook_ir::EventDecl| {
        let f = fields_text(&e.fields);
        if f.is_empty() {
            format!("{kw} {n}")
        } else {
            format!("{kw} {n} {{ {f} }}")
        }
    };
    keyed(
        &mut out,
        "event",
        b.events
            .iter()
            .map(|(n, e)| (n.clone(), event("event", n, e))),
        a.events
            .iter()
            .map(|(n, e)| (n.clone(), event("event", n, e))),
    );
    keyed(
        &mut out,
        "emits",
        b.emits
            .iter()
            .map(|(n, e)| (n.clone(), event("emits", n, e))),
        a.emits
            .iter()
            .map(|(n, e)| (n.clone(), event("emits", n, e))),
    );
    keyed(
        &mut out,
        "call",
        b.calls.iter().map(|(n, c)| (n.clone(), call_text(n, c))),
        a.calls.iter().map(|(n, c)| (n.clone(), call_text(n, c))),
    );
    let children = |n: &String, c: &roadbook_ir::ChildrenDecl| {
        (n.clone(), format!("children {n}: {} by {}", c.src, c.key))
    };
    keyed(
        &mut out,
        "children",
        b.children.iter().map(|(n, c)| children(n, c)),
        a.children.iter().map(|(n, c)| children(n, c)),
    );
    out
}

/// `task upload(size: int) -> int ! Failed emits Progress { percent: int }`, as the formatter
/// writes it: one `emits` clause on the signature's line, several each on a line of its own.
fn task_text(name: &str, t: &roadbook_ir::TaskDecl) -> String {
    let mut s = signature_text("task", name, &t.params, t.returns.as_ref(), &t.errors);
    let sep = if t.emits.len() > 1 { "\n  " } else { " " };
    for (n, e) in &t.emits {
        let f = fields_text(&e.fields);
        if f.is_empty() {
            s.push_str(&format!("{sep}emits {n}"));
        } else {
            s.push_str(&format!("{sep}emits {n} {{ {f} }}"));
        }
    }
    s
}

/// `call reserve(orderId: string, items: Item[]) -> Reservation ! OutOfStock`
fn call_text(name: &str, c: &roadbook_ir::Call) -> String {
    signature_text("call", name, &c.params, c.returns.as_ref(), &c.errors)
}

/// `kw name(p: T) -> R ! E | F`, the signature of a task or a call.
fn signature_text(
    kw: &str,
    name: &str,
    params: &[roadbook_ir::Field],
    returns: Option<&roadbook_ir::TypeRef>,
    errors: &[String],
) -> String {
    let mut s = format!("{kw} {name}({})", fields_text(params));
    if let Some(r) = returns {
        s.push_str(&format!(" -> {r}"));
    }
    if !errors.is_empty() {
        s.push_str(&format!(" ! {}", errors.join(" | ")));
    }
    s
}

/// Changes between two lists of named texts: `var attempts`, before and after.
fn keyed(
    out: &mut Vec<Change>,
    what: &str,
    before: impl Iterator<Item = (String, String)>,
    after: impl Iterator<Item = (String, String)>,
) {
    let before: Vec<(String, String)> = before.collect();
    let after: Vec<(String, String)> = after.collect();
    for (n, ta) in &after {
        let tb = before.iter().find(|(m, _)| m == n).map(|(_, t)| t.clone());
        if tb.as_ref() != Some(ta) {
            out.push(change(&format!("{what} {n}"), tb, Some(ta.clone())));
        }
    }
    for (n, tb) in &before {
        if !after.iter().any(|(m, _)| m == n) {
            out.push(change(&format!("{what} {n}"), Some(tb.clone()), None));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ir(src: &str) -> Ir {
        let c = roadbook_compiler::compile_source("chart.road", src, &Default::default());
        assert!(!c.has_errors(), "{}", c.render());
        c.ir.unwrap()
    }

    const BASE: &str = r#"
task charge(n: int) -> int ! Declined
error Declined
action log(n: int)

machine M(n: int) -> int ! Declined {
  var tries: int = 0
  event Go
  event Stop

  initial A

  /// Waits.
  state A {
    on Go {
      if n > 1 -> B  set tries = tries + 1
      else     -> C
    }
    on Stop -> Out
  }

  state B {
    invoke charge(n) {
      done -> Out
      fail Declined -> Bad
    }
  }

  state C {
    initial C1
    state C1 { after 1s -> C2 }
    state C2 { on Go -> Out }
  }

  final state Out { output tries }
  final state Bad { fail Declined }
}
"#;

    fn status_of<'a>(d: &'a ChartDiff, id: &str) -> Option<&'a Element> {
        d.machines[0]
            .1
            .states
            .iter()
            .find(|(s, _)| s == id)
            .map(|(_, e)| e)
    }

    fn transition_of<'a>(d: &'a ChartDiff, id: &str, i: usize) -> Option<&'a Element> {
        d.machines[0]
            .1
            .transitions
            .iter()
            .find(|((s, j), _)| s == id && *j == i)
            .map(|(_, e)| e)
    }

    fn whats(e: &Element) -> Vec<&str> {
        e.changes.iter().map(|c| c.what.as_str()).collect()
    }

    #[test]
    fn identical_charts_have_no_changes_whatever_the_formatting() {
        let a = ir(BASE);
        let b = ir(&BASE.replace("  on Stop -> Out", "  // a comment\n  on Stop  ->  Out"));
        let d = diff(Some(&a), Some(&b), "main");
        assert!(!d.changed());
        assert_eq!(d.machines[0].1.status, Status::Unchanged);
        assert_eq!(d.ir.machines["M"].root, b.machines["M"].root);
    }

    #[test]
    fn an_added_state_and_its_transition() {
        let after = BASE
            .replace(
                "    on Stop -> Out\n",
                "    on Stop -> Out\n    after 5s -> Late\n",
            )
            .replace(
                "  final state Out",
                "  state Late { on Go -> Out }\n\n  final state Out",
            );
        let d = diff(Some(&ir(BASE)), Some(&ir(&after)), "main");
        assert_eq!(d.machines[0].1.status, Status::Changed);
        assert_eq!(status_of(&d, "M.Late").unwrap().status, Status::Added);
        // The new `after` arrow on A (index 3) and Late's own `on Go`.
        assert_eq!(transition_of(&d, "M.A", 3).unwrap().status, Status::Added);
        assert_eq!(
            transition_of(&d, "M.Late", 0).unwrap().status,
            Status::Added
        );
        let (m, s, t) = d.counts();
        assert_eq!((m.changed, s.added, t.added), (1, 1, 2));
        assert_eq!((s.removed, s.changed, t.removed, t.changed), (0, 0, 0, 0));
    }

    #[test]
    fn a_removed_state_is_put_back_where_it_was() {
        let after = BASE
            .replace("    on Stop -> Out\n", "")
            .replace("  final state Bad { fail Declined }\n", "")
            .replace("      fail Declined -> Bad\n", "      fail * -> Out\n");
        let d = diff(Some(&ir(BASE)), Some(&ir(&after)), "main");
        assert_eq!(status_of(&d, "M.Bad").unwrap().status, Status::Removed);
        let root = &d.ir.machines["M"].root;
        let names: Vec<&str> = root.states.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["A", "B", "C", "Out", "Bad"]);
        // Removed transitions follow the state's others, and point into the before sources.
        let a = &root.states[0];
        assert_eq!(a.transitions.len(), 3);
        assert_eq!(a.transitions[2].targets, ["M.Out"]);
        assert_eq!(transition_of(&d, "M.A", 2).unwrap().status, Status::Removed);
        assert_eq!(transition_of(&d, "M.B", 1).unwrap().status, Status::Added);
        assert_eq!(transition_of(&d, "M.B", 2).unwrap().status, Status::Removed);
        let src = a.transitions[2].span.file as usize;
        assert_eq!(d.ir.sources[src].path, "chart.road @ main");
        let bad = &root.states[4];
        assert_eq!(
            d.ir.sources[bad.span.file as usize].path,
            "chart.road @ main"
        );
        // A removed branch keeps a group of its own, not another block's.
        let groups: Vec<u32> = a.transitions.iter().map(|t| t.group).collect();
        assert_eq!(groups[0], groups[1]);
        assert_ne!(groups[2], groups[0]);
    }

    #[test]
    fn a_nested_removed_state_goes_back_between_its_siblings() {
        let before = BASE.replace(
            "    state C2 { on Go -> Out }",
            "    state Mid {}\n    state C2 { on Go -> Out }",
        );
        let d = diff(Some(&ir(&before)), Some(&ir(BASE)), "main");
        let c = &d.ir.machines["M"].root.states[2];
        let names: Vec<&str> = c.states.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["C1", "Mid", "C2"]);
        assert_eq!(status_of(&d, "M.C.Mid").unwrap().status, Status::Removed);
    }

    #[test]
    fn changed_guard_effect_target_and_trigger() {
        let after = BASE
            .replace(
                "if n > 1 -> B  set tries = tries + 1",
                "if n > 2 -> B  set tries = tries + 2",
            )
            .replace("    on Stop -> Out", "    on Stop -> Bad")
            .replace("after 1s -> C2", "after 2s -> C2");
        let d = diff(Some(&ir(BASE)), Some(&ir(&after)), "main");
        let go = transition_of(&d, "M.A", 0).unwrap();
        assert_eq!(go.status, Status::Changed);
        assert_eq!(whats(go), ["guard", "effects"]);
        assert_eq!(go.changes[0].before.as_deref(), Some("[n > 1]"));
        assert_eq!(go.changes[0].after.as_deref(), Some("[n > 2]"));
        assert_eq!(
            go.changes[1].after.as_deref(),
            Some("set tries = tries + 2")
        );
        // The else branch is unchanged.
        assert!(transition_of(&d, "M.A", 1).is_none());
        let stop = transition_of(&d, "M.A", 2).unwrap();
        assert_eq!(whats(stop), ["target"]);
        assert_eq!(stop.changes[0].before.as_deref(), Some("Out"));
        assert_eq!(stop.changes[0].after.as_deref(), Some("Bad"));
        let after1 = transition_of(&d, "M.C.C1", 0).unwrap();
        assert_eq!(whats(after1), ["trigger"]);
        assert_eq!(after1.changes[0].after.as_deref(), Some("after 2s"));
        // No state changed, only transitions.
        assert!(d.machines[0].1.states.is_empty());
        let (_, s, t) = d.counts();
        assert_eq!((s.changed, t.changed, t.added, t.removed), (0, 3, 0, 0));
    }

    #[test]
    fn a_new_branch_matches_the_others_by_guard() {
        let after = BASE.replace(
            "      if n > 1 -> B  set tries = tries + 1\n",
            "      if n > 1 -> B  set tries = tries + 1\n      if n > 0 -> Out\n",
        );
        let d = diff(Some(&ir(BASE)), Some(&ir(&after)), "main");
        assert!(transition_of(&d, "M.A", 0).is_none());
        assert_eq!(transition_of(&d, "M.A", 1).unwrap().status, Status::Added);
        assert!(
            transition_of(&d, "M.A", 2).is_none(),
            "the else branch is the same"
        );
    }

    #[test]
    fn changed_invoke_entry_doc_and_output() {
        let after = BASE
            .replace("invoke charge(n)", "invoke charge(n + 1)")
            .replace(
                "  /// Waits.\n  state A {",
                "  /// Waits for Go.\n  state A {\n    entry do log(n)",
            )
            .replace("output tries", "output tries + 1");
        let d = diff(Some(&ir(BASE)), Some(&ir(&after)), "main");
        let a = status_of(&d, "M.A").unwrap();
        assert_eq!(whats(a), ["doc", "entry"]);
        assert_eq!(a.changes[1].before, None);
        assert_eq!(a.changes[1].after.as_deref(), Some("do log(n)"));
        let b = status_of(&d, "M.B").unwrap();
        assert_eq!(whats(b), ["invoke"]);
        assert_eq!(b.changes[0].before.as_deref(), Some("charge(n)"));
        assert_eq!(b.changes[0].after.as_deref(), Some("charge(n + 1)"));
        assert_eq!(whats(status_of(&d, "M.Out").unwrap()), ["output"]);
    }

    #[test]
    fn machine_level_changes() {
        let after = BASE
            .replace(
                "var tries: int = 0",
                "var tries: int = 1\n  var extra: bool = false",
            )
            .replace("  event Stop\n", "  event Stop\n  event Pause\n");
        let d = diff(Some(&ir(BASE)), Some(&ir(&after)), "main");
        let m = &d.machines[0].1;
        assert_eq!(m.status, Status::Changed);
        let whats: Vec<&str> = m.changes.iter().map(|c| c.what.as_str()).collect();
        assert_eq!(whats, ["var tries", "var extra", "event Pause"]);
        assert_eq!(m.changes[0].after.as_deref(), Some("var tries: int = 1"));
    }

    const CALLS: &str = r#"
error Busy

machine P() {
  call ask(n: int) -> int
  call gone()
  var pending: call ask
  event Go
  initial A
  state A {
    on ask -> B  set pending = call
  }
  state B {
    on Go -> A  reply pending done 1
  }
}

machine Q(p: ref P) {
  initial S
  state S {
    invoke p.ask(1) {
      done   -> T
      fail * -> T
    }
  }
  final state T {}
}
"#;

    #[test]
    fn calls_are_declarations_of_the_machine_and_their_uses_are_compared() {
        let after = CALLS
            .replace(
                "call ask(n: int) -> int\n  call gone()",
                "call ask(n: int, m: int) -> int ! Busy\n  call fresh() -> bool",
            )
            .replace(
                "    on ask -> B  set pending = call\n",
                "    on ask -> B  set pending = call\n    on fresh reply call done true\n",
            )
            .replace("reply pending done 1", "reply pending done 2")
            .replace("invoke p.ask(1)", "invoke p.ask(1, 2)");
        let d = diff(Some(&ir(CALLS)), Some(&ir(&after)), "main");
        let p = &d.machines[0].1;
        assert_eq!(p.status, Status::Changed);
        let what: Vec<&str> = p.changes.iter().map(|c| c.what.as_str()).collect();
        assert_eq!(what, ["call ask", "call fresh", "call gone"]);
        let texts: Vec<(Option<&str>, Option<&str>)> = p
            .changes
            .iter()
            .map(|c| (c.before.as_deref(), c.after.as_deref()))
            .collect();
        assert_eq!(
            texts,
            [
                (
                    Some("call ask(n: int) -> int"),
                    Some("call ask(n: int, m: int) -> int ! Busy")
                ),
                (None, Some("call fresh() -> bool")),
                (Some("call gone()"), None),
            ]
        );
        // `on fresh` is a new transition beside `on ask`, matched by its trigger.
        assert_eq!(transition_of(&d, "P.A", 0), None);
        let added = transition_of(&d, "P.A", 1).unwrap();
        assert_eq!(added.status, Status::Added);
        let b = transition_of(&d, "P.B", 0).unwrap();
        assert_eq!(whats(b), ["effects"]);
        assert_eq!(b.changes[0].before.as_deref(), Some("reply pending done 1"));
        assert_eq!(b.changes[0].after.as_deref(), Some("reply pending done 2"));
        // The caller: its invoke, written as the chart writes it.
        let (name, q) = &d.machines[1];
        assert_eq!(name, "Q");
        let (id, s) = &q.states[0];
        assert_eq!(id, "Q.S");
        assert_eq!(whats(s), ["invoke"]);
        assert_eq!(s.changes[0].before.as_deref(), Some("p.ask(1)"));
        assert_eq!(s.changes[0].after.as_deref(), Some("p.ask(1, 2)"));
        // A removed machine's calls point into the before version's sources.
        let d = diff(Some(&ir(CALLS)), Some(&ir("")), "main");
        let p = &d.ir.machines["P"];
        assert_eq!(p.calls["ask"].span.file, p.span.file);
    }

    #[test]
    fn the_machine_body_is_the_machine_not_a_state() {
        // The doc comment, `initial` and a transition of the machine's body.
        let base = BASE.replace(
            "machine M(n: int)",
            "/// Takes a payment.\nmachine M(n: int)",
        );
        let after = base
            .replace("/// Takes a payment.", "/// Takes a card payment.")
            .replace("  initial A\n", "  initial C\n")
            .replace("  event Stop\n", "  event Stop\n\n  on Stop -> Out\n");
        let d = diff(Some(&ir(&base)), Some(&ir(&after)), "main");
        let m = &d.machines[0].1;
        assert_eq!(m.status, Status::Changed);
        let whats: Vec<&str> = m.changes.iter().map(|c| c.what.as_str()).collect();
        assert_eq!(whats, ["doc", "initial"]);
        assert_eq!(m.changes[1].before.as_deref(), Some("A"));
        assert_eq!(m.changes[1].after.as_deref(), Some("C"));
        // No state changed: the machine is not counted as one, nor its doc twice.
        assert!(m.states.is_empty(), "{:?}", m.states);
        assert_eq!(m.state_counts(), Counts::default());
        assert_eq!(m.transition_counts().added, 1);
        assert!(m.transitions.iter().any(|((id, _), _)| id == "M"));
    }

    #[test]
    fn a_machine_from_another_file_is_moved() {
        let mut b = ir(BASE);
        b.sources[0].path = "old.road".into();
        let d = diff(Some(&b), Some(&ir(BASE)), "main");
        let m = &d.machines[0].1;
        assert_eq!(m.status, Status::Changed);
        assert_eq!(
            m.moved,
            Some(("old.road".to_string(), "chart.road".to_string()))
        );
        assert!(m.only_moved());
        assert_eq!(m.changes[0].what, "file");
    }

    #[test]
    fn users_of_a_declaration_directly_or_through_another() {
        let both = format!(
            "{BASE}\ntype Wrap {{ d: Declined }}\nmachine N(w: Wrap) {{\n  initial X\n  final state X {{}}\n}}\nmachine O() {{\n  initial X\n  final state X {{}}\n}}\n"
        );
        let c = ir(&both);
        assert_eq!(users(&c, "Declined"), ["M", "N"]);
        assert_eq!(users(&c, "charge"), ["M"]);
        assert_eq!(users(&c, "log"), Vec::<String>::new());
    }

    #[test]
    fn machines_added_and_removed() {
        let other = "machine N() {\n  initial X\n  state X { on Go -> Y }\n  event Go\n  final state Y {}\n}\n";
        let both = format!("{BASE}\n{other}");
        let d = diff(Some(&ir(BASE)), Some(&ir(&both)), "main");
        let (_, n) = &d.machines[1];
        assert_eq!(n.status, Status::Added);
        assert!(n.states.iter().all(|(_, e)| e.status == Status::Added));
        assert_eq!(n.state_counts().added, 2);
        assert_eq!(n.transition_counts().added, 1);
        assert_eq!(d.machines[0].1.status, Status::Unchanged);

        let d = diff(Some(&ir(&both)), Some(&ir(BASE)), "main");
        assert_eq!(d.machines[1].0, "N");
        assert_eq!(d.machines[1].1.status, Status::Removed);
        assert!(d.ir.machines.contains_key("N"));
        let n = &d.ir.machines["N"];
        assert_eq!(d.ir.sources[n.span.file as usize].path, "chart.road @ main");
        assert_eq!(
            d.ir.sources[n.root.states[0].span.file as usize].path,
            "chart.road @ main"
        );

        // A chart added or removed: everything in it is.
        let d = diff(None, Some(&ir(BASE)), "main");
        assert_eq!(d.machines[0].1.status, Status::Added);
        let d = diff(Some(&ir(BASE)), None, "main");
        assert_eq!(d.machines[0].1.status, Status::Removed);
        assert!(d.decls.iter().all(|(_, e)| e.status == Status::Removed));
        assert_eq!(d.ir.machines.len(), 1);
    }

    #[test]
    fn history_states_added_removed_and_changed_in_kind() {
        let before = BASE.replace(
            "    initial C1\n",
            "    initial C1\n    history Last\n    history Gone\n",
        );
        let after = BASE.replace(
            "    initial C1\n",
            "    initial C1\n    deep history Last\n    history New\n",
        );
        let d = diff(Some(&ir(&before)), Some(&ir(&after)), "main");
        let last = status_of(&d, "M.C.Last").unwrap();
        assert_eq!(last.status, Status::Changed);
        assert_eq!(
            last.changes,
            [change(
                "kind",
                "history".to_string(),
                "deep history".to_string()
            )]
        );
        assert_eq!(status_of(&d, "M.C.New").unwrap().status, Status::Added);
        assert_eq!(status_of(&d, "M.C.Gone").unwrap().status, Status::Removed);
        let m = &d.machines[0].1;
        let all = Counts {
            added: 1,
            removed: 1,
            changed: 1,
        };
        // Counted with the states, and on their own.
        assert_eq!(m.state_counts(), all);
        assert_eq!(m.history_counts(), all);
        assert_eq!(
            d.to_json()["machines"]["M"]["counts"]["history"],
            all.to_json()
        );
        // The removed one is put back where it was, in the merged chart.
        let c = &d.ir.machines["M"].root.states[2];
        let names: Vec<&str> = c.states.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Last", "Gone", "New", "C1", "C2"]);
        assert_eq!(c.states[1].kind, StateKind::History);
    }

    #[test]
    fn a_renamed_state_is_removed_and_added() {
        let after = BASE
            .replace("state B {", "state Paying {")
            .replace("-> B ", "-> Paying ");
        let d = diff(Some(&ir(BASE)), Some(&ir(&after)), "main");
        assert_eq!(status_of(&d, "M.B").unwrap().status, Status::Removed);
        assert_eq!(status_of(&d, "M.Paying").unwrap().status, Status::Added);
        let go = transition_of(&d, "M.A", 0).unwrap();
        assert_eq!(whats(go), ["target"]);
    }

    #[test]
    fn all_children_removed_keeps_the_container() {
        let after = BASE
            .replace(
                "  state C {\n    initial C1\n    state C1 { after 1s -> C2 }\n    state C2 { on Go -> Out }\n  }",
                "  state C { on Go -> Out }",
            );
        let d = diff(Some(&ir(BASE)), Some(&ir(&after)), "main");
        let c = &d.ir.machines["M"].root.states[2];
        assert_eq!(c.kind, StateKind::Compound);
        assert_eq!(c.initial.as_deref(), Some("M.C.C1"));
        assert_eq!(c.states.len(), 2);
        assert_eq!(whats(status_of(&d, "M.C").unwrap()), ["kind", "initial"]);
    }

    #[test]
    fn a_sensitive_mark_added_or_removed_is_a_change() {
        // The machine hash leaves marks out, so the diff must find them itself.
        let after = BASE
            .replace("task charge(n: int)", "task charge(sensitive n: int)")
            .replace("machine M(n: int)", "machine M(sensitive n: int)")
            .replace("  var tries: int = 0", "  sensitive var tries: int = 0")
            .replace("set tries = tries + 1", "set tries = 1")
            .replace("output tries", "output 0");
        let before = BASE
            .replace("set tries = tries + 1", "set tries = 1")
            .replace("output tries", "output 0");
        let (b, a) = (ir(&before), ir(&after));
        assert_eq!(b.machines["M"].hash, a.machines["M"].hash);
        let d = diff(Some(&b), Some(&a), "main");
        assert_eq!(d.machines[0].1.status, Status::Changed);
        let whats: Vec<&str> = d.machines[0]
            .1
            .changes
            .iter()
            .map(|c| c.what.as_str())
            .collect();
        assert_eq!(whats, ["signature", "var tries"]);
        let names: Vec<(&str, Status)> = d
            .decls
            .iter()
            .map(|(n, e)| (n.as_str(), e.status))
            .collect();
        assert_eq!(names, [("charge", Status::Changed)]);
    }

    const PEERS: &str = r#"
action note(status: Ended)

machine Peer() {
  event Stop
  initial A
  state A { on Stop -> B }
  final state B {}
}

machine Holder(peer: ref Peer, other: ref Peer, third: ref Peer) {
  initial Watching
  state Watching {
    on peer.ended if event.status == Ended.Done -> Gone
    on other.ended -> Gone
  }
  final state Gone {}
}
"#;

    /// `on x.ended` is matched by its trigger, peer included, like `on x.E`: a changed guard is
    /// a change of the transition, another peer another transition. The built-in `Ended`,
    /// which comes into `types` with its first use, is no declaration of the chart's.
    #[test]
    fn a_peer_s_end() {
        let after = PEERS
            .replace("Ended.Done -> Gone", "Ended.Defect -> Gone")
            .replace("on other.ended -> Gone", "on third.ended -> Gone");
        let d = diff(Some(&ir(PEERS)), Some(&ir(&after)), "main");
        let at = |i| {
            d.machines
                .iter()
                .find(|(n, _)| n == "Holder")
                .unwrap()
                .1
                .transitions
                .iter()
                .find(|((s, j), _)| s == "Holder.Watching" && *j == i)
                .map(|(_, e)| e)
        };
        let guarded = at(0).unwrap();
        assert_eq!(whats(guarded), ["guard"]);
        assert_eq!(
            guarded.changes[0].after.as_deref(),
            Some("[event.status == Ended.Defect]")
        );
        assert_eq!(
            plain_trigger_text(&d.ir.machines["Holder"].root.states[0].transitions[1].trigger),
            "third.ended"
        );
        assert_eq!(at(1).unwrap().status, Status::Added);
        // The removed one is put back after it, in the merged IR.
        assert_eq!(at(2).unwrap().status, Status::Removed);
        assert_eq!(
            plain_trigger_text(&d.ir.machines["Holder"].root.states[0].transitions[2].trigger),
            "other.ended"
        );
        assert!(d.decls.is_empty(), "{:?}", d.decls);

        // A chart that starts naming `Ended`, and one that stops.
        let none = PEERS
            .replace("action note(status: Ended)\n", "")
            .replace(" if event.status == Ended.Done", "");
        let d = diff(Some(&ir(&none)), Some(&ir(PEERS)), "main");
        assert!(d.ir.types.contains_key("Ended"));
        assert_eq!(
            d.decls
                .iter()
                .map(|(n, e)| (n.as_str(), e.status))
                .collect::<Vec<_>>(),
            [("note", Status::Added)]
        );
        let d = diff(Some(&ir(PEERS)), Some(&ir(&none)), "main");
        assert!(
            d.ir.types.contains_key("Ended"),
            "the removed guard still reads it"
        );
        assert_eq!(
            d.decls
                .iter()
                .map(|(n, e)| (n.as_str(), e.status))
                .collect::<Vec<_>>(),
            [("note", Status::Removed)]
        );
    }

    #[test]
    fn a_task_s_emits_are_part_of_its_declaration() {
        let base = BASE.replace(
            "task charge(n: int) -> int ! Declined",
            "task charge(n: int) -> int ! Declined emits Progress { percent: int }",
        );
        let after = base.replace("Progress { percent: int }", "Progress { percent: float }");
        let d = diff(Some(&ir(&base)), Some(&ir(&after)), "main");
        let (_, charge) = d.decls.iter().find(|(n, _)| n == "charge").unwrap();
        assert_eq!(charge.status, Status::Changed);
        let c = &charge.changes[0];
        assert_eq!(
            c.after.as_deref(),
            Some("task charge(n: int) -> int ! Declined emits Progress { percent: float }")
        );
        let two = base.replace(
            "emits Progress { percent: int }",
            "emits Progress { percent: int } emits Stage",
        );
        let d = diff(Some(&ir(&base)), Some(&ir(&two)), "main");
        let (_, charge) = d.decls.iter().find(|(n, _)| n == "charge").unwrap();
        assert_eq!(
            charge.changes[0].after.as_deref(),
            Some(
                "task charge(n: int) -> int ! Declined\n  emits Progress { percent: int }\n  emits Stage"
            )
        );
    }

    #[test]
    fn declarations() {
        let after = BASE
            .replace("action log(n: int)", "action log(n: int, m: int)")
            .replace(
                "error Declined\n",
                "error Declined { code: int }\ntype Extra { a: int }\n",
            )
            .replace("fail Declined }", "fail Declined { code: 1 } }");
        let d = diff(Some(&ir(BASE)), Some(&ir(&after)), "main");
        let names: Vec<(&str, Status)> = d
            .decls
            .iter()
            .map(|(n, e)| (n.as_str(), e.status))
            .collect();
        assert!(names.contains(&("Declined", Status::Changed)), "{names:?}");
        assert!(names.contains(&("Extra", Status::Added)), "{names:?}");
        assert!(names.contains(&("log", Status::Changed)), "{names:?}");
    }
}
