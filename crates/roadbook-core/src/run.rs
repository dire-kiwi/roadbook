//! `start` and `step`, exactly as `spec/execution.md` gives them.

use std::collections::VecDeque;

use roadbook_ir::{
    Address, AddressKind, Effect, InvokeKind, Machine, ReplyOutcome, State, StateKind, Transition,
    Trigger, TypeRef, UNANSWERED,
};
use serde_json::{Map, Value};

use crate::eval::{Fns, Scope, eval};
use crate::program::{MachineIndex, Program};
use crate::{
    CallEntry, Command, Defect, Input, InvokeEntry, Object, Record, RunResult, SETTLE_LIMIT, Saved,
    Snapshot, SpawnedEntry, SrcKind, Status, TimerEntry,
};

/// The built-in error every call can fail with: `{"_tag": "Unanswered"}`.
fn unanswered() -> Value {
    serde_json::json!({ "_tag": UNANSWERED })
}

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

/// The implicit names a transition binds, and their values: none, one of `event`, `result`
/// and `error`, or `event` and `call` for a transition that takes a call.
type Implicit<'a> = &'a [(&'static str, Value)];

/// A transition: its state's index and its index in that state's `transitions`.
type Tr = (usize, usize);

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
    /// The children spawned during this macrostep, not started yet: collection, key, args.
    pending: Vec<(String, Value, Object)>,
    /// `key`, while the transitions an input from a spawned child takes are selected and run.
    key: Option<Value>,
    fns: &'f mut Fns<'g>,
}

type R<T = ()> = Result<T, Defect>;

impl Program {
    /// Starts machine `machine` as run `run` with `input` (its arguments keyed by parameter
    /// name). The run id names the run's child runs in `sent` records.
    pub fn start(
        &self,
        machine: &str,
        run: &str,
        input: Object,
        fns: &mut Fns<'_>,
    ) -> Result<StepResult, UnknownMachine> {
        let (m, idx) = self
            .machine(machine)
            .ok_or_else(|| UnknownMachine(machine.to_owned()))?;
        let snap = Snapshot {
            machine: machine.to_owned(),
            run: run.to_owned(),
            ir: m.hash.clone(),
            status: Status::Running,
            active: Vec::new(),
            input,
            vars: Map::new(),
            invokes: Vec::new(),
            calls: Vec::new(),
            spawned: Vec::new(),
            timers: Vec::new(),
            next_invoke: 1,
            next_timer: 1,
            next_message: 1,
            history: Default::default(),
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

impl Program {
    /// Starts run `run` again from the saved snapshot `saved`, which
    /// [`Program::check_saved`] has passed (`spec/execution.md#restoring-a-snapshot`): writes
    /// `restored`, restarts the invokes that were running and the timers not yet due (for the
    /// time that remains until their `due`), and queues the expiry of those already due at
    /// `now` with `timer.fire`, the earliest first. `migrated` is the hash the snapshot had
    /// before a migration. No state is entered or exited and the run does not settle.
    pub fn restore(
        &self,
        saved: &Saved,
        run: &str,
        now: u64,
        migrated: Option<String>,
        fns: &mut Fns<'_>,
    ) -> Result<StepResult, UnknownMachine> {
        let (m, idx) = self
            .machine(&saved.machine)
            .ok_or_else(|| UnknownMachine(saved.machine.clone()))?;
        let snap = saved.to_snapshot(run);
        let restored = Record::Restored {
            machine: saved.machine.clone(),
            ir: m.hash.clone(),
            migrated,
            snapshot: Box::new(saved.clone()),
        };
        let mut cx = Ctx::new(self, m, idx, snap.clone(), fns);
        cx.trace(restored.clone());
        match cx.restart(saved, now) {
            Ok(()) => Ok(cx.finish()),
            Err(d) => {
                let mut cx = Ctx::new(self, m, idx, snap, cx.fns);
                cx.trace(restored);
                cx.fail_with_defect(d);
                Ok(cx.finish())
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
            pending: Vec::new(),
            key: None,
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

    /// Emits a trace record, redacted (`spec/trace.md#redaction`). An `event`, `dropped` or
    /// `stale` record holds an input [`Ctx::take`] has already redacted.
    fn trace(&mut self, mut record: Record) {
        self.program.redactor().record(self.m, &mut record);
        self.cmds.push(Command::Trace { record });
    }

    fn eval(&mut self, e: &roadbook_ir::Expr, implicit: Implicit<'_>) -> R<Value> {
        let scope = Scope {
            program: self.program,
            machine: self.m,
            input: &self.snap.input,
            vars: &self.snap.vars,
            implicit,
            key: self.key.as_ref(),
        };
        eval(e, &scope, self.fns)
    }

    /// The value of parameter or variable `name`, read as an expression `ref` reads it.
    fn read(&self, name: &str, implicit: Implicit<'_>) -> R<Value> {
        let scope = Scope {
            program: self.program,
            machine: self.m,
            input: &self.snap.input,
            vars: &self.snap.vars,
            implicit,
            key: self.key.as_ref(),
        };
        crate::eval::lookup(name, &scope)
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
                let value = self.eval(init, &[])?;
                self.snap.vars.insert(v.name.clone(), value);
            } else if matches!(*self.program.resolve(&v.ty), TypeRef::Optional(_)) {
                self.snap.vars.insert(v.name.clone(), Value::Null);
            }
        }
        let mut list = vec![0];
        list.extend(self.default_entry(0));
        self.enter_states(&list, &[])?;
        self.settle()?;
        if self.snap.status == Status::Running {
            self.start_work()?;
        }
        Ok(())
    }

    /// The states entered below `s` when it is entered with no more specific target, in
    /// document order: a compound state's initial chain, every region of a parallel state.
    fn default_entry(&self, s: usize) -> Vec<usize> {
        let mut out = Vec::new();
        self.default_entry_into(s, &mut out);
        out
    }

    fn default_entry_into(&self, s: usize, out: &mut Vec<usize>) {
        let state = self.state(s);
        match state.kind {
            StateKind::Compound => {
                if state.initial.is_some() {
                    let c = self.idx.initial[s].expect("an initial state is a child");
                    out.push(c);
                    self.default_entry_into(c, out);
                }
            }
            StateKind::Parallel => {
                for &c in &self.idx.children[s] {
                    out.push(c);
                    self.default_entry_into(c, out);
                }
            }
            StateKind::Atomic | StateKind::Final | StateKind::History | StateKind::DeepHistory => {}
        }
    }

    // ---- restore -------------------------------------------------------------------------

    fn restart(&mut self, saved: &Saved, now: u64) -> R {
        for ch in self.snap.spawned.clone() {
            self.start_child(&ch);
        }
        for s in self.active.clone() {
            let state = self.state(s);
            if let Some(inv) = self.snap.invokes.iter().find(|i| i.state == state.id) {
                let id = inv.id;
                self.start_invoke(s, id)?;
            }
            let mut timers: Vec<_> = saved
                .timers
                .iter()
                .filter(|t| t.state == state.id && t.due > now)
                .collect();
            timers.sort_by_key(|t| t.id);
            for tm in timers {
                let delay = tm.due - now;
                self.trace(Record::TimerStarted {
                    timer: tm.id,
                    state: state.id.clone(),
                    index: tm.index,
                    delay,
                });
                self.cmds.push(Command::TimerStart {
                    timer: tm.id,
                    delay,
                });
            }
        }
        let mut due: Vec<_> = saved.timers.iter().filter(|t| t.due <= now).collect();
        due.sort_by_key(|t| (t.due, t.id));
        for tm in due {
            self.cmds.push(Command::TimerFire { timer: tm.id });
        }
        Ok(())
    }

    // ---- step ----------------------------------------------------------------------------

    fn take(&mut self, input: Input) -> R {
        // The input as its record shows it, redacted while the invoke or spawned child it may
        // be the outcome of is still in the snapshot. `None` when the IR marks nothing sensitive.
        let redactor = self.program.redactor();
        let shown = redactor.active().then(|| {
            let mut shown = input.clone();
            redactor.input(self.m, &self.snap.invokes, &self.snap.spawned, &mut shown);
            shown
        });
        let chosen: Vec<Tr>;
        let bound: Vec<(&'static str, Value)>;
        match &input {
            Input::Cancel => return self.cancel_run(),
            Input::InvokeDone { invoke, .. }
            | Input::InvokeFail { invoke, .. }
            | Input::InvokeDefect { invoke, .. } => {
                let Some(pos) = self.snap.invokes.iter().position(|i| i.id == *invoke) else {
                    self.trace(Record::Stale {
                        event: shown.unwrap_or(input),
                    });
                    return Ok(());
                };
                let inv = self.snap.invokes.remove(pos);
                let owner = self.index_of(&inv.state);
                match &input {
                    Input::InvokeDefect { defect, .. } => {
                        let defect = defect.clone();
                        self.trace(Record::Event {
                            event: shown.unwrap_or(input),
                        });
                        self.fail_with_defect(defect);
                        return Ok(());
                    }
                    Input::InvokeDone { value, .. } => {
                        bound = vec![("result", value.clone())];
                        chosen = self
                            .select_in(owner, &bound, |t| matches!(t, Trigger::InvokeDone), None)?
                            .into_iter()
                            .collect();
                    }
                    Input::InvokeFail { error, .. } => {
                        let tag = error.get("_tag").and_then(Value::as_str).unwrap_or("");
                        if !self.invoke_declares(owner, &inv.src, tag) {
                            return Err(Defect::Undeclared {
                                error: tag.to_owned(),
                            });
                        }
                        let tag = tag.to_owned();
                        bound = vec![("error", error.clone())];
                        let s = self.state(owner);
                        let listed = s.transitions.iter().any(
                            |t| matches!(&t.trigger, Trigger::InvokeFail { error } if *error == tag),
                        );
                        let want = if listed { tag } else { "*".to_owned() };
                        chosen = self
                            .select_in(
                                owner,
                                &bound,
                                |t| matches!(t, Trigger::InvokeFail { error } if *error == want),
                                None,
                            )?
                            .into_iter()
                            .collect();
                    }
                    _ => unreachable!(),
                }
            }
            Input::SpawnDone { spawn, .. }
            | Input::SpawnFail { spawn, .. }
            | Input::SpawnDefect { spawn, .. } => {
                let Some(pos) = self.snap.spawned.iter().position(|c| c.id == *spawn) else {
                    self.trace(Record::Stale {
                        event: shown.unwrap_or(input),
                    });
                    return Ok(());
                };
                let ch = self.snap.spawned.remove(pos);
                let machine = self.collection_machine(&ch.children).to_owned();
                self.key = Some(ch.key.clone());
                match &input {
                    Input::SpawnDefect { defect, .. } => {
                        let defect = defect.clone();
                        self.trace(Record::Event {
                            event: shown.unwrap_or(input),
                        });
                        self.fail_with_defect(defect);
                        return Ok(());
                    }
                    Input::SpawnDone { value, .. } => {
                        bound = vec![("result", value.clone())];
                        chosen = self.select_walks(|cx, s| {
                            let state = cx.state(s);
                            for (i, t) in state.transitions.iter().enumerate() {
                                if matches!(&t.trigger, Trigger::SpawnDone { children } if *children == ch.children)
                                    && cx.holds(t, &bound)?
                                {
                                    return Ok(Some((s, i)));
                                }
                            }
                            Ok(None)
                        })?;
                    }
                    Input::SpawnFail { error, .. } => {
                        let tag = error.get("_tag").and_then(Value::as_str).unwrap_or("");
                        if !self.declares(&machine, tag) {
                            return Err(Defect::Undeclared {
                                error: tag.to_owned(),
                            });
                        }
                        let tag = tag.to_owned();
                        bound = vec![("error", error.clone())];
                        let children = ch.children.clone();
                        chosen = self.select_walks(|cx, s| {
                            let state = cx.state(s);
                            let is = |t: &Trigger, want: &str| {
                                matches!(t, Trigger::SpawnFail { children: c, error } if *c == children && error == want)
                            };
                            let listed = state.transitions.iter().any(|t| is(&t.trigger, &tag));
                            let want = if listed { tag.as_str() } else { "*" };
                            for (i, t) in state.transitions.iter().enumerate() {
                                if is(&t.trigger, want) && cx.holds(t, &bound)? {
                                    return Ok(Some((s, i)));
                                }
                            }
                            Ok(None)
                        })?;
                    }
                    _ => unreachable!(),
                }
            }
            Input::After { timer } => {
                let Some(pos) = self.snap.timers.iter().position(|t| t.id == *timer) else {
                    self.trace(Record::Stale {
                        event: shown.unwrap_or(input),
                    });
                    return Ok(());
                };
                let tm = self.snap.timers.remove(pos);
                let owner = self.index_of(&tm.state);
                let group = self.state(owner).transitions[tm.index].group;
                bound = Vec::new();
                chosen = self
                    .select_in(
                        owner,
                        &[],
                        |t| matches!(t, Trigger::After { .. }),
                        Some(group),
                    )?
                    .into_iter()
                    .collect();
            }
            Input::Event { value } | Input::Message { value, .. } => {
                bound = vec![("event", value.clone())];
                chosen = self.select_event(value)?;
            }
            Input::Call {
                from,
                invoke,
                call,
                args,
            } => {
                let pending = CallEntry {
                    from: from.clone(),
                    invoke: *invoke,
                    call: call.clone(),
                };
                bound = vec![
                    ("event", Value::Object(args.clone())),
                    ("call", pending.value()),
                ];
                chosen = self.select_call(call, &bound)?;
                if chosen.is_empty() {
                    // A strict machine fails on a call it does not handle; its driver answers
                    // the call `Unanswered` as the run ends.
                    if self.m.strict {
                        return Err(Defect::Unhandled {
                            event: Box::new(input.clone()),
                        });
                    }
                    // Dropped, and answered at once, so that its caller does not wait.
                    self.trace(Record::Dropped {
                        event: shown.unwrap_or(input),
                    });
                    self.answer(pending, ReplyOutcome::Fail, unanswered());
                    return Ok(());
                }
                self.trace(Record::Event {
                    event: shown.unwrap_or(input),
                });
                // Pending from now on, so that a `reply` among the effects finds it.
                self.snap.calls.push(pending);
                return self.go(&chosen, &bound);
            }
            Input::Emitted {
                from,
                invoke,
                value,
                ..
            } => {
                if let Some(i) = invoke {
                    let child = self.snap.spawned.iter().find(|c| c.id == *i);
                    if child.is_none() && !self.snap.invokes.iter().any(|e| e.id == *i) {
                        self.trace(Record::Stale {
                            event: shown.unwrap_or(input),
                        });
                        return Ok(());
                    }
                    self.key = child.map(|c| c.key.clone());
                }
                bound = vec![("event", value.clone())];
                chosen = self.select_emitted(from, *invoke, value)?;
            }
            Input::Ended { from, status } => {
                bound = vec![("event", serde_json::json!({ "status": status.as_str() }))];
                chosen = self.select_ended(from, &bound)?;
            }
            Input::Defect { .. } => unreachable!("handled by step"),
        }
        if chosen.is_empty() {
            // A strict machine fails on one of its own events, or a message, that it does not
            // handle (a call is above).
            if self.m.strict && matches!(input, Input::Event { .. } | Input::Message { .. }) {
                return Err(Defect::Unhandled {
                    event: Box::new(input.clone()),
                });
            }
            self.trace(Record::Dropped {
                event: shown.unwrap_or(input),
            });
            return Ok(());
        }
        self.trace(Record::Event {
            event: shown.unwrap_or(input),
        });
        self.go(&chosen, &bound)
    }

    /// The rest of a step that takes `chosen`: its microstep, settling, and starting work.
    fn go(&mut self, chosen: &[Tr], bound: Implicit<'_>) -> R {
        self.microstep(chosen, bound)?;
        self.key = None;
        self.settle()?;
        if self.snap.status == Status::Running {
            self.start_work()?;
        }
        Ok(())
    }

    /// The machine the children of collection `children` run.
    fn collection_machine(&self, children: &str) -> &'p str {
        self.m
            .children
            .get(children)
            .map(|c| c.machine.as_str())
            .unwrap_or_default()
    }

    /// Whether the invoke of state `owner`, of `src`, declares the error `tag`. A call's are
    /// its declared errors and `Unanswered`.
    fn invoke_declares(&self, owner: usize, src: &str, tag: &str) -> bool {
        let ir = self.program.ir();
        if let Some(inv) = &self.state(owner).invoke
            && inv.kind == InvokeKind::Call
        {
            let call = inv
                .to
                .as_ref()
                .and_then(|to| ir.machines.get(&to.machine))
                .and_then(|m| m.calls.get(&inv.src));
            return tag == UNANSWERED || call.is_some_and(|c| c.errors.iter().any(|e| e == tag));
        }
        self.declares(src, tag)
    }

    /// Whether task or machine `src` declares the error `tag`.
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
        implicit: Implicit<'_>,
        matches: impl Fn(&Trigger) -> bool,
        group: Option<u32>,
    ) -> R<Option<(usize, usize)>> {
        let state = self.state(s);
        for (i, t) in state.transitions.iter().enumerate() {
            if matches(&t.trigger)
                && group.is_none_or(|g| g == t.group)
                && self.holds(t, implicit)?
            {
                return Ok(Some((s, i)));
            }
        }
        Ok(None)
    }

    fn select_event(&mut self, value: &Value) -> R<Vec<Tr>> {
        let tag = value.get("_tag").and_then(Value::as_str).unwrap_or("");
        let bound = [("event", value.clone())];
        self.select_walks(|cx, s| {
            let state = cx.state(s);
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
                if candidate && cx.holds(t, &bound)? {
                    return Ok(Some((s, i)));
                }
            }
            Ok(None)
        })
    }

    /// `on c`: the candidates of each state are its transitions that take call `call`, with
    /// `event` and `call` bound as `bound` gives them. There is no fallback to `on *`.
    fn select_call(&mut self, call: &str, bound: Implicit<'_>) -> R<Vec<Tr>> {
        self.select_walks(|cx, s| {
            let state = cx.state(s);
            for (i, t) in state.transitions.iter().enumerate() {
                let candidate = matches!(&t.trigger, Trigger::Call { call: c } if c == call);
                if candidate && cx.holds(t, bound)? {
                    return Ok(Some((s, i)));
                }
            }
            Ok(None)
        })
    }

    /// The active leaves, in document order: the active states with no active child.
    fn active_leaves(&self) -> Vec<usize> {
        self.active
            .iter()
            .copied()
            .filter(|&s| !self.active.iter().any(|&c| self.idx.parents[c] == Some(s)))
            .collect()
    }

    /// `selectByWalks`: each active leaf, in document order, walks outward to the first state
    /// that offers a transition (`first`, which tries the state's candidates in document
    /// order); the transitions found, each once, with conflicts removed. A state's offer is
    /// remembered, so a state that several walks reach is searched once.
    fn select_walks(
        &mut self,
        mut first: impl FnMut(&mut Self, usize) -> R<Option<Tr>>,
    ) -> R<Vec<Tr>> {
        // A run has few active states: a list is quicker than a map.
        let mut offered: Vec<(usize, Option<Tr>)> = Vec::new();
        let mut enabled: Vec<Tr> = Vec::new();
        for leaf in self.active_leaves() {
            let mut cur = Some(leaf);
            while let Some(s) = cur {
                let t = match offered.iter().find(|(o, _)| *o == s) {
                    Some((_, t)) => *t,
                    None => {
                        let t = first(self, s)?;
                        offered.push((s, t));
                        t
                    }
                };
                if let Some(t) = t {
                    if !enabled.contains(&t) {
                        enabled.push(t);
                    }
                    break;
                }
                cur = self.idx.parents[s];
            }
        }
        Ok(self.remove_conflicts(enabled))
    }

    /// SCXML's `removeConflictingTransitions`.
    fn remove_conflicts(&self, enabled: Vec<Tr>) -> Vec<Tr> {
        let mut kept: Vec<(Tr, Vec<usize>)> = Vec::new();
        for t1 in enabled {
            let x1 = self.exit_set(t1);
            let mut preempted = false;
            let mut replaced = Vec::new();
            for (t2, x2) in &kept {
                if x1.iter().any(|s| x2.contains(s)) {
                    if self.idx.is_ancestor(t2.0, t1.0) {
                        replaced.push(*t2);
                    } else {
                        preempted = true;
                        break;
                    }
                }
            }
            if !preempted {
                kept.retain(|(t, _)| !replaced.contains(t));
                kept.push((t1, x1));
            }
        }
        kept.into_iter().map(|(t, _)| t).collect()
    }

    fn target(&self, (source, index): Tr) -> Option<usize> {
        self.idx.targets[source][index]
    }

    /// The domain of a transition from `source` to `target`: the root for one of the root's,
    /// else the nearest proper ancestor of `source` that is a compound state and a proper
    /// ancestor of `target`. Never a parallel state.
    fn domain(&self, source: usize, target: usize) -> usize {
        if source == 0 {
            return 0;
        }
        let mut cur = self.idx.parents[source];
        while let Some(x) = cur {
            if self.state(x).kind == StateKind::Compound && self.idx.is_ancestor(x, target) {
                return x;
            }
            cur = self.idx.parents[x];
        }
        0
    }

    /// The active states a transition exits, in document order: none for one with no target.
    fn exit_set(&self, t: Tr) -> Vec<usize> {
        let Some(target) = self.target(t) else {
            return Vec::new();
        };
        let domain = self.domain(t.0, target);
        self.active
            .iter()
            .copied()
            .filter(|&s| self.idx.is_ancestor(domain, s))
            .collect()
    }

    /// The states a transition with a target enters: the path from just below its domain to
    /// the target, the target's default entry, and every other region of a parallel state on
    /// the path by default entry.
    fn entry_set(&self, t: Tr, target: usize) -> Vec<usize> {
        if self.state(target).kind.is_history() {
            return self.history_entry_set(t, target);
        }
        let domain = self.domain(t.0, target);
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
        let mut set = path.clone();
        self.default_entry_into(target, &mut set);
        for &p in &path {
            if p != target && self.state(p).kind == StateKind::Parallel {
                for &c in &self.idx.children[p] {
                    if !path.contains(&c) {
                        set.push(c);
                        self.default_entry_into(c, &mut set);
                    }
                }
            }
        }
        set
    }

    /// `entrySet` for a transition to history state `h`: the paths from just below the domain
    /// to what `h` recorded (or to the states its parent's default entry starts with), their
    /// default entries, and every region of a parallel state among them that none of them is
    /// in, by default entry. Unsorted.
    fn history_entry_set(&self, t: Tr, h: usize) -> Vec<usize> {
        let domain = self.domain(t.0, h);
        let p = self.idx.parents[h].expect("a history state has a parent");
        let parent = self.state(p);
        let targets: Vec<usize> = match self.snap.history.get(&self.state(h).id) {
            Some(record) => record.iter().map(|id| self.index_of(id)).collect(),
            None if parent.kind == StateKind::Compound => {
                parent.initial.iter().map(|id| self.index_of(id)).collect()
            }
            None => self.idx.children[p].clone(),
        };
        let mut set = Vec::new();
        for r in targets {
            let mut cur = Some(r);
            while let Some(c) = cur.filter(|&c| c != domain) {
                if !set.contains(&c) {
                    set.push(c);
                }
                cur = self.idx.parents[c];
            }
            self.default_entry_into(r, &mut set);
        }
        let mut i = 0;
        while i < set.len() {
            let q = set[i];
            if self.state(q).kind == StateKind::Parallel {
                for &c in &self.idx.children[q] {
                    let covered = set.iter().any(|&x| x == c || self.idx.is_ancestor(c, x));
                    if !covered {
                        set.push(c);
                        self.default_entry_into(c, &mut set);
                    }
                }
            }
            i += 1;
        }
        set
    }

    /// Whether `address` names the sender `from` of an emitted event, which this run's
    /// invoke `invoke` started (`None`: a peer this run holds a `ref` to).
    fn names_sender(&self, address: &Address, from: &str, invoke: Option<u64>) -> bool {
        match address.kind {
            AddressKind::Child => invoke.is_some_and(|i| {
                self.snap
                    .invokes
                    .iter()
                    .any(|e| e.id == i && Some(&e.state) == address.state.as_ref())
            }),
            AddressKind::Children => invoke.is_some_and(|i| {
                self.snap
                    .spawned
                    .iter()
                    .any(|c| c.id == i && c.children == address.name)
            }),
            AddressKind::Ref => {
                invoke.is_none()
                    && self
                        .snap
                        .input
                        .get(&address.name)
                        .or_else(|| self.snap.vars.get(&address.name))
                        .and_then(Value::as_str)
                        == Some(from)
            }
        }
    }

    /// Whether `address`, of an `ended` trigger, names run `from`: its parameter or variable
    /// (of type `ref M`, `ref M?` or `ref M[]`) holds `from`, or a list that has it, now. A
    /// `null`, and a variable with no value, name no run.
    fn names_peer(&self, address: &Address, from: &str) -> bool {
        let held = self
            .snap
            .input
            .get(&address.name)
            .or_else(|| self.snap.vars.get(&address.name));
        match held {
            Some(Value::String(id)) => id == from,
            Some(Value::Array(items)) => items.iter().any(|v| v.as_str() == Some(from)),
            _ => false,
        }
    }

    /// `selectEnded`: the candidates of each state are its `ended` transitions whose address
    /// names run `from` when the input is taken. There is no fallback to `on *`.
    fn select_ended(&mut self, from: &str, bound: Implicit<'_>) -> R<Vec<Tr>> {
        self.select_walks(|cx, s| {
            let state = cx.state(s);
            for (i, t) in state.transitions.iter().enumerate() {
                let candidate = matches!(
                    &t.trigger,
                    Trigger::Ended { from: address } if cx.names_peer(address, from)
                );
                if candidate && cx.holds(t, bound)? {
                    return Ok(Some((s, i)));
                }
            }
            Ok(None)
        })
    }

    fn select_emitted(&mut self, from: &str, invoke: Option<u64>, value: &Value) -> R<Vec<Tr>> {
        let tag = value.get("_tag").and_then(Value::as_str).unwrap_or("");
        let bound = [("event", value.clone())];
        self.select_walks(|cx, s| {
            let state = cx.state(s);
            for (i, t) in state.transitions.iter().enumerate() {
                let candidate = matches!(
                    &t.trigger,
                    Trigger::Emitted { from: address, event }
                        if event == tag && cx.names_sender(address, from, invoke)
                );
                if candidate && cx.holds(t, &bound)? {
                    return Ok(Some((s, i)));
                }
            }
            Ok(None)
        })
    }

    // ---- microsteps ----------------------------------------------------------------------

    /// Takes the transitions of `t` together: their `transition` records, every exit they
    /// cause (innermost first), each transition's effects in order, every entry (in document
    /// order).
    fn microstep(&mut self, t: &[Tr], implicit: Implicit<'_>) -> R {
        for &(source, index) in t {
            let state = self.state(source);
            self.trace(Record::Transition {
                from: state.id.clone(),
                index,
                to: state.transitions[index].target.clone(),
            });
        }
        let mut exits: Vec<usize> = t.iter().flat_map(|&x| self.exit_set(x)).collect();
        exits.sort_unstable();
        exits.dedup();
        self.record_history(&exits);
        for s in exits.into_iter().rev() {
            self.exit_state(s)?;
        }
        for &(source, index) in t {
            let effects = &self.state(source).transitions[index].effects;
            self.run_effects(effects, implicit)?;
        }
        let mut entries: Vec<usize> = Vec::new();
        for &x in t {
            if let Some(target) = self.target(x) {
                entries.extend(self.entry_set(x, target));
            }
        }
        entries.sort_unstable();
        entries.dedup();
        self.enter_states(&entries, &[])
    }

    /// `recordHistory`: for each state about to be exited, what each of its history states
    /// records, from the active states as they are before anything is exited.
    fn record_history(&mut self, exits: &[usize]) {
        for &s in exits {
            for h in &self.state(s).states {
                let record: Vec<usize> = match h.kind {
                    StateKind::History => self.idx.children[s]
                        .iter()
                        .copied()
                        .filter(|&c| self.is_active(c))
                        .collect(),
                    StateKind::DeepHistory => self
                        .active_leaves()
                        .into_iter()
                        .filter(|&l| self.idx.is_ancestor(s, l))
                        .collect(),
                    _ => continue,
                };
                let ids = record.iter().map(|&i| self.state(i).id.clone()).collect();
                self.snap.history.insert(h.id.clone(), ids);
            }
        }
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
        self.run_effects(&self.state(s).exit, &[])?;
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
                    Some(_) => self.complete(s),
                }
            }
        }
        Ok(())
    }

    /// Queues the completions that entering final state `f` causes: its compound parent's,
    /// then each enclosing parallel state's that is now in a final state, innermost first.
    fn complete(&mut self, f: usize) {
        let mut s = f;
        if let Some(p) = self.idx.parents[f] {
            if self.state(p).kind == StateKind::Compound {
                self.completion.push_back(p);
                s = p;
            }
        }
        while let Some(p) = self.idx.parents[s] {
            if self.state(p).kind != StateKind::Parallel || !self.in_final(p) {
                break;
            }
            self.completion.push_back(p);
            s = p;
        }
    }

    fn is_active(&self, s: usize) -> bool {
        self.active.binary_search(&s).is_ok()
    }

    /// SCXML's `isInFinalState`, for an active state.
    fn in_final(&self, s: usize) -> bool {
        if !self.is_active(s) {
            return false;
        }
        let children = &self.idx.children[s];
        match self.state(s).kind {
            StateKind::Final => true,
            StateKind::Atomic | StateKind::History | StateKind::DeepHistory => false,
            StateKind::Compound => children
                .iter()
                .any(|&c| self.is_active(c) && self.state(c).kind == StateKind::Final),
            StateKind::Parallel => children.iter().all(|&c| self.in_final(c)),
        }
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
            match e {
                Effect::Set { .. } => {}
                Effect::Do { action, args } => {
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
                Effect::Send {
                    to,
                    key: None,
                    value,
                    ..
                } if to.kind == AddressKind::Children => {
                    // `send name.*.E`: the value once, then a message of its own to each child.
                    let value = self.eval(value, implicit)?;
                    let children: Vec<u64> = self
                        .snap
                        .spawned
                        .iter()
                        .filter(|c| c.children == to.name)
                        .map(|c| c.id)
                        .collect();
                    for id in children {
                        let message = self.next_message();
                        let to = format!("{}.{id}", self.snap.run);
                        self.trace(Record::Sent {
                            message,
                            to: to.clone(),
                            value: value.clone(),
                        });
                        self.cmds.push(Command::Send {
                            to,
                            invoke: Some(id),
                            message,
                            value: value.clone(),
                        });
                    }
                }
                Effect::Send { to, key, value, .. } => {
                    let message = self.next_message();
                    let receiver = to.machine.as_str();
                    let (to, invoke, value) = match to.kind {
                        AddressKind::Children => {
                            let key = match key {
                                Some(k) => key_form(self.eval(k, implicit)?),
                                None => Value::Null,
                            };
                            let value = self.eval(value, implicit)?;
                            let id = self
                                .snap
                                .spawned
                                .iter()
                                .find(|c| c.children == to.name && c.key == key)
                                .map(|c| c.id);
                            let Some(id) = id else {
                                self.trace(Record::Undelivered {
                                    message,
                                    to: None,
                                    value,
                                });
                                continue;
                            };
                            (format!("{}.{id}", self.snap.run), Some(id), value)
                        }
                        AddressKind::Child => {
                            let inv = self
                                .snap
                                .invokes
                                .iter()
                                .find(|i| Some(&i.state) == to.state.as_ref())
                                .map(|i| i.id);
                            let value = self.eval(value, implicit)?;
                            let Some(inv) = inv else {
                                let mut value = value;
                                self.program.redactor().sent(receiver, &mut value);
                                self.trace(Record::Undelivered {
                                    message,
                                    to: None,
                                    value,
                                });
                                continue;
                            };
                            (format!("{}.{inv}", self.snap.run), Some(inv), value)
                        }
                        AddressKind::Ref => {
                            let to = self.read(&to.name, implicit)?;
                            let to = to.as_str().unwrap_or_default().to_owned();
                            (to, None, self.eval(value, implicit)?)
                        }
                    };
                    let mut shown = value.clone();
                    self.program.redactor().sent(receiver, &mut shown);
                    self.trace(Record::Sent {
                        message,
                        to: to.clone(),
                        value: shown,
                    });
                    self.cmds.push(Command::Send {
                        to,
                        invoke,
                        message,
                        value,
                    });
                }
                Effect::Emit { value, .. } => {
                    let message = self.next_message();
                    let value = self.eval(value, implicit)?;
                    self.trace(Record::Emitted {
                        message,
                        value: value.clone(),
                    });
                    self.cmds.push(Command::Emit { message, value });
                }
                Effect::Reply {
                    call,
                    outcome,
                    value,
                } => {
                    // The pending-call value first, then the result or error.
                    let c = self.eval(call, implicit)?;
                    let pos = self.snap.calls.iter().position(|p| {
                        Some(p.from.as_str()) == c["from"].as_str()
                            && Some(p.invoke) == c["invoke"].as_u64()
                    });
                    let Some(pos) = pos else {
                        let call = match &*self.program.resolve(&call.ty) {
                            TypeRef::Call(n) => n.clone(),
                            other => other.to_string(),
                        };
                        return Err(Defect::Answered { call });
                    };
                    let v = match value {
                        Some(v) => self.eval(v, implicit)?,
                        None => Value::Null,
                    };
                    let pending = self.snap.calls.remove(pos);
                    self.answer(pending, *outcome, v);
                }
                Effect::Spawn {
                    children,
                    machine,
                    key,
                    args,
                } => {
                    let key = key_form(self.eval(key, implicit)?);
                    let params = &self.program.ir().machines[machine.as_str()].params;
                    let mut obj = Map::new();
                    for (p, a) in params.iter().zip(args) {
                        let v = self.eval(a, implicit)?;
                        obj.insert(p.name.clone(), v);
                    }
                    self.stop_child(children, &key);
                    self.pending.push((children.clone(), key, obj));
                }
                Effect::Stop { children, key } => {
                    let key = key_form(self.eval(key, implicit)?);
                    self.stop_child(children, &key);
                }
            }
        }
        Ok(())
    }

    /// Answers the pending call `p`: its `replied` record and the `reply` command.
    fn answer(&mut self, p: CallEntry, outcome: ReplyOutcome, value: Value) {
        self.trace(Record::Replied {
            to: p.from.clone(),
            invoke: p.invoke,
            call: p.call.clone(),
            outcome,
            value: value.clone(),
        });
        self.cmds.push(Command::Reply {
            to: p.from,
            invoke: p.invoke,
            call: p.call,
            outcome,
            value,
        });
    }

    /// Stops the child of `children` with `key`: a running one with a record, a pending one
    /// silently.
    fn stop_child(&mut self, children: &str, key: &Value) {
        self.pending
            .retain(|(c, k, _)| !(c == children && k == key));
        if let Some(pos) = self
            .snap
            .spawned
            .iter()
            .position(|c| c.children == children && c.key == *key)
        {
            let ch = self.snap.spawned.remove(pos);
            self.trace(Record::SpawnStopped {
                spawn: ch.id,
                children: ch.children,
                key: ch.key,
            });
            self.cmds.push(Command::SpawnStop { spawn: ch.id });
        }
    }

    /// When the run finishes or is cancelled, after every exit: stops every child still
    /// running, in the order they started, and drops the pending ones.
    fn stop_children(&mut self) {
        for ch in std::mem::take(&mut self.snap.spawned) {
            self.trace(Record::SpawnStopped {
                spawn: ch.id,
                children: ch.children,
                key: ch.key,
            });
            self.cmds.push(Command::SpawnStop { spawn: ch.id });
        }
        self.pending.clear();
    }

    /// Writes `spawn.started` for spawned child `ch` and starts it.
    fn start_child(&mut self, ch: &SpawnedEntry) {
        let src = self.collection_machine(&ch.children).to_owned();
        self.trace(Record::SpawnStarted {
            spawn: ch.id,
            children: ch.children.clone(),
            key: ch.key.clone(),
            src: src.clone(),
            args: ch.args.clone(),
        });
        self.cmds.push(Command::SpawnStart {
            spawn: ch.id,
            src,
            args: ch.args.clone(),
        });
    }

    fn next_message(&mut self) -> u64 {
        let id = self.snap.next_message;
        self.snap.next_message += 1;
        id
    }

    // ---- settling and work ---------------------------------------------------------------

    fn settle(&mut self) -> R {
        let mut count = 0;
        loop {
            if self.snap.status != Status::Running {
                return Ok(());
            }
            let mut t = self.select_walks(|cx, s| {
                cx.select_in(s, &[], |t| matches!(t, Trigger::Always), None)
            })?;
            while t.is_empty() {
                let Some(p) = self.completion.pop_front() else {
                    break;
                };
                t.extend(self.select_in(p, &[], |t| matches!(t, Trigger::Done), None)?);
            }
            if t.is_empty() {
                return Ok(());
            }
            count += 1;
            if count > SETTLE_LIMIT {
                return Err(Defect::Limit);
            }
            self.microstep(&t, &[])?;
        }
    }

    fn start_work(&mut self) -> R {
        for (children, key, args) in std::mem::take(&mut self.pending) {
            let ch = SpawnedEntry {
                id: self.snap.next_invoke,
                children,
                key,
                args,
            };
            self.snap.next_invoke += 1;
            self.snap.spawned.push(ch.clone());
            self.start_child(&ch);
        }
        for s in self.active.clone() {
            if !self.entered[s] {
                continue;
            }
            let state = self.state(s);
            if let Some(inv) = &state.invoke {
                let id = self.snap.next_invoke;
                self.snap.next_invoke += 1;
                self.snap.invokes.push(InvokeEntry {
                    id,
                    state: state.id.clone(),
                    src: inv.callee(),
                });
                self.start_invoke(s, id)?;
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
                let v = self.eval(delay, &[])?;
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

    /// Evaluates the arguments of state `s`'s invoke and starts it as invoke `id`: its
    /// `invoke.started` record and `invoke.start` command.
    fn start_invoke(&mut self, s: usize, id: u64) -> R {
        let state = self.state(s);
        let Some(inv) = &state.invoke else {
            return Ok(());
        };
        let ir = self.program.ir();
        // A call's peer is read before its arguments are evaluated, as a `send`'s address is.
        let (to, params) = match (inv.kind, &inv.to) {
            (InvokeKind::Call, Some(to)) => {
                let id = self.read(&to.name, &[])?;
                let call = &ir.machines[to.machine.as_str()].calls[inv.src.as_str()];
                (
                    Some(id.as_str().unwrap_or_default().to_owned()),
                    &call.params,
                )
            }
            (InvokeKind::Machine, _) => (None, &ir.machines[inv.src.as_str()].params),
            _ => (None, &ir.tasks[inv.src.as_str()].params),
        };
        let mut args = Map::new();
        for (p, a) in params.iter().zip(&inv.args) {
            let v = self.eval(a, &[])?;
            args.insert(p.name.clone(), v);
        }
        self.trace(Record::InvokeStarted {
            invoke: id,
            state: state.id.clone(),
            src: inv.callee(),
            args: args.clone(),
        });
        self.cmds.push(Command::InvokeStart {
            invoke: id,
            state: state.id.clone(),
            src: inv.src.clone(),
            src_kind: match inv.kind {
                InvokeKind::Task => SrcKind::Task,
                InvokeKind::Machine => SrcKind::Machine,
                InvokeKind::Call => SrcKind::Call,
            },
            to,
            alias: inv.alias.clone(),
            args,
        });
        Ok(())
    }

    // ---- ending --------------------------------------------------------------------------

    fn finish_run(&mut self, f: usize) -> R {
        let fs = self.state(f);
        let (status, value) = if let Some(out) = &fs.output {
            (Status::Done, self.eval(out, &[])?)
        } else if let Some(fail) = &fs.fail {
            (Status::Failed, self.eval(fail, &[])?)
        } else {
            (Status::Done, Value::Null)
        };
        for s in self.active.clone().into_iter().rev() {
            self.exit_state(s)?;
        }
        self.stop_children();
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
        self.stop_children();
        self.snap.status = Status::Cancelled;
        self.trace(Record::Cancelled);
        Ok(())
    }

    fn fail_with_defect(&mut self, d: Defect) {
        for inv in std::mem::take(&mut self.snap.invokes) {
            self.cmds.push(Command::InvokeCancel { invoke: inv.id });
        }
        for ch in std::mem::take(&mut self.snap.spawned) {
            self.cmds.push(Command::SpawnStop { spawn: ch.id });
        }
        self.pending.clear();
        for tm in std::mem::take(&mut self.snap.timers) {
            self.cmds.push(Command::TimerCancel { timer: tm.id });
        }
        self.snap.status = Status::Defect;
        self.snap.result = Some(RunResult::Defect(d.clone()));
        self.trace(Record::Defect { defect: d });
    }
}

/// A key in the one form keys are compared in: an integer written as a float (`1.0`, `-0.0`),
/// which a host can hand the core in an event or a function's result, as the integer. Keys are
/// equal when their JSON values are (`spec/execution.md#spawned-children`).
fn key_form(mut key: Value) -> Value {
    crate::saved::integral_numbers(&mut key);
    key
}
