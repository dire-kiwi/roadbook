//! Runs every case in `spec/conformance/cases` through the production driver, with a
//! virtual `Timer` and scripted host code, and compares each trace byte for byte with the
//! expected `trace.jsonl` (`spec/conformance/README.md`, "How a runner works").
//!
//! The runner is its own executor: it polls the future of every top-level run (the root run
//! and those of `start` steps) until a round of polls leaves no wake-up behind. Every child
//! run is polled inside its top-level run's future, and scripted invokes and timers wake one
//! only when the runner ends or fires them, while a message or emitted event that one run
//! delivers to another wakes the receiver, so that is exactly "quiescent".
//!
//! Saved snapshots go through the production API too: a `snapshot` step is
//! `RunHandle::save`, a `suspend` step `RunHandle::suspend` (whose run's future then ends with
//! `RunEnd::Suspended`), and a `restore` step `dynamic::restore` with the runner's migration
//! in the `Config`. Each snapshot is compared byte for byte, in canonical JSON, with its file.
//!
//! With `ROADBOOK_CONFORMANCE_OUT=<dir>`, each case's actual trace is also written to
//! `<dir>/<case>.jsonl`, and each snapshot it saves to `<dir>/<case>/<name>.snapshot.json`.
//!
//! A second test restores every committed snapshot file of the cases (the canonical form both
//! runtimes write) and lets the run go on. With `ROADBOOK_RESTORE_FROM=<dir>` it restores the
//! files `<dir>/<case>/<name>.snapshot.json` instead: snapshots another runtime saved, such as
//! the TypeScript runner's output.
//!
//! A case that `requires` a feature not in [`SUPPORTED`] is skipped, not failed.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};

use roadbook::dynamic::{self, Host, Invocation, Outcome};
use roadbook::{
    BoxFuture, Config, Defect, Input, MemorySink, Program, RunEnd, RunHandle, Saved, Timer,
};
use roadbook_conformance::{
    LoadedCase, Outcome as ScriptOutcome, Status, Step, arrange, compare_run_ids, conformance_dir,
    load_all, snapshot_text, to_lines,
};
use roadbook_core::{Object, SrcKind};
use serde_json::Value;

/// The features of `spec/conformance/case.schema.json`'s `requires` this runtime
/// implements.
const SUPPORTED: &[&str] = &["messaging", "parallel", "snapshots", "history"];

// ---- the executor ------------------------------------------------------------------------

#[derive(Default)]
struct Flag(AtomicBool);

impl Wake for Flag {
    fn wake(self: Arc<Self>) {
        self.0.store(true, Ordering::SeqCst);
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.store(true, Ordering::SeqCst);
    }
}

// ---- the virtual timer -------------------------------------------------------------------

#[derive(Default)]
struct Fire {
    fired: AtomicBool,
    waker: Mutex<Option<Waker>>,
}

impl Fire {
    fn fire(&self) {
        self.fired.store(true, Ordering::SeqCst);
        if let Some(w) = self.waker.lock().unwrap().take() {
            w.wake();
        }
    }
}

struct Pending {
    run: String,
    id: u64,
    due: u64,
    fire: Arc<Fire>,
}

#[derive(Default)]
struct VirtualTimer {
    now: AtomicU64,
    running: Mutex<Vec<Pending>>,
}

impl VirtualTimer {
    fn forget(&self, run: &str, id: u64) {
        self.running
            .lock()
            .unwrap()
            .retain(|p| !(p.run == run && p.id == id));
    }

    /// The running timers due at or before `end`, in firing order: earliest due time, then
    /// run order, then timer id.
    fn due_by(&self, end: u64) -> Vec<(String, u64, u64)> {
        let mut due: Vec<(String, u64, u64)> = self
            .running
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.due <= end)
            .map(|p| (p.run.clone(), p.id, p.due))
            .collect();
        due.sort_by(|a, b| {
            a.2.cmp(&b.2)
                .then_with(|| compare_run_ids(&a.0, &b.0))
                .then_with(|| a.1.cmp(&b.1))
        });
        due
    }

    /// Takes timer `id` of `run` off the running list, giving what fires it.
    fn take(&self, run: &str, id: u64) -> Option<Arc<Fire>> {
        let mut running = self.running.lock().unwrap();
        let i = running.iter().position(|p| p.run == run && p.id == id)?;
        Some(running.remove(i).fire)
    }
}

struct TimerFuture {
    timer: Arc<VirtualTimer>,
    run: String,
    id: u64,
    fire: Arc<Fire>,
}

impl Future for TimerFuture {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.fire.fired.load(Ordering::SeqCst) {
            return Poll::Ready(());
        }
        *self.fire.waker.lock().unwrap() = Some(cx.waker().clone());
        Poll::Pending
    }
}

impl Drop for TimerFuture {
    fn drop(&mut self) {
        self.timer.forget(&self.run, self.id);
    }
}

