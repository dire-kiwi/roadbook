//! Definite assignment: a forward dataflow analysis proving that every read of a machine
//! variable happens after the variable has a value.
//!
//! A variable is *tracked* if it can be unassigned: it has no initial value and its type is
//! not optional. Parameters, variables with an initial value and optional variables (which
//! start as `null`) always have a value. Variables never lose their value, so the set of
//! assigned variables only grows during a run, and the analysis computes, for every point
//! where a chart reads a variable, a set of variables that is assigned whenever that point is
//! reached, whatever happened before. The facts, for a state `S`:
//!
//! - `in(S)`: assigned just before `S`'s `entry` runs, on every way into `S`. Each way in is a
//!   transition (or the start of the machine) followed by the states it enters, outermost
//!   first in document order, and the fact flows along that sequence: what the exits, the
//!   transition's `set`s and the `entry` of the states entered before `S` assign is assigned
//!   in `S`. The meet over the ways in is the intersection.
//! - `entered(S)`: `in(S)` and what `S`'s `entry` assigns. `output` and `fail` see this.
//! - `at(S)`: assigned whenever `S` is active and the step that entered it has entered all it
//!   enters, which is whenever a transition of `S` is selected, its invoke and timers start,
//!   or its `exit` runs. For an atomic or final state it is `entered(S)`. A compound state is
//!   then in one of its children, so it is `entered(S)` and the intersection of its children's
//!   `at`. A parallel state is in all its regions at once, so it is `entered(S)` and the union
//!   of its regions' `at`.
//! - `done(S)`: assigned when `S`'s `done` is taken. A compound state is in a final child, so
//!   it is the intersection of their `entered`; a parallel state has every region in a final
//!   configuration, so it is the union, over the regions, of what holds there.
//!
//! It is conservative in these ways, each documented in `language.md`:
//!
//! - a transition's guard does not narrow anything, and every transition whose source can be
//!   active is assumed to be taken, whatever its guard;
//! - an internal transition (no target) assigns nothing that the rest of the chart may rely
//!   on, since the state may be left before it is taken;
//! - `exit` effects and invoke arguments see what was assigned when the state was entered,
//!   not what a transition taken inside it assigned later;
//! - within a parallel state, a region relies only on what was assigned before it was
//!   entered and on its own transitions: an assignment made later by another region does not
//!   count, since the regions take transitions independently. Only `done` of the parallel
//!   state and the transitions written on it see all regions;
//! - the exits that run before a transition's effects count only for the source state and
//!   its ancestors below the transition's domain: which of the source's descendants are
//!   active is not known.
//!
//! The analysis is a worklist fixpoint started from "everything assigned" at states not yet
//! reached. Every fact only shrinks, a state's transitions are processed again only when its
//! `at` or `done` shrinks, and the intersection over a compound state's children is kept
//! incrementally, so the work is about the size of the chart times the number of tracked
//! variables.

use std::collections::HashMap;

use roadbook_ir::{self as ir, StateKind, Trigger};
use roadbook_syntax::{Diagnostic, Span};

use crate::codes;
use crate::machine::{MachineInfo, Reads, StateInfo};

/// A set of tracked variables.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Bits(Vec<u64>);

impl Bits {
    fn empty(n: usize) -> Bits {
        Bits(vec![0; n.div_ceil(64)])
    }

    fn full(n: usize) -> Bits {
        let mut b = Bits(vec![u64::MAX; n.div_ceil(64)]);
        if n % 64 != 0
            && let Some(last) = b.0.last_mut()
        {
            *last = (1u64 << (n % 64)) - 1;
        }
        b
    }

    fn has(&self, i: usize) -> bool {
        self.0[i / 64] & (1 << (i % 64)) != 0
    }

    fn insert(&mut self, i: usize) {
        self.0[i / 64] |= 1 << (i % 64);
    }

    fn union(&mut self, o: &Bits) {
        for (a, b) in self.0.iter_mut().zip(&o.0) {
            *a |= b;
        }
    }

    /// Intersects in place; whether `self` changed.
    fn meet(&mut self, o: &Bits) -> bool {
        let mut changed = false;
        for (a, b) in self.0.iter_mut().zip(&o.0) {
            let n = *a & b;
            changed |= n != *a;
            *a = n;
        }
        changed
    }

    fn with(&self, o: &Bits) -> Bits {
        let mut b = self.clone();
        b.union(o);
        b
    }
}

