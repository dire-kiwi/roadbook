//! `roadbook paths <Machine> [file] [--json] [--max N]`
//!
//! The paths through a machine from its initial state to each final state, to each state
//! with no way out, and to where each loop closes: one test to write per path, and the list a
//! coverage tool compares traces with.
//!
//! Without `file`, every `.road` file of the project is compiled and the machine is the one
//! declaration of that name among them (a usage error, exit 2, if there is none or there are
//! several, listing where). Declarations are told apart by the file they are in, as the
//! loader locates it, so a file of a package in the project, compiled as a project file and
//! imported through the package by another, declares its machine once. With `file`, the machine is taken from that file's compilation:
//! declared there or imported. Compile errors are reported as `check` reports them (exit 1).
//!
//! The graph:
//!
//! - A position is a leaf state: atomic, final, or parallel (a parallel state is one
//!   position; the paths do not go inside its regions). The path starts at the machine's
//!   initial leaf: the root's `initial`, then each compound's `initial` in turn. Entering a
//!   target enters its initial descendants the same way; a target inside a parallel state
//!   puts the position at the outermost parallel state containing it.
//! - A step takes one transition with a target (an internal transition is not a step). From
//!   a position the transitions available are its own and then each ancestor's, innermost
//!   first, each state's in written order; from a parallel state, the transitions in its
//!   regions whose target is outside it (not `done`) come first, in written order, since they
//!   leave it as `roadbook check` (RB0902) counts them. Each branch of a guarded block is a step of its
//!   own, showing its guard (`[else]` for the unguarded last branch of a block with guards).
//!   A `done` is available only from a parallel state (its own) and from a final state (its
//!   parent's). An `always` with no guard (or a block ending in one) is taken at once, so
//!   when one is available the steps are the `always` branches up to and including it.
//! - A final state directly under the root ends the path: `final`. Any other final state
//!   ends its parent: the steps from it are the parent's `done` transitions, or, if the
//!   parent has none, what the position allows otherwise.
//! - A position with no step at all ends the path: `stuck`.
//! - No path takes the same transition (owner state, index) twice, so each loop is taken at
//!   most once. A position whose every step was taken already on the path ends it: `loop`,
//!   where going on would repeat a transition. So a machine with no final or stuck state
//!   (`A ⇄ B`) still has paths, and every transition the search can reach is on one.
//! - A step to a history state enters what it recorded on the path so far, or, with no record,
//!   its parent's initial descendants (a parallel parent: the parallel state's position). As
//!   the runtime records on each exit (`recordHistory` in `spec/execution.md`), each step
//!   records, before it enters anything, for each state it leaves on the way from the position
//!   to the transition's domain (the history state's parent counted as the parent of a target
//!   that is one): a shallow history state, the position of the child that was active, entered
//!   from its start; a deep one, the position itself. History inside a parallel position needs
//!   no record: whatever it restores, the position is the parallel state.
//! - Child machines are not entered: an invoke's `done` and `fail` are ordinary steps, and so
//!   are a call's (`fail Unanswered` among them) and a callee's `on c`, labelled `c`.
//! - The search is depth-first in that order, so the output is deterministic. It stops after
//!   `--max` paths (1000 by default) or [`BUDGET`] steps searched, prints what it found and
//!   says on standard error that it stopped; the exit code is still 0.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use roadbook_compiler::loader::FileRef;
use roadbook_compiler::statespace::{guard_text, trigger_text};
use roadbook_compiler::{Compilation, Ir, Loader};
use roadbook_ir::{Machine, State, StateKind, Transition, Trigger};

use crate::workspace::Workspace;
use crate::{Exit, report};

/// The most steps one search tries before it stops.
pub const BUDGET: usize = 1_000_000;

/// A path whose text is longer than this is printed one step per line.
const WIDTH: usize = 100;

#[derive(clap::Args)]
pub struct Args {
    /// The machine.
    machine: String,
    /// The `.road` file to compile, which declares or imports the machine. Default: the one
    /// file of the project that declares it.
    file: Option<PathBuf>,
    /// Print the paths as one JSON object on standard output.
    #[arg(long)]
    json: bool,
    /// Stop after this many paths.
    #[arg(long, value_name = "N", default_value_t = 1000,
          value_parser = clap::value_parser!(u64).range(1..))]
    max: u64,
}