/// The driver gets this handle to the shared virtual timer.
struct TimerHandle(Arc<VirtualTimer>);

impl Timer for TimerHandle {
    fn now(&self) -> u64 {
        self.0.now.load(Ordering::SeqCst)
    }

    fn start(&self, run: &str, timer: u64, delay: u64) -> BoxFuture<'static, ()> {
        let fire = Arc::new(Fire::default());
        self.0.running.lock().unwrap().push(Pending {
            run: run.to_owned(),
            id: timer,
            due: self.now() + delay,
            fire: fire.clone(),
        });
        Box::pin(TimerFuture {
            timer: self.0.clone(),
            run: run.to_owned(),
            id: timer,
            fire,
        })
    }

    fn cancel(&self, run: &str, timer: u64) {
        self.0.forget(run, timer);
    }
}

// ---- scripted invokes --------------------------------------------------------------------

#[derive(Default)]
struct Slot {
    outcome: Mutex<Option<Outcome>>,
    waker: Mutex<Option<Waker>>,
}

struct Scripted {
    run: String,
    invoke: u64,
    src: String,
    slot: Arc<Slot>,
    /// Its outcome was given (or appended to its run's queue by a `together`).
    resolved: bool,
}

#[derive(Default)]
struct Registry {
    invokes: Mutex<Vec<Scripted>>,
}

impl Registry {
    fn remove(&self, run: &str, invoke: u64) {
        self.invokes
            .lock()
            .unwrap()
            .retain(|s| !(s.run == run && s.invoke == invoke));
    }

    /// The one running scripted invoke of `task` (in `run`, if given), marked resolved:
    /// its run, its id and its slot.
    fn resolve(&self, task: &str, run: Option<&str>) -> Result<(String, u64, Arc<Slot>), String> {
        let mut invokes = self.invokes.lock().unwrap();
        let mut found: Vec<&mut Scripted> = invokes
            .iter_mut()
            .filter(|s| !s.resolved && s.src == task && run.is_none_or(|r| r == s.run))
            .collect();
        match found.len() {
            1 => {
                let s = &mut found[0];
                s.resolved = true;
                Ok((s.run.clone(), s.invoke, s.slot.clone()))
            }
            0 => Err(format!("no running scripted invoke of `{task}`")),
            n => Err(format!(
                "{n} running scripted invokes of `{task}`; name a run"
            )),
        }
    }
}

struct ScriptedFuture {
    registry: Arc<Registry>,
    run: String,
    invoke: u64,
    slot: Arc<Slot>,
}

impl Future for ScriptedFuture {
    type Output = Outcome;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Outcome> {
        if let Some(o) = self.slot.outcome.lock().unwrap().take() {
            return Poll::Ready(o);
        }
        *self.slot.waker.lock().unwrap() = Some(cx.waker().clone());
        Poll::Pending
    }
}

impl Drop for ScriptedFuture {
    fn drop(&mut self) {
        self.registry.remove(&self.run, self.invoke);
    }
}

fn scripted_outcome(o: &ScriptOutcome) -> Outcome {
    match o {
        ScriptOutcome::Done(v) => Outcome::Done(v.clone()),
        ScriptOutcome::Fail(e) => Outcome::Fail(e.clone()),
        ScriptOutcome::Defect(m) => Outcome::Defect(Defect::host(m.clone())),
    }
}

fn outcome_input(invoke: u64, o: Outcome) -> Input {
    match o {
        Outcome::Done(value) => Input::InvokeDone { invoke, value },
        Outcome::Fail(error) => Input::InvokeFail { invoke, error },
        Outcome::Defect(defect) => Input::InvokeDefect { invoke, defect },
    }
}

// ---- the host ----------------------------------------------------------------------------

struct CaseHost<'c> {
    case: &'c LoadedCase,
    program: &'c Program,
    registry: Arc<Registry>,
    problems: Mutex<Vec<String>>,
}

impl Host for CaseHost<'_> {
    fn function(&self, name: &str, args: Vec<Value>) -> Result<Value, Defect> {
        match self.case.case.fns.call(name, &args) {
            Some(v) => Ok(v.clone()),
            None => {
                let call = format!("{name}({})", Value::Array(args));
                self.problems
                    .lock()
                    .unwrap()
                    .push(format!("unexpected call {call}"));
                Err(Defect::host(format!("unexpected call {call}")))
            }
        }
    }

    fn action<'a>(&'a self, name: &str, args: Object) -> BoxFuture<'a, Result<(), Defect>> {
        let r = match self.case.case.actions.defect(name, &Value::Object(args)) {
            Some(m) => Err(Defect::host(m)),
            None => Ok(()),
        };
        Box::pin(std::future::ready(r))
    }

    fn invoke<'a>(&'a self, call: Invocation) -> BoxFuture<'a, Outcome> {
        if call.kind == SrcKind::Task || self.case.case.is_stubbed(&call.src) {
            let slot = Arc::new(Slot::default());
            self.registry.invokes.lock().unwrap().push(Scripted {
                run: call.run.clone(),
                invoke: call.invoke,
                src: call.src.clone(),
                slot: slot.clone(),
                resolved: false,
            });
            return Box::pin(ScriptedFuture {
                registry: self.registry.clone(),
                run: call.run,
                invoke: call.invoke,
                slot,
            });
        }
        // Every other machine runs from the chart, as the invoke's child run.
        Box::pin(async move {
            match dynamic::run(self.program, &call.src, call.args, &call.refs, self).await {
                Ok(v) => Outcome::Done(v),
                Err(e) => Outcome::Fail(e),
            }
        })
    }
}