/// Where a read happens, which says which fact it is checked against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ctx {
    /// Before the state's `entry` (and inside it, with its `set`s so far).
    In(usize),
    /// After the state's `entry`: `output` and `fail`.
    Entered(usize),
    /// While the state is active: its transitions, invoke, timers and `exit`.
    At(usize),
    /// When the state's `done` is taken.
    Done(usize),
}

/// Which kind of item a read is in, for the help line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Entry,
    Exit,
    Other,
}

/// A read that might see no value.
struct Found {
    span: Span,
    var: usize,
    ctx: Ctx,
    part: Part,
}

/// A way into states: the start of the machine, or a transition with a target.
struct Edge {
    /// The state the transition is written in, `None` for the start.
    source: Option<usize>,
    /// Whether the fact at the source is `done` rather than `at`.
    done: bool,
    /// What the `exit` of the source and of its ancestors below the domain assign: the exits
    /// that surely run before the transition's effects.
    exits: Bits,
    /// What the transition's `set`s assign.
    sets: Bits,
    /// The states entered, in the order they are entered.
    chain: Vec<usize>,
}

struct Chart<'a> {
    states: Vec<&'a ir::State>,
    parent: Vec<Option<usize>>,
    children: Vec<Vec<usize>>,
    /// The index just past the state's last descendant, in document order.
    end: Vec<usize>,
    initial: Vec<Option<usize>>,
}

