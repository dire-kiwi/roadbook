//! The driver: one future per run, one FIFO per run, and the core's commands carried out in
//! order (`spec/execution.md#the-driver`).

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::future::poll_fn;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::task::{Context, Poll};

use roadbook_core::{
    Command, Defect, Input, Object, Program, Record, RestoreError, RunResult, Saved, Snapshot,
    SrcKind, Status,
};
use roadbook_ir::{Field, TypeRef};
use serde_json::{Map, Value};

use crate::defect::defect_from_panic;
use crate::dynamic::{Host, Invocation, Outcome};
use crate::env::{BoxFuture, Config, TraceRecord};
use crate::handle::{Peers, with_scope};
use crate::shared::{Finished, InvokeContext, RunHandle, Shared, with_context};

/// When a timer started at `now` with `delay` is due: their sum, capped at 2^53 - 1, the
/// largest due time a saved snapshot can hold (`spec/execution.md#time`).
fn due(now: u64, delay: u64) -> u64 {
    now.saturating_add(delay)
        .min(roadbook_ir::MAX_SAFE_INTEGER as u64)
}

/// How a run ended, as [`crate::dynamic::start`] reports it.
#[derive(Debug, Clone, PartialEq)]
pub enum RunEnd {
    /// It reached a top-level final state with an output (`null` for none).
    Done(Value),
    /// It reached a top-level final state with `fail`.
    Failed(Value),
    /// It was cancelled from outside.
    Cancelled,
    /// It ended with a defect.
    Defect(Defect),
    /// It was suspended once idle ([`crate::RunHandle::suspend`]), giving its saved snapshot.
    /// It has not ended: it goes on from the snapshot when that is restored.
    Suspended(Box<Saved>),
}

/// How a driver begins its run.
enum Begin<'a> {
    /// `start` with the machine's arguments.
    Start(&'a str, Object),
    /// `restore` from a checked snapshot, with the hash it had before a migration.
    Restore(Box<Saved>, Option<String>),
}

/// What the run loop takes next.
enum Next {
    Input(Input),
    Suspend,
}

/// A running invoke or spawned child: its future and, for a machine, the context it is
/// polled in. `spawn` says that its outcome is a `spawn.*` input.
struct Slot<'a> {
    fut: BoxFuture<'a, Outcome>,
    ctx: Option<Arc<InvokeContext>>,
    spawn: bool,
}

pub(crate) struct Driver<'a> {
    program: &'a Program,
    host: &'a dyn Host,
    begin: Option<Begin<'a>>,
    config: Config,
    id: String,
    parent: Option<String>,
    shared: Arc<Shared>,
    /// For a child run, the invoke context it claimed.
    ctx: Option<Arc<InvokeContext>>,
    /// The runs this run holds a `ref` to, by id, from its arguments, or the host defect
    /// that checking its handles met.
    peers: Arc<Peers>,
    bad_ref: Option<Defect>,
    queue: VecDeque<Input>,
    invokes: BTreeMap<u64, Slot<'a>>,
    /// The context of every machine invoke whose outcome the core has not taken and that
    /// has not been cancelled: where a message to its child run goes.
    contexts: BTreeMap<u64, Arc<InvokeContext>>,
    timers: BTreeMap<u64, BoxFuture<'static, ()>>,
    /// When each timer of the snapshot is due, on the timer's clock.
    dues: BTreeMap<u64, u64>,
    /// The due times of the snapshot this run was restored from, for `timer.fire`.
    restored_dues: BTreeMap<u64, u64>,
    snap: Arc<Snapshot>,
    final_state: Option<String>,
    cancel_requested: bool,
    cancel_taken: bool,
    /// Whether a suspend request stops the run: a top-level run's does, unless it was
    /// started to run to its end (`run`, `run_with`). A child run ignores it.
    suspendable: bool,
    /// The message and value of the last `sent` record: the core writes it, redacted, right
    /// before the `send` command, and an `undelivered` record repeats it.
    last_sent: Option<(u64, Value)>,
}

/// Calls a host function with no invoke context and the run's handles in scope, catching a
/// panic as a host defect.
fn call_function(
    host: &dyn Host,
    peers: &Arc<Peers>,
    name: &str,
    args: Vec<Value>,
) -> Result<Value, Defect> {
    with_context(None, || {
        with_scope(Some(peers), || {
            catch_unwind(AssertUnwindSafe(|| host.function(name, args)))
                .unwrap_or_else(|p| Err(defect_from_panic(p)))
        })
    })
}

/// The run ids in `value`, a value of type `ty`, each with the machine its `ref` type names:
/// the `ref`s in it.
fn ref_ids(program: &Program, ty: &TypeRef, value: &Value, out: &mut Vec<(String, String)>) {
    match (&*program.ir().expand_top(ty), value) {
        (TypeRef::Ref(m), Value::String(id)) => out.push((id.clone(), m.clone())),
        (TypeRef::Optional(t), v) if !v.is_null() => ref_ids(program, t, v, out),
        (TypeRef::List(t), Value::Array(items)) => {
            for v in items {
                ref_ids(program, t, v, out);
            }
        }
        (TypeRef::Map(_, t), Value::Object(entries)) => {
            for v in entries.values() {
                ref_ids(program, t, v, out);
            }
        }
        _ => {}
    }
}