// ---- running a case ----------------------------------------------------------------------

/// One top-level run: its future until it ends, then how it ended.
struct Top<'a> {
    id: String,
    fut: Option<BoxFuture<'a, RunEnd>>,
    end: Option<RunEnd>,
    handle: RunHandle,
    /// The status the case expects it to have after the last step.
    expected: Status,
    /// The snapshot a `suspend` step waits for it to give, until it has given it.
    suspending: Option<String>,
}

struct Runner<'a> {
    runs: Vec<Top<'a>>,
    flag: Arc<Flag>,
}

impl Runner<'_> {
    /// Polls every top-level run that has not ended, in run order, until a round of polls
    /// leaves no wake-up behind.
    fn settle(&mut self) -> Result<(), String> {
        let waker = Waker::from(self.flag.clone());
        let mut cx = Context::from_waker(&waker);
        for _ in 0..100_000 {
            self.flag.0.store(false, Ordering::SeqCst);
            let mut polled = false;
            for run in &mut self.runs {
                let Some(fut) = run.fut.as_mut() else {
                    continue;
                };
                polled = true;
                if let Poll::Ready(end) = fut.as_mut().poll(&mut cx) {
                    run.end = Some(end);
                    run.fut = None;
                }
            }
            if !polled || !self.flag.0.load(Ordering::SeqCst) {
                return Ok(());
            }
        }
        Err("the runs never became quiescent".into())
    }

    /// The top-level run `id`, which must not have ended.
    /// The top-level run `id`, ended or not.
    fn top(&self, id: &str) -> Result<&RunHandle, String> {
        self.runs
            .iter()
            .find(|r| r.id == id)
            .map(|r| &r.handle)
            .ok_or_else(|| format!("no top-level run {id}"))
    }

    fn running(&self, id: &str) -> Result<&RunHandle, String> {
        let run = self
            .runs
            .iter()
            .find(|r| r.id == id)
            .ok_or_else(|| format!("no top-level run {id}"))?;
        if run.end.is_some() || run.handle.is_finished() {
            return Err(format!("the top-level run {id} has already ended"));
        }
        Ok(&run.handle)
    }

    /// The top-level run `id`, which must not have ended nor be suspended or being
    /// suspended: one an event or a cancel can be given to.
    fn awake(&self, id: &str) -> Result<&RunHandle, String> {
        let handle = self.running(id)?;
        if self
            .runs
            .iter()
            .any(|r| r.id == id && r.suspending.is_some())
        {
            return Err(format!("the top-level run {id} is being suspended"));
        }
        Ok(handle)
    }

    /// Compares the snapshot of every run a `suspend` step waits for that has become idle
    /// (its future ended with `Suspended`) or has ended.
    fn check_suspended(&mut self, c: &LoadedCase) -> Result<(), String> {
        for run in &mut self.runs {
            let (Some(name), Some(end)) = (&run.suspending, &run.end) else {
                continue;
            };
            let saved = match end {
                RunEnd::Suspended(saved) => (**saved).clone(),
                RunEnd::Defect(d) => {
                    return Err(format!("{} was suspended but ended with {d}", run.id));
                }
                _ => run
                    .handle
                    .save()
                    .ok_or_else(|| format!("{} has no saved snapshot", run.id))?,
            };
            compare_snapshot(c, name, &saved)?;
            run.suspending = None;
        }
        Ok(())
    }
}

/// Compares a saved snapshot with the case's file `<name>.snapshot.json`.
fn compare_snapshot(c: &LoadedCase, name: &str, saved: &Saved) -> Result<(), String> {
    let expected = c
        .snapshots
        .get(name)
        .ok_or_else(|| format!("no {name}.snapshot.json"))?;
    let actual = snapshot_text(&saved.to_value());
    if let Some(dir) = std::env::var_os("ROADBOOK_CONFORMANCE_OUT") {
        let dir = std::path::Path::new(&dir).join(&c.name);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        std::fs::write(dir.join(format!("{name}.snapshot.json")), &actual)
            .map_err(|e| e.to_string())?;
    }
    if actual != *expected {
        return Err(format!(
            "snapshot {name}:\n  expected {}  actual   {actual}",
            expected
        ));
    }
    Ok(())
}