impl<'a> Chart<'a> {
    fn new(root: &'a ir::State) -> Chart<'a> {
        let mut c = Chart {
            states: Vec::new(),
            parent: Vec::new(),
            children: Vec::new(),
            end: Vec::new(),
            initial: Vec::new(),
        };
        c.add(root, None);
        let ids: HashMap<&str, usize> = c
            .states
            .iter()
            .enumerate()
            .map(|(i, s)| (s.id.as_str(), i))
            .collect();
        c.initial = c
            .states
            .iter()
            .map(|s| s.initial.as_deref().and_then(|i| ids.get(i).copied()))
            .collect();
        c
    }

    fn add(&mut self, s: &'a ir::State, parent: Option<usize>) -> usize {
        let i = self.states.len();
        self.states.push(s);
        self.parent.push(parent);
        self.children.push(Vec::new());
        self.end.push(0);
        for c in &s.states {
            let ci = self.add(c, Some(i));
            self.children[i].push(ci);
        }
        self.end[i] = self.states.len();
        i
    }

    fn kind(&self, i: usize) -> StateKind {
        self.states[i].kind
    }

    /// Whether `a` is `d` or one of its ancestors.
    fn contains(&self, a: usize, d: usize) -> bool {
        a <= d && d < self.end[a]
    }

    /// The states entered with `s`, `s` first: down `path` (each a child of the one before)
    /// and then by default entry, with every region of a parallel state, in document order.
    fn enter(&self, s: usize, path: &[usize], out: &mut Vec<usize>) {
        out.push(s);
        match self.kind(s) {
            StateKind::Compound => match path.first() {
                Some(&next) => self.enter(next, &path[1..], out),
                None => {
                    if let Some(i) = self.initial[s] {
                        self.enter(i, &[], out);
                    }
                }
            },
            StateKind::Parallel => {
                for &r in &self.children[s] {
                    if path.first() == Some(&r) {
                        self.enter(r, &path[1..], out);
                    } else {
                        self.enter(r, &[], out);
                    }
                }
            }
            _ => {}
        }
    }

    /// The transition's domain, as in SCXML: for the root, the root; otherwise the nearest
    /// proper ancestor of the source that is a proper ancestor of the target and not parallel.
    fn domain(&self, from: usize, to: usize) -> usize {
        if from == 0 {
            return 0;
        }
        let mut a = self.parent[from];
        while let Some(x) = a {
            if x != to && self.contains(x, to) && self.kind(x) != StateKind::Parallel {
                return x;
            }
            a = self.parent[x];
        }
        0
    }
}

/// Runs the analysis on one machine and reports every read that might see no value.
pub(crate) fn check(m: &ir::Machine, info: &MachineInfo, diags: &mut Vec<Diagnostic>) {
    initialisers(info, diags);
    let tracked: HashMap<&str, usize> = info
        .vars
        .iter()
        .filter(|v| !v.init && !v.optional)
        .enumerate()
        .map(|(i, v)| (v.name.as_str(), i))
        .collect();
    if tracked.is_empty() {
        return;
    }
    let chart = Chart::new(&m.root);
    let state_infos: Vec<Option<&StateInfo>> = chart
        .states
        .iter()
        .map(|s| info.states.get(&s.id))
        .collect();
    if state_infos.iter().any(Option::is_none) {
        return;
    }
    let mut flow = Flow::new(&chart, &tracked);
    flow.solve();
    flow.report(info, &state_infos, diags);
}

/// An initial value may read the parameters and the variables declared before it that have a
/// value by then: an initial value of their own, or an optional type.
fn initialisers(info: &MachineInfo, diags: &mut Vec<Diagnostic>) {
    for (i, v) in info.vars.iter().enumerate() {
        for (name, span) in &v.init_reads {
            let Some(j) = info.vars.iter().position(|x| &x.name == name) else {
                continue;
            };
            let w = &info.vars[j];
            if j < i && (w.init || w.optional) {
                continue;
            }
            let (label, help) = if j >= i {
                (
                    "declared here, after the variable whose initial value reads it",
                    "initial values are computed in the order the variables are declared; declare it first, or assign this variable in an `entry` instead",
                )
            } else {
                (
                    "declared here with no initial value",
                    "give it an initial value, or assign this variable in an `entry` instead",
                )
            };
            diags.push(
                Diagnostic::error(
                    codes::UNASSIGNED_READ,
                    *span,
                    format!(
                        "`{name}` has no value yet when the initial value of `{}` is computed",
                        v.name
                    ),
                )
                .with_secondary(w.span, label)
                .with_help(help),
            );
        }
    }
}

struct Flow<'c, 'a> {
    chart: &'c Chart<'a>,
    tracked: &'c HashMap<&'c str, usize>,
    n: usize,
    reached: Vec<bool>,
    in_: Vec<Bits>,
    entered: Vec<Bits>,
    at: Vec<Bits>,
    done: Vec<Bits>,
    /// For a compound state, the intersection of its children's `at`.
    children_at: Vec<Bits>,
    /// For a compound state, the intersection of its final children's `entered`.
    finals: Vec<Bits>,
    entry_gen: Vec<Bits>,
    /// The start, then every transition with a target, by source.
    edges: Vec<Edge>,
    /// For each state's transitions, by index, the index of its edge if it has a target.
    out: Vec<Vec<Option<usize>>>,
    work: Vec<usize>,
    queued: Vec<bool>,
}

impl<'c, 'a> Flow<'c, 'a> {
    fn new(chart: &'c Chart<'a>, tracked: &'c HashMap<&'c str, usize>) -> Flow<'c, 'a> {
        let n = tracked.len();
        let count = chart.states.len();
        let gen_of = |effects: &[ir::Effect]| {
            let mut b = Bits::empty(n);
            for e in effects {
                if let ir::Effect::Set { var, .. } = e
                    && let Some(&i) = tracked.get(var.as_str())
                {
                    b.insert(i);
                }
            }
            b
        };
        let entry_gen: Vec<Bits> = chart.states.iter().map(|s| gen_of(&s.entry)).collect();
        let exit_gen: Vec<Bits> = chart.states.iter().map(|s| gen_of(&s.exit)).collect();
        let ids: HashMap<&str, usize> = chart
            .states
            .iter()
            .enumerate()
            .map(|(i, s)| (s.id.as_str(), i))
            .collect();

        let mut start = Vec::new();
        chart.enter(0, &[], &mut start);
        let mut edges = vec![Edge {
            source: None,
            done: false,
            exits: Bits::empty(n),
            sets: Bits::empty(n),
            chain: start,
        }];
        let mut out: Vec<Vec<Option<usize>>> = vec![Vec::new(); count];
        for (s, st) in chart.states.iter().enumerate() {
            for t in &st.transitions {
                let Some(&to) = t.target.as_deref().and_then(|x| ids.get(x)) else {
                    out[s].push(None);
                    continue;
                };
                let domain = chart.domain(s, to);
                let mut exits = Bits::empty(n);
                if s != 0 {
                    let mut x = Some(s);
                    while let Some(e) = x
                        && e != domain
                    {
                        exits.union(&exit_gen[e]);
                        x = chart.parent[e];
                    }
                }
                let mut path = Vec::new();
                let mut x = to;
                while x != domain {
                    path.push(x);
                    match chart.parent[x] {
                        Some(p) => x = p,
                        None => break,
                    }
                }
                path.reverse();
                let mut chain = Vec::new();
                if let Some((&first, rest)) = path.split_first() {
                    chart.enter(first, rest, &mut chain);
                }
                out[s].push(Some(edges.len()));
                edges.push(Edge {
                    source: Some(s),
                    done: matches!(t.trigger, Trigger::Done),
                    exits,
                    sets: gen_of(&t.effects),
                    chain,
                });
            }
        }
        let full = Bits::full(n);
        Flow {
            chart,
            tracked,
            n,
            reached: vec![false; count],
            in_: vec![full.clone(); count],
            entered: vec![full.clone(); count],
            at: vec![full.clone(); count],
            done: vec![full.clone(); count],
            children_at: vec![full.clone(); count],
            finals: vec![full; count],
            entry_gen,
            edges,
            out,
            work: Vec::new(),
            queued: vec![false; count],
        }
    }