/// The run ids in the arguments `args` of something with parameters `params`, each with the
/// machine its `ref` type names.
fn arg_ref_ids(program: &Program, params: &[Field], args: &Object) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for p in params {
        if let Some(v) = args.get(&p.name) {
            ref_ids(program, &p.ty, v, &mut out);
        }
    }
    out
}

/// Drops `x`, catching a panic in its `Drop`. Host futures are dropped this way, so that a
/// future that panics while it is dropped cannot unwind through the driver.
fn drop_caught<T>(x: T) -> Result<(), Defect> {
    catch_unwind(AssertUnwindSafe(move || drop(x))).map_err(defect_from_panic)
}

/// Opens a run of `machine` with arguments `input` on `shared`: finds the handles of the runs
/// that `input` names in `ref` arguments among `refs`, and subscribes the run to what each of
/// them emits, before anything else can happen (`spawn` returns only after this). A run id
/// with no handle there, a handle to a run of another machine (by name or machine hash) and
/// a child run's handle are host defects before the run starts, as an argument that does not
/// encode is; the run then subscribes to nothing.
pub(crate) fn open(
    program: &Program,
    machine: &str,
    input: &Object,
    refs: &[RunHandle],
    shared: &Arc<Shared>,
) -> Result<Arc<Peers>, Defect> {
    let Some(m) = program.ir().machines.get(machine) else {
        return Ok(Arc::default());
    };
    let mut peers = Peers::new();
    for (id, of) in arg_ref_ids(program, &m.params, input) {
        if peers.contains_key(&id) {
            continue;
        }
        let Some(h) = refs.iter().find(|h| h.id() == id) else {
            return Err(Defect::host(format!(
                "machine `{machine}` was given `{id}` as a `ref`, but no handle to a run with \
                 that id"
            )));
        };
        if h.child {
            return Err(Defect::host(format!(
                "machine `{machine}` was given `{id}` as a `ref {of}`, but that is a child run, \
                 which only its parent can reach"
            )));
        }
        let hash = program.ir().machines.get(&of).map(|m| m.hash.as_str());
        if *h.machine != *of || hash.is_some_and(|hash| *h.hash != *hash) {
            let what = if *h.machine == *of {
                format!("another version of `{of}`")
            } else {
                format!("a `{}`", h.machine)
            };
            return Err(Defect::host(format!(
                "machine `{machine}` was given `{id}` as a `ref {of}`, but that run is {what}"
            )));
        }
        peers.insert(id, h.clone());
    }
    for peer in peers.values() {
        let events = program.heard(machine, &peer.machine);
        peer.shared.subscribe(shared.clone(), events);
    }
    Ok(Arc::new(peers))
}

/// The saved snapshot of a run that ended with `defect` before it started (a `ref` argument
/// it could not hold, or one that did not encode): no state is active.
pub(crate) fn early_defect(
    program: &Program,
    machine: &str,
    run: &str,
    input: Object,
    defect: Defect,
) -> Saved {
    let mut snap = blank();
    snap.machine = machine.to_owned();
    snap.run = run.to_owned();
    snap.ir = program
        .ir()
        .machines
        .get(machine)
        .map(|m| m.hash.clone())
        .unwrap_or_default();
    snap.input = input;
    snap.status = Status::Defect;
    snap.result = Some(RunResult::Defect(defect));
    Saved::new(&snap, |t| t.delay)
}

/// Checks a saved snapshot for restoring as a run of `machine` (or of the machine it names),
/// applying `config`'s migration when the hashes differ. A migration that panics has failed.
pub(crate) fn check(
    program: &Program,
    machine: Option<&str>,
    saved: &Saved,
    config: &Config,
) -> Result<(Saved, Option<String>), RestoreError> {
    let value = saved.to_value();
    match &config.migrate {
        Some(f) => {
            let mut migrate = |v: Value| {
                catch_unwind(AssertUnwindSafe(|| f(v))).unwrap_or_else(|p| {
                    Err(match defect_from_panic(p) {
                        Defect::Host { message } => message,
                        d => d.to_string(),
                    })
                })
            };
            program.check_saved(value, machine, Some(&mut migrate))
        }
        None => program.check_saved(value, machine, None),
    }
}

fn blank() -> Snapshot {
    Snapshot {
        machine: String::new(),
        run: String::new(),
        ir: String::new(),
        status: Status::Running,
        active: Vec::new(),
        input: Map::new(),
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
    }
}

