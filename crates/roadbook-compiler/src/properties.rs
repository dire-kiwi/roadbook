//! The evaluation of `check` lines: each is a search over the machine's state space
//! ([`statespace`]), which [`crate::check`] explores once per machine that has checks, in the
//! entry file of each compilation. [`crate::compile`] evaluates none, so `gen`, `export` and
//! the Rust macro never do.
//!
//! The graph keeps free what the chart does not decide (guards, which timer expires first,
//! what other machines do and whether they act first: children and spawned children ending,
//! peers emitting, a peer answering a call the machine made, another run calling it;
//! [`statespace::Assumption`]), so a check can be proved, refuted, or hold on conditions
//! ([`Verdict`]):
//!
//! - `never S1 and … Sn` fails if some configuration, a transient one of settling or a final
//!   one included, has every `Si` active; otherwise it is proved.
//! - `reachable S` fails if no configuration has `S` active, which proves `S` unreachable
//!   whatever the guards and the other machines do. It is proved if a path to `S` relies on no
//!   assumption, and conditional otherwise.
//! - `S only after T` fails if some path reaches a configuration with `S` active without having
//!   passed through one with `T` active: `T` active in the same configuration counts, and so
//!   does the initial one. Otherwise it is proved.
//! - `can finish` fails if some configuration cannot reach a top-level final state. It is
//!   proved if from every configuration the host can bring the run to a final state whatever
//!   the guards decide and whatever other machines do first: by choosing its events, task
//!   outcomes and timers sure to expire first ([`Eval::sure`]). A call's answer and a call
//!   from another run are other machines' moves, not the host's. Otherwise it is conditional: a path exists, but something the chart
//!   does not decide could block it.
//!
//! A failure is an error at the check, `RB0501` to `RB0504`, with the counterexample that
//! relies on the fewest assumptions (then the shortest): one label per transition, and one
//! per guard it assumes of a transition it did not take, at their spans, numbered in path
//! order, and the whole path as text in the help, with the assumptions it relies on, since
//! the data, the timers' delays or the other machines may block it at run time. A machine
//! that is too large to explore gets the warning `RB0912` at each of its checks.
//!
//! The graph covers runs started from the chart: a run restored from a snapshot made by an
//! earlier version of the chart may be in a configuration this one cannot reach, and a
//! passing check says nothing about it.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, VecDeque};

use roadbook_ir as ir;
use roadbook_syntax::diagnostic::Code;
use roadbook_syntax::{Diagnostic, FileId, Pos, Span};

use crate::checks::{Check, Property, StatePath};
use crate::codes;
use crate::statespace::{
    self, Assumption, EdgeId, Graph, Input, MAX_SENDER_NAMES, NodeId, Sender, StateIndex, TooLarge,
    guard_text, trigger_text,
};

/// What evaluating a check found.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Verdict {
    /// It holds whatever the data.
    Proved,
    /// It holds only on conditions the chart does not decide: a `reachable` or `can finish`
    /// whose every witness relies on assumptions (guards, the order of timers, what other
    /// machines do).
    Conditional,
    /// It does not hold, even with every guard free: an error.
    Failed,
    /// The machine has too many configurations to explore (`RB0912`).
    Unchecked,
}

impl Verdict {
    /// The JSON form: `proved`, `conditional`, `failed`, `unchecked`.
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Proved => "proved",
            Verdict::Conditional => "conditional",
            Verdict::Failed => "failed",
            Verdict::Unchecked => "unchecked",
        }
    }

    /// The text form: `proved`, `conditional`, `failed`, `not checked`.
    pub fn text(self) -> &'static str {
        match self {
            Verdict::Proved => "proved",
            Verdict::Conditional => "conditional",
            Verdict::Failed => "failed",
            Verdict::Unchecked => "not checked",
        }
    }
}

/// A transition taken on a check's path, as `roadbook paths --json` gives its steps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    /// The id of the state the transition belongs to.
    pub from: String,
    /// Its index among that state's transitions.
    pub index: usize,
    /// The trigger, as the viewer labels it.
    pub trigger: String,
    /// The guard's text, `else` for the unguarded last branch of a guarded block.
    pub guard: Option<String>,
    /// The targets' ids, as the IR's `targets`: `[]` for a transition with no target.
    pub to: Vec<String>,
}

/// A check and its verdict.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckResult {
    /// The machine the check is written in.
    pub machine: String,
    /// The property, as the formatter writes it after `check`: `never A and B`.
    pub check: String,
    /// The check's doc comment, `""` if none.
    pub doc: String,
    /// The check line.
    pub span: Span,
    pub verdict: Verdict,
    /// What the path relies on, in path order, each once: "`approved()` holds", "`after 5s` in
    /// `A` expires before the other running timers", "peer `inventory` emits `Reserved`".
    /// For `can finish`, those of the part of the path from the first configuration that
    /// the host cannot bring to a final state whatever the guards decide.
    pub assumptions: Vec<String>,
    /// The transitions of the path from the initial configuration: the counterexample of a
    /// failed check, the witness of a `reachable` that holds, and for a `can finish` that is
    /// conditional, the way to the first configuration from which the host cannot force the end and
    /// on to a final state. Empty when there is none.
    pub path: Vec<Step>,
    /// The path as text, each configuration as its active leaves below the machine:
    /// `Idle --Go--> Paying.Charging + Log.Open`. Empty when there is no path.
    pub path_text: String,
}

/// Evaluates the checks written in the compilation's entry file (file 0), exploring each
/// machine that has one once, with at most `budget` configurations. Failures and `RB0912` go
/// to `diags`.
pub(crate) fn evaluate(
    ir: &ir::Ir,
    checks: &[Check],
    files: &[(String, String)],
    budget: usize,
    diags: &mut Vec<Diagnostic>,
) -> Vec<CheckResult> {
    let entry: Vec<&Check> = checks.iter().filter(|c| c.span.file == FileId(0)).collect();
    let mut machines: Vec<&str> = Vec::new();
    for c in &entry {
        if !machines.contains(&c.machine.as_str()) {
            machines.push(&c.machine);
        }
    }
    let mut out = Vec::new();
    for name in machines {
        let Some(machine) = ir.machines.get(name) else {
            continue;
        };
        let mine = entry.iter().filter(|c| c.machine == name);
        match statespace::explore(ir, machine, budget) {
            Ok(graph) => {
                let eval = Eval::new(machine, &graph, files);
                for c in mine {
                    out.push(eval.check(c, diags));
                }
            }
            Err(too_large) => {
                let (message, help) = match &too_large {
                    TooLarge::Nodes { .. } => (
                        format!("not checked: `{name}` has more than {budget} configurations"),
                        "a check explores every configuration the machine can be in, and gives up past this many; regions and history multiply them".to_string(),
                    ),
                    TooLarge::Senders { event, names } => (
                        format!(
                            "not checked: `{name}` expects `{event}` from a peer through {names} `ref` names, more than the {MAX_SENDER_NAMES} a check tries every set of"
                        ),
                        "any set of those names may hold the peer that sends it, and each set is an input of its own".to_string(),
                    ),
                    TooLarge::Combinations { budget } => (
                        format!(
                            "not checked: the transitions of `{name}`'s regions combine in more than {budget} ways"
                        ),
                        "each input is tried with every combination of the regions' guard outcomes; fewer regions with guards on the same input make fewer".to_string(),
                    ),
                };
                for c in mine {
                    diags.push(
                        Diagnostic::warning(codes::NOT_CHECKED, c.span, message.clone())
                            .with_help(help.clone()),
                    );
                    out.push(result(c, Verdict::Unchecked, Vec::new(), None));
                }
            }
        }
    }
    out
}