    /// The fact as an edge enters its first state: at the source, then the exits and the
    /// transition's `set`s.
    fn edge_fact(&self, e: &Edge) -> Bits {
        let mut f = match e.source {
            None => Bits::empty(self.n),
            Some(s) if e.done => self.done[s].clone(),
            Some(s) => self.at[s].clone(),
        };
        f.union(&e.exits);
        f.union(&e.sets);
        f
    }

    fn solve(&mut self) {
        self.take(0);
        while let Some(s) = self.work.pop() {
            self.queued[s] = false;
            for k in 0..self.out[s].len() {
                if let Some(e) = self.out[s][k] {
                    self.take(e);
                }
            }
        }
    }

    fn push(&mut self, s: usize) {
        if !self.queued[s] {
            self.queued[s] = true;
            self.work.push(s);
        }
    }

    /// Flows the fact of edge `k` along the states it enters.
    fn take(&mut self, k: usize) {
        let mut f = self.edge_fact(&self.edges[k]);
        for i in 0..self.edges[k].chain.len() {
            let s = self.edges[k].chain[i];
            let first = !self.reached[s];
            self.reached[s] = true;
            if self.in_[s].meet(&f) || first {
                self.update(s, first);
            }
            f.union(&self.entry_gen[s]);
        }
    }

    /// Recomputes what depends on `in(s)`, which shrank, up the ancestors.
    fn update(&mut self, s: usize, first: bool) {
        let entered = self.in_[s].with(&self.entry_gen[s]);
        self.entered[s] = entered;
        if first {
            self.push(s);
        }
        let mut x = s;
        loop {
            if self.recompute(x) {
                self.push(x);
            }
            let Some(p) = self.chart.parent[x] else {
                break;
            };
            if self.chart.kind(p) == StateKind::Compound {
                let at = self.at[x].clone();
                self.children_at[p].meet(&at);
                if self.chart.kind(x) == StateKind::Final {
                    let e = self.entered[x].clone();
                    self.finals[p].meet(&e);
                }
            }
            x = p;
        }
    }

    /// What holds when `s` is in a final configuration: as `done(s)` for a compound or
    /// parallel state, `entered(s)` for a final one. An atomic state never is.
    fn fin(&self, s: usize) -> Bits {
        match self.chart.kind(s) {
            StateKind::Final => self.entered[s].clone(),
            StateKind::Compound | StateKind::Parallel => self.done[s].clone(),
            StateKind::Atomic => self.at[s].clone(),
        }
    }

    /// Recomputes `at(s)` and `done(s)`; whether either changed.
    fn recompute(&mut self, s: usize) -> bool {
        let (at, done) = match self.chart.kind(s) {
            StateKind::Atomic | StateKind::Final => (self.entered[s].clone(), self.done[s].clone()),
            StateKind::Compound => (
                self.entered[s].with(&self.children_at[s]),
                self.entered[s].with(&self.finals[s]),
            ),
            StateKind::Parallel => {
                let mut at = self.entered[s].clone();
                let mut done = self.entered[s].clone();
                for &r in &self.chart.children[s] {
                    at.union(&self.at[r]);
                    done.union(&self.fin(r));
                }
                (at, done)
            }
        };
        let changed = at != self.at[s] || done != self.done[s];
        self.at[s] = at;
        self.done[s] = done;
        changed
    }