impl<'a> Driver<'a> {
    /// A root run with id `id`, whose `ref` arguments name runs among `refs`. It is open,
    /// and subscribed to its peers, from now on.
    #[allow(clippy::too_many_arguments)]
    pub fn root(
        program: &'a Program,
        machine: &'a str,
        input: Object,
        refs: &[RunHandle],
        host: &'a dyn Host,
        config: &Config,
        id: String,
        shared: Arc<Shared>,
    ) -> Self {
        let peers = open(program, machine, &input, refs, &shared);
        Self::opened(program, machine, input, peers, host, config, id, shared)
    }

    /// A root run that [`open`] has opened on `shared`, giving `peers`.
    #[allow(clippy::too_many_arguments)]
    pub fn opened(
        program: &'a Program,
        machine: &'a str,
        input: Object,
        peers: Result<Arc<Peers>, Defect>,
        host: &'a dyn Host,
        config: &Config,
        id: String,
        shared: Arc<Shared>,
    ) -> Self {
        Self::new(
            program,
            Begin::Start(machine, input),
            peers,
            host,
            config.clone(),
            id,
            None,
            shared,
            None,
        )
    }

    /// A root run restored from `saved`, which [`check`] has passed. It is open from now on,
    /// and holds no `ref`s.
    pub fn restored(
        program: &'a Program,
        (saved, migrated): (Saved, Option<String>),
        host: &'a dyn Host,
        config: &Config,
        id: String,
        shared: Arc<Shared>,
    ) -> Self {
        Self::new(
            program,
            Begin::Restore(Box::new(saved), migrated),
            Ok(Arc::default()),
            host,
            config.clone(),
            id,
            None,
            shared,
            None,
        )
    }

