//! The erased host interface: host code called by name with JSON values.
//!
//! The driver talks only to a [`Host`]. The `Handlers` trait that `roadbook::include!`
//! generates for each machine is adapted to it, decoding arguments into the chart's Rust
//! types and encoding results. A host can also implement it directly, as the conformance
//! runner does from a case's script.
//!
//! A `ref` is a run id in these values. The handles the ids stand for travel beside them:
//! [`run`], [`run_with`] and [`start`] take the handles of the runs the input names, and an
//! [`Invocation`] carries those its arguments name, so a host that runs an invoked machine
//! passes them on.

use roadbook_core::{Defect, Input, Object, Program, RestoreError, Saved, SrcKind};
use serde_json::Value;

use crate::driver::{Driver, RunEnd};
pub use crate::emit::DynEmitter as Emitter;
use crate::env::{BoxFuture, Config};
use crate::shared::{RunHandle, Shared, claim_context};

/// Host code, called by name with values in their JSON form.
///
/// The driver catches panics around every call and every poll of a returned future, and
/// turns them into host defects ([`Defect::Host`]), so an implementation may panic.
pub trait Host: Send + Sync {
    /// Calls the pure function `name` with positional arguments. An `Err` is a host defect,
    /// for example a result that could not be encoded.
    fn function(&self, name: &str, args: Vec<Value>) -> Result<Value, Defect>;

    /// Runs action `name` with arguments keyed by parameter name.
    fn action<'a>(&'a self, name: &str, args: Object) -> BoxFuture<'a, Result<(), Defect>>;

    /// Starts the task or machine of `call`, invoked or spawned. The future's output becomes
    /// the invoke's (or spawned child's) outcome. For a machine, the future is polled inside
    /// an invoke context, so a [`run`] (or a generated `run`) inside it becomes the child run.
    /// A task emits through [`Invocation::emitter`]. A call of a peer ([`SrcKind::Call`])
    /// never comes here: the driver makes it itself.
    fn invoke<'a>(&'a self, call: Invocation) -> BoxFuture<'a, Outcome>;
}

/// One invoke the driver starts.
#[derive(Debug, Clone, PartialEq)]
pub struct Invocation {
    /// Task or machine; never [`SrcKind::Call`].
    pub kind: SrcKind,
    /// The task's or machine's name.
    pub src: String,
    /// Its arguments, keyed by parameter name. A `ref` in them is a run id.
    pub args: Object,
    /// The handles of the runs whose ids are `ref`s in `args`: what a [`run`] of an invoked
    /// machine is given with them.
    pub refs: Vec<RunHandle>,
    /// The id of the run that starts it.
    pub run: String,
    /// The invoke's id in that run, or the spawned child's: the two share one sequence.
    pub invoke: u64,
    /// The invoke's `as` name, `None` without one and for a spawned child: what tells two
    /// invokes of the same task in one state apart.
    pub alias: Option<String>,
    /// What a task emits its events through (`spec/execution.md#the-emitter`): live for an
    /// invoke with `as` and for a spawned task, inert for an invoke without `as` and for a
    /// machine.
    pub emitter: Emitter,
}

/// How an invoke ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// It succeeded with this value (`null` for nothing).
    Done(Value),
    /// It failed with this error, a tagged record.
    Fail(Value),
    /// It ended with a defect.
    Defect(Defect),
}

/// Runs machine `machine` of `program` with `input` and `host`, to its end. A `ref` in
/// `input` is a run id, and `refs` holds the handle of each run so named. A `ref M` whose
/// handle is not in `refs`, is a run of another machine (or another version of `M`, by
/// machine hash), or is a child run's handle (from [`Config::on_run`]) is a host defect
/// before the run starts, with no record.
///
/// Polled inside an invoke context whose token is unclaimed (that is, from a machine
/// invoke's future), it claims the token and becomes that invoke's child run: it writes to
/// the parent's trace sink on the parent's timer, and its parent cancels it gracefully. When
/// its parent was restored from a snapshot that holds this child run's, it restores that
/// snapshot instead of starting, and `input` is not used; a snapshot of another machine (or
/// of another version of it) is a host defect before its first record. Else it is a root run
/// with `Config::default()`.
///
/// Returns the output (`Ok`) or the error (`Err`). A run that ends with a defect resumes
/// unwinding with a [`Defect`] payload. A cancelled child run never returns: it reports
/// through the token, and its parent drops the future. The run goes on to its end: a suspend
/// request through its [`RunHandle`] (from [`Config::on_run`]) is ignored, as a child run
/// ignores one; [`start`] gives a root run that can be suspended.
pub async fn run(
    program: &Program,
    machine: &str,
    input: Object,
    refs: &[RunHandle],
    host: &dyn Host,
) -> Result<Value, Value> {
    let (driver, child) = match claim_context() {
        Some(ctx) => (
            Driver::child(program, machine, input, refs, host, &ctx),
            true,
        ),
        None => {
            let config = Config::default();
            let id = config.root_id();
            (
                Driver::root(
                    program,
                    machine,
                    input,
                    refs,
                    host,
                    &config,
                    id,
                    Shared::claimed(),
                )
                .run_to_the_end(),
                false,
            )
        }
    };
    let (end, cancelled) = driver.drive_reporting().await;
    finish(end, child && cancelled).await
}