    fn fact(&self, c: Ctx) -> &Bits {
        match c {
            Ctx::In(s) => &self.in_[s],
            Ctx::Entered(s) => &self.entered[s],
            Ctx::At(s) => &self.at[s],
            Ctx::Done(s) => &self.done[s],
        }
    }

    /// A state whose `in` lacks variable `v` and explains why `c`'s fact lacks it.
    fn culprit(&self, c: Ctx, v: usize) -> usize {
        let lacks = |s: usize, b: &Bits| self.reached[s] && !b.has(v);
        match c {
            Ctx::In(s) | Ctx::Entered(s) => s,
            Ctx::At(s) => match self.chart.kind(s) {
                StateKind::Compound | StateKind::Parallel if self.entered[s].has(v) => s,
                StateKind::Compound | StateKind::Parallel => self.chart.children[s]
                    .iter()
                    .find(|&&k| lacks(k, &self.at[k]))
                    .map_or(s, |&k| self.culprit(Ctx::At(k), v)),
                _ => s,
            },
            Ctx::Done(s) => {
                let kids = &self.chart.children[s];
                let found = match self.chart.kind(s) {
                    StateKind::Compound => kids
                        .iter()
                        .find(|&&k| {
                            self.chart.kind(k) == StateKind::Final && lacks(k, &self.entered[k])
                        })
                        .map(|&k| Ctx::Entered(k)),
                    StateKind::Parallel => {
                        kids.iter().find(|&&k| lacks(k, &self.fin(k))).map(|&k| {
                            match self.chart.kind(k) {
                                StateKind::Final => Ctx::Entered(k),
                                StateKind::Atomic => Ctx::At(k),
                                _ => Ctx::Done(k),
                            }
                        })
                    }
                    _ => None,
                };
                found.map_or(s, |c| self.culprit(c, v))
            }
        }
    }

    /// For variable `v`, the first edge that brings each state a fact without `v`.
    fn witnesses(&self, v: usize) -> Vec<Option<usize>> {
        let mut first = vec![None; self.chart.states.len()];
        for (k, e) in self.edges.iter().enumerate() {
            if let Some(s) = e.source
                && !self.reached[s]
            {
                continue;
            }
            let mut f = self.edge_fact(e);
            for &s in &e.chain {
                if !f.has(v) && first[s].is_none() {
                    first[s] = Some(k);
                }
                f.union(&self.entry_gen[s]);
            }
        }
        first
    }

    /// The reads of `reads` that `fact` does not cover.
    fn check(&self, reads: &Reads, fact: &Bits, ctx: Ctx, part: Part, out: &mut Vec<Found>) {
        for (name, span) in reads {
            if let Some(&var) = self.tracked.get(name.as_str())
                && !fact.has(var)
            {
                out.push(Found {
                    span: *span,
                    var,
                    ctx,
                    part,
                });
            }
        }
    }

    /// The reads of an effect list: every `set` runs first, each seeing the ones before it,
    /// then the other effects, which see them all.
    fn check_effects(
        &self,
        (list, reads): (&[ir::Effect], &[Reads]),
        base: &Bits,
        ctx: Ctx,
        part: Part,
        out: &mut Vec<Found>,
    ) {
        let mut f = base.clone();
        for (e, r) in list.iter().zip(reads) {
            if let ir::Effect::Set { var, .. } = e {
                self.check(r, &f, ctx, part, out);
                if let Some(&i) = self.tracked.get(var.as_str()) {
                    f.insert(i);
                }
            }
        }
        for (e, r) in list.iter().zip(reads) {
            if !matches!(e, ir::Effect::Set { .. }) {
                self.check(r, &f, ctx, part, out);
            }
        }
    }

