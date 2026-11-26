//! The analyses that need the whole resolved chart: `always` cycles, unbounded recursion and
//! the warnings. They run on the IR, once the chart has no other errors.

use std::collections::{HashMap, HashSet};

use roadbook_ir::{self as ir, InvokeKind, StateKind, Trigger};
use roadbook_syntax::{Diagnostic, Span};

use crate::Options;
use crate::check::Checker;
use crate::codes;
use crate::machine::MachineInfo;
use crate::project::Project;

/// A machine's states by id, with their parents, in document order.
struct Tree<'a> {
    root: &'a str,
    states: HashMap<&'a str, &'a ir::State>,
    parent: HashMap<&'a str, &'a str>,
    order: Vec<&'a str>,
}

impl<'a> Tree<'a> {
    fn new(root: &'a ir::State) -> Tree<'a> {
        let mut t = Tree {
            root: &root.id,
            states: HashMap::new(),
            parent: HashMap::new(),
            order: Vec::new(),
        };
        t.add(root, None);
        t
    }

    fn add(&mut self, s: &'a ir::State, parent: Option<&'a str>) {
        self.states.insert(&s.id, s);
        self.order.push(&s.id);
        if let Some(p) = parent {
            self.parent.insert(&s.id, p);
        }
        for c in &s.states {
            self.add(c, Some(&s.id));
        }
    }

    /// Proper ancestors, nearest first.
    fn ancestors(&self, id: &'a str) -> Vec<&'a str> {
        let mut out = Vec::new();
        let mut cur = id;
        while let Some(&p) = self.parent.get(cur) {
            out.push(p);
            cur = p;
        }
        out
    }

    /// `id` and the states entered with it: the initial child of a compound state, every
    /// child of a parallel state, recursively.
    fn enter_down(&self, id: &'a str, out: &mut Vec<&'a str>) {
        out.push(id);
        let s = self.states[id];
        match s.kind {
            StateKind::Compound => {
                if let Some(i) = s.initial.as_deref()
                    && let Some((&k, _)) = self.states.get_key_value(i)
                {
                    self.enter_down(k, out);
                }
            }
            StateKind::Parallel => {
                for c in &s.states {
                    self.enter_down(&c.id, out);
                }
            }
            _ => {}
        }
    }

    /// The states entered by a transition written in `from` with target `to`: those below the
    /// deepest state that strictly contains both (the machine root at most), down to `to`,
    /// with the other regions of any parallel state on the way, then `to`'s initial chain. A
    /// transition to the state itself or to an ancestor leaves and re-enters it.
    fn entry_set(&self, from: &'a str, to: &'a str) -> Vec<&'a str> {
        let from_anc = self.ancestors(from);
        let domain = self
            .ancestors(to)
            .into_iter()
            .find(|a| from_anc.contains(a))
            .unwrap_or(self.root);
        let mut path = vec![to];
        let mut cur = to;
        while let Some(&p) = self.parent.get(cur) {
            if p == domain {
                break;
            }
            path.push(p);
            cur = p;
        }
        path.reverse();
        let mut out = Vec::new();
        for (i, &p) in path.iter().enumerate() {
            if let Some(&parent) = self.parent.get(p)
                && self.states[parent].kind == StateKind::Parallel
            {
                for c in &self.states[parent].states {
                    if c.id != p {
                        self.enter_down(&c.id, &mut out);
                    }
                }
            }
            if i + 1 == path.len() {
                self.enter_down(p, &mut out);
            } else {
                out.push(p);
            }
        }
        out
    }

    /// The states that can become active, starting from the initial configuration and
    /// following the transitions `follow` accepts.
    fn reachable(&self, follow: impl Fn(&ir::Transition) -> bool) -> HashSet<&'a str> {
        let mut start = Vec::new();
        self.enter_down(self.root, &mut start);
        let mut seen: HashSet<&'a str> = start.iter().copied().collect();
        let mut work = start;
        while let Some(x) = work.pop() {
            for t in &self.states[x].transitions {
                if let Some(to) = t.target.as_deref()
                    && follow(t)
                    && let Some((&to, _)) = self.states.get_key_value(to)
                {
                    for s in self.entry_set(x, to) {
                        if seen.insert(s) {
                            work.push(s);
                        }
                    }
                }
            }
        }
        seen
    }
}