fn status_of(end: &Option<RunEnd>) -> Status {
    match end {
        None => Status::Running,
        Some(RunEnd::Done(_)) => Status::Done,
        Some(RunEnd::Failed(_)) => Status::Failed,
        Some(RunEnd::Cancelled) => Status::Cancelled,
        Some(RunEnd::Defect(_)) => Status::Defect,
        // A suspended run has not ended.
        Some(RunEnd::Suspended(_)) => Status::Running,
    }
}

fn object(v: &Value) -> Result<Object, String> {
    match v {
        Value::Object(m) => Ok(m.clone()),
        _ => Err("an `input` is not an object".into()),
    }
}

/// Advances virtual time by `d`, firing the timers due on the way one at a time, earliest
/// first, and letting the runs settle after each.
fn advance(timer: &VirtualTimer, runner: &mut Runner<'_>, d: u64) -> Result<(), String> {
    let end = timer.now.load(Ordering::SeqCst) + d;
    while let Some((run, id, due)) = timer.due_by(end).into_iter().next() {
        timer.now.store(due, Ordering::SeqCst);
        if let Some(fire) = timer.take(&run, id) {
            fire.fire();
        }
        runner.settle()?;
    }
    timer.now.store(end, Ordering::SeqCst);
    Ok(())
}