    pub fn child(
        program: &'a Program,
        machine: &'a str,
        input: Object,
        refs: &[RunHandle],
        host: &'a dyn Host,
        ctx: &Arc<InvokeContext>,
    ) -> Self {
        let id = format!("{}.{}", ctx.parent, ctx.invoke);
        let peers = open(program, machine, &input, refs, &ctx.token);
        Self::new(
            program,
            Begin::Start(machine, input),
            peers,
            host,
            ctx.config.clone(),
            id,
            Some(ctx.parent.clone()),
            ctx.token.clone(),
            Some(ctx.clone()),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new(
        program: &'a Program,
        begin: Begin<'a>,
        peers: Result<Arc<Peers>, Defect>,
        host: &'a dyn Host,
        config: Config,
        id: String,
        parent: Option<String>,
        shared: Arc<Shared>,
        ctx: Option<Arc<InvokeContext>>,
    ) -> Self {
        let (peers, bad_ref) = match peers {
            Ok(p) => (p, None),
            Err(d) => (Arc::default(), Some(d)),
        };
        Driver {
            program,
            host,
            begin: Some(begin),
            config,
            id,
            parent,
            shared,
            ctx,
            peers,
            bad_ref,
            queue: VecDeque::new(),
            invokes: BTreeMap::new(),
            contexts: BTreeMap::new(),
            timers: BTreeMap::new(),
            dues: BTreeMap::new(),
            restored_dues: BTreeMap::new(),
            snap: Arc::new(blank()),
            final_state: None,
            cancel_requested: false,
            cancel_taken: false,
            suspendable: true,
            last_sent: None,
        }
    }

    /// The run goes on to its end: a suspend request through its handle is ignored, as a
    /// child run ignores one. For `run` and `run_with`, whose callers get an output or an
    /// error and nothing else.
    pub fn run_to_the_end(mut self) -> Self {
        self.suspendable = false;
        self
    }

    pub async fn drive(self) -> RunEnd {
        self.drive_reporting().await.0
    }

    /// Runs to the end. Also says whether the run took a cancel request, in which case a
    /// defect it ended with happened in its cancel step.
    pub async fn drive_reporting(mut self) -> (RunEnd, bool) {
        let begin = self.begin.take().expect("a driver runs once");
        let machine = match &begin {
            Begin::Start(m, _) => (*m).to_owned(),
            Begin::Restore(saved, _) => saved.machine.clone(),
        };
        // A run given a handle it cannot hold ends before it starts, with no record.
        if let Some(defect) = self.bad_ref.take() {
            if let Begin::Start(m, input) = begin {
                self.shared.set_saved(early_defect(
                    self.program,
                    m,
                    &self.id,
                    input,
                    defect.clone(),
                ));
            }
            self.close();
            self.shared.answer_unanswered();
            self.shared.finish(Finished::Ok);
            return (RunEnd::Defect(defect), false);
        }
        if let Some(observer) = &self.config.observer {
            let child = self.parent.is_some();
            let mut handle =
                RunHandle::new(self.shared.clone(), &self.id, self.program, &machine, child);
            if !self.suspendable {
                handle = handle.runs_to_the_end();
            }
            observer(&self.id, handle);
        }
        let host = self.host;
        let peers = self.peers.clone();
        let mut fns = |n: &str, a: Vec<Value>| call_function(host, &peers, n, a);
        let (snap, commands) = match begin {
            Begin::Start(machine, input) => self.program.start(machine, &self.id, input, &mut fns),
            Begin::Restore(saved, migrated) => {
                self.restored_dues = saved.timers.iter().map(|t| (t.id, t.due)).collect();
                let now = self.config.timer.now();
                self.program
                    .restore(&saved, &self.id, now, migrated, &mut fns)
            }
        }
        .unwrap_or_else(|e| panic!("{e}"));
        self.set_snapshot(snap);
        self.carry_out(commands).await;
        self.shared.listeners_started();
        self.publish();
        while self.snap.status == Status::Running {
            let input = match self.take().await {
                Next::Input(input) => input,
                Next::Suspend => return self.suspend().await,
            };
            let invoke = input.invoke_id().or(input.spawn_id());
            let timer = input.timer_id();
            let (snap, commands) = self.program.step(&self.snap, input, &mut |n, a| {
                call_function(host, &peers, n, a)
            });
            self.set_snapshot(snap);
            // The core has taken this outcome or expiry: forget its source. An invoke still
            // held here had its outcome queued from outside, so what its future does now is
            // ignored, as for a cancelled one: a panic while dropping it included.
            if let Some(i) = invoke {
                self.contexts.remove(&i);
                self.shared.forget_call(i);
            }
            if let Some(slot) = invoke.and_then(|i| self.invokes.remove(&i)) {
                let _ = drop_caught(slot);
            }
            if let Some(t) = timer {
                self.timers.remove(&t);
            }
            self.carry_out(commands).await;
            self.publish();
        }
        self.end().await
    }

    /// Takes the core's snapshot. A run closes as soon as its core gives a snapshot that is
    /// not `running`, before that step's commands are carried out.
    fn set_snapshot(&mut self, snap: Snapshot) {
        self.snap = Arc::new(snap);
        if self.snap.status != Status::Running {
            self.close();
        }
    }

    /// Closes the run for good and ends its subscriptions. Idempotent.
    fn close(&self) {
        self.shared.close();
        for peer in self.peers.values() {
            peer.shared.unsubscribe(&self.shared);
        }
    }

    /// Publishes the state the run is in and its saved snapshot, as the last step left it.
    fn publish(&mut self) {
        let leaf = match &self.final_state {
            Some(f) => Some(f.as_str()),
            None => self
                .program
                .current_state(&self.snap.machine, &self.snap.active),
        };
        if let Some(leaf) = leaf {
            self.shared.set_state(leaf);
        }
        let timers = &self.snap.timers;
        self.dues.retain(|id, _| timers.iter().any(|t| t.id == *id));
        let now = self.config.timer.now();
        let dues = timers
            .iter()
            .map(|t| self.dues.get(&t.id).copied().unwrap_or(due(now, t.delay)))
            .collect();
        self.shared.publish(self.snap.clone(), dues);
    }

    /// The next input: a cancel request if there is one, else the front of the queue; or, for
    /// a top-level run asked to suspend, the stop once it is idle.
    async fn take(&mut self) -> Next {
        poll_fn(|cx| {
            loop {
                self.poll_sources(cx);
                if self.cancel_requested {
                    self.cancel_taken = true;
                    return Poll::Ready(Next::Input(Input::Cancel));
                }
                if self.queue.is_empty()
                    && self.snap.invokes.is_empty()
                    && self.snap.calls.is_empty()
                    && self.snap.spawned.is_empty()
                    && self.parent.is_none()
                    && self.suspendable
                    && self.shared.suspend_requested()
                {
                    // Idle as far as the inbox drained above shows. The run closes only if
                    // nothing has arrived since (an event, a message, a cancel request): that
                    // check and the close are one operation, so what was pushed while the
                    // run was still open is taken, not discarded with the closed inbox.
                    if self.shared.close_if_idle() {
                        return Poll::Ready(Next::Suspend);
                    }
                    continue;
                }
                return match self.queue.pop_front() {
                    Some(input) => Poll::Ready(Next::Input(input)),
                    None => Poll::Pending,
                };
            }
        })
        .await
    }

    /// Stops an idle run that was asked to suspend: closes it, cancels its timers through the
    /// timer without records (they are in the snapshot), writes `suspended`, and gives the
    /// saved snapshot. No exit effect runs.
    async fn suspend(mut self) -> (RunEnd, bool) {
        self.close();
        // An idle run has no pending call, and nothing queued.
        self.shared.answer_unanswered();
        self.queue.clear();
        self.contexts.clear();
        for (_, slot) in std::mem::take(&mut self.invokes) {
            let _ = self.cancel_slot(slot).await;
        }
        for (timer, fut) in std::mem::take(&mut self.timers) {
            drop(fut);
            self.config.timer.cancel(&self.id, timer);
        }
        self.write(Record::Suspended);
        let saved = self
            .shared
            .saved()
            .map(|s| (*s).clone())
            .unwrap_or_else(|| {
                Saved::new(&self.snap, |t| {
                    self.dues.get(&t.id).copied().unwrap_or(t.delay)
                })
            });
        self.shared.finish(Finished::Ok);
        (RunEnd::Suspended(Box::new(saved)), false)
    }

    /// Finds out what has arrived and pushes it onto the queue: the inbox (events and inputs
    /// sent from outside), then invoke outcomes and timer expiries, by ascending id.
    fn poll_sources(&mut self, cx: &mut Context<'_>) {
        if self.shared.drain(&mut self.queue, cx.waker()) {
            self.cancel_requested = true;
        }
        let mut ended = Vec::new();
        for (&id, slot) in self.invokes.iter_mut() {
            let polled = with_context(slot.ctx.as_ref(), || {
                catch_unwind(AssertUnwindSafe(|| slot.fut.as_mut().poll(cx)))
            });
            match polled {
                Ok(Poll::Pending) => {}
                Ok(Poll::Ready(outcome)) => ended.push((id, outcome)),
                Err(p) => ended.push((id, Outcome::Defect(defect_from_panic(p)))),
            }
        }
        // What a child run emitted in the poll that ended it is in the inbox: it goes before
        // the child's outcome.
        if !ended.is_empty() && self.shared.drain(&mut self.queue, cx.waker()) {
            self.cancel_requested = true;
        }
        for (invoke, mut outcome) in ended {
            // A future that panics while it is dropped after giving its outcome is host code
            // failing before its invoke ended: the outcome is that host defect.
            let mut spawn = false;
            if let Some(slot) = self.invokes.remove(&invoke) {
                spawn = slot.spawn;
                if let Err(d) = drop_caught(slot) {
                    outcome = Outcome::Defect(d);
                }
            }
            self.queue.push_back(match (outcome, spawn) {
                (Outcome::Done(value), false) => Input::InvokeDone { invoke, value },
                (Outcome::Fail(error), false) => Input::InvokeFail { invoke, error },
                (Outcome::Defect(defect), false) => Input::InvokeDefect { invoke, defect },
                (Outcome::Done(value), true) => Input::SpawnDone {
                    spawn: invoke,
                    value,
                },
                (Outcome::Fail(error), true) => Input::SpawnFail {
                    spawn: invoke,
                    error,
                },
                (Outcome::Defect(defect), true) => Input::SpawnDefect {
                    spawn: invoke,
                    defect,
                },
            });
        }
        let mut fired = Vec::new();
        for (&id, t) in self.timers.iter_mut() {
            if t.as_mut().poll(cx).is_ready() {
                fired.push(id);
            }
        }
        for timer in fired {
            self.timers.remove(&timer);
            self.queue.push_back(Input::After { timer });
        }
    }

    async fn carry_out(&mut self, commands: Vec<Command>) {
        for c in commands {
            if let Err(defect) = self.carry(c).await {
                // A run that has already ended with a defect is not changed by another: this
                // is a child's cancel step failing while `failWithDefect`'s cancels are carried
                // out. It is ignored, and the rest of the commands (more cancels, the `defect`
                // record) are carried out.
                if self.snap.status == Status::Defect {
                    continue;
                }
                let host = self.host;
                let peers = self.peers.clone();
                let (snap, rest) =
                    self.program
                        .step(&self.snap, Input::Defect { defect }, &mut |n, a| {
                            call_function(host, &peers, n, a)
                        });
                self.set_snapshot(snap);
                for c in rest {
                    // A defect met here is ignored.
                    let _ = self.carry(c).await;
                }
                return;
            }
        }
    }

    /// Carries out one command. `Err` is a defect it met: a host defect in an action, or a
    /// defect in the cancel step of a child run.
    async fn carry(&mut self, c: Command) -> Result<(), Defect> {
        match c {
            Command::Trace { record } => {
                if let Record::Sent { message, value, .. } = &record {
                    self.last_sent = Some((*message, value.clone()));
                }
                self.write(record);
                Ok(())
            }
            Command::Action { action, args } => {
                let host = self.host;
                let peers = self.peers.clone();
                let created = with_context(None, || {
                    with_scope(Some(&peers), || {
                        catch_unwind(AssertUnwindSafe(|| host.action(&action, args)))
                    })
                });
                let mut fut = created.map_err(defect_from_panic)?;
                let done = poll_fn(|cx| {
                    let polled = with_context(None, || {
                        catch_unwind(AssertUnwindSafe(|| fut.as_mut().poll(cx)))
                    });
                    match polled {
                        Ok(Poll::Ready(r)) => Poll::Ready(r),
                        Ok(Poll::Pending) => {
                            self.poll_sources(cx);
                            Poll::Pending
                        }
                        Err(p) => Poll::Ready(Err(defect_from_panic(p))),
                    }
                })
                .await;
                // A panic while dropping the action's future is the action failing.
                let dropped = drop_caught(fut);
                done.and(dropped)
            }
            Command::InvokeStart {
                invoke,
                src_kind: SrcKind::Call,
                src,
                to,
                args,
                ..
            } => {
                self.make_call(invoke, src, to, args);
                Ok(())
            }
            Command::InvokeStart {
                invoke,
                src,
                src_kind,
                alias,
                args,
                ..
            } => {
                self.start_invoke(invoke, src, src_kind, alias.is_some(), args, false);
                Ok(())
            }
            // A spawned child is a machine invoke whose emitted events reach this run, as an
            // `as` child's do: those this run's machine has an `on` for.
            Command::SpawnStart { spawn, src, args } => {
                self.start_invoke(spawn, src, SrcKind::Machine, true, args, true);
                Ok(())
            }
            Command::InvokeCancel { invoke } | Command::SpawnStop { spawn: invoke } => {
                // A call is abandoned: its answer, if it comes, is not pushed.
                self.shared.forget_call(invoke);
                self.contexts.remove(&invoke);
                match self.invokes.remove(&invoke) {
                    Some(slot) => self.cancel_slot(slot).await,
                    None => Ok(()),
                }
            }
            Command::Reply {
                to,
                invoke,
                outcome,
                value,
                ..
            } => {
                if let Some(answer) = self.shared.take_answer(&to, invoke) {
                    answer.answer(outcome, value);
                }
                Ok(())
            }
            Command::Send {
                to,
                invoke,
                message,
                value,
            } => {
                let input = Input::Message {
                    from: self.id.clone(),
                    message,
                    value: value.clone(),
                };
                let delivered = match invoke {
                    // A context the parent no longer has belongs to an invoke whose outcome
                    // was taken: the core does not send to one.
                    Some(i) => self.contexts.get(&i).is_none_or(|c| c.token.deliver(input)),
                    None => self.peers.get(&to).is_some_and(|p| p.shared.deliver(input)),
                };
                if !delivered {
                    // The value as the `sent` record showed it, redacted. The core always
                    // writes that record first; were it missing, only the event's tag is shown.
                    let value = match self.last_sent.take() {
                        Some((m, shown)) if m == message => shown,
                        _ => serde_json::json!({ "_tag": value.get("_tag") }),
                    };
                    self.write(Record::Undelivered {
                        message,
                        to: Some(to),
                        value,
                    });
                }
                Ok(())
            }
            Command::Emit { message, value } => {
                let tag = value
                    .get("_tag")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if let Some(ctx) = self.ctx.as_ref().filter(|c| c.heard.contains(tag)) {
                    ctx.parent_queue.deliver(Input::Emitted {
                        from: self.id.clone(),
                        invoke: Some(ctx.invoke),
                        message,
                        value: value.clone(),
                    });
                }
                for run in self.shared.subscribers(tag) {
                    if !Arc::ptr_eq(&run, &self.shared) {
                        run.deliver(Input::Emitted {
                            from: self.id.clone(),
                            invoke: None,
                            message,
                            value: value.clone(),
                        });
                    }
                }
                // Host listeners: no filter, no record.
                self.shared.tell_listeners(&value);
                Ok(())
            }
            Command::TimerStart { timer, delay } => {
                let due = due(self.config.timer.now(), delay);
                let fut = self.config.timer.start(&self.id, timer, delay);
                self.timers.insert(timer, fut);
                self.dues.insert(timer, due);
                Ok(())
            }
            Command::TimerFire { timer } => {
                // A restored timer already due keeps the due time it was saved with.
                if let Some(&due) = self.restored_dues.get(&timer) {
                    self.dues.insert(timer, due);
                }
                self.queue.push_back(Input::After { timer });
                Ok(())
            }
            Command::TimerCancel { timer } => {
                if self.timers.remove(&timer).is_some() {
                    self.config.timer.cancel(&self.id, timer);
                }
                Ok(())
            }
        }
    }

    /// Makes call `call` of the peer `to` with invoke `invoke` (`spec/execution.md#carrying-out-calls`):
    /// pushes the call onto the peer's queue with a way to answer it, if the peer is open, or
    /// else answers it `Unanswered` at once, onto this run's own queue. No record either way,
    /// and no host code: a call has no implementation to bind.
    fn make_call(&mut self, invoke: u64, call: String, to: Option<String>, args: Object) {
        let answer = self.shared.make_call(invoke);
        let input = Input::Call {
            from: self.id.clone(),
            invoke,
            call,
            args,
        };
        let peer = to.as_ref().and_then(|to| self.peers.get(to));
        let refused = match peer {
            Some(peer) => peer.shared.deliver_call(input, answer).err(),
            None => Some(answer),
        };
        if let Some(answer) = refused {
            answer.unanswered();
        }
    }

    /// Starts an invoke, or (`spawn`) a spawned child, whose outcome is then a `spawn.*` input.
    /// A child run that `delivers` (invoked with `as`, or spawned) delivers to this run the
    /// events it emits that this run's machine has an `on` for.
    fn start_invoke(
        &mut self,
        invoke: u64,
        src: String,
        src_kind: SrcKind,
        delivers: bool,
        args: Object,
        spawn: bool,
    ) {
        let ctx = (src_kind == SrcKind::Machine).then(|| {
            Arc::new(InvokeContext {
                config: self.config.clone(),
                parent: self.id.clone(),
                invoke,
                token: Arc::new(Shared::default()),
                parent_queue: self.shared.clone(),
                heard: if delivers {
                    self.program.heard(&self.snap.machine, &src)
                } else {
                    HashSet::new()
                },
            })
        });
        if let Some(ctx) = &ctx {
            self.contexts.insert(invoke, ctx.clone());
        }
        let ir = self.program.ir();
        let params = match src_kind {
            SrcKind::Task => ir.tasks.get(&src).map(|t| &t.params[..]),
            SrcKind::Machine => ir.machines.get(&src).map(|m| &m.params[..]),
            // Made by `make_call`.
            SrcKind::Call => None,
        };
        let refs = arg_ref_ids(self.program, params.unwrap_or_default(), &args)
            .iter()
            .filter_map(|(id, _)| self.peers.get(id).cloned())
            .collect();
        let call = Invocation {
            kind: src_kind,
            src,
            args,
            refs,
            run: self.id.clone(),
            invoke,
        };
        let host = self.host;
        let peers = self.peers.clone();
        let created = with_context(ctx.as_ref(), || {
            with_scope(Some(&peers), || {
                catch_unwind(AssertUnwindSafe(|| host.invoke(call)))
            })
        });
        let fut = created.unwrap_or_else(|p| {
            let d = defect_from_panic(p);
            Box::pin(async move { Outcome::Defect(d) })
        });
        self.invokes.insert(invoke, Slot { fut, ctx, spawn });
    }

    /// Stops a running invoke. A task's or stub's future is dropped at once. A child run is
    /// signalled through its token and polled until it reports that its cancel step is
    /// done; only then is its future dropped. What a cancelled invoke does is ignored, so a
    /// panic while dropping its future is caught and ignored.
    async fn cancel_slot(&mut self, mut slot: Slot<'a>) -> Result<(), Defect> {
        let Some(ctx) = slot.ctx.clone() else {
            let _ = drop_caught(slot);
            return Ok(());
        };
        if !ctx.token.is_claimed() || ctx.token.finished().is_some() {
            let _ = drop_caught(slot);
            return Ok(());
        }
        ctx.token.request_cancel();
        let how = poll_fn(|cx| {
            if let Some(f) = ctx.token.finished() {
                return Poll::Ready(f);
            }
            let polled = with_context(Some(&ctx), || {
                catch_unwind(AssertUnwindSafe(|| slot.fut.as_mut().poll(cx)))
            });
            if let Some(f) = ctx.token.finished() {
                return Poll::Ready(f);
            }
            match polled {
                Ok(Poll::Pending) => {
                    self.poll_sources(cx);
                    Poll::Pending
                }
                // The host code ended without its run reporting: nothing more to wait for.
                _ => Poll::Ready(Finished::Ok),
            }
        })
        .await;
        let _ = drop_caught(slot);
        match how {
            Finished::Ok => Ok(()),
            Finished::Defect(d) => Err(d),
        }
    }

    fn write(&mut self, record: Record) {
        if let Record::Done { state, .. } | Record::Failed { state, .. } = &record {
            self.final_state = Some(state.clone());
        }
        if !self.config.sink.enabled() {
            return;
        }
        let record = TraceRecord {
            t: self.config.timer.now(),
            run: self.id.clone(),
            parent: self.parent.clone(),
            record,
        };
        self.config.sink.record(&record);
    }

    /// Ends the run: discards the queue, stops whatever it still holds (a child run is
    /// cancelled through its token first), and reports through the token.
    async fn end(mut self) -> (RunEnd, bool) {
        self.close();
        self.shared.answer_unanswered();
        self.queue.clear();
        self.contexts.clear();
        for (_, slot) in std::mem::take(&mut self.invokes) {
            let _ = self.cancel_slot(slot).await;
        }
        for (timer, _) in std::mem::take(&mut self.timers) {
            self.config.timer.cancel(&self.id, timer);
        }
        let end = match self.snap.result.clone() {
            Some(RunResult::Output(v)) => RunEnd::Done(v),
            Some(RunResult::Error(e)) => RunEnd::Failed(e),
            Some(RunResult::Defect(d)) => RunEnd::Defect(d),
            None => RunEnd::Cancelled,
        };
        self.shared.finish(match (&end, self.cancel_taken) {
            (RunEnd::Defect(d), true) => Finished::Defect(d.clone()),
            _ => Finished::Ok,
        });
        (end, self.cancel_taken)
    }
}

/// A run's future dropped before it ended (a hard stop) drops what it holds; a host future
/// that panics then is ignored, as for a cancelled invoke. The run is then finished: its
/// handles say so, and a parent waiting for a child run that host code dropped stops
/// waiting.
impl Drop for Driver<'_> {
    fn drop(&mut self) {
        self.close();
        // A hard stop answers the calls it was given, so that their callers do not wait for
        // a run that is gone.
        self.shared.answer_unanswered();
        for (_, slot) in std::mem::take(&mut self.invokes) {
            let _ = drop_caught(slot);
        }
        self.shared.finish(Finished::Ok);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use serde_json::json;

    use super::*;
    use crate::env::MemorySink;

    /// Nothing but actions that return at once.
    struct Quiet;

    impl Host for Quiet {
        fn function(&self, name: &str, _: Vec<Value>) -> Result<Value, Defect> {
            Err(Defect::host(format!("no function {name}")))
        }

        fn action<'a>(&'a self, _: &str, _: Object) -> BoxFuture<'a, Result<(), Defect>> {
            Box::pin(async { Ok(()) })
        }

        fn invoke<'a>(&'a self, _: Invocation) -> BoxFuture<'a, Outcome> {
            Box::pin(std::future::pending())
        }
    }

    /// An event pushed onto a run's inbox while the run decides to suspend is either taken
    /// (the push came first) or refused (`deliver` gives `false`, the run had closed): never
    /// accepted and then discarded. One thread drives the run, another pushes events and asks
    /// it to suspend half-way; every event `deliver` accepted is in the count it saved.
    /// A counter that waits for `Add` in a state with no work.
    fn counter() -> Program {
        const CHART: &str = "machine C() {\n  var n: int = 0\n  event Add { by: int }\n  \
                             initial S\n  state S { on Add set n = n + event.by }\n}\n";
        let c = roadbook_compiler::compile_source("c.road", CHART, &Default::default());
        assert!(!c.has_errors(), "{}", c.render());
        Program::new(c.ir.unwrap()).unwrap()
    }

    /// `resume` checks the snapshot, then the event, before anything runs or is written: an
    /// event that is not one the machine accepts (an unknown tag, a field missing or of the
    /// wrong type) is the caller's host defect, as one that does not encode is for `send`.
    #[test]
    fn resume_checks_the_event_before_anything_runs() {
        let program = counter();
        let mut no_fns = |n: &str, _: Vec<Value>| Err(Defect::host(n));
        let (snap, _) = program
            .start("C", "r1", Object::new(), &mut no_fns)
            .unwrap();
        let saved = Saved::new(&snap, |t| t.delay);
        let sink = Arc::new(MemorySink::new());
        let config = Config::new().sink(sink.clone());
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let resume = |saved: &Saved, event: Value| {
            catch_unwind(AssertUnwindSafe(|| {
                rt.block_on(crate::dynamic::resume(
                    &config,
                    &program,
                    None,
                    saved,
                    Some(event),
                    &Quiet,
                ))
            }))
        };
        for bad in [
            json!({"_tag": "Add", "by": "x"}),
            json!({"_tag": "Add"}),
            json!({"_tag": "Add", "by": 1, "and": 2}),
            json!({"_tag": "Nope"}),
            json!(7),
        ] {
            let payload = resume(&saved, bad.clone()).expect_err("a bad event panics");
            let defect = payload.downcast::<Defect>().expect("with a Defect");
            assert!(matches!(*defect, Defect::Host { .. }), "{bad}: {defect}");
        }
        assert!(sink.records().is_empty(), "a bad request wrote records");
        // A snapshot that is refused is refused first.
        let mut wrong = saved.clone();
        wrong.status = Status::Done;
        let refused = resume(&wrong, json!({"_tag": "Nope"})).unwrap();
        assert_eq!(refused.unwrap_err().reason(), "ended");
        // A good one, its `int` written as a float.
        let after = resume(&saved, json!({"_tag": "Add", "by": 2.0}))
            .unwrap()
            .unwrap();
        assert_eq!(after.vars["n"], json!(2));
    }

    #[test]
    fn an_event_that_arrives_while_a_run_suspends_is_taken_or_refused() {
        let program = counter();
        let add = || Input::Event {
            value: json!({"_tag": "Add", "by": 1}),
        };
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (mut lost, mut raced) = (0, 0);
        for round in 0..3000_u64 {
            let shared = Shared::claimed();
            let config = Config::new().sink(Arc::new(MemorySink::new())).run_id("r1");
            let driver = Driver::root(
                &program,
                "C",
                Object::new(),
                &[],
                &Quiet,
                &config,
                "r1".into(),
                shared.clone(),
            );
            let accepted = AtomicU64::new(0);
            let end = std::thread::scope(|s| {
                let pusher = s.spawn(|| {
                    shared.request_suspend();
                    // Pauses of varying length, so that the run often finds its inbox empty
                    // and decides to stop just as the next event arrives.
                    for i in 0..50 {
                        for _ in 0..(round * 7 + i * 13) % 300 {
                            std::hint::spin_loop();
                        }
                        if shared.deliver(add()) {
                            accepted.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                });
                let end = rt.block_on(driver.drive());
                pusher.join().unwrap();
                end
            });
            let RunEnd::Suspended(saved) = end else {
                panic!("the run was not suspended: {end:?}");
            };
            let taken = saved.vars["n"].as_u64().unwrap();
            let accepted = accepted.load(Ordering::SeqCst);
            if taken != accepted {
                lost += 1;
            }
            if accepted < 50 {
                raced += 1;
            }
        }
        assert_eq!(
            lost, 0,
            "rounds that lost an accepted event ({raced} rounds raced)"
        );
    }
}