pub fn run(args: Args) -> Exit {
    let (ir, exit) = match &args.file {
        Some(file) => compile_file(file, &args.machine),
        None => search(&args.machine),
    };
    let Some(ir) = ir else { return exit };
    let machine = &ir.machines[&args.machine];
    let found = paths(
        machine,
        usize::try_from(args.max).unwrap_or(usize::MAX),
        BUDGET,
    );
    if args.json {
        print!("{}", json(&args.machine, &found));
    } else {
        print!("{}", text(&args.machine, machine, &found));
    }
    match found.stopped {
        Some(Stop::Max) => eprintln!(
            "stopped after {} paths; the machine has more: use --max to raise it",
            found.paths.len()
        ),
        Some(Stop::Budget) => eprintln!(
            "stopped after searching {BUDGET} steps with {} paths found; the machine may have more",
            found.paths.len()
        ),
        None => {}
    }
    Exit::Ok
}

/// Compiles `file` and checks it has the machine.
fn compile_file(file: &std::path::Path, name: &str) -> (Option<Ir>, Exit) {
    if !file.is_file() {
        return (
            None,
            report::fatal(format!("`{}` is not a file", file.display())),
        );
    }
    let ws = match Workspace::find(Some(file)) {
        Ok(ws) => ws,
        Err(e) => return (None, report::fatal(e)),
    };
    let entry = ws.project_path(file);
    let out = match roadbook_compiler::compile(&mut ws.loader(), &entry, &ws.options()) {
        Ok(out) => out,
        Err(e) => return (None, report::fatal(e)),
    };
    let exit = report::diagnostics(&out, false);
    let Some(ir) = out.ir else {
        return (None, exit);
    };
    if !ir.machines.contains_key(name) {
        let names: Vec<&str> = ir.machines.keys().map(String::as_str).collect();
        let has = if names.is_empty() {
            "it has none".to_string()
        } else {
            format!("it has: {}", names.join(", "))
        };
        return (
            None,
            report::fatal(format!("no machine `{name}` in `{entry}` ({has})")),
        );
    }
    (Some(ir), exit)
}

/// Compiles every file of the project and finds the one declaration of the machine.
fn search(name: &str) -> (Option<Ir>, Exit) {
    let ws = match Workspace::find(None) {
        Ok(ws) => ws,
        Err(e) => return (None, report::fatal(e)),
    };
    let files = match ws.inputs(&[]) {
        Ok(f) => f,
        Err(e) => return (None, report::fatal(e)),
    };
    let options = ws.options();
    let mut loader = ws.loader();
    // Each distinct declaration: where its file is (the loader's location, so a file of a
    // package in the project, also compiled as a project file, is one file), `file:line` to
    // show, and the compilation to take it from: preferably the one whose entry declares it.
    let mut found: Vec<(Option<PathBuf>, String, Compilation)> = Vec::new();
    let mut errors = false;
    for f in &files {
        let c = match roadbook_compiler::compile(&mut loader, f, &options) {
            Ok(c) => c,
            Err(e) => return (None, report::fatal(e)),
        };
        if c.has_errors() {
            errors = true;
            continue;
        }
        let Some(m) = c.ir.as_ref().and_then(|ir| ir.machines.get(name)) else {
            continue;
        };
        let ir = c.ir.as_ref().unwrap();
        let file = m.span.file as usize;
        let src = &ir.sources[file];
        let place = match &src.package {
            Some(p) => format!("{p}/{}:{}", src.path, m.span.line),
            None => format!("{}:{}", src.path, m.span.line),
        };
        let location = loader.location(&FileRef {
            package: src.package.clone(),
            path: src.path.clone(),
        });
        let same = |(l, p, _): &&mut (Option<PathBuf>, String, Compilation)| match (l, &location) {
            (Some(l), Some(here)) => l == here,
            _ => *p == place,
        };
        match found.iter_mut().find(same) {
            Some(slot) if file == 0 => {
                slot.1 = place;
                slot.2 = c;
            }
            Some(_) => {}
            None => found.push((location, place, c)),
        }
    }
    if errors {
        // Report every error once, as `roadbook check` does.
        return match roadbook_compiler::check(&mut ws.loader(), &files, &options) {
            Ok(out) => (None, report::diagnostics(&out, false)),
            Err(e) => (None, report::fatal(e)),
        };
    }
    match found.len() {
        0 => (
            None,
            report::fatal(format!(
                "no machine `{name}` in the project: none of its {} `.road` files declares it",
                files.len()
            )),
        ),
        1 => {
            let (_, _, c) = found.pop().unwrap();
            let exit = report::diagnostics(&c, false);
            (c.ir, exit)
        }
        n => {
            let mut places: Vec<&str> = found.iter().map(|(_, p, _)| p.as_str()).collect();
            places.sort();
            (
                None,
                report::fatal(format!(
                    "{n} machines are named `{name}`, in {}: name the file, as in `roadbook paths {name} <file>`",
                    places.join(", ")
                )),
            )
        }
    }
}