/// Like [`run`], but always a root run, set up by `config`. It too ignores suspend requests.
pub async fn run_with(
    config: &Config,
    program: &Program,
    machine: &str,
    input: Object,
    refs: &[RunHandle],
    host: &dyn Host,
) -> Result<Value, Value> {
    let driver = Driver::root(
        program,
        machine,
        input,
        refs,
        host,
        config,
        config.root_id(),
        Shared::claimed(),
    )
    .run_to_the_end();
    finish(driver.drive().await, false).await
}

/// `cancelled_child`: a child run that took its parent's cancel request, which reports how
/// its cancel step ended through the token instead.
async fn finish(end: RunEnd, cancelled_child: bool) -> Result<Value, Value> {
    match end {
        RunEnd::Done(v) => Ok(v),
        RunEnd::Failed(e) => Err(e),
        RunEnd::Defect(defect) if !cancelled_child => std::panic::resume_unwind(Box::new(defect)),
        // Reported through the token: a child run cancelled, or stopped with its suspended
        // tree (a root run of `run` ignores suspend requests). The parent drops this future.
        _ => std::future::pending().await,
    }
}

/// Starts a root run and gives a handle to it: the future runs it to its end and gives how
/// it ended, without unwinding. Nothing happens until the future is polled, but the run is
/// open, and subscribed to what the runs its `ref`s name emit, from the start: messages and
/// emitted events that reach it before are queued. The handle can be given to other runs as
/// a `ref` argument.
pub fn start<'a>(
    config: &Config,
    program: &'a Program,
    machine: &'a str,
    input: Object,
    refs: &[RunHandle],
    host: &'a dyn Host,
) -> (BoxFuture<'a, RunEnd>, RunHandle) {
    let shared = Shared::claimed();
    let id = config.root_id();
    let handle = RunHandle::new(shared.clone(), &id, program, machine, false);
    let driver = Driver::root(program, machine, input, refs, host, config, id, shared);
    (Box::pin(driver.drive()), handle)
}

/// Restores a saved snapshot as a new root run (`spec/execution.md#restoring-a-snapshot`) and
/// gives a handle to it, as [`start`] does. `machine` is the machine to restore, or `None` for
/// the one the snapshot names. The snapshot is checked first, applying `config`'s migration
/// ([`Config::migrate`]) when its hash is not the machine's, and refused with a
/// [`RestoreError`] if it does not fit; nothing is written to the trace then. The new run gets
/// a new id ([`Config::run_id`], or the next default one), and its first record is
/// `restored`.
pub fn restore<'a>(
    config: &Config,
    program: &'a Program,
    machine: Option<&str>,
    saved: &Saved,
    host: &'a dyn Host,
) -> Result<(BoxFuture<'a, RunEnd>, RunHandle), RestoreError> {
    let checked = crate::driver::check(program, machine, saved, config)?;
    let shared = Shared::claimed();
    let id = config.root_id();
    let handle = RunHandle::new(shared.clone(), &id, program, &checked.0.machine, false);
    let driver = Driver::restored(program, checked, host, config, id, shared);
    Ok((Box::pin(driver.drive()), handle))
}

/// The request handler's operation (`spec/execution.md#suspending-and-resume`): restores
/// `saved` as a new root run, gives it `event` (an encoded event) if there is one, and
/// suspends it once it is idle, giving the saved snapshot. Expiries of timers that fell due
/// while the run was saved are taken before the event. A run that ends gives its final
/// snapshot (`done`, `failed`, `cancelled`); one that ends with a defect resumes unwinding
/// with the [`Defect`], as [`run`] does.
///
/// Both inputs are checked before anything runs or is written: the snapshot first (a
/// [`RestoreError`]), then the event, which must be one the machine accepts, with exactly its
/// fields, each of its type. An event that is not is the caller's host defect, as one that
/// does not encode is for `Actor::send`: `resume` resumes unwinding with the [`Defect`].
pub async fn resume(
    config: &Config,
    program: &Program,
    machine: Option<&str>,
    saved: &Saved,
    event: Option<Value>,
    host: &dyn Host,
) -> Result<Saved, RestoreError> {
    let checked = crate::driver::check(program, machine, saved, config)?;
    let event = event.map(|value| {
        program
            .check_event(&checked.0.machine, value)
            .unwrap_or_else(|e| {
                std::panic::resume_unwind(Box::new(Defect::host(format!(
                    "resume was given an event that is not one of the machine's: {e}"
                ))))
            })
    });
    let shared = Shared::claimed();
    if let Some(value) = event {
        shared.push([Input::Event { value }]);
    }
    shared.request_suspend();
    let id = config.root_id();
    let driver = Driver::restored(program, checked, host, config, id, shared.clone());
    Ok(suspended(driver.drive().await, &shared))
}

/// The saved snapshot a run that was asked to suspend gives: the one it stopped with, or the
/// final one of a run that ended. A run that ended with a defect resumes unwinding with it.
pub(crate) fn suspended(end: RunEnd, shared: &Shared) -> Saved {
    match end {
        RunEnd::Suspended(saved) => *saved,
        RunEnd::Defect(d) => std::panic::resume_unwind(Box::new(d)),
        RunEnd::Done(_) | RunEnd::Failed(_) | RunEnd::Cancelled => shared
            .saved()
            .map(|s| (*s).clone())
            .expect("a run that ended has a saved snapshot"),
    }
}
