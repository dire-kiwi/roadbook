//! `start` and `step`, exactly as `spec/execution.md` gives them.

use std::collections::VecDeque;

use roadbook_ir::{Effect, Machine, State, StateKind, Transition, Trigger, TypeRef};
use serde_json::{Map, Value};

use crate::eval::{Fns, Scope, eval};
use crate::program::{MachineIndex, Program};
use crate::{
    Command, Defect, Input, InvokeEntry, Object, Record, RunResult, SETTLE_LIMIT, Snapshot,
    SrcKind, Status, TimerEntry,
};

/// What `start` and `step` return: the new snapshot and the commands to carry out in order.
pub type StepResult = (Snapshot, Vec<Command>);

/// Why `start` could not even begin: the machine is not in the program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownMachine(pub String);

impl std::fmt::Display for UnknownMachine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the IR has no machine `{}`", self.0)
    }
}

impl std::error::Error for UnknownMachine {}

/// The implicit name a transition binds, and its value.
type Implicit<'a> = Option<(&'static str, &'a Value)>;

struct Ctx<'p, 'f, 'g> {
    program: &'p Program,
    m: &'p Machine,
    idx: &'p MachineIndex,
    snap: Snapshot,
    /// The active states, by index, in document order.
    active: Vec<usize>,
    cmds: Vec<Command>,
    entered: Vec<bool>,
    completion: VecDeque<usize>,
    fns: &'f mut Fns<'g>,
}

type R<T = ()> = Result<T, Defect>;

impl Program {
    /// Starts machine `machine` with `input` (its arguments keyed by parameter name).
    pub fn start(
        &self,
        machine: &str,
        input: Object,
        fns: &mut Fns<'_>,
    ) -> Result<StepResult, UnknownMachine> {
        let (m, idx) = self
            .machine(machine)
            .ok_or_else(|| UnknownMachine(machine.to_owned()))?;
        let snap = Snapshot {
            machine: machine.to_owned(),
            ir: m.hash.clone(),
            status: Status::Running,
            active: Vec::new(),
            input,
            vars: Map::new(),
            invokes: Vec::new(),
            timers: Vec::new(),
            next_invoke: 1,
            next_timer: 1,
            result: None,
        };
        let started = Record::Started {
            machine: machine.to_owned(),
            ir: m.hash.clone(),
            input: snap.input.clone(),
        };
        let mut cx = Ctx::new(self, m, idx, snap.clone(), fns);
        cx.trace(started.clone());
        match cx.start() {
            Ok(()) => Ok(cx.finish()),
            Err(d) => {
                let mut cx = Ctx::new(self, m, idx, snap, cx.fns);
                cx.trace(started);
                cx.fail_with_defect(d);
                Ok(cx.finish())
            }
        }
    }

    /// Takes one input. `snap` is not modified: the new snapshot is returned.
    pub fn step(&self, snap: &Snapshot, input: Input, fns: &mut Fns<'_>) -> StepResult {
        let Some((m, idx)) = self.machine(&snap.machine) else {
            return (snap.clone(), Vec::new());
        };
        if let Input::Defect { defect } = input {
            let mut cx = Ctx::new(self, m, idx, snap.clone(), fns);
            if snap.status != Status::Defect {
                cx.fail_with_defect(defect);
            }
            return cx.finish();
        }
        if snap.status != Status::Running {
            return (snap.clone(), Vec::new());
        }
        let mut cx = Ctx::new(self, m, idx, snap.clone(), fns);
        match cx.take(input) {
            Ok(()) => cx.finish(),
            Err(d) => {
                // Atomic: discard everything and fail the snapshot we were given.
                let mut cx = Ctx::new(self, m, idx, snap.clone(), cx.fns);
                cx.fail_with_defect(d);
                cx.finish()
            }
        }
    }
}