/// Runs one case and gives its trace in the compared form.
fn run_case(c: &LoadedCase) -> Result<String, String> {
    let ir =
        c.ir.clone()
            .ok_or_else(|| format!("no IR at {}", c.ir_path.display()))?;
    roadbook_ir::check_version(&ir.ir_version).map_err(|e| e.to_string())?;
    if !ir.machines.contains_key(&c.case.machine) {
        return Err(format!("the IR has no machine {}", c.case.machine));
    }
    let program = Program::new(ir).map_err(|e| e.to_string())?;
    let registry = Arc::new(Registry::default());
    let host = CaseHost {
        case: c,
        program: &program,
        registry: registry.clone(),
        problems: Mutex::new(Vec::new()),
    };
    let timer = Arc::new(VirtualTimer::default());
    let sink = Arc::new(MemorySink::new());
    let handles: Arc<Mutex<HashMap<String, RunHandle>>> = Arc::default();
    let seen = handles.clone();
    let config = Config::new()
        .sink(sink.clone())
        .timer(Arc::new(TimerHandle(timer.clone())))
        .on_run(move |id, h| {
            seen.lock().unwrap().insert(id.to_owned(), h);
        });
    let root_config = config.clone().run_id(roadbook_conformance::ROOT_RUN);
    let (fut, root) = dynamic::start(
        &root_config,
        &program,
        &c.case.machine,
        object(&c.case.input)?,
        &[],
        &host,
    );
    let mut runner = Runner {
        runs: vec![Top {
            id: roadbook_conformance::ROOT_RUN.to_owned(),
            fut: Some(fut),
            end: None,
            handle: root,
            expected: c.case.status,
            suspending: None,
        }],
        flag: Arc::new(Flag::default()),
    };
    runner.settle()?;

    for (i, step) in c.case.script.iter().enumerate() {
        let at = |e: String| format!("step {}: {e}", i + 1);
        let gives_input = matches!(
            step,
            Step::Event(_) | Step::Task(_) | Step::Cancel(_) | Step::Together(_)
        );
        if gives_input && runner.runs.iter().all(|r| r.end.is_some()) {
            return Err(at(
                "every top-level run has already ended or been suspended".into(),
            ));
        }
        match step {
            Step::Advance(d) => advance(&timer, &mut runner, *d).map_err(at)?,
            Step::Event(e) => {
                runner.awake(e.run()).map_err(at)?.send(e.event.clone());
                runner.settle().map_err(at)?;
            }
            Step::Task(t) => {
                let (_, _, slot) = registry.resolve(&t.task, t.run.as_deref()).map_err(at)?;
                *slot.outcome.lock().unwrap() = Some(scripted_outcome(&t.outcome));
                if let Some(w) = slot.waker.lock().unwrap().take() {
                    w.wake();
                }
                runner.settle().map_err(at)?;
            }
            Step::Cancel(run) => {
                let run = run.as_deref().unwrap_or(roadbook_conformance::ROOT_RUN);
                runner.awake(run).map_err(at)?.cancel();
                runner.settle().map_err(at)?;
            }
            Step::Start(s) => {
                let expected = format!("r{}", runner.runs.len() + 1);
                if s.run != expected {
                    return Err(at(format!("the run is {expected}, not {}", s.run)));
                }
                if !program.ir().machines.contains_key(&s.machine) {
                    return Err(at(format!("the IR has no machine {}", s.machine)));
                }
                let refs: Vec<RunHandle> = runner.runs.iter().map(|r| r.handle.clone()).collect();
                let (fut, handle) = dynamic::start(
                    &config.clone().run_id(s.run.clone()),
                    &program,
                    &s.machine,
                    object(&s.input).map_err(at)?,
                    &refs,
                    &host,
                );
                runner.runs.push(Top {
                    id: s.run.clone(),
                    fut: Some(fut),
                    end: None,
                    handle,
                    expected: s.status,
                    suspending: None,
                });
                runner.settle().map_err(at)?;
            }
            Step::Together(items) => {
                let mut inputs: Vec<(String, Input)> = Vec::new();
                for item in items {
                    match item {
                        Step::Event(e) => inputs.push((
                            e.run().to_owned(),
                            Input::Event {
                                value: e.event.clone(),
                            },
                        )),
                        Step::Task(t) => {
                            let (run, invoke, _) =
                                registry.resolve(&t.task, t.run.as_deref()).map_err(at)?;
                            inputs.push((run, outcome_input(invoke, scripted_outcome(&t.outcome))));
                        }
                        Step::Advance(d) => {
                            let end = timer.now.load(Ordering::SeqCst) + d;
                            timer.now.store(end, Ordering::SeqCst);
                            for (run, id, _) in timer.due_by(end) {
                                timer.take(&run, id);
                                inputs.push((run, Input::After { timer: id }));
                            }
                        }
                        _ => {
                            return Err(at(
                                "a `together` holds only events, tasks and an advance".into()
                            ));
                        }
                    }
                }
                let run = inputs[0].0.clone();
                if inputs.iter().any(|(r, _)| *r != run) {
                    return Err(at("a `together` has inputs for two runs".into()));
                }
                let handle = handles
                    .lock()
                    .unwrap()
                    .get(&run)
                    .cloned()
                    .ok_or_else(|| at(format!("no run {run}")))?;
                handle.push(inputs.into_iter().map(|(_, i)| i).collect());
                runner.settle().map_err(at)?;
            }
            Step::Snapshot(snap) => {
                // A run that has ended (a defect included) gives its final snapshot.
                let saved = runner
                    .top(snap.run())
                    .map_err(at)?
                    .save()
                    .ok_or_else(|| at(format!("{} has no saved snapshot", snap.run())))?;
                compare_snapshot(c, &snap.name, &saved).map_err(at)?;
            }
            Step::Suspend(snap) => {
                let run = runner
                    .runs
                    .iter_mut()
                    .find(|r| r.id == snap.run())
                    .ok_or_else(|| at(format!("no top-level run {}", snap.run())))?;
                if run.suspending.is_some() {
                    return Err(at(format!("{} is already being suspended", run.id)));
                }
                if !matches!(
                    run.end,
                    None | Some(RunEnd::Done(_) | RunEnd::Failed(_) | RunEnd::Cancelled)
                ) {
                    return Err(at(format!("{} cannot be suspended", run.id)));
                }
                run.suspending = Some(snap.name.clone());
                run.handle.suspend();
                runner.settle().map_err(at)?;
            }
            Step::Restore(r) => {
                let text = c
                    .snapshots
                    .get(&r.name)
                    .ok_or_else(|| at(format!("no {}.snapshot.json", r.name)))?;
                let records = sink.records().len();
                // The runner's migration: called once, with the snapshot being restored.
                let calls: Arc<Mutex<Vec<Value>>> = Arc::default();
                let mut restore_config = config.clone();
                if let Some(m) = &r.migrate {
                    let to: Value = serde_json::from_str(
                        c.snapshots
                            .get(m)
                            .ok_or_else(|| at(format!("no {m}.snapshot.json")))?,
                    )
                    .map_err(|e| at(e.to_string()))?;
                    let seen = calls.clone();
                    restore_config = restore_config.migrate(move |old| {
                        seen.lock().unwrap().push(old);
                        Ok(to.clone())
                    });
                }
                let id = format!("r{}", runner.runs.len() + 1);
                // Restored as the machine the snapshot names, if the chart runs it, so that a
                // migration to a snapshot of another machine is refused (`machine`).
                let named: Option<String> = serde_json::from_str::<Value>(text)
                    .ok()
                    .and_then(|v| v["machine"].as_str().map(str::to_owned))
                    .filter(|m| program.ir().machines.contains_key(m) && !c.case.is_stubbed(m));
                let machine = named.unwrap_or_else(|| c.case.machine.clone());
                let restored = Saved::from_json(text).and_then(|saved| {
                    dynamic::restore(
                        &restore_config.clone().run_id(id.clone()),
                        &program,
                        Some(&machine),
                        &saved,
                        &host,
                    )
                });
                if let Some(m) = &r.migrate {
                    let calls = calls.lock().unwrap();
                    let from: Value = serde_json::from_str(text).map_err(|e| at(e.to_string()))?;
                    if calls.len() != 1 || snapshot_text(&calls[0]) != snapshot_text(&from) {
                        return Err(at(format!(
                            "the migration to {m} was called {} times, not once with {}",
                            calls.len(),
                            r.name
                        )));
                    }
                }
                match (restored, &r.refused) {
                    (Err(e), Some(reason)) if e.reason() == reason => {
                        if sink.records().len() != records {
                            return Err(at("a refused restore wrote records".into()));
                        }
                    }
                    (Err(e), _) => return Err(at(e.to_string())),
                    (Ok(_), Some(reason)) => {
                        return Err(at(format!("the restore was not refused ({reason})")));
                    }
                    (Ok((fut, handle)), None) => {
                        let run = r.run.clone().unwrap_or_default();
                        if run != id {
                            return Err(at(format!("the run is {id}, not {run}")));
                        }
                        runner.runs.push(Top {
                            id,
                            fut: Some(fut),
                            end: None,
                            handle,
                            expected: r.status.unwrap_or(Status::Running),
                            suspending: None,
                        });
                        runner.settle().map_err(at)?;
                    }
                }
            }
        }
        runner.check_suspended(c).map_err(at)?;
    }
    if let Some(run) = runner.runs.iter().find(|r| r.suspending.is_some()) {
        return Err(format!("{} was never suspended", run.id));
    }

    let problems = host.problems.lock().unwrap().clone();
    if !problems.is_empty() {
        return Err(problems.join("; "));
    }
    let records: Vec<Value> = sink.records().iter().map(|r| r.to_json()).collect();
    let text = to_lines(&arrange(records));
    for run in &runner.runs {
        let status = status_of(&run.end);
        if status != run.expected {
            return Err(format!(
                "run {} is {status:?}, expected {:?}\n{text}",
                run.id, run.expected
            ));
        }
    }
    Ok(text)
}