fn result(
    c: &Check,
    verdict: Verdict,
    assumptions: Vec<String>,
    path: Option<Shown>,
) -> CheckResult {
    let (path, path_text) = path.map_or((Vec::new(), String::new()), |p| (p.steps, p.text));
    CheckResult {
        machine: c.machine.clone(),
        check: c.text.clone(),
        doc: c.doc.clone(),
        span: c.span,
        verdict,
        assumptions,
        path,
        path_text,
    }
}

/// A path as a result shows it.
struct Shown {
    steps: Vec<Step>,
    text: String,
}

/// One machine's graph, with what describing its paths needs.
struct Eval<'a> {
    graph: &'a Graph,
    /// The machine's id, which the ids of its states start with.
    root: &'a str,
    states: HashMap<&'a str, &'a ir::State>,
    files: &'a [(String, String)],
}

impl<'a> Eval<'a> {
    fn new(machine: &'a ir::Machine, graph: &'a Graph, files: &'a [(String, String)]) -> Self {
        fn add<'s>(s: &'s ir::State, map: &mut HashMap<&'s str, &'s ir::State>) {
            map.insert(&s.id, s);
            for c in &s.states {
                add(c, map);
            }
        }
        let mut states = HashMap::new();
        add(&machine.root, &mut states);
        Eval {
            graph,
            root: &machine.root.id,
            states,
            files,
        }
    }

    fn check(&self, c: &Check, diags: &mut Vec<Diagnostic>) -> CheckResult {
        let g = self.graph;
        let start = g.start();
        let index = |p: &StatePath| {
            g.state_index(&p.id)
                .expect("a check names a state of its machine")
        };
        match &c.property {
            Property::Never(states) => {
                let all: Vec<StateIndex> = states.iter().map(index).collect();
                let together = |n: NodeId| all.iter().all(|&s| g.is_active(n, s));
                let Some(path) = g.fewest_assumptions_path(start, together) else {
                    return result(c, Verdict::Proved, Vec::new(), None);
                };
                let names: Vec<String> = states
                    .iter()
                    .map(|s| format!("`{}`", self.relative(&s.id)))
                    .collect();
                let what = match names.len() {
                    1 => format!("{} can be active", names[0]),
                    _ => format!("{} can be active together", and_list(&names)),
                };
                let message = format!("`{}` does not hold: {what}", c.text);
                self.fail(codes::NEVER_ACTIVE, c, message, &path, "", diags)
            }
            Property::Reachable(state) => {
                let s = index(state);
                let Some(path) = g.fewest_assumptions_path(start, |n| g.is_active(n, s)) else {
                    diags.push(
                        Diagnostic::error(
                            codes::UNREACHABLE_CHECK_STATE,
                            c.span,
                            format!(
                                "`{}` cannot be reached, whatever the guards",
                                self.relative(&state.id)
                            ),
                        )
                        .with_help(
                            "no path from the initial configuration enters it, even with each guard holding or failing as the path needs, any timer expiring first and other machines doing anything they can",
                        ),
                    );
                    return result(c, Verdict::Failed, Vec::new(), None);
                };
                let assumed = self.assumptions(&path);
                let verdict = if assumed.is_empty() {
                    Verdict::Proved
                } else {
                    Verdict::Conditional
                };
                result(c, verdict, assumed, Some(self.shown(&path)))
            }
            Property::OnlyAfter { state, after } => {
                let Some(path) = self.before(index(state), index(after)) else {
                    return result(c, Verdict::Proved, Vec::new(), None);
                };
                let message = format!(
                    "`{}` can be active before `{}` has been",
                    self.relative(&state.id),
                    self.relative(&after.id)
                );
                self.fail(codes::ACTIVE_BEFORE, c, message, &path, "", diags)
            }
            Property::CanFinish => {
                let can = self.reaches_final();
                if let Some(path) = g.fewest_assumptions_path(start, |n| !can[n.index()]) {
                    let name = self.root;
                    if !g.nodes().any(|n| g.is_final(n)) {
                        let message = format!(
                            "`{name}` can never finish: no path reaches a top-level final state"
                        );
                        diags.push(Diagnostic::error(codes::CANNOT_FINISH, c.span, message).with_help(
                            "`check can finish` asks that a top-level final state can be reached from every configuration the machine can be in",
                        ));
                        return result(c, Verdict::Failed, Vec::new(), None);
                    }
                    let message = format!(
                        "`{name}` can reach a configuration from which no final state can be reached"
                    );
                    let at = path.last().map_or(start, |&e| g.edge(e).to);
                    let tail = format!(
                        "; from `{}`, no final state can be reached",
                        self.config(at)
                    );
                    return self.fail(codes::CANNOT_FINISH, c, message, &path, &tail, diags);
                }
                let sure = self.sure();
                let Some(mut path) = g.fewest_assumptions_path(start, |n| !sure[n.index()]) else {
                    return result(c, Verdict::Proved, Vec::new(), None);
                };
                let at = path.last().map_or(start, |&e| g.edge(e).to);
                let on = g
                    .fewest_assumptions_path(at, |n| g.is_final(n))
                    .expect("every configuration can reach a final state");
                let assumed = self.assumptions(&on);
                path.extend(on);
                result(c, Verdict::Conditional, assumed, Some(self.shown(&path)))
            }
        }
    }

    /// The error of a check that fails with the counterexample `path`, and its result.
    /// `tail` ends the help.
    fn fail(
        &self,
        code: Code,
        c: &Check,
        message: String,
        path: &[EdgeId],
        tail: &str,
        diags: &mut Vec<Diagnostic>,
    ) -> CheckResult {
        let assumed = self.assumptions(path);
        let shown = self.shown(path);
        let mut help = if self.visible(path) {
            shown.text.clone()
        } else {
            format!("at the start: {}", shown.text)
        };
        if !assumed.is_empty() {
            help.push_str(&format!(", {}", assumption_phrase(&assumed)));
        }
        help.push_str(tail);
        let mut d = Diagnostic::error(code, c.span, message);
        for (span, label) in self.labels(path) {
            d = d.with_secondary(span, label);
        }
        diags.push(d.with_help(help));
        result(c, Verdict::Failed, assumed, Some(shown))
    }

    /// A path from the start to a configuration with `s` active that has not passed through
    /// one with `t` active (that configuration included), relying on as few guard outcomes
    /// as it can, then the shortest: a search of the graph's product with "has `t` been
    /// active", as [`Graph::fewest_assumptions_path`] searches the graph.
    fn before(&self, s: StateIndex, t: StateIndex) -> Option<Vec<EdgeId>> {
        let g = self.graph;
        let key = |n: NodeId, seen: bool| n.index() * 2 + usize::from(seen);
        let mut best: Vec<Option<(u32, u32)>> = vec![None; g.node_count() * 2];
        let mut prev: Vec<Option<(EdgeId, usize)>> = vec![None; g.node_count() * 2];
        let start = g.start();
        let seen = g.is_active(start, t);
        best[key(start, seen)] = Some((0, 0));
        let mut heap = BinaryHeap::from([Reverse(((0u32, 0u32), start, seen))]);
        while let Some(Reverse((cost, n, seen))) = heap.pop() {
            let k = key(n, seen);
            if best[k] != Some(cost) {
                continue;
            }
            if !seen && g.is_active(n, s) {
                let mut path = Vec::new();
                let mut at = k;
                while let Some((e, from)) = prev[at] {
                    path.push(e);
                    at = from;
                }
                path.reverse();
                return Some(path);
            }
            for &e in g.outgoing(n) {
                let edge = g.edge(e);
                let m = edge.to;
                let now = seen || g.is_active(m, t);
                let next = (cost.0 + edge.assumptions.len() as u32, cost.1 + 1);
                let j = key(m, now);
                if best[j].is_none_or(|b| next < b) {
                    best[j] = Some(next);
                    prev[j] = Some((e, k));
                    heap.push(Reverse((next, m, now)));
                }
            }
        }
        None
    }

    /// For each node, whether a final node can be reached from it, through any steps.
    fn reaches_final(&self) -> Vec<bool> {
        let g = self.graph;
        let mut ok = vec![false; g.node_count()];
        let mut queue: VecDeque<NodeId> = g.nodes().filter(|&n| g.is_final(n)).collect();
        for n in &queue {
            ok[n.index()] = true;
        }
        while let Some(n) = queue.pop_front() {
            for &e in g.incoming(n) {
                let edge = g.edge(e);
                if !ok[edge.from.index()] {
                    ok[edge.from.index()] = true;
                    queue.push_back(edge.from);
                }
            }
        }
        ok
    }

    /// For each node, whether the host can bring the run from it to a final state whatever the
    /// guards decide: the attractor of the final nodes in a game where the host picks the
    /// input, the data the guard outcomes and other machines may act first. A final node is
    /// sure. A settling node, where nobody chooses, is sure if every step from it leads to a
    /// sure node. A stable node is sure if some input the host controls (one whose steps
    /// assume only guard outcomes and that no other machine acts first: the machine's own
    /// events, task outcomes, a timer sure to expire first) leads to a sure node whatever its
    /// guards decide, including when they all fail and it takes nothing, and every move of
    /// another machine from it (an edge that assumes [`Assumption::Machine`]) leads to a sure
    /// node too.
    fn sure(&self) -> Vec<bool> {
        let g = self.graph;
        let mut sure = vec![false; g.node_count()];
        let mut queue: VecDeque<NodeId> = g.nodes().filter(|&n| g.is_final(n)).collect();
        for n in &queue {
            sure[n.index()] = true;
        }
        while let Some(n) = queue.pop_front() {
            for &e in g.incoming(n) {
                let p = g.edge(e).from;
                if !sure[p.index()] && self.wins(p, &sure) {
                    sure[p.index()] = true;
                    queue.push_back(p);
                }
            }
        }
        sure
    }

    /// Whether node `n` is sure, given the nodes known to be (see [`Eval::sure`]).
    fn wins(&self, n: NodeId, sure: &[bool]) -> bool {
        let g = self.graph;
        let out = g.outgoing(n);
        let to_sure = |e: &EdgeId| sure[g.edge(*e).to.index()];
        if g.is_settling(n) {
            return !out.is_empty() && out.iter().all(to_sure);
        }
        // The other machines may act first: whatever they do must still be winnable.
        let theirs = out.iter().all(|e| {
            let edge = g.edge(*e);
            !edge
                .assumptions
                .iter()
                .any(|a| matches!(a, Assumption::Machine(_)))
                || sure[edge.to.index()]
        });
        if !theirs {
            return false;
        }
        let mut tried: Vec<&Input> = Vec::new();
        for &e in out {
            let input = &g.edge(e).input;
            if tried.contains(&input) {
                continue;
            }
            tried.push(input);
            let edges: Vec<&EdgeId> = out.iter().filter(|x| g.edge(**x).input == *input).collect();
            let controlled = edges.iter().all(|x| {
                g.edge(**x)
                    .assumptions
                    .iter()
                    .all(|a| matches!(a, Assumption::Guard { .. } | Assumption::Unpreempted { .. }))
            });
            if controlled && !g.takes_nothing(n, input) && edges.iter().all(|x| to_sure(x)) {
                return true;
            }
        }
        false
    }

    /// The id below the machine: `Processing.Paying`.
    fn relative<'s>(&self, id: &'s str) -> &'s str {
        id.strip_prefix(self.root)
            .and_then(|r| r.strip_prefix('.'))
            .unwrap_or(id)
    }

    fn state(&self, s: StateIndex) -> &'a ir::State {
        self.states[self.graph.state_id(s)]
    }

    /// A configuration as its active leaves below the machine, in document order: `A.B + C`.
    fn config(&self, n: NodeId) -> String {
        let g = self.graph;
        let active = g.active(n);
        let mut leaves = Vec::new();
        for (i, &s) in active.iter().enumerate() {
            let id = g.state_id(s);
            // A state's descendants come right after it in document order.
            let parent = active.get(i + 1).is_some_and(|&next| {
                g.state_id(next)
                    .strip_prefix(id)
                    .is_some_and(|r| r.starts_with('.'))
            });
            if !parent && id != self.root {
                leaves.push(self.relative(id));
            }
        }
        leaves.join(" + ")
    }

    /// Whether the path shows a step: whether any of its edges is not the end of settling.
    fn visible(&self, path: &[EdgeId]) -> bool {
        path.iter()
            .any(|&e| self.graph.edge(e).input != Input::Settled)
    }

    /// A transition as a step shows it: `Go [x > 0]`.
    fn step_text(&self, s: StateIndex, index: usize) -> String {
        let state = self.state(s);
        let trigger = trigger_text(&state.transitions[index].trigger);
        match guard_text(state, index) {
            Some(g) => format!("{trigger} [{g}]"),
            None => trigger,
        }
    }

    fn shown(&self, path: &[EdgeId]) -> Shown {
        let g = self.graph;
        let mut steps = Vec::new();
        let mut text = self.config(g.start());
        for &e in path {
            let edge = g.edge(e);
            let label = if edge.transitions.is_empty() {
                // A step that only consumes an invoke or a timer, which matters to what comes
                // next; the end of settling changes nothing to show.
                let trigger = match &edge.input {
                    Input::Settled => continue,
                    Input::After { state, index } => {
                        trigger_text(&self.state(*state).transitions[*index].trigger)
                    }
                    Input::InvokeDone { .. } => "done".to_string(),
                    Input::InvokeFail { error, .. } => format!("fail {error}"),
                    other => g.describe(other),
                };
                format!("{trigger} (no transition)")
            } else {
                let mut parts: Vec<String> = Vec::new();
                for t in &edge.transitions {
                    let part = self.step_text(t.state, t.index);
                    if !parts.contains(&part) {
                        parts.push(part);
                    }
                    let state = self.state(t.state);
                    let tr = &state.transitions[t.index];
                    steps.push(Step {
                        from: state.id.clone(),
                        index: t.index,
                        trigger: trigger_text(&tr.trigger),
                        guard: guard_text(state, t.index),
                        to: tr.targets.clone(),
                    });
                }
                parts.join(", ")
            };
            text.push_str(&format!(" --{label}--> {}", self.config(edge.to)));
        }
        Shown { steps, text }
    }

    /// One label per transition on the path, numbered, at the transition's span:
    /// `2. always [x > 0] -> B`.
    fn labels(&self, path: &[EdgeId]) -> Vec<(Span, String)> {
        let mut out = Vec::new();
        for &e in path {
            let edge = self.graph.edge(e);
            // The guards of transitions it did not take that it relies on: skipped on the way
            // to what it took, or, for a step that takes nothing, all of them.
            for a in &edge.assumptions {
                let Assumption::Guard { transition, holds } = a else {
                    continue;
                };
                if edge.transitions.contains(transition) {
                    continue;
                }
                let tr = &self.state(transition.state).transitions[transition.index];
                let label = format!(
                    "{}. {}: assumed to {}",
                    out.len() + 1,
                    self.step_text(transition.state, transition.index),
                    if *holds { "hold" } else { "fail" }
                );
                out.push((self.span(&tr.span), label));
            }
            for t in &edge.transitions {
                let tr = &self.state(t.state).transitions[t.index];
                let to = if tr.targets.is_empty() {
                    " (no target)".to_string()
                } else {
                    let targets: Vec<&str> =
                        tr.targets.iter().map(|id| self.relative(id)).collect();
                    format!(" -> {}", targets.join(", "))
                };
                let label = format!(
                    "{}. {}{to}",
                    out.len() + 1,
                    self.step_text(t.state, t.index)
                );
                out.push((self.span(&tr.span), label));
            }
        }
        out
    }

    /// What a path relies on, in path order, each once, as text.
    fn assumptions(&self, path: &[EdgeId]) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for &e in path {
            for a in &self.graph.edge(e).assumptions {
                if let Some(text) = self.assumption(a)
                    && !out.contains(&text)
                {
                    out.push(text);
                }
            }
        }
        out
    }

    /// An assumption as text: "`approved()` holds", "`after 5s` in `A` expires before the
    /// other running timers", "`Checkout` (invoked in `Paying`) ends `done`".
    fn assumption(&self, a: &Assumption) -> Option<String> {
        let invoked = |s: &StateIndex| {
            let state = self.state(*s);
            let src = state.invoke.as_ref().map_or("", |i| i.src.as_str());
            format!(
                "`{src}` (invoked in `{}`)",
                self.relative(self.graph.state_id(*s))
            )
        };
        Some(match a {
            Assumption::Guard { transition, holds } => {
                let state = self.state(transition.state);
                let guard = state.transitions[transition.index].guard.as_ref()?;
                let verb = if *holds { "holds" } else { "fails" };
                format!("`{}` {verb}", guard.text)
            }
            Assumption::TimerFirst { state, index } => format!(
                "`{}` in `{}` expires before the other running timers",
                trigger_text(&self.state(*state).transitions[*index].trigger),
                self.relative(self.graph.state_id(*state))
            ),
            Assumption::RaiseFirst {
                name: Some(name), ..
            } => format!("the delayed raise `{name}` expires before the other running timers"),
            Assumption::RaiseFirst { name: None, event } => {
                format!("a delayed `raise {event}` expires before the other running timers")
            }
            Assumption::Machine(input) => match input {
                Input::InvokeDone { state } => match self.call(state) {
                    Some((peer, call)) => format!("peer `{peer}` answers `{call}` with `done`"),
                    None => format!("{} ends `done`", invoked(state)),
                },
                Input::InvokeFail { state, error } => match self.call(state) {
                    Some((peer, call)) if error == ir::UNANSWERED => {
                        format!("peer `{peer}` leaves `{call}` unanswered")
                    }
                    Some((peer, call)) => {
                        format!("peer `{peer}` answers `{call}` with `fail {error}`")
                    }
                    None => format!("{} fails with `{error}`", invoked(state)),
                },
                Input::Call(c) => format!("another run calls `{c}`"),
                Input::Emitted { from, event } => match from {
                    Sender::Child(s) => format!("{} emits `{event}`", invoked(s)),
                    Sender::Refs(names) => {
                        let names: Vec<String> = names.iter().map(|n| format!("`{n}`")).collect();
                        if event == crate::statespace::ENDED {
                            // A peer's end (`on name.ended`), not an emitted event.
                            if names.len() == 1 {
                                format!("peer {} ends", names[0])
                            } else {
                                format!("the peer that {} hold ends", and_list(&names))
                            }
                        } else if names.len() == 1 {
                            format!("peer {} emits `{event}`", names[0])
                        } else {
                            format!("the peer that {} hold emits `{event}`", and_list(&names))
                        }
                    }
                    Sender::Children(c) => format!("a child of `{c}` emits `{event}`"),
                    Sender::Callers => format!("a caller emits `{event}`"),
                    Sender::Peers(names) => {
                        let names: Vec<String> = names.iter().map(|n| format!("`{n}`")).collect();
                        format!("a peer that {} holds emits `{event}`", and_list(&names))
                    }
                },
                Input::SpawnDone { children } => format!("a child of `{children}` ends `done`"),
                Input::SpawnFail { children, error } => {
                    format!("a child of `{children}` fails with `{error}`")
                }
                other => self.graph.describe(other),
            },
            Assumption::Unpreempted { senders } => {
                let names: Vec<String> = senders.iter().map(|x| self.sender(x)).collect();
                let verb = if names.len() == 1 { "does" } else { "do" };
                // A peer that answers a call, and a caller, act otherwise than by ending or
                // emitting.
                let calls = senders.iter().any(|x| match x {
                    Sender::Callers => true,
                    Sender::Refs(names) | Sender::Peers(names) => self.is_callee(names),
                    _ => false,
                });
                let act = if calls { "act" } else { "end or emit" };
                format!("{} {verb} not {act} first", and_list(&names))
            }
        })
    }

    /// Another machine, for a message: "`Child` (invoked in `A`)", "peer `inventory`", "the
    /// children of `kids`".
    fn sender(&self, s: &Sender) -> String {
        match s {
            Sender::Child(state) => {
                let src = self
                    .state(*state)
                    .invoke
                    .as_ref()
                    .map_or("", |i| i.src.as_str());
                format!(
                    "`{src}` (invoked in `{}`)",
                    self.relative(self.graph.state_id(*state))
                )
            }
            Sender::Refs(names) => {
                let names: Vec<String> = names.iter().map(|n| format!("`{n}`")).collect();
                if names.len() == 1 {
                    format!("peer {}", names[0])
                } else {
                    format!("the peer that {} hold", and_list(&names))
                }
            }
            Sender::Children(c) => format!("the children of `{c}`"),
            Sender::Callers => "the runs that call it".to_string(),
            Sender::Peers(names) => {
                let names: Vec<String> = names.iter().map(|n| format!("`{n}`")).collect();
                if names.len() == 1 {
                    format!("peer {}", names[0])
                } else {
                    format!("the peers that {} hold", and_list(&names))
                }
            }
        }
    }

    /// For the invoke of `s`, if it is a call: the name its peer is held by, and the call.
    fn call(&self, s: &StateIndex) -> Option<(&'a str, &'a str)> {
        let inv = self.state(*s).invoke.as_ref()?;
        let to = inv.to.as_ref()?;
        Some((to.name.as_str(), inv.src.as_str()))
    }

    /// Whether some state's invoke calls the peer that one of `names` holds.
    fn is_callee(&self, names: &[String]) -> bool {
        self.states.values().any(|st| {
            st.invoke
                .as_ref()
                .and_then(|i| i.to.as_ref())
                .is_some_and(|to| names.contains(&to.name))
        })
    }

    /// An IR span as a span of the compilation's files, with offsets.
    fn span(&self, s: &ir::Span) -> Span {
        let file = FileId(s.file);
        let text = self
            .files
            .get(s.file as usize)
            .map_or("", |(_, t)| t.as_str());
        Span {
            file,
            start: pos(text, s.line, s.col),
            end: pos(text, s.end_line, s.end_col),
        }
    }
}