    fn report(
        &self,
        info: &MachineInfo,
        states: &[Option<&StateInfo>],
        diags: &mut Vec<Diagnostic>,
    ) {
        let mut found = Vec::new();
        for (s, st) in self.chart.states.iter().enumerate() {
            if !self.reached[s] {
                continue;
            }
            let Some(si) = states[s] else { continue };
            let at = Ctx::At(s);
            let out = &mut found;
            self.check_effects(
                (&st.entry, &si.entry_reads),
                &self.in_[s],
                Ctx::In(s),
                Part::Entry,
                out,
            );
            self.check_effects((&st.exit, &si.exit_reads), &self.at[s], at, Part::Exit, out);
            self.check(&si.invoke_reads, &self.at[s], at, Part::Other, out);
            self.check(
                &si.result_reads,
                &self.entered[s],
                Ctx::Entered(s),
                Part::Other,
                out,
            );
            let mut last_group = None;
            for (ti, (t, tr)) in st.transitions.iter().zip(&si.transition_reads).enumerate() {
                let ctx = if matches!(t.trigger, Trigger::Done) {
                    Ctx::Done(s)
                } else {
                    at
                };
                // Every branch of an `after` block has its delay; it is evaluated once.
                if last_group != Some(t.group) {
                    self.check(&tr.delay, &self.at[s], at, Part::Other, out);
                }
                last_group = Some(t.group);
                self.check(&tr.guard, self.fact(ctx), ctx, Part::Other, out);
                // The exits run before the effects.
                let mut base = self.fact(ctx).clone();
                if let Some(k) = self.out[s][ti] {
                    base.union(&self.edges[k].exits);
                }
                self.check_effects((&t.effects, &tr.effects), &base, ctx, Part::Other, out);
            }
        }
        let mut witnesses: HashMap<usize, Vec<Option<usize>>> = HashMap::new();
        for f in found {
            let w = witnesses
                .entry(f.var)
                .or_insert_with(|| self.witnesses(f.var));
            diags.push(self.diagnostic(info, states, &f, w));
        }
    }

    fn diagnostic(
        &self,
        info: &MachineInfo,
        states: &[Option<&StateInfo>],
        f: &Found,
        witnesses: &[Option<usize>],
    ) -> Diagnostic {
        let (v, span, ctx) = (f.var, f.span, f.ctx);
        let name = info
            .vars
            .iter()
            .map(|x| x.name.as_str())
            .find(|x| self.tracked.get(x) == Some(&v))
            .unwrap_or_default();
        let (Ctx::In(s) | Ctx::Entered(s) | Ctx::At(s) | Ctx::Done(s)) = ctx;
        let id = &self.chart.states[s].id;
        let mut d = Diagnostic::error(
            codes::UNASSIGNED_READ,
            span,
            format!("`{name}` might not be assigned yet when this is evaluated"),
        );
        let culprit = self.culprit(ctx, v);
        let cid = &self.chart.states[culprit].id;
        let decl = info.vars.iter().find(|x| x.name == name).map(|x| x.span);
        match witnesses[culprit] {
            Some(k) if k > 0 => {
                let src = self.edges[k].source.unwrap_or(0);
                let ti = self.out[src].iter().position(|&e| e == Some(k));
                if let Some(t) = ti.and_then(|ti| states[src]?.transition_reads.get(ti)) {
                    d = d.with_secondary(
                        t.span,
                        format!("this enters `{cid}` without assigning `{name}`"),
                    );
                }
            }
            Some(_) => {
                let initial = self.chart.parent[culprit]
                    .and_then(|p| states[p])
                    .and_then(|p| p.initial_span);
                match initial {
                    Some(sp) if culprit != 0 => {
                        d = d.with_secondary(
                            sp,
                            format!("`{cid}` is entered when `{}` starts, and nothing before it assigns `{name}`", info.name),
                        );
                    }
                    _ => {
                        if let Some(sp) = decl {
                            d = d.with_secondary(sp, "declared here with no initial value");
                        }
                    }
                }
            }
            None => {
                if let Some(sp) = decl {
                    d = d.with_secondary(sp, "declared here with no initial value");
                }
            }
        }
        let in_region = {
            let mut a = self.chart.parent[s];
            let mut found = false;
            while let Some(x) = a {
                found |= self.chart.kind(x) == StateKind::Parallel;
                a = self.chart.parent[x];
            }
            found
        };
        let help = match f.part {
            Part::Exit => format!(
                "`exit` can run as soon as `{id}` is entered (a transition out, or cancelling the run), so assign `{name}` on every way into `{id}`, give it an initial value, or make its type optional"
            ),
            Part::Entry => format!(
                "assign `{name}` on every way into `{id}` (a `set` in the transition, or in an `entry` that runs first), give it an initial value, or make its type optional"
            ),
            Part::Other => format!(
                "assign `{name}` on every way into `{cid}`, give it an initial value, or make its type optional"
            ),
        };
        let help = if in_region {
            format!(
                "{help}; inside a parallel state, an assignment another region makes after this one was entered does not count"
            )
        } else {
            help
        };
        d.with_help(help)
    }
}