/// How a path ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    /// At a final state directly under the root.
    Final,
    /// At a state with no step out.
    Stuck,
    /// Where every step out was taken already on the path: going on would repeat one.
    Loop,
}

/// One transition taken.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step<'a> {
    /// The id of the state the transition belongs to, as a trace's `transition` names it.
    pub from: &'a str,
    /// Its index among that state's transitions.
    pub index: usize,
    /// The trigger, as the viewer labels it.
    pub trigger: String,
    /// The guard's text, `else` for the unguarded last branch of a guarded block.
    pub guard: Option<String>,
    /// The targets: states, or history states (several, for a transition into several regions
    /// of a parallel state).
    pub to: Vec<&'a str>,
    /// The position the step leads to: the first target's initial leaf, or the parallel state
    /// holding it; for a history state, what it recorded on the path, or its parent's.
    pub at: &'a str,
}

#[derive(Clone, Debug)]
pub struct Path<'a> {
    pub end: End,
    /// The id of the state it ends at.
    pub state: &'a str,
    pub steps: Vec<Step<'a>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// `--max` paths were found and there is another.
    Max,
    /// The step budget ran out.
    Budget,
}

pub struct Found<'a> {
    /// The initial leaf, where every path starts.
    pub start: &'a str,
    pub paths: Vec<Path<'a>>,
    pub stopped: Option<Stop>,
}

/// The paths through `machine`, at most `max` of them, trying at most `budget` steps.
pub fn paths(machine: &Machine, max: usize, budget: usize) -> Found<'_> {
    let mut g = Graph {
        states: HashMap::new(),
        parent: HashMap::new(),
        root: &machine.root.id,
        max,
        budget,
        found: Vec::new(),
        stopped: None,
    };
    g.index(&machine.root, None);
    let start = g.position(&machine.root.id);
    g.walk(
        start,
        &mut Vec::new(),
        &mut HashSet::new(),
        &mut HashMap::new(),
    );
    Found {
        start,
        paths: g.found,
        stopped: g.stopped,
    }
}

struct Graph<'a> {
    states: HashMap<&'a str, &'a State>,
    parent: HashMap<&'a str, &'a str>,
    root: &'a str,
    max: usize,
    budget: usize,
    found: Vec<Path<'a>>,
    stopped: Option<Stop>,
}

impl<'a> Graph<'a> {
    fn index(&mut self, s: &'a State, parent: Option<&'a str>) {
        self.states.insert(&s.id, s);
        if let Some(p) = parent {
            self.parent.insert(&s.id, p);
        }
        for c in &s.states {
            self.index(c, Some(&s.id));
        }
    }