fn first_difference(expected: &str, actual: &str) -> String {
    let (e, a): (Vec<&str>, Vec<&str>) = (expected.lines().collect(), actual.lines().collect());
    for i in 0..e.len().max(a.len()) {
        let (x, y) = (e.get(i), a.get(i));
        if x != y {
            return format!(
                "line {}:\n  expected {}\n  actual   {}",
                i + 1,
                x.unwrap_or(&"(nothing)"),
                y.unwrap_or(&"(nothing)")
            );
        }
    }
    "the traces differ only in line endings".into()
}

#[test]
fn every_case_passes_byte_for_byte() {
    let cases = load_all(&conformance_dir()).expect("every case loads");
    let out = std::env::var_os("ROADBOOK_CONFORMANCE_OUT").map(std::path::PathBuf::from);
    if let Some(dir) = &out {
        std::fs::create_dir_all(dir).unwrap();
    }
    let mut failures = Vec::new();
    let mut skipped = 0;
    for c in &cases {
        if c.case.needs_more_than(SUPPORTED) {
            skipped += 1;
            continue;
        }
        match run_case(c) {
            Ok(actual) => {
                if let Some(dir) = &out {
                    std::fs::write(dir.join(format!("{}.jsonl", c.name)), &actual).unwrap();
                }
                if actual != c.expected {
                    failures.push(format!(
                        "{}: {}",
                        c.name,
                        first_difference(&c.expected, &actual)
                    ));
                }
            }
            Err(e) => failures.push(format!("{}: {e}", c.name)),
        }
    }
    let run = cases.len() - skipped;
    let passed = run - failures.len();
    println!(
        "conformance: {passed} of {run} cases pass byte for byte, {skipped} skipped (they require features this runtime does not implement)"
    );
    assert!(
        failures.is_empty(),
        "{} of {} cases failed:\n\n{}",
        failures.len(),
        run,
        failures.join("\n\n")
    );
}

