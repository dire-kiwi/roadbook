//! The evaluation of `check` lines: each is a search over the machine's state space
//! ([`statespace`]), which [`crate::check`] explores once per machine that has checks, in the
//! entry file of each compilation. [`crate::compile`] evaluates none, so `gen`, `export` and
//! the Rust macro never do.
//!
//! The graph keeps every guard free, so a check can be proved, refuted, or hold only if guards
//! allow ([`Verdict`]):
//!
//! - `never S1 and … Sn` fails if some configuration, a transient one of settling or a final
//!   one included, has every `Si` active; otherwise it is proved.
//! - `reachable S` fails if no configuration has `S` active, which proves `S` unreachable
//!   whatever the guards. It is proved if a path to `S` relies on no guard outcome, and holds
//!   if guards allow otherwise.
//! - `S only after T` fails if some path reaches a configuration with `S` active without having
//!   passed through one with `T` active: `T` active in the same configuration counts, and so
//!   does the initial one. Otherwise it is proved.
//! - `can finish` fails if some configuration cannot reach a top-level final state. It is
//!   proved if every configuration can reach one through steps that rely on no guard outcome,
//!   and holds if guards allow otherwise.
//!
//! A failure is an error at the check, `RB0501` to `RB0504`, with the counterexample that
//! relies on the fewest guard outcomes (then the shortest): one label per transition, at its
//! span, and the whole path as text in the help, with the guard outcomes it relies on, since
//! the data may block it at run time. A machine with more configurations than the budget is
//! not explored, and each of its checks gets the warning `RB0910`.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, VecDeque};

use roadbook_ir as ir;
use roadbook_syntax::diagnostic::Code;
use roadbook_syntax::{Diagnostic, FileId, Pos, Span};

use crate::checks::{Check, Property, StatePath};
use crate::codes;
use crate::statespace::{
    self, Assumption, EdgeId, Graph, Input, NodeId, StateIndex, guard_text, trigger_text,
};

/// What evaluating a check found.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Verdict {
    /// It holds whatever the data.
    Proved,
    /// It holds only if some guards hold or fail as the path needs: a `reachable` or `can
    /// finish` whose every witness relies on guard outcomes.
    IfGuardsAllow,
    /// It does not hold, even with every guard free: an error.
    Failed,
    /// The machine has too many configurations to explore (`RB0910`).
    Unchecked,
}

impl Verdict {
    /// The JSON form: `proved`, `ifGuardsAllow`, `failed`, `unchecked`.
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Proved => "proved",
            Verdict::IfGuardsAllow => "ifGuardsAllow",
            Verdict::Failed => "failed",
            Verdict::Unchecked => "unchecked",
        }
    }

    /// The text form: `proved`, `if guards allow`, `failed`, `not checked`.
    pub fn text(self) -> &'static str {
        match self {
            Verdict::Proved => "proved",
            Verdict::IfGuardsAllow => "if guards allow",
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
    /// The target's id; `None` for an internal transition.
    pub to: Option<String>,
}

/// An outcome of a guard that a path relies on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    /// The guard's text: `x > 0`.
    pub guard: String,
    /// Whether it holds on the path, or fails.
    pub holds: bool,
}

impl Outcome {
    /// `x > 0 holds`, `ready fails`.
    pub fn text(&self) -> String {
        format!("{} {}", self.guard, self.verb())
    }

    fn verb(&self) -> &'static str {
        if self.holds { "holds" } else { "fails" }
    }
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
    /// The guard outcomes the path relies on, in path order, each once. For `can finish`,
    /// those of the part of the path from the first configuration that cannot finish
    /// unconditionally.
    pub guards: Vec<Outcome>,
    /// The transitions of the path from the initial configuration: the counterexample of a
    /// failed check, the witness of a `reachable` that holds, and for a `can finish` that
    /// holds if guards allow, the way to the first configuration that cannot finish
    /// unconditionally and on to a final state. Empty when there is none.
    pub path: Vec<Step>,
    /// The path as text, each configuration as its active leaves below the machine:
    /// `Idle --Go--> Paying.Charging + Log.Open`. Empty when there is no path.
    pub path_text: String,
}