    /// Where entering `target` puts the path.
    fn position(&self, target: &'a str) -> &'a str {
        let mut id = target;
        while let Some(initial) = self.states[id].initial.as_deref() {
            id = initial;
        }
        let mut outer = id;
        let mut up = self.parent.get(id).copied();
        while let Some(a) = up {
            if self.states[a].kind == StateKind::Parallel {
                outer = a;
            }
            up = self.parent.get(a).copied();
        }
        outer
    }

    /// The transition's domain (`spec/execution.md`): the root for a transition of the root,
    /// the source for an internal one, else the nearest compound proper ancestor of its source
    /// that is a proper ancestor of every target. A history target counts as a child of its
    /// parent, as any state.
    fn domain(&self, source: &'a str, t: &Transition) -> &'a str {
        if source == self.root || t.internal {
            return source;
        }
        let mut up = self.parent.get(source).copied();
        while let Some(d) = up {
            let below = |target: &String| {
                target.len() > d.len()
                    && target.starts_with(d)
                    && target.as_bytes()[d.len()] == b'.'
            };
            if self.states[d].kind != StateKind::Parallel && t.targets.iter().all(below) {
                return d;
            }
            up = self.parent.get(d).copied();
        }
        self.root
    }

    /// What a step taking `source`'s transition `t` from `at` records, as (history state,
    /// position it would restore): for each state on the way up from `at` that the step leaves
    /// (each below its domain), its history states. None when the domain is below `at`, a
    /// parallel state: what the step leaves inside it is not part of the position.
    fn records(&self, at: &'a str, source: &'a str, t: &Transition) -> Vec<(&'a str, &'a str)> {
        let domain = self.domain(source, t);
        let mut out = Vec::new();
        let within = at == domain
            || (at.len() > domain.len()
                && at.starts_with(domain)
                && at.as_bytes()[domain.len()] == b'.');
        if !within {
            return out;
        }
        // The states left, innermost first, each with the one below it on the way (none for
        // the position itself).
        let mut below: Option<&'a str> = None;
        let mut id = at;
        while id != domain {
            for h in self.states[id]
                .states
                .iter()
                .filter(|h| h.kind.is_history())
            {
                let restores = match (h.kind, below) {
                    (StateKind::History, Some(child)) => self.position(child),
                    _ => at,
                };
                out.push((h.id.as_str(), restores));
            }
            below = Some(id);
            match self.parent.get(id) {
                Some(p) => id = p,
                None => break,
            }
        }
        out
    }

    /// Where a step to `target` puts the path, given what the history states recorded.
    fn entered(&self, target: &'a str, history: &HashMap<&'a str, &'a str>) -> &'a str {
        if !self.states[target].kind.is_history() {
            return self.position(target);
        }
        match history.get(target) {
            Some(at) => at,
            // With no record, the history state's first default, or its parent's default entry.
            None => match self.states[target].default.first() {
                Some(d) => self.position(d),
                None => self.position(self.parent[target]),
            },
        }
    }

    /// The transitions a step from `at` can take, as (owner, index), in order.
    fn available(&self, at: &'a str) -> Vec<(&'a str, usize)> {
        let leaf = self.states[at];
        let parent = self.parent.get(at).copied();
        if leaf.kind == StateKind::Final {
            let Some(parent) = parent.filter(|p| *p != self.root) else {
                return Vec::new();
            };
            let done: Vec<(&str, usize)> = self.states[parent]
                .transitions
                .iter()
                .enumerate()
                .filter(|(_, t)| matches!(t.trigger, Trigger::Done) && !t.targets.is_empty())
                .map(|(i, _)| (parent, i))
                .collect();
            if !done.is_empty() {
                return done;
            }
        }
        let mut all: Vec<(&'a str, usize, &'a Transition)> = Vec::new();
        if leaf.kind == StateKind::Parallel {
            // The transitions in its regions that leave it are ways out of it too, as for
            // `roadbook check` (RB0902): innermost first, so before its own.
            let inside = format!("{at}.");
            for c in &leaf.states {
                self.region_exits(c, &inside, &mut all);
            }
        }
        let mut up = Some(at);
        while let Some(id) = up {
            for (i, t) in self.states[id].transitions.iter().enumerate() {
                let own_done = id == at && leaf.kind == StateKind::Parallel;
                if !matches!(t.trigger, Trigger::Done) || own_done {
                    all.push((id, i, t));
                }
            }
            up = self.parent.get(id).copied();
        }
        let always = |t: &Transition| matches!(t.trigger, Trigger::Always);
        // An unguarded `always` is taken at once: only the `always` branches up to it can be.
        if let Some(cut) = all.iter().position(|a| always(a.2) && a.2.guard.is_none()) {
            all.truncate(cut + 1);
            all.retain(|a| always(a.2));
        }
        all.into_iter()
            .filter(|a| !a.2.targets.is_empty())
            .map(|(id, i, _)| (id, i))
            .collect()
    }

    /// The transitions of `s` and the states inside it, in document order, whose target is
    /// not inside the parallel state whose id is `inside` less its last `.`: the ways out of
    /// it from its regions. A `done` is not one: a region's ending is the parallel state's.
    fn region_exits(
        &self,
        s: &'a State,
        inside: &str,
        out: &mut Vec<(&'a str, usize, &'a Transition)>,
    ) {
        for (i, t) in s.transitions.iter().enumerate() {
            let leaves = t.targets.iter().any(|to| !to.starts_with(inside));
            if leaves && !matches!(t.trigger, Trigger::Done) {
                out.push((&s.id, i, t));
            }
        }
        for c in &s.states {
            self.region_exits(c, inside, out);
        }
    }

    /// Lists the path so far, ending at `at`. Returns false once the search must stop.
    fn record(&mut self, end: End, at: &'a str, steps: &[Step<'a>]) -> bool {
        if self.found.len() == self.max {
            self.stopped = Some(Stop::Max);
            return false;
        }
        self.found.push(Path {
            end,
            state: at,
            steps: steps.to_vec(),
        });
        true
    }

    /// Searches on from `at`, with what the history states recorded on the path so far.
    /// Returns false once the search must stop.
    fn walk(
        &mut self,
        at: &'a str,
        steps: &mut Vec<Step<'a>>,
        used: &mut HashSet<(&'a str, usize)>,
        history: &mut HashMap<&'a str, &'a str>,
    ) -> bool {
        if self.budget == 0 {
            self.stopped = Some(Stop::Budget);
            return false;
        }
        self.budget -= 1;
        let options = self.available(at);
        if options.is_empty() {
            let leaf = self.states[at];
            let end = if leaf.kind == StateKind::Final && self.parent.get(at) == Some(&self.root) {
                End::Final
            } else {
                End::Stuck
            };
            return self.record(end, at, steps);
        }
        let mut went_on = false;
        for (from, index) in options {
            if !used.insert((from, index)) {
                continue;
            }
            went_on = true;
            let owner = self.states[from];
            let t = &owner.transitions[index];
            let to: Vec<&'a str> = t.targets.iter().map(String::as_str).collect();
            // Record before entering: a step that leaves a state and targets its history
            // state enters where it was a moment before.
            let mut undo = Vec::new();
            for (h, restores) in self.records(at, from, t) {
                undo.push((h, history.insert(h, restores)));
            }
            let next = self.entered(to[0], history);
            steps.push(Step {
                from,
                index,
                trigger: trigger_text(owner, &t.trigger),
                guard: guard_text(owner, index),
                to,
                at: next,
            });
            let go_on = self.walk(next, steps, used, history);
            steps.pop();
            used.remove(&(from, index));
            for (h, before) in undo.into_iter().rev() {
                match before {
                    Some(b) => history.insert(h, b),
                    None => history.remove(h),
                };
            }
            if !go_on {
                return false;
            }
        }
        if !went_on {
            // Every way on was taken already: the path ends here, where it would loop.
            return self.record(End::Loop, at, steps);
        }
        true
    }
}

/// The id below the machine: `Processing.Paying`.
fn relative<'s>(machine: &str, id: &'s str) -> &'s str {
    id.strip_prefix(machine)
        .and_then(|r| r.strip_prefix('.'))
        .unwrap_or(id)
}

/// `Checkout: 6 paths`, then a numbered block per path.
pub fn text(name: &str, machine: &Machine, found: &Found) -> String {
    let n = found.paths.len();
    let mut out = format!("{name}: {n} path{}\n", if n == 1 { "" } else { "s" });
    let states = {
        let mut map = HashMap::new();
        index_states(&machine.root, &mut map);
        map
    };
    for (i, p) in found.paths.iter().enumerate() {
        let head = format!("{}. ", i + 1);
        let start = relative(name, found.start);
        let steps: Vec<String> = p
            .steps
            .iter()
            .map(|s| {
                let guard = s
                    .guard
                    .as_ref()
                    .map(|g| format!(" [{g}]"))
                    .unwrap_or_default();
                // Through a history state: where it led, and which.
                let via = match states[s.to[0]].kind {
                    StateKind::History => format!(" (history {})", relative(name, s.to[0])),
                    StateKind::DeepHistory => {
                        format!(" (deep history {})", relative(name, s.to[0]))
                    }
                    _ => String::new(),
                };
                format!("--{}{guard}--> {}{via}", s.trigger, relative(name, s.at))
            })
            .collect();
        let end = match p.end {
            End::Stuck => " (stuck)".to_string(),
            End::Loop => " (loops)".to_string(),
            End::Final => {
                let s = states[p.state];
                match (&s.output, &s.fail) {
                    (Some(o), _) => format!(" (final: output {})", o.text),
                    (None, Some(f)) => format!(" (final: fail {})", f.text),
                    (None, None) => " (final)".to_string(),
                }
            }
        };
        let line = if steps.is_empty() {
            format!("{head}{start}{end}")
        } else {
            format!("{head}{start} {}{end}", steps.join(" "))
        };
        if line.chars().count() <= WIDTH || steps.is_empty() {
            out.push_str(&line);
        } else {
            let indent = " ".repeat(head.len() + 2);
            out.push_str(&format!("{head}{start}"));
            for s in &steps {
                out.push_str(&format!("\n{indent}{s}"));
            }
            out.push_str(&end);
        }
        out.push('\n');
    }
    out
}

fn index_states<'a>(s: &'a State, map: &mut HashMap<&'a str, &'a State>) {
    map.insert(&s.id, s);
    for c in &s.states {
        index_states(c, map);
    }
}