pub(crate) fn run(
    doc: &ir::Ir,
    ck: &Checker<'_>,
    project: &Project,
    options: &Options,
    diags: &mut Vec<Diagnostic>,
) {
    let infos: HashMap<&str, &MachineInfo> =
        ck.machines.iter().map(|m| (m.name.as_str(), m)).collect();
    let mut invokes_eagerly: Vec<(&str, Vec<(&str, Span)>)> = Vec::new();
    for (name, m) in &doc.machines {
        let Some(info) = infos.get(name.as_str()) else {
            continue;
        };
        let tree = Tree::new(&m.root);
        always_cycles(&tree, info, diags);
        warnings(
            &tree,
            info,
            project.roots.contains(&info.file),
            options,
            diags,
        );

        // States entered without waiting: through `always`, `done` and invoke outcomes.
        let eager = tree.reachable(|t| {
            !matches!(
                t.trigger,
                Trigger::Event { .. } | Trigger::Any | Trigger::After { .. }
            )
        });
        let mut edges = Vec::new();
        for id in &tree.order {
            let s = tree.states[id];
            if eager.contains(id)
                && let Some(inv) = &s.invoke
                && inv.kind == InvokeKind::Machine
                && let Some(span) = info.states.get(*id).and_then(|i| i.invoke_span)
            {
                edges.push((inv.src.as_str(), span));
            }
        }
        invokes_eagerly.push((name.as_str(), edges));
    }
    recursion(&invokes_eagerly, diags);
    unused(ck, project, diags);
}

/// The first branch of a state's `always` block, if it has no guard.
fn unguarded_always(s: &ir::State) -> Option<usize> {
    let i = s
        .transitions
        .iter()
        .position(|t| matches!(t.trigger, Trigger::Always))?;
    s.transitions[i].guard.is_none().then_some(i)
}

fn always_cycles(tree: &Tree<'_>, info: &MachineInfo, diags: &mut Vec<Diagnostic>) {
    let trigger_span =
        |id: &str, i: usize| info.states.get(id).and_then(|s| s.triggers.get(i)).copied();
    let mut edges: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut nodes = Vec::new();
    for &id in &tree.order {
        let s = tree.states[id];
        let Some(i) = unguarded_always(s) else {
            continue;
        };
        match s.transitions[i].target.as_deref() {
            None => {
                if let Some(span) = trigger_span(id, i) {
                    diags.push(
                        Diagnostic::error(
                            codes::ALWAYS_CYCLE,
                            span,
                            format!("`always` with no guard and no target is taken again and again: `{id}` never settles"),
                        )
                        .with_help("give it a guard that its own effects make false, or a target"),
                    );
                }
            }
            Some(to) if id == tree.root => {
                // The machine itself is never left, so its `always` is taken again after every
                // step, unless the machine has finished.
                let ends = tree.parent.get(to) == Some(&tree.root)
                    && tree.states.get(to).is_some_and(|t| t.kind == StateKind::Final);
                if !ends && let Some(span) = trigger_span(id, i) {
                    diags.push(
                        Diagnostic::error(
                            codes::ALWAYS_CYCLE,
                            span,
                            format!("`always` with no guard in the body of `{id}` is taken again and again: the machine is never left, so it never settles"),
                        )
                        .with_help("give it a guard that its own effects make false, or move it into a state"),
                    );
                }
            }
            Some(to) => {
                if let Some((&to, _)) = tree.states.get_key_value(to) {
                    nodes.push(id);
                    edges.insert(id, tree.entry_set(id, to));
                }
            }
        }
    }
    let has: HashSet<&str> = nodes.iter().copied().collect();
    let graph: HashMap<&str, Vec<&str>> = edges
        .into_iter()
        .map(|(k, v)| (k, v.into_iter().filter(|n| has.contains(n)).collect()))
        .collect();
    for cycle in cycles(&nodes, &graph) {
        let path: Vec<String> = cycle
            .iter()
            .chain(std::iter::once(&cycle[0]))
            .map(|s| format!("`{s}`"))
            .collect();
        let Some(first) = trigger_span(
            cycle[0],
            unguarded_always(tree.states[cycle[0]]).unwrap_or(0),
        ) else {
            continue;
        };
        let mut d = Diagnostic::error(
            codes::ALWAYS_CYCLE,
            first,
            format!(
                "unguarded `always` transitions form a cycle: {}",
                path.join(" -> ")
            ),
        )
        .with_help("the machine would never settle; guard one of them with `if`");
        for &id in &cycle[1..] {
            if let Some(span) = trigger_span(id, unguarded_always(tree.states[id]).unwrap_or(0)) {
                d = d.with_secondary(span, format!("then `{id}`"));
            }
        }
        diags.push(d);
    }
}