/// The position at 1-based `line` and `col` (in Unicode scalar values) of `text`.
fn pos(text: &str, line: u32, col: u32) -> Pos {
    let start: usize = text
        .split_inclusive('\n')
        .take(line.saturating_sub(1) as usize)
        .map(str::len)
        .sum();
    let rest = &text[start..];
    let byte = rest
        .char_indices()
        .nth(col.saturating_sub(1) as usize)
        .map_or(rest.len(), |(i, _)| i);
    Pos {
        offset: (start + byte) as u32,
        line,
        col,
    }
}

/// "if `x > 0` holds and `ready` fails on the way".
fn assumption_phrase(assumptions: &[String]) -> String {
    format!("if {} on the way", and_list(assumptions))
}

/// "a", "a and b", "a, b and c".
fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// `checks: 6 proved, 2 conditional, 1 failed`: the nonzero counts of each verdict.
/// `None` when no check was evaluated.
pub fn summary(results: &[CheckResult]) -> Option<String> {
    if results.is_empty() {
        return None;
    }
    let parts: Vec<String> = [
        Verdict::Proved,
        Verdict::Conditional,
        Verdict::Failed,
        Verdict::Unchecked,
    ]
    .iter()
    .filter_map(|&v| {
        let n = results.iter().filter(|r| r.verdict == v).count();
        (n > 0).then(|| format!("{n} {}", v.text()))
    })
    .collect();
    Some(format!("checks: {}", parts.join(", ")))
}

