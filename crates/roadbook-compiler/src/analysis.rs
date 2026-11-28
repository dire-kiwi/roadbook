//! The analyses that need the whole resolved chart: `always` cycles, unbounded recursion,
//! definite assignment (in `assign`) and the warnings. They run on the IR, once the chart has
//! no other errors.

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

    /// Whether `a` is a proper descendant of `of`.
    fn inside(&self, a: &str, of: &str) -> bool {
        let mut cur = a;
        while let Some(&p) = self.parent.get(cur) {
            if p == of {
                return true;
            }
            cur = p;
        }
        false
    }

    /// The domain of transition `t` written in `from` (`spec/execution.md`): the root for the
    /// machine body's, `from` for an internal one, else the nearest proper ancestor of `from`
    /// that is not a parallel state and contains every target.
    fn domain(&self, from: &'a str, t: &ir::Transition) -> &'a str {
        if from == self.root || t.internal {
            return from;
        }
        self.ancestors(from)
            .into_iter()
            .find(|&a| {
                self.states[a].kind != StateKind::Parallel
                    && t.targets.iter().all(|x| self.inside(x, a))
            })
            .unwrap_or(self.root)
    }

    /// The states transition `t` written in `from` leaves, among `from` and the states
    /// containing it: those below its domain. None for an internal one.
    fn left_by(&self, from: &'a str, t: &ir::Transition) -> Vec<&'a str> {
        let domain = self.domain(from, t);
        std::iter::once(from)
            .chain(self.ancestors(from))
            .take_while(|&s| s != domain && s != self.root)
            .collect()
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
                for c in s.states.iter().filter(|c| !c.kind.is_history()) {
                    self.enter_down(&c.id, out);
                }
            }
            _ => {}
        }
    }

    /// The states entered by transition `t` written in `from`: for each target, the states
    /// from just below the domain down to it, and its initial chain; then every region of a
    /// parallel state among them that none of them is in, by default entry. A transition to the
    /// state itself or to an ancestor leaves and re-enters it.
    ///
    /// A transition to a history state enters its parent's default entry: a history state
    /// only restores states that were entered before.
    fn entry_set(&self, from: &'a str, t: &ir::Transition) -> Vec<&'a str> {
        let domain = self.domain(from, t);
        let mut out: Vec<&'a str> = Vec::new();
        for to in &t.targets {
            let Some((&to, _)) = self.states.get_key_value(to.as_str()) else {
                continue;
            };
            let to = if self.states[to].kind.is_history() {
                let parent = self.parent[to];
                if parent == domain {
                    // From inside the parent, which is not left: its default entry, below it.
                    let mut below = Vec::new();
                    self.enter_down(parent, &mut below);
                    out.extend(&below[1..]);
                    continue;
                }
                parent
            } else {
                to
            };
            let mut cur = to;
            while let Some(&p) = self.parent.get(cur) {
                if p == domain {
                    break;
                }
                out.push(p);
                cur = p;
            }
            self.enter_down(to, &mut out);
        }
        let mut i = 0;
        while i < out.len() {
            let q = self.states[out[i]];
            if q.kind == StateKind::Parallel {
                for c in q.states.iter().filter(|c| !c.kind.is_history()) {
                    let covered = out.iter().any(|&x| x == c.id || self.inside(x, &c.id));
                    if !covered {
                        self.enter_down(&c.id, &mut out);
                    }
                }
            }
            i += 1;
        }
        let mut seen = HashSet::new();
        out.retain(|x| seen.insert(*x));
        out
    }

    /// Every state history state `h` could restore: each child of its parent with its
    /// default entry for a shallow one, every state below the parent for a deep one. A loop
    /// through a history state goes back into what it recorded, so the cycle check counts
    /// all of these, not only the parent's default entry.
    fn could_restore(&self, h: &'a str) -> Vec<&'a str> {
        let parent = self.states[self.parent[h]];
        let mut out = Vec::new();
        for c in parent.states.iter().filter(|c| !c.kind.is_history()) {
            if self.states[h].kind == StateKind::DeepHistory {
                self.below(&c.id, &mut out);
            } else {
                self.enter_down(&c.id, &mut out);
            }
        }
        out
    }

    /// `id` and every state below it, history states left out.
    fn below(&self, id: &'a str, out: &mut Vec<&'a str>) {
        out.push(id);
        for c in self.states[id]
            .states
            .iter()
            .filter(|c| !c.kind.is_history())
        {
            self.below(&c.id, out);
        }
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
                if !t.targets.is_empty() && follow(t) {
                    for s in self.entry_set(x, t) {
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
        if !options
            .allow_unassigned
            .contains(&project.files[info.file].path)
        {
            crate::assign::check(m, info, diags);
        }
        // Another package's charts are its own business: only errors are reported in them.
        if project.files[info.file].file.package.is_none() {
            let root_file = project.roots.contains(&info.file);
            warnings(&tree, info, root_file, options, diags);
            message_warnings(doc, m, info, root_file, diags);
        }

        // States entered without waiting: through `always`, `done` and invoke outcomes,
        // which can come at once. Only `on` (a spawned child's `on name.done` and
        // `on name.fail` included: no child exists until something spawned it, and a peer's
        // `on name.ended`, which arrives on the queue) and `after` wait.
        let waits = |t: &ir::Transition| {
            matches!(
                t.trigger,
                Trigger::Event { .. }
                    | Trigger::Emitted { .. }
                    | Trigger::Call { .. }
                    | Trigger::Any
                    | Trigger::After { .. }
                    | Trigger::SpawnDone { .. }
                    | Trigger::SpawnFail { .. }
                    | Trigger::Ended { .. }
            )
        };
        let eager = tree.reachable(|t| !waits(t));
        let mut edges = Vec::new();
        for id in &tree.order {
            let s = tree.states[id];
            if !eager.contains(id) {
                continue;
            }
            let si = info.states.get(*id);
            if let Some(inv) = &s.invoke
                && inv.kind == InvokeKind::Machine
                && let Some(span) = si.and_then(|i| i.invoke_span)
            {
                edges.push((inv.src.as_str(), span));
            }
            // A spawn in the `entry` of a state entered without waiting, or in a transition
            // taken without waiting, starts its machine without waiting too; so does one in
            // the `exit` of the state such a transition is written in, or of a state around
            // it that it leaves. (A transition to a top-level final state ends the run, and
            // what its exits spawn never starts.)
            if let Some(si) = si {
                for target in spawned(m, &s.entry) {
                    edges.push((target, si.name_span));
                }
                for (i, t) in s.transitions.iter().enumerate() {
                    if waits(t) {
                        continue;
                    }
                    let Some(r) = si.transition_reads.get(i) else {
                        continue;
                    };
                    for target in spawned(m, &t.effects) {
                        edges.push((target, r.span));
                    }
                    if t.targets.is_empty() {
                        continue;
                    }
                    let ends = t.targets.iter().any(|to| {
                        tree.parent.get(to.as_str()) == Some(&tree.root)
                            && tree.states.get(to.as_str()).map(|s| s.kind)
                                == Some(StateKind::Final)
                    });
                    if ends {
                        continue;
                    }
                    for left in tree.left_by(id, t) {
                        for target in spawned(m, &tree.states[left].exit) {
                            edges.push((target, r.span));
                        }
                    }
                }
            }
        }
        invokes_eagerly.push((name.as_str(), edges));
    }
    recursion(&invokes_eagerly, diags);
}

/// Warns of each transition that leaves a parallel state it is written in, towards a target
/// inside it, so that the parallel state is entered again and some of its regions start over
/// by default entry: the outermost such parallel state, with the regions no target is in. A
/// target that is a history state of the parallel state restores every region, and the
/// branches a `retry` adds are left out.
fn regions_reset(tree: &Tree<'_>, info: &MachineInfo, diags: &mut Vec<Diagnostic>) {
    for &id in &tree.order[1..] {
        let s = tree.states[id];
        let Some(si) = info.states.get(id) else {
            continue;
        };
        for (i, t) in s.transitions.iter().enumerate() {
            if t.targets.is_empty() || t.internal || si.retry_branches.contains(&i) {
                continue;
            }
            let left = tree.left_by(id, t);
            let Some(&p) = left.iter().rev().find(|&&p| {
                p != id
                    && tree.states[p].kind == StateKind::Parallel
                    && t.targets.iter().any(|x| tree.inside(x, p))
            }) else {
                continue;
            };
            let restores_all = t.targets.iter().any(|x| {
                tree.parent.get(x.as_str()) == Some(&p)
                    && tree
                        .states
                        .get(x.as_str())
                        .is_some_and(|h| h.kind.is_history())
            });
            if restores_all {
                continue;
            }
            let reset: Vec<&str> = tree.states[p]
                .states
                .iter()
                .filter(|c| !c.kind.is_history())
                .filter(|c| {
                    !t.targets
                        .iter()
                        .any(|x| x == &c.id || tree.inside(x, &c.id))
                })
                .map(|c| c.id.as_str())
                .collect();
            if reset.is_empty() {
                continue;
            }
            let Some(r) = si.transition_reads.get(i) else {
                continue;
            };
            let names: Vec<String> = reset.iter().map(|c| format!("`{}`", short(c, p))).collect();
            let what = if names.len() == 1 {
                format!("{} starts over from its initial state", names[0])
            } else {
                format!("{} start over from their initial states", list(&names))
            };
            let below = t.targets.iter().all(|x| tree.inside(x, id));
            let help = if below {
                let targets: Vec<String> = t
                    .targets
                    .iter()
                    .map(|x| format!(".{}", short(x, id)))
                    .collect();
                format!(
                    "to keep `{}` and the other regions, write the target with a leading dot, `-> {}`, which changes only the states below `{}`; to start `{}` over on purpose, target it: `-> {}`",
                    short(id, p),
                    targets.join(", "),
                    short(id, p),
                    short(p, tree.root),
                    short(p, tree.root)
                )
            } else {
                format!(
                    "to keep the other regions where they are, write the transition inside the region it changes, or in `{0}` with a leading dot (`-> .{1}`), or list a target in every region; to start `{0}` over on purpose, target it: `-> {0}`",
                    short(p, tree.root),
                    t.targets
                        .iter()
                        .map(|x| short(x, p))
                        .collect::<Vec<_>>()
                        .join(", .")
                )
            };
            diags.push(
                Diagnostic::warning(
                    codes::REGIONS_RESET,
                    r.span,
                    format!(
                        "this transition leaves parallel state `{}` and enters it again: {what}",
                        short(p, tree.root)
                    ),
                )
                .with_help(help),
            );
        }
    }
}

/// `id` relative to its ancestor `of`: `P.A.X` relative to `P` is `A.X`.
fn short<'s>(id: &'s str, of: &str) -> &'s str {
    id.strip_prefix(of)
        .and_then(|rest| rest.strip_prefix('.'))
        .unwrap_or(id)
}

/// `a`, `a and b`, `a, b and c`.
fn list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
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
        let t = &s.transitions[i];
        match t.targets.first().map(String::as_str) {
            _ if t.internal => {
                if let Some(span) = trigger_span(id, i) {
                    diags.push(
                        Diagnostic::error(
                            codes::ALWAYS_CYCLE,
                            span,
                            format!("`always` with no guard and an internal target is taken again and again: it never leaves `{id}`, so `{id}` never settles"),
                        )
                        .with_help("give it a guard that its own effects make false, or remove the dot so that it leaves the state"),
                    );
                }
            }
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
            Some(_) if id == tree.root => {
                // The machine itself is never left, so its `always` is taken again after every
                // step, unless the machine has finished.
                let ends = t.targets.iter().any(|to| {
                    tree.parent.get(to.as_str()) == Some(&tree.root)
                        && tree
                            .states
                            .get(to.as_str())
                            .is_some_and(|t| t.kind == StateKind::Final)
                });
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
            Some(_) => {
                nodes.push(id);
                let mut entered = tree.entry_set(id, t);
                for to in &t.targets {
                    if let Some((&to, _)) = tree.states.get_key_value(to.as_str())
                        && tree.states[to].kind.is_history()
                    {
                        entered.extend(tree.could_restore(to));
                    }
                }
                edges.insert(id, entered);
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

/// The machines the `spawn` effects of a list start.
fn spawned<'m>(m: &'m ir::Machine, effects: &'m [ir::Effect]) -> Vec<&'m str> {
    effects
        .iter()
        .filter_map(|e| match e {
            ir::Effect::Spawn { children, .. } => {
                m.children.get(children).map(|c| c.machine.as_str())
            }
            _ => None,
        })
        .collect()
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
            format!("`{}` starts itself", cycle[0])
        } else {
            format!("`{}` starts itself through {}", cycle[0], path.join(" -> "))
        };
        let mut d = Diagnostic::error(
            codes::UNBOUNDED_RECURSION,
            first,
            format!("{what} without waiting for an event or an `after` (unbounded recursion)"),
        )
        .with_label("entered without waiting")
        .with_help(
            "put an `on` or `after` transition on the way to the state that invokes or spawns it",
        );
        for (i, m) in cycle.iter().enumerate().skip(1) {
            if let Some(s) = span(m, next(i)) {
                d = d.with_secondary(s, format!("then `{m}` starts `{}`", next(i)));
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
    // A `retry`'s own branches are not a way out: they stop once the retries run out.
    let leaves = |s: &ir::State| {
        let added = info.states.get(&s.id).map(|i| &i.retry_branches);
        s.transitions.iter().enumerate().any(|(i, t)| {
            !t.targets.is_empty()
                && !matches!(t.trigger, Trigger::Done)
                && !added.is_some_and(|a| a.contains(&i))
        })
    };
    let targeted: HashSet<&str> = tree
        .order
        .iter()
        .flat_map(|id| &tree.states[id].transitions)
        .flat_map(|t| t.targets.iter().map(String::as_str))
        .collect();
    for &id in &tree.order[1..] {
        let s = tree.states[id];
        let Some(si) = info.states.get(id) else {
            continue;
        };
        let parent = tree.parent[id];
        if s.kind.is_history() {
            // Never entered itself: only whether anything goes to it matters.
            if !targeted.contains(id) {
                let word = if s.kind == StateKind::DeepHistory {
                    "deep history"
                } else {
                    "history"
                };
                diags.push(
                    Diagnostic::warning(
                        codes::UNUSED_HISTORY,
                        si.name_span,
                        format!("{word} state `{id}` is never used"),
                    )
                    .with_help(format!("no transition targets it; a transition to it returns to where `{parent}` was when it was last left")),
                );
            }
            continue;
        }
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
                    format!(
                        "state `{}` has no way out",
                        si.written_as.as_deref().unwrap_or(id)
                    ),
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
    regions_reset(tree, info, diags);
    for (c, span) in &info.children {
        if root_file && !info.spawned.contains(c) {
            diags.push(
                Diagnostic::warning(
                    codes::NEVER_SPAWNED,
                    *span,
                    format!("no `spawn` fills the collection `{c}`"),
                )
                .with_help(format!(
                    "start its children with `spawn {c}[key] = Machine(…)`, or remove it"
                )),
            );
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
    if root_file {
        for (c, span) in &info.calls {
            if !info.taken_calls.contains(c) {
                diags.push(
                    Diagnostic::warning(
                        codes::CALL_NEVER_TAKEN,
                        *span,
                        format!("no `on {c}` of `{}` takes the call `{c}`", info.name),
                    )
                    .with_help(format!(
                        "every `{c}` call is dropped and its caller gets `Unanswered`; take it with `on {c}` in the states that answer it"
                    )),
                );
            }
        }
        for (v, span) in &info.call_vars {
            if !info.replied.contains(v) {
                diags.push(
                    Diagnostic::warning(
                        codes::CALL_NEVER_REPLIED,
                        *span,
                        format!("`{v}` holds a pending call that no `reply` answers"),
                    )
                    .with_help(format!(
                        "answer it with `reply {v} done …` or `reply {v} fail …`; a call left unanswered fails with `Unanswered` when `{}` ends",
                        info.name
                    )),
                );
            }
        }
    }
}

/// Whether a machine's IR has an `emit` of `event` anywhere.
fn emits_somewhere(s: &ir::State, event: &str) -> bool {
    let has = |effects: &[ir::Effect]| {
        effects
            .iter()
            .any(|e| matches!(e, ir::Effect::Emit { event: x, .. } if x == event))
    };
    has(&s.entry)
        || has(&s.exit)
        || s.transitions.iter().any(|t| has(&t.effects))
        || s.states.iter().any(|c| emits_somewhere(c, event))
}

/// `emits` that the machine never emits, and `on name.E` for an `E` that the sender never
/// emits.
fn message_warnings(
    doc: &ir::Ir,
    m: &ir::Machine,
    info: &MachineInfo,
    root_file: bool,
    diags: &mut Vec<Diagnostic>,
) {
    if root_file {
        for (e, span) in &info.emits {
            if !emits_somewhere(&m.root, e) {
                diags.push(
                    Diagnostic::warning(
                        codes::NEVER_EMITTED,
                        *span,
                        format!("`{}` declares `emits {e}` but never emits it", info.name),
                    )
                    .with_help(format!(
                        "emit it with `emit {e}` where it happens, or remove the declaration"
                    )),
                );
            }
        }
    }
    for (span, machine, event) in &info.receives {
        if let Some(sender) = doc.machines.get(machine)
            && sender.emits.contains_key(event)
            && !emits_somewhere(&sender.root, event)
        {
            diags.push(
                Diagnostic::warning(
                    codes::RECEIVES_NOTHING,
                    *span,
                    format!("`{machine}` declares `emits {event}` but never emits it, so this is never taken"),
                )
                .with_help(format!("`{machine}` has no `emit {event}`")),
            );
        }
    }
}

/// A function, action or task, by the project path of its file and its name; or an emitted
/// event, by the file of its machine and `Machine.Event`.
pub(crate) type DeclKey = (String, String);

/// A declaration an "unused" warning may be given for, if no checked file uses it.
#[derive(Clone, Debug)]
pub(crate) struct UnusedCandidate {
    pub key: DeclKey,
    /// "function", "action", "task", or "emitted event" for an `emits` no `on` receives.
    pub what: &'static str,
    pub span: Span,
}

/// The functions, actions and tasks of a compilation, and those it uses.
pub(crate) fn uses(
    ck: &Checker<'_>,
    project: &Project,
) -> (Vec<UnusedCandidate>, HashSet<DeclKey>) {
    use roadbook_syntax::ast::DeclKind;
    let key = |file: usize, name: &str| (project.files[file].path.clone(), name.to_string());
    let mut candidates = Vec::new();
    for e in &ck.names.decls {
        if e.duplicate || ck.is_compiled(e.file) {
            continue;
        }
        let what = match &e.decl.kind {
            DeclKind::Fn { .. } => "function",
            DeclKind::Action { .. } => "action",
            DeclKind::Task { .. } => "task",
            DeclKind::Machine(m) => {
                for x in m.body.emits() {
                    let name = format!("{}.{}", e.decl.name.name, x.name.name);
                    candidates.push(UnusedCandidate {
                        key: key(e.file, &name),
                        what: "emitted event",
                        span: x.name.span,
                    });
                }
                continue;
            }
            _ => continue,
        };
        {
            candidates.push(UnusedCandidate {
                key: key(e.file, &e.decl.name.name),
                what,
                span: e.decl.name.span,
            });
        }
    }
    let mut used: HashSet<DeclKey> = ck
        .used
        .iter()
        .filter_map(|n| {
            ck.names
                .global
                .get(n.as_str())
                .map(|id| key(ck.names.decls[id.0].file, n))
        })
        .collect();
    for (machine, event) in &ck.received {
        if let Some(id) = ck.names.global.get(machine.as_str()) {
            used.insert(key(
                ck.names.decls[id.0].file,
                &format!("{machine}.{event}"),
            ));
        }
    }
    (candidates, used)
}

/// The "unused" warning for a candidate.
pub(crate) fn unused_warning(c: &UnusedCandidate) -> Diagnostic {
    if c.what == "emitted event" {
        let (machine, event) = c.key.1.split_once('.').unwrap_or(("", &c.key.1));
        return Diagnostic::warning(
            codes::NEVER_RECEIVED,
            c.span,
            format!("nothing receives the event `{event}` that `{machine}` emits"),
        )
        .with_help(format!(
            "no checked chart receives it with `on <name>.{event}`; check the charts that use `{machine}` together with this one, or remove the declaration (host code holding a handle may still receive it)"
        ));
    }
    Diagnostic::warning(
        codes::UNUSED_DECLARATION,
        c.span,
        format!("{} `{}` is never used", c.what, c.key.1),
    )
    .with_help("remove it, or use it in a machine")
}