/// Restores saved snapshot `name` of case `c` (the file's `text`) as run `x1`, at virtual
/// time 0, with the case's chart and a scripted host. Gives `false` if it is refused for the
/// reason it must be (the reason a `restore` step of the case without a migration gives, else
/// `ended` for a run that ended, `ir` for one saved with another hash), and `true` once the
/// restored run has gone on: right after restoring it saves the same snapshot under its new
/// id, its invokes have started again, and each of its timers fires at its saved deadline and
/// is taken. A file only restored may have its numbers in any form; it is restored as the
/// canonical snapshot it stands for.
fn restore_and_go_on(c: &LoadedCase, name: &str, text: &str) -> Result<bool, String> {
    let value: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let must_refuse = c.case.script.iter().find_map(|s| match s {
        Step::Restore(r) if r.name == name && r.migrate.is_none() => r.refused.clone(),
        _ => None,
    });
    // A file only restored to be refused as `invalid` may break the schema on purpose.
    let errors =
        roadbook_conformance::schema_errors(&roadbook_conformance::snapshot_validator(), &value);
    if !errors.is_empty() && must_refuse.as_deref() != Some("invalid") {
        return Err(format!("not a saved snapshot: {}", errors.join("; ")));
    }
    let written = c
        .case
        .snapshot_names()
        .iter()
        .any(|(n, current)| n == name && *current);
    if written && snapshot_text(&value) != text {
        return Err("not in canonical form".into());
    }
    let saved = match (Saved::from_json(text), &must_refuse) {
        (Ok(saved), _) => saved,
        (Err(e), Some(reason)) if e.reason() == reason => return Ok(false),
        (Err(e), _) => return Err(e.to_string()),
    };
    let ir = c.ir.clone().ok_or("no IR")?;
    let program = Program::new(ir).map_err(|e| e.to_string())?;
    let hash = program
        .ir()
        .machines
        .get(&saved.machine)
        .map(|m| m.hash.clone())
        .ok_or_else(|| format!("the IR has no machine {}", saved.machine))?;
    let must_refuse = must_refuse.or(if saved.status != roadbook::Status::Running {
        Some("ended".to_owned())
    } else if saved.ir != hash {
        Some("ir".to_owned())
    } else {
        None
    });
    let registry = Arc::new(Registry::default());
    let host = CaseHost {
        case: c,
        program: &program,
        registry,
        problems: Mutex::new(Vec::new()),
    };
    let timer = Arc::new(VirtualTimer::default());
    let sink = Arc::new(MemorySink::new());
    let config = Config::new()
        .sink(sink.clone())
        .timer(Arc::new(TimerHandle(timer.clone())))
        .run_id("x1");
    let (fut, handle) = match (
        dynamic::restore(&config, &program, None, &saved, &host),
        must_refuse,
    ) {
        (Err(e), Some(reason)) if e.reason() == reason => return Ok(false),
        (Err(e), _) => return Err(e.to_string()),
        (Ok(_), Some(reason)) => return Err(format!("not refused ({reason})")),
        (Ok(restored), None) => restored,
    };
    let mut runner = Runner {
        runs: vec![Top {
            id: "x1".into(),
            fut: Some(fut),
            end: None,
            handle: handle.clone(),
            expected: Status::Running,
            suspending: None,
        }],
        flag: Arc::new(Flag::default()),
    };
    runner.settle()?;
    // Saved again as the current version: a version 1 snapshot gains an empty `history`.
    let mut expected = saved.clone();
    expected.run = "x1".into();
    expected.snapshot_version = Saved::VERSION;
    expected.history.get_or_insert_default();
    let again = handle.save().ok_or("no saved snapshot after restoring")?;
    if again.to_canonical() != expected.to_canonical() {
        return Err(format!(
            "saved right after restoring:\n  expected {}\n  actual   {}",
            expected.to_canonical(),
            again.to_canonical()
        ));
    }
    let last = saved.timers.iter().map(|t| t.due).max().unwrap_or(0);
    advance(&timer, &mut runner, last)?;
    let records: Vec<Value> = sink
        .records()
        .iter()
        .filter(|r| r.run == "x1")
        .map(|r| r.to_json())
        .collect();
    for inv in &saved.invokes {
        let started = records
            .iter()
            .any(|r| r["kind"] == "invoke.started" && r["invoke"] == inv.id);
        if !started {
            return Err(format!("invoke {} was not started again", inv.id));
        }
    }
    for tm in &saved.timers {
        let taken = records.iter().any(|r| {
            r["kind"] == "event"
                && r["event"]["kind"] == "after"
                && r["event"]["timer"] == tm.id
                && r["t"] == tm.due
        });
        if !taken {
            return Err(format!("timer {} did not expire at {}", tm.id, tm.due));
        }
    }
    let problems = host.problems.lock().unwrap().clone();
    if !problems.is_empty() {
        return Err(problems.join("; "));
    }
    Ok(true)
}