/// `{"machine": …, "paths": [{"end", "state", "steps": [{"from", "index", "trigger",
/// "guard", "to"}]}], "truncated": …}`, one path per line.
pub fn json(name: &str, found: &Found) -> String {
    let q = |s: &str| serde_json::to_string(s).unwrap();
    let paths: Vec<String> = found
        .paths
        .iter()
        .map(|p| {
            let steps: Vec<String> = p
                .steps
                .iter()
                .map(|s| {
                    format!(
                        "{{\"from\": {}, \"index\": {}, \"trigger\": {}, \"guard\": {}, \"to\": {}}}",
                        q(s.from),
                        s.index,
                        q(&s.trigger),
                        s.guard.as_deref().map_or("null".to_string(), q),
                        serde_json::to_string(&s.to).unwrap()
                    )
                })
                .collect();
            let end = match p.end {
                End::Final => "final",
                End::Stuck => "stuck",
                End::Loop => "loop",
            };
            format!(
                "    {{\"end\": \"{end}\", \"state\": {}, \"steps\": [{}]}}",
                q(p.state),
                steps.join(", ")
            )
        })
        .collect();
    let list = if paths.is_empty() {
        "[]".to_string()
    } else {
        format!("[\n{}\n  ]", paths.join(",\n"))
    };
    format!(
        "{{\n  \"machine\": {},\n  \"paths\": {list},\n  \"truncated\": {}\n}}\n",
        q(name),
        found.stopped.is_some()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine(src: &str) -> Machine {
        let out = roadbook_compiler::compile_source("m.road", src, &Default::default());
        assert!(!out.has_errors(), "{}", out.render());
        out.ir.unwrap().machines.shift_remove("M").unwrap()
    }

    fn lines(m: &Machine, max: usize) -> String {
        text("M", m, &paths(m, max, BUDGET))
    }

    #[test]
    fn a_compound_is_entered_at_its_initial_leaf_and_its_events_apply_inside() {
        let m = machine(
            "machine M() {\n  event Go\n  event Stop\n  initial A\n  state A {\n    initial B\n    on Stop -> Out\n    state B {\n      on Go -> C\n    }\n    state C {}\n  }\n  final state Out {}\n}\n",
        );
        assert_eq!(
            lines(&m, 10),
            "M: 2 paths\n1. A.B --Go--> A.C --Stop--> Out (final)\n2. A.B --Stop--> Out (final)\n"
        );
    }

    #[test]
    fn an_unguarded_always_is_taken_at_once() {
        let m = machine(
            "machine M() {\n  event Stop\n  initial A\n  on Stop -> Out\n  state A {\n    always -> B\n  }\n  state B {}\n  final state Out {}\n}\n",
        );
        assert_eq!(
            lines(&m, 10),
            "M: 1 path\n1. A --always--> B --Stop--> Out (final)\n"
        );
    }

    #[test]
    fn a_history_state_enters_what_it_recorded_on_the_path() {
        // Leaving W from B records B (entered from its start, B1); from A, A.
        let m = machine(
            "machine M() {\n  event Go\n  event Out\n  event Back\n  initial W\n  state W {\n    initial A\n    history H\n    state A { on Go -> B }\n    state B {\n      initial B1\n      state B1 {}\n    }\n    on Out -> Away\n  }\n  state Away { on Back -> W.H }\n}\n",
        );
        assert_eq!(
            lines(&m, 10),
            "M: 2 paths\n1. W.A --Go--> W.B.B1 --Out--> Away --Back--> W.B.B1 (history W.H) (loops)\n2. W.A --Out--> Away --Back--> W.A (history W.H) --Go--> W.B.B1 (loops)\n"
        );
        let found = paths(&m, 10, BUDGET);
        let back = &found.paths[0].steps[2];
        assert_eq!((back.to.as_slice(), back.at), (&["M.W.H"][..], "M.W.B.B1"));
    }

    #[test]
    fn a_deep_history_state_restores_the_leaf_and_with_no_record_its_parents_start() {
        let m = machine(
            "machine M() {\n  event Go\n  event Out\n  event Again\n  initial Start\n  state Start {\n    on Again -> W.D\n  }\n  state W {\n    initial A\n    deep history D\n    history S\n    state A { on Go -> B }\n    state B {\n      initial B1\n      state B1 { on Go -> B2 }\n      state B2 { on Out -> Start }\n    }\n  }\n}\n",
        );
        assert_eq!(
            lines(&m, 10),
            "M: 1 path\n1. Start --Again--> W.A (deep history W.D) --Go--> W.B.B1 --Go--> W.B.B2 --Out--> Start (loops)\n"
        );
        // With B2 recorded: the deep history state restores B2, the shallow one B from its start.
        let g = |s: &str| {
            machine(&format!(
                "machine M() {{\n  event Go\n  event Out\n  event Again\n  initial W\n  state Start {{\n    on Again -> W.{s}\n  }}\n  state W {{\n    initial A\n    deep history D\n    history S\n    state A {{ on Go -> B }}\n    state B {{\n      initial B1\n      state B1 {{ on Go -> B2 }}\n      state B2 {{ on Out -> Start }}\n    }}\n  }}\n}}\n"
            ))
        };
        let deep = g("D");
        let deep = paths(&deep, 10, BUDGET);
        assert_eq!(deep.paths[0].steps.last().unwrap().at, "M.W.B.B2");
        let shallow = g("S");
        let shallow = paths(&shallow, 10, BUDGET);
        assert_eq!(shallow.paths[0].steps.last().unwrap().at, "M.W.B.B1");
    }

    #[test]
    fn a_step_inside_the_parent_to_its_history_state_records_nothing() {
        // `Jump` stays inside W: W is not left, nothing is recorded, and H enters W's start.
        let m = machine(
            "machine M() {\n  event Go\n  event Jump\n  initial W\n  state W {\n    initial A\n    history H\n    state A { on Go -> B }\n    state B { on Jump -> H }\n  }\n}\n",
        );
        assert_eq!(
            lines(&m, 10),
            "M: 1 path\n1. W.A --Go--> W.B --Jump--> W.A (history W.H) (loops)\n"
        );
    }

    #[test]
    fn the_budget_stops_the_search() {
        let m = machine(
            "machine M() {\n  event Go\n  initial A\n  state A {\n    on Go -> B\n  }\n  final state B {}\n}\n",
        );
        let found = paths(&m, 10, 1);
        assert_eq!(found.stopped, Some(Stop::Budget));
        assert!(found.paths.is_empty());
    }
}