/// The elementary cycles to report: one per strongly connected component that has a cycle,
/// as a path from its first node (in `nodes` order) back to it.
fn cycles<'a>(nodes: &[&'a str], graph: &HashMap<&'a str, Vec<&'a str>>) -> Vec<Vec<&'a str>> {
    let mut out = Vec::new();
    let mut done: HashSet<&str> = HashSet::new();
    for &start in nodes {
        if done.contains(start) {
            continue;
        }
        // Shortest path from start back to start (breadth first).
        let mut prev: HashMap<&str, &str> = HashMap::new();
        let mut queue = std::collections::VecDeque::from([start]);
        let mut found = false;
        let mut visited: HashSet<&str> = HashSet::new();
        while let Some(x) = queue.pop_front() {
            for &y in graph.get(x).map(Vec::as_slice).unwrap_or(&[]) {
                if y == start {
                    prev.insert(start, x);
                    found = true;
                    break;
                }
                if !done.contains(y) && visited.insert(y) {
                    prev.insert(y, x);
                    queue.push_back(y);
                }
            }
            if found {
                break;
            }
        }
        if found {
            let mut path = vec![start];
            let mut cur = prev[start];
            while cur != start {
                path.push(cur);
                cur = prev[cur];
            }
            path[1..].reverse();
            done.extend(path.iter().copied());
            out.push(path);
        }
        done.insert(start);
    }
    out
}

fn recursion(edges: &[(&str, Vec<(&str, Span)>)], diags: &mut Vec<Diagnostic>) {
    let nodes: Vec<&str> = edges.iter().map(|(m, _)| *m).collect();
    let graph: HashMap<&str, Vec<&str>> = edges
        .iter()
        .map(|(m, es)| (*m, es.iter().map(|(n, _)| *n).collect()))
        .collect();
    let span = |from: &str, to: &str| {
        edges
            .iter()
            .find(|(m, _)| *m == from)
            .and_then(|(_, es)| es.iter().find(|(n, _)| *n == to))
            .map(|(_, s)| *s)
    };
    for cycle in cycles(&nodes, &graph) {
        let next = |i: usize| cycle[(i + 1) % cycle.len()];
        let path: Vec<String> = cycle
            .iter()
            .chain(std::iter::once(&cycle[0]))
            .map(|s| format!("`{s}`"))
            .collect();
        let Some(first) = span(cycle[0], next(0)) else {
            continue;
        };
        let what = if cycle.len() == 1 {
            format!("`{}` invokes itself", cycle[0])
        } else {
            format!(
                "`{}` invokes itself through {}",
                cycle[0],
                path.join(" -> ")
            )
        };
        let mut d = Diagnostic::error(
            codes::UNBOUNDED_RECURSION,
            first,
            format!("{what} without waiting for an event or an `after` (unbounded recursion)"),
        )
        .with_label("entered without waiting")
        .with_help("put an `on` or `after` transition on the way to the state that invokes it");
        for (i, m) in cycle.iter().enumerate().skip(1) {
            if let Some(s) = span(m, next(i)) {
                d = d.with_secondary(s, format!("then `{m}` invokes `{}`", next(i)));
            }
        }
        diags.push(d);
    }
}

