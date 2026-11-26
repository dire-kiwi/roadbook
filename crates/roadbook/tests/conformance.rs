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
//! With `ROADBOOK_CONFORMANCE_OUT=<dir>`, each case's actual trace is also written to
//! `<dir>/<case>.jsonl`.
//!
//! A case that `requires` a feature not in [`SUPPORTED`] is skipped, not failed.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};

use roadbook::dynamic::{self, Host, Invocation, Outcome};
use roadbook::{BoxFuture, Config, Defect, Input, MemorySink, Program, RunEnd, RunHandle, Timer};
use roadbook_conformance::{
    LoadedCase, Outcome as ScriptOutcome, Status, Step, arrange, compare_run_ids, conformance_dir,
    load_all, to_lines,
};
use roadbook_core::{Object, SrcKind};
use serde_json::Value;

/// The features of `spec/conformance/case.schema.json`'s `requires` this runtime
/// implements.
const SUPPORTED: &[&str] = &["messaging"];

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
}

fn status_of(end: &Option<RunEnd>) -> Status {
    match end {
        None => Status::Running,
        Some(RunEnd::Done(_)) => Status::Done,
        Some(RunEnd::Failed(_)) => Status::Failed,
        Some(RunEnd::Cancelled) => Status::Cancelled,
        Some(RunEnd::Defect(_)) => Status::Defect,
    }
}

fn object(v: &Value) -> Result<Object, String> {
    match v {
        Value::Object(m) => Ok(m.clone()),
        _ => Err("an `input` is not an object".into()),
    }
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
        }],
        flag: Arc::new(Flag::default()),
    };
    runner.settle()?;

    for (i, step) in c.case.script.iter().enumerate() {
        let at = |e: String| format!("step {}: {e}", i + 1);
        if !matches!(step, Step::Start(_)) && runner.runs.iter().all(|r| r.end.is_some()) {
            return Err(at("every top-level run has already ended".into()));
        }
        match step {
            Step::Advance(d) => {
                let end = timer.now.load(Ordering::SeqCst) + d;
                while let Some((run, id, due)) = timer.due_by(end).into_iter().next() {
                    timer.now.store(due, Ordering::SeqCst);
                    if let Some(fire) = timer.take(&run, id) {
                        fire.fire();
                    }
                    runner.settle().map_err(at)?;
                }
                timer.now.store(end, Ordering::SeqCst);
            }
            Step::Event(e) => {
                runner.running(e.run()).map_err(at)?.send(e.event.clone());
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
                runner.running(run).map_err(at)?.cancel();
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
            // Cases with these steps require `snapshots`, which this runtime does not
            // implement yet, so they are skipped before they get here.
            Step::Snapshot(_) | Step::Restore(_) | Step::Suspend(_) => {
                return Err(at("saving and restoring are not implemented yet".into()));
            }
        }
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