/// Every check, one line each: `examples/pay.road:12: Pay: check never A and B: proved`.
/// Beneath it, indented: its doc comment, as `/// …` lines, and for a conditional check what it
/// relies on and its path.
pub fn list(results: &[CheckResult], files: &[(String, String)]) -> String {
    let mut out = String::new();
    for r in results {
        let file = files
            .get(r.span.file.0 as usize)
            .map_or("", |(p, _)| p.as_str());
        out.push_str(&format!(
            "{file}:{}: {}: check {}: {}\n",
            r.span.start.line,
            r.machine,
            r.check,
            r.verdict.text()
        ));
        for line in r.doc.lines() {
            out.push_str(format!("    /// {line}").trim_end());
            out.push('\n');
        }
        if r.verdict == Verdict::Conditional {
            out.push_str(&format!("    {}\n", assumption_phrase(&r.assumptions)));
            out.push_str(&format!("    {}\n", r.path_text));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MemoryLoader, Options};

    /// `check` of `src`: the results, and the rendered diagnostics.
    fn run(src: &str) -> (Vec<CheckResult>, String) {
        run_with(src, &Options::default())
    }

    fn run_with(src: &str, options: &Options) -> (Vec<CheckResult>, String) {
        let mut loader = MemoryLoader::new().with("m.road", src);
        let out = crate::check(&mut loader, &["m.road".to_string()], options).unwrap();
        (out.checks.clone(), out.render())
    }

    /// The verdict of each check, by its text.
    fn verdicts(results: &[CheckResult]) -> Vec<(&str, Verdict)> {
        results
            .iter()
            .map(|r| (r.check.as_str(), r.verdict))
            .collect()
    }

    const PAY: &str = "\
fn ok() -> bool
machine M() {
  event Go
  event Back
  initial Idle
  state Idle {
    on Go if ok() -> Paying
  }
  state Paying {
    on Back -> Idle
    always -> Charged
  }
  state Charged { on Go -> Done }
  state Refunded {}
  final state Done {}
";

    fn chart(checks: &str) -> String {
        format!("{PAY}{checks}}}\n")
    }

    #[test]
    fn never_is_proved_or_fails_with_a_counterexample() {
        let (r, rendered) = run(&chart(
            "  check never Charged\n  check never Idle and Paying\n",
        ));
        assert_eq!(
            verdicts(&r),
            [
                ("never Charged", Verdict::Failed),
                ("never Idle and Paying", Verdict::Proved)
            ]
        );
        assert!(
            rendered
                .contains("error[RB0501]: `never Charged` does not hold: `Charged` can be active"),
            "{rendered}"
        );
        // The labels, at each transition, and the path with the guard it needs.
        assert!(rendered.contains("1. Go [ok()] -> Paying"), "{rendered}");
        assert!(rendered.contains("2. always -> Charged"), "{rendered}");
        assert!(
            rendered.contains(
                "help: Idle --Go [ok()]--> Paying --always--> Charged, if `ok()` holds on the way"
            ),
            "{rendered}"
        );
        assert_eq!(r[0].assumptions, ["`ok()` holds"]);
        let to = |s: &str| Some(s.to_string());
        assert_eq!(
            r[0].path,
            [
                Step {
                    from: "M.Idle".into(),
                    index: 0,
                    trigger: "Go".into(),
                    guard: to("ok()"),
                    to: vec!["M.Paying".into()],
                },
                Step {
                    from: "M.Paying".into(),
                    index: 1,
                    trigger: "always".into(),
                    guard: None,
                    to: vec!["M.Charged".into()],
                },
            ]
        );
        // The settling configuration counts: `Paying` is passed through.
        let (r, rendered) = run(&chart("  check never Paying\n"));
        assert_eq!(verdicts(&r), [("never Paying", Verdict::Failed)]);
        assert!(
            rendered.contains("help: Idle --Go [ok()]--> Paying, if"),
            "{rendered}"
        );
    }

    #[test]
    fn a_counterexample_with_no_assumptions_says_nothing_of_them() {
        let src = "machine M() {\n  event Go\n  initial A\n  state A { on Go -> B }\n  state B {}\n  check never B\n}\n";
        let (r, rendered) = run(src);
        assert_eq!(r[0].verdict, Verdict::Failed);
        assert!(r[0].assumptions.is_empty());
        assert!(rendered.contains("= help: A --Go--> B\n"), "{rendered}");
        // At the start: no steps.
        let (r, rendered) = run(&src.replace("never B", "never A"));
        assert_eq!(r[0].verdict, Verdict::Failed);
        assert!(r[0].path.is_empty());
        assert!(rendered.contains("= help: at the start: A\n"), "{rendered}");
    }

    #[test]
    fn never_together_in_regions() {
        let src = "\
fn ok() -> bool
machine M() {
  event Go
  initial P
  parallel state P {
    state L {
      initial L1
      state L1 { on Go -> L2 }
      state L2 {}
    }
    state R {
      initial R1
      state R1 { on Go if ok() -> R2 }
      state R2 {}
    }
  }
  check never P.L.L1 and P.R.R2
  check never P.L.L2 and P.R.R1
}
";
        let (r, rendered) = run(src);
        // Both regions take `Go` in one microstep, so L1 and R2 are never active together;
        // R1's guard may fail, so L2 and R1 can be.
        assert_eq!(
            verdicts(&r),
            [
                ("never P.L.L1 and P.R.R2", Verdict::Proved),
                ("never P.L.L2 and P.R.R1", Verdict::Failed)
            ]
        );
        assert!(
            rendered.contains("`P.L.L2` and `P.R.R1` can be active together"),
            "{rendered}"
        );
        assert!(
            rendered.contains(
                "help: P.L.L1 + P.R.R1 --Go--> P.L.L2 + P.R.R1, if `ok()` fails on the way"
            ),
            "{rendered}"
        );
    }

    #[test]
    fn reachable_is_proved_conditional_or_fails() {
        let (r, rendered) = run(&chart(
            "  check reachable Idle\n  check reachable Charged\n  check reachable Refunded\n",
        ));
        assert_eq!(
            verdicts(&r),
            [
                ("reachable Idle", Verdict::Proved),
                ("reachable Charged", Verdict::Conditional),
                ("reachable Refunded", Verdict::Failed)
            ]
        );
        assert_eq!(r[1].assumptions, ["`ok()` holds"]);
        assert_eq!(
            r[1].path_text,
            "Idle --Go [ok()]--> Paying --always--> Charged"
        );
        assert!(r[0].path.is_empty() && r[0].path_text == "Idle");
        assert!(
            rendered.contains("error[RB0502]: `Refunded` cannot be reached, whatever the guards"),
            "{rendered}"
        );
    }

    #[test]
    fn only_after_searches_paths_that_have_not_passed_through_the_state() {
        let src = "\
fn ok() -> bool
machine M() {
  event Go
  event Skip
  initial Idle
  state Idle {
    on Go -> Checked
    on Skip if ok() -> Paid
  }
  state Checked { on Go -> Paid }
  state Paid { on Go -> Idle }
  check Paid only after Checked
  check Checked only after Idle
}
";
        let (r, rendered) = run(src);
        assert_eq!(
            verdicts(&r),
            [
                ("Paid only after Checked", Verdict::Failed),
                ("Checked only after Idle", Verdict::Proved)
            ]
        );
        assert!(
            rendered.contains("error[RB0503]: `Paid` can be active before `Checked` has been"),
            "{rendered}"
        );
        assert!(
            rendered.contains("help: Idle --Skip [ok()]--> Paid, if `ok()` holds on the way"),
            "{rendered}"
        );
        // Once `Checked` has been active, a later visit to `Paid` by the other way is fine.
        let fine = src.replace("on Skip if ok() -> Paid", "on Skip -> Idle");
        let (r, _) = run(&fine);
        assert_eq!(r[0].verdict, Verdict::Proved);
    }

    #[test]
    fn only_after_counts_a_state_entered_in_the_same_microstep() {
        let src = "\
machine M() {
  event Go
  initial Idle
  state Idle { on Go -> P }
  parallel state P {
    state L {
      initial T
      state T {}
    }
    state R {
      initial S
      state S {}
    }
  }
  check P.R.S only after P.L.T
  check P.L.T only after P.R.S
  check Idle only after Idle
  check P only after P.L.T
}
";
        let (r, _) = run(src);
        assert!(
            r.iter().all(|r| r.verdict == Verdict::Proved),
            "{:?}",
            verdicts(&r)
        );
    }

    #[test]
    fn can_finish_is_proved_conditional_or_fails() {
        let proved = "machine M() {\n  event Go\n  initial A\n  state A { on Go -> B }\n  final state B {}\n  check can finish\n}\n";
        let (r, _) = run(proved);
        assert_eq!(r[0].verdict, Verdict::Proved);
        assert!(r[0].path.is_empty());

        // From B the only way to the end is guarded.
        let guarded = "\
fn ok() -> bool
machine M() {
  event Go
  initial A
  state A { on Go -> B }
  state B { on Go if ok() -> C }
  final state C {}
  check can finish
}
";
        let (r, rendered) = run(guarded);
        assert_eq!(r[0].verdict, Verdict::Conditional, "{rendered}");
        assert_eq!(r[0].assumptions, ["`ok()` holds"]);
        assert_eq!(r[0].path_text, "A --Go--> B --Go [ok()]--> C");
        assert!(rendered.is_empty(), "{rendered}");

        let stuck = "\
machine M() {
  event Go
  event Stop
  initial A
  state A {
    on Go -> B
    on Stop -> Loop
  }
  state Loop { on Go -> Loop }
  final state B {}
  check can finish
}
";
        let (r, rendered) = run(stuck);
        assert_eq!(r[0].verdict, Verdict::Failed);
        assert!(
            rendered.contains(
                "error[RB0504]: `M` can reach a configuration from which no final state can be reached"
            ),
            "{rendered}"
        );
        assert!(
            rendered.contains("help: A --Stop--> Loop; from `Loop`, no final state can be reached"),
            "{rendered}"
        );

        let never = "machine M() {\n  event Go\n  initial A\n  state A { on Go -> B }\n  state B { on Go -> A }\n  check can finish\n}\n";
        let (r, rendered) = run(never);
        assert_eq!(r[0].verdict, Verdict::Failed);
        assert!(rendered.contains("`M` can never finish"), "{rendered}");
    }

    #[test]
    fn a_timer_that_never_comes_first_makes_can_finish_conditional() {
        // A real run loops A, B, A, …: the 1 s timer always expires before the 5 s one.
        let src = "machine M() {\n  initial A\n  state A {\n    after 1s -> B\n    after 5s -> C\n  }\n  state B { after 1s -> A }\n  final state C {}\n  check can finish\n  check reachable B\n  check reachable C\n}\n";
        let (r, rendered) = run(src);
        assert!(rendered.is_empty(), "{rendered}");
        assert_eq!(
            verdicts(&r),
            [
                ("can finish", Verdict::Conditional),
                ("reachable B", Verdict::Proved),
                ("reachable C", Verdict::Conditional)
            ]
        );
        let timer = "`after 5s` in `A` expires before the other running timers";
        assert_eq!(r[0].assumptions, [timer]);
        assert_eq!(r[0].path_text, "A --after 5s--> C");
        assert_eq!(r[2].assumptions, [timer]);
    }

    #[test]
    fn what_another_machine_does_is_an_assumption() {
        let src = "\
error Lost
machine Kid() ! Lost {
  emits Ping
  initial A
  state A { entry emit Ping }
}
machine M(peer: ref Kid) {
  children kids: Kid by string
  initial A
  state A {
    invoke Kid() as kid {
      done      -> Bad
      fail Lost -> Gone
    }
    on kid.Ping -> Pinged
  }
  state Pinged {
    on peer.Ping -> Spawning
  }
  state Spawning {
    entry spawn kids[\"a\"] = Kid()
    on kids.done -> Bad
  }
  final state Bad {}
  final state Gone {}
  check never Bad
  check reachable Gone
  check reachable Spawning
}
";
        let (r, rendered) = run(src);
        assert_eq!(
            verdicts(&r),
            [
                ("never Bad", Verdict::Failed),
                ("reachable Gone", Verdict::Conditional),
                ("reachable Spawning", Verdict::Conditional)
            ],
            "{rendered}"
        );
        assert!(
            rendered.contains(
                "help: A --done--> Bad, if `Kid` (invoked in `A`) ends `done` on the way"
            ),
            "{rendered}"
        );
        assert_eq!(
            r[1].assumptions,
            ["`Kid` (invoked in `A`) fails with `Lost`"]
        );
        assert_eq!(
            r[2].assumptions,
            [
                "`Kid` (invoked in `A`) emits `Ping`",
                "peer `peer` emits `Ping`"
            ]
        );
    }

    /// A call's answer, `Unanswered` included, and a call from another run are other
    /// machines' moves: they are assumptions, they race the machine's own inputs, and the host
    /// cannot count on them to finish.
    #[test]
    fn calls_are_moves_of_other_machines() {
        let src = "\
error Busy
machine Desk() {
  call take(n: int) -> int ! Busy
  event Close
  initial Open
  state Open {
    on take -> Taken  reply call done event.n
    on Close -> Shut
  }
  state Taken {}
  final state Shut {}
  check reachable Taken
  check can finish
}
machine Asker(desk: ref Desk) {
  event Quit
  initial Asking
  state Asking {
    invoke desk.take(1) {
      done            -> Got
      fail Busy       -> Busy
      fail Unanswered -> Lost
    }
    on Quit -> Got
  }
  final state Got {}
  final state Busy {}
  final state Lost {}
  check reachable Lost
  check reachable Got
  check can finish
}
";
        let (r, rendered) = run(src);
        assert_eq!(
            verdicts(&r),
            [
                ("reachable Taken", Verdict::Conditional),
                ("can finish", Verdict::Failed),
                ("reachable Lost", Verdict::Conditional),
                ("reachable Got", Verdict::Conditional),
                ("can finish", Verdict::Proved)
            ],
            "{rendered}"
        );
        assert_eq!(r[0].assumptions, ["another run calls `take`"]);
        assert_eq!(r[2].assumptions, ["peer `desk` leaves `take` unanswered"]);
        // `Quit` is the host's, but the peer may answer first.
        assert_eq!(r[3].assumptions, ["peer `desk` does not act first"]);
    }

    #[test]
    fn can_finish_is_proved_when_every_outcome_of_a_guard_finishes() {
        let src = "\
fn approved() -> bool
machine M() {
  event Pay
  initial Idle
  state Idle {
    on Pay {
      if approved() -> Charged
      else          -> Declined
    }
  }
  final state Charged {}
  final state Declined {}
  check can finish
}
";
        let (r, rendered) = run(src);
        assert_eq!(
            verdicts(&r),
            [("can finish", Verdict::Proved)],
            "{rendered}"
        );
        // With no `else`, the guard may fail and leave the run where it is: the host cannot
        // force the end.
        let (r, _) = run(&src.replace("      else          -> Declined\n", ""));
        assert_eq!(r[0].verdict, Verdict::Conditional);
        assert_eq!(r[0].assumptions, ["`approved()` holds"]);
        // A child machine's outcome is not the host's to give.
        let child = "machine Kid() {\n  initial A\n  final state A {}\n}\nmachine M() {\n  initial A\n  state A {\n    invoke Kid() {\n      done -> B\n    }\n  }\n  final state B {}\n  check can finish\n}\n";
        let (r, _) = run(child);
        assert_eq!(r[0].verdict, Verdict::Conditional);
        assert_eq!(r[0].assumptions, ["`Kid` (invoked in `A`) ends `done`"]);
    }

    #[test]
    fn a_guard_assumed_of_a_step_that_takes_nothing_has_a_label() {
        let src = "fn ok() -> bool\nmachine M() {\n  initial A\n  state A { after 1s if ok() -> Done }\n  final state Done {}\n  check can finish\n}\n";
        let (r, rendered) = run(src);
        assert_eq!(r[0].verdict, Verdict::Failed);
        assert!(r[0].path.is_empty());
        assert_eq!(r[0].assumptions, ["`ok()` fails"]);
        assert!(
            rendered.contains("1. after 1s [ok()]: assumed to fail"),
            "{rendered}"
        );
        assert!(
            rendered.contains("help: A --after 1s (no transition)--> A, if `ok()` fails on the way; from `A`, no final state can be reached"),
            "{rendered}"
        );
        // A skipped `always` before an event is taken.
        let src = "fn ok() -> bool\nmachine M() {\n  event Go\n  initial A\n  state A {\n    always if ok() -> Done\n    on Go -> B\n  }\n  state B {}\n  final state Done {}\n  check never B\n}\n";
        let (_, rendered) = run(src);
        assert!(
            rendered.contains("1. always [ok()]: assumed to fail")
                && rendered.contains("2. Go -> B"),
            "{rendered}"
        );
    }

    #[test]
    fn never_reports_the_path_with_the_fewest_assumptions_in_all() {
        let src = "fn a() -> bool\nfn b() -> bool\nfn c() -> bool\nfn d() -> bool\nfn e() -> bool\nmachine M() {\n  event Go\n  event Next\n  initial A\n  state A {\n    on Go {\n      if a() -> Z\n      if b() -> Z\n      if c() -> T\n    }\n    on Next if d() -> B\n  }\n  state B { on Next if e() -> T }\n  state Z {}\n  state T {}\n  check never T\n}\n";
        let (r, _) = run(src);
        assert_eq!(r[0].assumptions, ["`d()` holds", "`e()` holds"]);
    }

    #[test]
    fn a_path_that_another_machine_can_preempt_is_conditional() {
        let src = "\
machine Child() {
  initial Done
  final state Done {}
}
machine Parent() {
  initial A
  state A {
    invoke Child() {
      done -> D
    }
    after 1s -> T
  }
  final state T {}
  final state D {}
  check reachable T
  check can finish
  check never T
}
";
        let (r, rendered) = run(src);
        assert_eq!(
            verdicts(&r),
            [
                ("reachable T", Verdict::Conditional),
                ("can finish", Verdict::Proved),
                ("never T", Verdict::Failed)
            ],
            "{rendered}"
        );
        let race = "`Child` (invoked in `A`) does not end or emit first";
        assert_eq!(r[0].assumptions, [race]);
        assert_eq!(r[2].assumptions, [race]);
        assert!(
            rendered.contains(&format!("help: A --after 1s--> T, if {race} on the way")),
            "{rendered}"
        );
    }

    #[test]
    fn can_finish_wins_against_every_move_of_another_machine_or_not_at_all() {
        let chart = |done: &str| {
            format!(
                "\
fn ok() -> bool
error Lost
machine Kid() ! Lost {{
  initial A
  final state A {{}}
}}
machine M() {{
  event Stop
  event Go
  initial A
  state A {{
    invoke Kid() {{
      done      -> {done}
      fail Lost -> End
    }}
    on Stop -> End
  }}
  state W {{ on Go if ok() -> End }}
  final state End {{}}
  check can finish
}}
"
            )
        };
        // Whatever the child does ends the run, and `Stop` does too.
        let (r, rendered) = run(&chart("End"));
        assert_eq!(r[0].verdict, Verdict::Proved, "{rendered}");
        // Its `done` leads to W, where `Go`'s guard may keep failing: the host could end the
        // run with `Stop`, but the child may end first.
        let (r, rendered) = run(&chart("W"));
        assert_eq!(r[0].verdict, Verdict::Conditional, "{rendered}");
        assert_eq!(
            r[0].assumptions,
            ["`Kid` (invoked in `A`) does not end or emit first"]
        );
    }

    #[test]
    fn past_the_budget_checks_are_not_checked() {
        let src = "machine M() {\n  event Go\n  initial A\n  state A { on Go -> B }\n  state B { on Go -> A }\n  check reachable B\n  check can finish\n}\n";
        let options = Options {
            max_configurations: Some(1),
            ..Options::default()
        };
        let (r, rendered) = run_with(src, &options);
        assert_eq!(
            verdicts(&r),
            [
                ("reachable B", Verdict::Unchecked),
                ("can finish", Verdict::Unchecked)
            ]
        );
        assert_eq!(rendered.matches("warning[RB0912]").count(), 2, "{rendered}");
        assert!(
            rendered.contains("not checked: `M` has more than 1 configurations"),
            "{rendered}"
        );
        assert_eq!(summary(&r).unwrap(), "checks: 2 not checked");
        // A peer's event expected through one name more than a check tries every set of.
        let (r, rendered) = run(&peers(MAX_SENDER_NAMES + 1));
        assert_eq!(verdicts(&r), [("never B", Verdict::Unchecked)]);
        assert!(
            rendered.contains("not checked: `M` expects `Ready` from a peer through 9 `ref` names, more than the 8 a check tries every set of"),
            "{rendered}"
        );
    }

    /// A machine that expects a peer's `Ready` through `n` `ref` names, each its own way to `B`.
    fn peers(n: usize) -> String {
        let names: Vec<String> = (0..n).map(|i| format!("p{i}: ref Peer")).collect();
        let ons: String = (0..n)
            .map(|i| format!("    on p{i}.Ready -> B\n"))
            .collect();
        format!(
            "machine Peer() {{\n  emits Ready\n  initial A\n  state A {{ entry emit Ready }}\n}}\nmachine M({}) {{\n  event Go\n  initial A\n  state A {{\n{ons}  }}\n  state B {{ on Go -> A }}\n  check never B\n}}\n",
            names.join(", ")
        )
    }

    #[test]
    fn as_many_peer_names_as_a_check_tries_explore_quickly() {
        // Each set of names is an input of its own, but the race each edge relies on names the
        // peers together, so the work grows with the sets and not with their square.
        let started = std::time::Instant::now();
        let (r, rendered) = run(&peers(MAX_SENDER_NAMES));
        assert_eq!(verdicts(&r), [("never B", Verdict::Failed)], "{rendered}");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn compile_evaluates_no_check_and_check_only_the_entrys() {
        let lib = "machine L() {\n  initial A\n  state A {}\n  check can finish\n}\n";
        let main = "import { L } from \"./lib.road\"\nmachine M() {\n  initial A\n  state A { invoke L() { done -> B } }\n  final state B {}\n  check never A\n}\n";
        let mut loader = MemoryLoader::new()
            .with("lib.road", lib)
            .with("main.road", main);
        let compiled = crate::compile(&mut loader, "main.road", &Options::default()).unwrap();
        assert!(compiled.checks.is_empty());
        assert!(!compiled.has_errors(), "{}", compiled.render());
        let checked =
            crate::check(&mut loader, &["main.road".to_string()], &Options::default()).unwrap();
        assert_eq!(verdicts(&checked.checks), [("never A", Verdict::Failed)]);
        // Both roots: each check once.
        let both = crate::check(
            &mut loader,
            &["main.road".to_string(), "lib.road".to_string()],
            &Options::default(),
        )
        .unwrap();
        assert_eq!(
            verdicts(&both.checks),
            [
                ("never A", Verdict::Failed),
                ("can finish", Verdict::Failed)
            ]
        );
    }

    #[test]
    fn the_summary_and_the_list() {
        let (r, _) = run(&chart(
            "  check reachable Idle\n  /// Charged, if approved.\n  check reachable Charged\n  check never Refunded\n  check never Charged\n",
        ));
        assert_eq!(
            summary(&r).unwrap(),
            "checks: 2 proved, 1 conditional, 1 failed"
        );
        assert_eq!(summary(&[]), None);
        let files = vec![("m.road".to_string(), String::new())];
        let line = PAY.lines().count() + 1;
        assert_eq!(
            list(&r, &files),
            format!(
                "m.road:{line}: M: check reachable Idle: proved\n\
                 m.road:{}: M: check reachable Charged: conditional\n    /// Charged, if approved.\n    if `ok()` holds on the way\n    Idle --Go [ok()]--> Paying --always--> Charged\n\
                 m.road:{}: M: check never Refunded: proved\n\
                 m.road:{}: M: check never Charged: failed\n",
                line + 2,
                line + 3,
                line + 4
            )
        );
    }
}