/// Evaluates the checks written in the compilation's entry file (file 0), exploring each
/// machine that has one once, with at most `budget` configurations. Failures and `RB0910` go
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
            Err(_) => {
                for c in mine {
                    diags.push(
                        Diagnostic::warning(
                            codes::NOT_CHECKED,
                            c.span,
                            format!("not checked: `{name}` has more than {budget} configurations"),
                        )
                        .with_help(
                            "a check explores every configuration the machine can be in, and gives up past this many; regions and history multiply them",
                        ),
                    );
                    out.push(result(c, Verdict::Unchecked, Vec::new(), None));
                }
            }
        }
    }
    out
}

fn result(c: &Check, verdict: Verdict, guards: Vec<Outcome>, path: Option<Shown>) -> CheckResult {
    let (path, path_text) = path.map_or((Vec::new(), String::new()), |p| (p.steps, p.text));
    CheckResult {
        machine: c.machine.clone(),
        check: c.text.clone(),
        doc: c.doc.clone(),
        span: c.span,
        verdict,
        guards,
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
                            "no path from the initial configuration enters it, even with each guard holding or failing as the path needs",
                        ),
                    );
                    return result(c, Verdict::Failed, Vec::new(), None);
                };
                let guards = self.guards(&path);
                let verdict = if guards.is_empty() {
                    Verdict::Proved
                } else {
                    Verdict::IfGuardsAllow
                };
                result(c, verdict, guards, Some(self.shown(&path)))
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
                let can = self.reaches_final(false);
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
                let sure = self.reaches_final(true);
                let Some(mut path) = g.fewest_assumptions_path(start, |n| !sure[n.index()]) else {
                    return result(c, Verdict::Proved, Vec::new(), None);
                };
                let at = path.last().map_or(start, |&e| g.edge(e).to);
                let on = g
                    .fewest_assumptions_path(at, |n| g.is_final(n))
                    .expect("every configuration can reach a final state");
                let guards = self.guards(&on);
                path.extend(on);
                result(c, Verdict::IfGuardsAllow, guards, Some(self.shown(&path)))
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
        let guards = self.guards(path);
        let shown = self.shown(path);
        let mut help = if self.visible(path) {
            shown.text.clone()
        } else {
            format!("at the start: {}", shown.text)
        };
        if !guards.is_empty() {
            help.push_str(&format!(", {}", guard_phrase(&guards)));
        }
        help.push_str(tail);
        let mut d = Diagnostic::error(code, c.span, message);
        for (span, label) in self.labels(path) {
            d = d.with_secondary(span, label);
        }
        diags.push(d.with_help(help));
        result(c, Verdict::Failed, guards, Some(shown))
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
                let next = (cost.0 + u32::from(!edge.assumptions.is_empty()), cost.1 + 1);
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

    /// For each node, whether a final node can be reached from it: through any steps, or,
    /// with `sure`, only through steps that rely on no guard outcome.
    fn reaches_final(&self, sure: bool) -> Vec<bool> {
        let g = self.graph;
        let mut ok = vec![false; g.node_count()];
        let mut queue: VecDeque<NodeId> = g.nodes().filter(|&n| g.is_final(n)).collect();
        for n in &queue {
            ok[n.index()] = true;
        }
        while let Some(n) = queue.pop_front() {
            for &e in g.incoming(n) {
                let edge = g.edge(e);
                if (sure && !edge.assumptions.is_empty()) || ok[edge.from.index()] {
                    continue;
                }
                ok[edge.from.index()] = true;
                queue.push_back(edge.from);
            }
        }
        ok
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
                        to: tr.target.clone(),
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
            for t in &self.graph.edge(e).transitions {
                let tr = &self.state(t.state).transitions[t.index];
                let to = match &tr.target {
                    Some(id) => format!(" -> {}", self.relative(id)),
                    None => " (internal)".to_string(),
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

    /// The guard outcomes a path relies on, in path order, each once.
    fn guards(&self, path: &[EdgeId]) -> Vec<Outcome> {
        let mut out: Vec<Outcome> = Vec::new();
        for &e in path {
            for &Assumption { transition, holds } in &self.graph.edge(e).assumptions {
                let state = self.state(transition.state);
                let Some(guard) = &state.transitions[transition.index].guard else {
                    continue;
                };
                let g = Outcome {
                    guard: guard.text.clone(),
                    holds,
                };
                if !out.contains(&g) {
                    out.push(g);
                }
            }
        }
        out
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
fn guard_phrase(guards: &[Outcome]) -> String {
    let parts: Vec<String> = guards
        .iter()
        .map(|o| format!("`{}` {}", o.guard, o.verb()))
        .collect();
    format!("if {} on the way", and_list(&parts))
}

/// "a", "a and b", "a, b and c".
fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// `checks: 6 proved, 2 if guards allow, 1 failed`: the nonzero counts of each verdict.
/// `None` when no check was evaluated.
pub fn summary(results: &[CheckResult]) -> Option<String> {
    if results.is_empty() {
        return None;
    }
    let parts: Vec<String> = [
        Verdict::Proved,
        Verdict::IfGuardsAllow,
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

/// Every check, one line each: `examples/pay.road:12: Pay: check never A and B: proved`. A
/// check that holds if guards allow has the guards and its path beneath, indented.
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
        if r.verdict == Verdict::IfGuardsAllow {
            out.push_str(&format!("    {}\n", guard_phrase(&r.guards)));
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

    fn texts(guards: &[Outcome]) -> Vec<String> {
        guards.iter().map(Outcome::text).collect()
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
        assert_eq!(texts(&r[0].guards), ["ok() holds"]);
        let to = |s: &str| Some(s.to_string());
        assert_eq!(
            r[0].path,
            [
                Step {
                    from: "M.Idle".into(),
                    index: 0,
                    trigger: "Go".into(),
                    guard: to("ok()"),
                    to: to("M.Paying"),
                },
                Step {
                    from: "M.Paying".into(),
                    index: 1,
                    trigger: "always".into(),
                    guard: None,
                    to: to("M.Charged"),
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
    fn a_counterexample_with_no_guards_says_nothing_of_them() {
        let src = "machine M() {\n  event Go\n  initial A\n  state A { on Go -> B }\n  state B {}\n  check never B\n}\n";
        let (r, rendered) = run(src);
        assert_eq!(r[0].verdict, Verdict::Failed);
        assert!(r[0].guards.is_empty());
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
    fn reachable_is_proved_holds_if_guards_allow_or_fails() {
        let (r, rendered) = run(&chart(
            "  check reachable Idle\n  check reachable Charged\n  check reachable Refunded\n",
        ));
        assert_eq!(
            verdicts(&r),
            [
                ("reachable Idle", Verdict::Proved),
                ("reachable Charged", Verdict::IfGuardsAllow),
                ("reachable Refunded", Verdict::Failed)
            ]
        );
        assert_eq!(texts(&r[1].guards), ["ok() holds"]);
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
    fn can_finish_is_proved_holds_if_guards_allow_or_fails() {
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
        assert_eq!(r[0].verdict, Verdict::IfGuardsAllow, "{rendered}");
        assert_eq!(texts(&r[0].guards), ["ok() holds"]);
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
        assert_eq!(rendered.matches("warning[RB0910]").count(), 2, "{rendered}");
        assert!(
            rendered.contains("not checked: `M` has more than 1 configurations"),
            "{rendered}"
        );
        assert_eq!(summary(&r).unwrap(), "checks: 2 not checked");
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
            "  check reachable Idle\n  check reachable Charged\n  check never Refunded\n  check never Charged\n",
        ));
        assert_eq!(
            summary(&r).unwrap(),
            "checks: 2 proved, 1 if guards allow, 1 failed"
        );
        assert_eq!(summary(&[]), None);
        let files = vec![("m.road".to_string(), String::new())];
        let line = PAY.lines().count() + 1;
        assert_eq!(
            list(&r, &files),
            format!(
                "m.road:{line}: M: check reachable Idle: proved\n\
                 m.road:{}: M: check reachable Charged: if guards allow\n    if `ok()` holds on the way\n    Idle --Go [ok()]--> Paying --always--> Charged\n\
                 m.road:{}: M: check never Refunded: proved\n\
                 m.road:{}: M: check never Charged: failed\n",
                line + 1,
                line + 2,
                line + 3
            )
        );
    }
}