fn warnings(
    tree: &Tree<'_>,
    info: &MachineInfo,
    root_file: bool,
    options: &Options,
    diags: &mut Vec<Diagnostic>,
) {
    let reach = tree.reachable(|_| true);
    let leaves = |s: &ir::State| {
        s.transitions
            .iter()
            .any(|t| t.target.is_some() && !matches!(t.trigger, Trigger::Done))
    };
    for &id in &tree.order[1..] {
        let s = tree.states[id];
        let Some(si) = info.states.get(id) else {
            continue;
        };
        let parent = tree.parent[id];
        if !reach.contains(id) {
            if reach.contains(parent) {
                diags.push(
                    Diagnostic::warning(
                        codes::UNREACHABLE_STATE,
                        si.name_span,
                        format!("state `{id}` can never be entered"),
                    )
                    .with_help(
                        "no transition that can be taken leads to it, and it is not an `initial`",
                    ),
                );
            }
            continue;
        }
        if s.kind == StateKind::Atomic
            && !leaves(s)
            && !tree.ancestors(id).iter().any(|a| leaves(tree.states[a]))
        {
            diags.push(
                Diagnostic::warning(
                    codes::NO_WAY_OUT,
                    si.name_span,
                    format!("state `{id}` has no way out"),
                )
                .with_help("nothing leaves it or a state containing it; add a transition, or make it a `final state`"),
            );
        }
        if options.warn_invoke_timeout
            && let (Some(inv), Some(span)) = (&s.invoke, si.invoke_span)
        {
            let timed = std::iter::once(id).chain(tree.ancestors(id)).any(|a| {
                tree.states[a]
                    .transitions
                    .iter()
                    .any(|t| matches!(t.trigger, Trigger::After { .. }))
            });
            if !timed {
                diags.push(
                    Diagnostic::warning(
                        codes::INVOKE_WITHOUT_TIMEOUT,
                        span,
                        format!("`invoke {}` has no timeout", inv.src),
                    )
                    .with_help(
                        "add an `after` transition to this state or to a state that contains it",
                    ),
                );
            }
        }
    }
    for (e, span) in &info.events {
        if root_file && !info.used_events.contains(e) {
            diags.push(
                Diagnostic::warning(
                    codes::UNUSED_DECLARATION,
                    *span,
                    format!("event `{e}` is never used"),
                )
                .with_help(format!("no state of `{}` has `on {e}`", info.name)),
            );
        }
    }
}

/// Functions, actions and tasks declared in a root file that nothing in the compilation uses.
fn unused(ck: &Checker<'_>, project: &Project, diags: &mut Vec<Diagnostic>) {
    use roadbook_syntax::ast::DeclKind;
    for e in &ck.names.decls {
        if e.duplicate || !project.roots.contains(&e.file) {
            continue;
        }
        let what = match e.decl.kind {
            DeclKind::Fn { .. } => "function",
            DeclKind::Action { .. } => "action",
            DeclKind::Task { .. } => "task",
            _ => continue,
        };
        let name = &e.decl.name.name;
        if !ck.used.contains(name) {
            diags.push(
                Diagnostic::warning(
                    codes::UNUSED_DECLARATION,
                    e.decl.name.span,
                    format!("{what} `{name}` is never used"),
                )
                .with_help("remove it, or use it in a machine"),
            );
        }
    }
}