/// The cross-runtime check: every saved snapshot file of the cases is what both runtimes
/// write, so restoring each one here restores what the other runtime saved.
#[test]
fn every_saved_snapshot_restores_and_goes_on() {
    let cases = load_all(&conformance_dir()).expect("every case loads");
    let from = std::env::var_os("ROADBOOK_RESTORE_FROM").map(std::path::PathBuf::from);
    let (mut restored, mut refused, mut failures) = (0, 0, Vec::new());
    for c in &cases {
        let files: Vec<(String, String)> = match &from {
            None => c.snapshots.clone().into_iter().collect(),
            Some(dir) => match std::fs::read_dir(dir.join(&c.name)) {
                Err(_) => Vec::new(),
                Ok(entries) => {
                    let mut files: Vec<(String, String)> = entries
                        .filter_map(Result::ok)
                        .filter_map(|e| {
                            let file = e.file_name().to_string_lossy().into_owned();
                            let name = file.strip_suffix(".snapshot.json")?.to_owned();
                            Some((name, std::fs::read_to_string(e.path()).ok()?))
                        })
                        .collect();
                    files.sort();
                    files
                }
            },
        };
        for (name, text) in files {
            match restore_and_go_on(c, &name, &text) {
                Ok(true) => restored += 1,
                Ok(false) => refused += 1,
                Err(e) => failures.push(format!("{}/{name}: {e}", c.name)),
            }
        }
    }
    println!("saved snapshots: {restored} restored and gone on, {refused} refused as they must be");
    assert!(restored > 0, "no snapshot was restored");
    assert!(
        failures.is_empty(),
        "{} snapshots failed:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

/// `Saved::from_value` checks `spec/snapshot.schema.json` by hand: it agrees with the schema
/// on every committed snapshot and on each of them broken in many ways.
#[test]
fn the_saved_snapshot_check_agrees_with_the_schema() {
    let validator = roadbook_conformance::snapshot_validator();
    let cases = load_all(&conformance_dir()).expect("every case loads");
    let breaks: Vec<(&str, Value)> = vec![
        ("snapshotVersion", serde_json::json!(2)),
        ("snapshotVersion", serde_json::json!("1")),
        ("machine", serde_json::json!("order")),
        ("machine", serde_json::json!("Or-der")),
        ("run", serde_json::json!("")),
        ("run", serde_json::json!("r1.0")),
        ("run", serde_json::json!("r 1")),
        ("ir", serde_json::json!("sha256:abc")),
        (
            "ir",
            serde_json::json!(format!("sha256:{}", "A".repeat(64))),
        ),
        ("status", serde_json::json!("paused")),
        ("active", serde_json::json!(["Order", "Order"])),
        ("active", serde_json::json!(["order"])),
        ("active", serde_json::json!("Order")),
        ("input", serde_json::json!({"Id": 1})),
        ("input", serde_json::json!([])),
        ("vars", serde_json::json!({"a_b": 1})),
        (
            "invokes",
            serde_json::json!([{"id": 0, "state": "A", "src": "x"}]),
        ),
        (
            "invokes",
            serde_json::json!([{"id": 1, "state": "A", "src": "x", "y": 1}]),
        ),
        (
            "invokes",
            serde_json::json!([{"id": 1, "state": "A", "src": "x_y"}]),
        ),
        ("invokes", serde_json::json!([{"id": 1, "state": "A"}])),
        (
            "timers",
            serde_json::json!([{"id": 1, "state": "A", "index": 0, "delay": 1}]),
        ),
        (
            "timers",
            serde_json::json!([{"id": 1, "state": "A", "index": -1, "delay": 1, "due": 1}]),
        ),
        (
            "timers",
            serde_json::json!([{"id": 1, "state": "A", "index": 0, "delay": 1.5, "due": 1}]),
        ),
        (
            "timers",
            serde_json::json!([{"id": 1, "state": "A", "index": 0, "delay": 1, "due": 9007199254740992u64}]),
        ),
        ("nextInvoke", serde_json::json!(0)),
        ("nextMessage", serde_json::json!(null)),
        (
            "result",
            serde_json::json!({"output": 1, "error": {"_tag": "E"}}),
        ),
        ("result", serde_json::json!({"error": {"tag": "E"}})),
        ("result", serde_json::json!({"error": {"_tag": "e"}})),
        ("result", serde_json::json!({"defect": {"message": "m"}})),
        ("result", serde_json::json!({"defect": {"kind": "limit"}})),
        ("result", serde_json::json!({"output": null})),
        ("result", serde_json::json!(1)),
        (
            "timers",
            serde_json::json!([{"id": 1, "state": "A.B", "index": 0, "delay": 0, "due": 0}]),
        ),
        ("snapshotVersion", serde_json::json!(1)),
        ("snapshotVersion", serde_json::json!(3)),
        ("history", serde_json::json!({})),
        ("history", serde_json::json!([])),
        ("history", serde_json::json!({"A.H": ["A.B"]})),
        ("history", serde_json::json!({"A.H": []})),
        ("history", serde_json::json!({"A.H": ["A.B", "A.B"]})),
        ("history", serde_json::json!({"A.H": "A.B"})),
        ("history", serde_json::json!({"a": ["A.B"]})),
        ("history", serde_json::json!({"A.H": ["A.b"]})),
    ];
    let mut checked = 0;
    for c in &cases {
        for text in c.snapshots.values() {
            let base: Value = serde_json::from_str(text).unwrap();
            let mut docs = vec![base.clone()];
            for key in base.as_object().unwrap().keys() {
                let mut d = base.clone();
                d.as_object_mut().unwrap().remove(key);
                docs.push(d);
            }
            let mut d = base.clone();
            d["extra"] = Value::Null;
            docs.push(d);
            for (key, value) in &breaks {
                let mut d = base.clone();
                d[*key] = value.clone();
                docs.push(d);
            }
            for d in docs {
                let schema = validator.is_valid(&d);
                // Refused as `invalid` exactly when the schema rejects it. (One that matches
                // the schema may still be refused as `ended`, for a result this runtime cannot
                // hold: a defect of a kind it does not know.)
                let ours =
                    Saved::from_value(d.clone()).map_or_else(|e| e.reason() != "invalid", |_| true);
                assert_eq!(schema, ours, "the schema says {schema} for {d}");
                checked += 1;
            }
        }
    }
    assert!(checked > 100);
}