impl<'p, 'f, 'g> Ctx<'p, 'f, 'g> {
    fn new(
        program: &'p Program,
        m: &'p Machine,
        idx: &'p MachineIndex,
        snap: Snapshot,
        fns: &'f mut Fns<'g>,
    ) -> Self {
        let mut active: Vec<usize> = snap
            .active
            .iter()
            .filter_map(|id| idx.by_id.get(id).copied())
            .collect();
        active.sort_unstable();
        Ctx {
            program,
            m,
            idx,
            snap,
            active,
            cmds: Vec::new(),
            entered: vec![false; idx.parents.len()],
            completion: VecDeque::new(),
            fns,
        }
    }

    fn finish(mut self) -> StepResult {
        self.snap.active = self
            .active
            .iter()
            .map(|&i| self.idx.state(self.m, i).id.clone())
            .collect();
        (self.snap, self.cmds)
    }

    fn state(&self, i: usize) -> &'p State {
        self.idx.state(self.m, i)
    }

    fn index_of(&self, id: &str) -> usize {
        self.idx.by_id[id]
    }

    fn trace(&mut self, record: Record) {
        self.cmds.push(Command::Trace { record });
    }

    fn eval(&mut self, e: &roadbook_ir::Expr, implicit: Implicit<'_>) -> R<Value> {
        let scope = Scope {
            program: self.program,
            machine: self.m,
            input: &self.snap.input,
            vars: &self.snap.vars,
            implicit,
        };
        eval(e, &scope, self.fns)
    }

    fn holds(&mut self, t: &Transition, implicit: Implicit<'_>) -> R<bool> {
        match &t.guard {
            None => Ok(true),
            Some(g) => Ok(self.eval(g, implicit)?.as_bool().unwrap_or(false)),
        }
    }

    // ---- start ---------------------------------------------------------------------------

    fn start(&mut self) -> R {
        for v in &self.m.vars {
            if let Some(init) = &v.init {
                let value = self.eval(init, None)?;
                self.snap.vars.insert(v.name.clone(), value);
            } else if matches!(self.program.resolve(&v.ty), TypeRef::Optional(_)) {
                self.snap.vars.insert(v.name.clone(), Value::Null);
            }
        }
        let mut list = vec![0];
        list.extend(self.default_entry(0));
        self.enter_states(&list, None)?;
        self.settle()?;
        if self.snap.status == Status::Running {
            self.start_work()?;
        }
        Ok(())
    }

    fn default_entry(&self, s: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let mut cur = self.state(s);
        while cur.kind == StateKind::Compound {
            let Some(init) = &cur.initial else { break };
            let i = self.index_of(init);
            out.push(i);
            cur = self.state(i);
        }
        out
    }

    // ---- step ----------------------------------------------------------------------------

    fn take(&mut self, input: Input) -> R {
        let chosen: Option<(usize, usize)>;
        let bound: Option<(&'static str, Value)>;
        match &input {
            Input::Cancel => return self.cancel_run(),
            Input::InvokeDone { invoke, .. }
            | Input::InvokeFail { invoke, .. }
            | Input::InvokeDefect { invoke, .. } => {
                let Some(pos) = self.snap.invokes.iter().position(|i| i.id == *invoke) else {
                    self.trace(Record::Stale { event: input });
                    return Ok(());
                };
                let inv = self.snap.invokes.remove(pos);
                let owner = self.index_of(&inv.state);
                match &input {
                    Input::InvokeDefect { defect, .. } => {
                        let defect = defect.clone();
                        self.trace(Record::Event { event: input });
                        self.fail_with_defect(defect);
                        return Ok(());
                    }
                    Input::InvokeDone { value, .. } => {
                        bound = Some(("result", value.clone()));
                        chosen = self.select_in(
                            owner,
                            bound.as_ref(),
                            |t| matches!(t, Trigger::InvokeDone),
                            None,
                        )?;
                    }
                    Input::InvokeFail { error, .. } => {
                        let tag = error.get("_tag").and_then(Value::as_str).unwrap_or("");
                        if !self.declares(&inv.src, tag) {
                            return Err(Defect::Undeclared {
                                error: tag.to_owned(),
                            });
                        }
                        let tag = tag.to_owned();
                        bound = Some(("error", error.clone()));
                        let s = self.state(owner);
                        let listed = s.transitions.iter().any(
                            |t| matches!(&t.trigger, Trigger::InvokeFail { error } if *error == tag),
                        );
                        let want = if listed { tag } else { "*".to_owned() };
                        chosen = self.select_in(
                            owner,
                            bound.as_ref(),
                            |t| matches!(t, Trigger::InvokeFail { error } if *error == want),
                            None,
                        )?;
                    }
                    _ => unreachable!(),
                }
            }
            Input::After { timer } => {
                let Some(pos) = self.snap.timers.iter().position(|t| t.id == *timer) else {
                    self.trace(Record::Stale { event: input });
                    return Ok(());
                };
                let tm = self.snap.timers.remove(pos);
                let owner = self.index_of(&tm.state);
                let group = self.state(owner).transitions[tm.index].group;
                bound = None;
                chosen = self.select_in(
                    owner,
                    None,
                    |t| matches!(t, Trigger::After { .. }),
                    Some(group),
                )?;
            }
            Input::Event { value } => {
                bound = Some(("event", value.clone()));
                chosen = self.select_event(value)?;
            }
            Input::Defect { .. } => unreachable!("handled by step"),
        }
        let Some(t) = chosen else {
            self.trace(Record::Dropped { event: input });
            return Ok(());
        };
        self.trace(Record::Event { event: input });
        self.microstep(t, bound.as_ref().map(|(n, v)| (*n, v)))?;
        self.settle()?;
        if self.snap.status == Status::Running {
            self.start_work()?;
        }
        Ok(())
    }

    fn declares(&self, src: &str, tag: &str) -> bool {
        let ir = self.program.ir();
        let errors = match (ir.tasks.get(src), ir.machines.get(src)) {
            (Some(t), _) => &t.errors,
            (None, Some(m)) => &m.errors,
            (None, None) => return false,
        };
        errors.iter().any(|e| e == tag)
    }

    /// The first transition of state `s` whose trigger matches and whose guard holds, in
    /// document order.
    fn select_in(
        &mut self,
        s: usize,
        implicit: Option<&(&'static str, Value)>,
        matches: impl Fn(&Trigger) -> bool,
        group: Option<u32>,
    ) -> R<Option<(usize, usize)>> {
        let state = self.state(s);
        for (i, t) in state.transitions.iter().enumerate() {
            if matches(&t.trigger)
                && group.is_none_or(|g| g == t.group)
                && self.holds(t, implicit.map(|(n, v)| (*n, v)))?
            {
                return Ok(Some((s, i)));
            }
        }
        Ok(None)
    }

    fn select_event(&mut self, value: &Value) -> R<Option<(usize, usize)>> {
        let tag = value.get("_tag").and_then(Value::as_str).unwrap_or("");
        let bound = ("event", value);
        for &s in self.active.clone().iter().rev() {
            let state = self.state(s);
            let has_on = state
                .transitions
                .iter()
                .any(|t| matches!(&t.trigger, Trigger::Event { event } if event == tag));
            for (i, t) in state.transitions.iter().enumerate() {
                let candidate = if has_on {
                    matches!(&t.trigger, Trigger::Event { event } if event == tag)
                } else {
                    matches!(t.trigger, Trigger::Any)
                };
                if candidate && self.holds(t, Some(bound))? {
                    return Ok(Some((s, i)));
                }
            }
        }
        Ok(None)
    }

    // ---- microsteps ----------------------------------------------------------------------

    fn microstep(&mut self, (source, index): (usize, usize), implicit: Implicit<'_>) -> R {
        let t = &self.state(source).transitions[index];
        self.trace(Record::Transition {
            from: self.state(source).id.clone(),
            index,
            to: t.target.clone(),
        });
        let Some(target_id) = &t.target else {
            return self.run_effects(&t.effects, implicit);
        };
        let target = self.index_of(target_id);
        let domain = if source == 0 {
            0
        } else {
            let mut d = self.idx.parents[source];
            while let Some(x) = d {
                if self.idx.is_ancestor(x, target) {
                    break;
                }
                d = self.idx.parents[x];
            }
            d.unwrap_or(0)
        };
        let exiting: Vec<usize> = self
            .active
            .iter()
            .rev()
            .copied()
            .filter(|&s| self.idx.is_ancestor(domain, s))
            .collect();
        for s in exiting {
            self.exit_state(s)?;
        }
        self.run_effects(&t.effects, implicit)?;
        let mut path = Vec::new();
        let mut cur = Some(target);
        while let Some(c) = cur {
            if c == domain {
                break;
            }
            path.push(c);
            cur = self.idx.parents[c];
        }
        path.reverse();
        path.extend(self.default_entry(target));
        self.enter_states(&path, None)
    }

    fn exit_state(&mut self, s: usize) -> R {
        let id = self.state(s).id.clone();
        if let Some(pos) = self.snap.invokes.iter().position(|i| i.state == id) {
            let inv = self.snap.invokes.remove(pos);
            self.trace(Record::InvokeCancelled {
                invoke: inv.id,
                state: id.clone(),
                src: inv.src,
            });
            self.cmds.push(Command::InvokeCancel { invoke: inv.id });
        }
        let mut mine: Vec<TimerEntry> = Vec::new();
        self.snap.timers.retain(|t| {
            if t.state == id {
                mine.push(t.clone());
                false
            } else {
                true
            }
        });
        mine.sort_by_key(|t| t.id);
        for tm in mine {
            self.trace(Record::TimerCancelled {
                timer: tm.id,
                state: id.clone(),
                index: tm.index,
            });
            self.cmds.push(Command::TimerCancel { timer: tm.id });
        }
        self.run_effects(&self.state(s).exit, None)?;
        self.active.retain(|&a| a != s);
        self.completion.retain(|&c| c != s);
        self.trace(Record::Exited { state: id });
        Ok(())
    }

    fn enter_states(&mut self, list: &[usize], implicit: Implicit<'_>) -> R {
        for &s in list {
            if let Err(pos) = self.active.binary_search(&s) {
                self.active.insert(pos, s);
            }
            let state = self.state(s);
            self.trace(Record::Entered {
                state: state.id.clone(),
            });
            self.run_effects(&state.entry, implicit)?;
            self.entered[s] = true;
            if state.kind == StateKind::Final {
                match self.idx.parents[s] {
                    Some(0) | None => return self.finish_run(s),
                    Some(p) => self.completion.push_back(p),
                }
            }
        }
        Ok(())
    }

    fn run_effects(&mut self, effects: &[Effect], implicit: Implicit<'_>) -> R {
        for e in effects {
            if let Effect::Set { var, value } = e {
                let v = self.eval(value, implicit)?;
                self.snap.vars.insert(var.clone(), v.clone());
                self.trace(Record::Set {
                    var: var.clone(),
                    value: v,
                });
            }
        }
        for e in effects {
            if let Effect::Do { action, args } = e {
                let decl = &self.program.ir().actions[action.as_str()];
                let mut obj = Map::new();
                for (p, a) in decl.params.iter().zip(args) {
                    let v = self.eval(a, implicit)?;
                    obj.insert(p.name.clone(), v);
                }
                self.trace(Record::Action {
                    action: action.clone(),
                    args: obj.clone(),
                });
                self.cmds.push(Command::Action {
                    action: action.clone(),
                    args: obj,
                });
            }
        }
        Ok(())
    }

    // ---- settling and work ---------------------------------------------------------------

    fn settle(&mut self) -> R {
        let mut count = 0;
        loop {
            if self.snap.status != Status::Running {
                return Ok(());
            }
            let mut t = None;
            for &s in self.active.clone().iter().rev() {
                t = self.select_in(s, None, |t| matches!(t, Trigger::Always), None)?;
                if t.is_some() {
                    break;
                }
            }
            while t.is_none() {
                let Some(p) = self.completion.pop_front() else {
                    break;
                };
                t = self.select_in(p, None, |t| matches!(t, Trigger::Done), None)?;
            }
            let Some(t) = t else { return Ok(()) };
            count += 1;
            if count > SETTLE_LIMIT {
                return Err(Defect::Limit);
            }
            self.microstep(t, None)?;
        }
    }

    fn start_work(&mut self) -> R {
        for s in self.active.clone() {
            if !self.entered[s] {
                continue;
            }
            let state = self.state(s);
            if let Some(inv) = &state.invoke {
                let id = self.snap.next_invoke;
                self.snap.next_invoke += 1;
                let ir = self.program.ir();
                let params = match inv.kind {
                    roadbook_ir::InvokeKind::Task => &ir.tasks[inv.src.as_str()].params,
                    roadbook_ir::InvokeKind::Machine => &ir.machines[inv.src.as_str()].params,
                };
                let mut args = Map::new();
                for (p, a) in params.iter().zip(&inv.args) {
                    let v = self.eval(a, None)?;
                    args.insert(p.name.clone(), v);
                }
                self.snap.invokes.push(InvokeEntry {
                    id,
                    state: state.id.clone(),
                    src: inv.src.clone(),
                });
                self.trace(Record::InvokeStarted {
                    invoke: id,
                    state: state.id.clone(),
                    src: inv.src.clone(),
                    args: args.clone(),
                });
                self.cmds.push(Command::InvokeStart {
                    invoke: id,
                    state: state.id.clone(),
                    src: inv.src.clone(),
                    src_kind: match inv.kind {
                        roadbook_ir::InvokeKind::Task => SrcKind::Task,
                        roadbook_ir::InvokeKind::Machine => SrcKind::Machine,
                    },
                    args,
                });
            }
            let mut seen = Vec::new();
            for (i, t) in state.transitions.iter().enumerate() {
                let Trigger::After { delay } = &t.trigger else {
                    continue;
                };
                if seen.contains(&t.group) {
                    continue;
                }
                seen.push(t.group);
                let v = self.eval(delay, None)?;
                let delay = v.as_u64().ok_or_else(|| {
                    Defect::host(format!(
                        "the delay `{}` is {v}, which is not a duration",
                        delay.text
                    ))
                })?;
                let id = self.snap.next_timer;
                self.snap.next_timer += 1;
                self.snap.timers.push(TimerEntry {
                    id,
                    state: state.id.clone(),
                    index: i,
                    delay,
                });
                self.trace(Record::TimerStarted {
                    timer: id,
                    state: state.id.clone(),
                    index: i,
                    delay,
                });
                self.cmds.push(Command::TimerStart { timer: id, delay });
            }
        }
        Ok(())
    }

    // ---- ending --------------------------------------------------------------------------

    fn finish_run(&mut self, f: usize) -> R {
        let fs = self.state(f);
        let (status, value) = if let Some(out) = &fs.output {
            (Status::Done, self.eval(out, None)?)
        } else if let Some(fail) = &fs.fail {
            (Status::Failed, self.eval(fail, None)?)
        } else {
            (Status::Done, Value::Null)
        };
        for s in self.active.clone().into_iter().rev() {
            self.exit_state(s)?;
        }
        self.snap.result = Some(if status == Status::Done {
            RunResult::Output(value.clone())
        } else {
            RunResult::Error(value.clone())
        });
        self.snap.status = status;
        self.completion.clear();
        let state = fs.id.clone();
        self.trace(if status == Status::Done {
            Record::Done {
                state,
                output: value,
            }
        } else {
            Record::Failed {
                state,
                error: value,
            }
        });
        Ok(())
    }

    fn cancel_run(&mut self) -> R {
        for s in self.active.clone().into_iter().rev() {
            self.exit_state(s)?;
        }
        self.snap.status = Status::Cancelled;
        self.trace(Record::Cancelled);
        Ok(())
    }

    fn fail_with_defect(&mut self, d: Defect) {
        for inv in std::mem::take(&mut self.snap.invokes) {
            self.cmds.push(Command::InvokeCancel { invoke: inv.id });
        }
        for tm in std::mem::take(&mut self.snap.timers) {
            self.cmds.push(Command::TimerCancel { timer: tm.id });
        }
        self.snap.status = Status::Defect;
        self.snap.result = Some(RunResult::Defect(d.clone()));
        self.trace(Record::Defect { defect: d });
    }
}
