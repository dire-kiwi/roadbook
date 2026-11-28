//! The core's data: snapshots, inputs, commands, trace records and defects, each serialising
//! to exactly the JSON form `spec/execution.md` and `spec/trace.md` give.

use std::collections::BTreeMap;
use std::fmt;

pub use roadbook_ir::ReplyOutcome;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A JSON object: arguments keyed by parameter name, variables keyed by name.
pub type Object = Map<String, Value>;

/// What each history state recorded, keyed by its id: state ids, in document order.
pub type History = BTreeMap<String, Vec<String>>;

/// A run's status. Only [`Status::Running`] is not final.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Still running.
    Running,
    /// Reached a top-level final state with an output (or none).
    Done,
    /// Reached a top-level final state with `fail`.
    Failed,
    /// Cancelled from outside.
    Cancelled,
    /// Ended with a defect.
    Defect,
}

/// How a peer's run closed: the built-in enum `Ended`, the `status` of an [`Input::Ended`]
/// and the type of `event.status` in an `on name.ended` transition. In JSON, its variant's
/// name: `"Done"`, `"Failed"`, `"Cancelled"`, `"Defect"` or `"Suspended"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Ended {
    /// It ended `done`.
    Done,
    /// It ended `failed`.
    Failed,
    /// It was cancelled, or (in Rust) its future was dropped.
    Cancelled,
    /// It ended with a defect.
    Defect,
    /// It was suspended: closed for good, its snapshot restored, if ever, as another run.
    Suspended,
}

impl Ended {
    /// The variant's name, the enum value in JSON.
    pub fn as_str(self) -> &'static str {
        match self {
            Ended::Done => "Done",
            Ended::Failed => "Failed",
            Ended::Cancelled => "Cancelled",
            Ended::Defect => "Defect",
            Ended::Suspended => "Suspended",
        }
    }

    /// The end of a run that closed with `status`, which is not `running`.
    pub fn of(status: Status) -> Option<Ended> {
        match status {
            Status::Running => None,
            Status::Done => Some(Ended::Done),
            Status::Failed => Some(Ended::Failed),
            Status::Cancelled => Some(Ended::Cancelled),
            Status::Defect => Some(Ended::Defect),
        }
    }
}

impl fmt::Display for Ended {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a run ended with a defect. In JSON, `{"kind": "host", "message": …}` and so on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Defect {
    /// Host code failed: a function, action or task threw or panicked, or returned a value
    /// that is not of its declared type.
    Host {
        /// What went wrong.
        message: String,
    },
    /// A variable was read before it had a value.
    Unassigned {
        /// The variable.
        var: String,
    },
    /// Integer arithmetic left the `int` range, or float arithmetic was not finite.
    Overflow {
        /// The `text` of the operator's IR node.
        expr: String,
    },
    /// A task or child machine failed with an error it does not declare.
    Undeclared {
        /// The error's tag.
        error: String,
    },
    /// Settling took more than [`crate::SETTLE_LIMIT`] microsteps.
    Limit,
    /// A `strict` machine was given one of its own events, a message or a call that enabled no
    /// transition.
    Unhandled {
        /// The input, an `event`, a `message` or a `call`, as an `event` record would show it.
        event: Box<Input>,
    },
    /// A `reply` was given a pending-call value whose call is not pending: it was answered
    /// already, or the run was restored.
    Answered {
        /// The call's name.
        call: String,
    },
}

impl Defect {
    /// A host defect with this message.
    pub fn host(message: impl Into<String>) -> Defect {
        Defect::Host {
            message: message.into(),
        }
    }

    /// The defect as its JSON object.
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

impl fmt::Display for Defect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Defect::Host { message } => write!(f, "host defect: {message}"),
            Defect::Unassigned { var } => write!(f, "`{var}` was read before it had a value"),
            Defect::Overflow { expr } => write!(f, "`{expr}` overflowed"),
            Defect::Undeclared { error } => {
                write!(
                    f,
                    "an invoke failed with `{error}`, which it does not declare"
                )
            }
            Defect::Limit => write!(f, "settling took more than 1000 microsteps"),
            Defect::Unhandled { event } => {
                let tag = match &**event {
                    Input::Event { value } | Input::Message { value, .. } => value
                        .get("_tag")
                        .and_then(Value::as_str)
                        .unwrap_or("an event"),
                    Input::Call { call, .. } => {
                        return write!(
                            f,
                            "a `{call}` call enabled no transition in a strict machine"
                        );
                    }
                    _ => "an input",
                };
                write!(f, "`{tag}` enabled no transition in a strict machine")
            }
            Defect::Answered { call } => {
                write!(f, "a `reply` answered a `{call}` call that is not pending")
            }
        }
    }
}

impl std::error::Error for Defect {}

/// A run's result, once it has one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum RunResult {
    /// `{"output": value}`: done, `null` for a machine that returns nothing.
    Output(Value),
    /// `{"error": value}`: failed with this error.
    Error(Value),
    /// `{"defect": defect}`: ended with a defect.
    Defect(Defect),
}

/// A running invoke in a snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvokeEntry {
    /// Its id within the run.
    pub id: u64,
    /// The id of the state that owns it.
    pub state: String,
    /// The task's or machine's name, or for a call the address and the call's name,
    /// `inventory.reserve`.
    pub src: String,
}

/// A call the run has taken and not answered yet: a pending call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallEntry {
    /// The caller's run id.
    pub from: String,
    /// The id of the caller's invoke that made the call.
    pub invoke: u64,
    /// The call's name, a key of the machine's `calls`.
    pub call: String,
}

impl CallEntry {
    /// Its pending-call value, `{"from", "invoke"}`: the value of `call` in the transition
    /// that took it.
    pub fn value(&self) -> Value {
        serde_json::json!({ "from": self.from, "invoke": self.invoke })
    }
}

/// A running spawned child in a snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpawnedEntry {
    /// Its id within the run, from the sequence invokes take theirs from.
    pub id: u64,
    /// Its collection.
    pub children: String,
    /// Its key: a string or an integer.
    pub key: Value,
    /// The arguments it was spawned with, keyed by parameter name.
    pub args: Object,
}

/// A running `after` timer in a snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimerEntry {
    /// Its id within the run.
    pub id: u64,
    /// The id of the state that owns it.
    pub state: String,
    /// The index in the state's `transitions` of its `after` block's first transition.
    pub index: usize,
    /// The evaluated delay, in milliseconds.
    pub delay: u64,
}

/// Everything the core knows about a run between steps. Plain JSON, never modified in place.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Snapshot {
    /// The machine's name.
    pub machine: String,
    /// The run's id, as given to `start`.
    pub run: String,
    /// The machine's hash from the IR.
    pub ir: String,
    /// The run's status.
    pub status: Status,
    /// The active states' ids, in document order.
    pub active: Vec<String>,
    /// The arguments, keyed by parameter name.
    pub input: Object,
    /// Every variable that has a value.
    pub vars: Object,
    /// The running invokes, in the order they were started.
    pub invokes: Vec<InvokeEntry>,
    /// The calls the run has taken and not answered yet, in the order it took them.
    #[serde(default)]
    pub calls: Vec<CallEntry>,
    /// The running spawned children, in the order they were started.
    pub spawned: Vec<SpawnedEntry>,
    /// The running timers, in the order they were started.
    pub timers: Vec<TimerEntry>,
    /// The id the next invoke will get.
    pub next_invoke: u64,
    /// The id the next timer will get.
    pub next_timer: u64,
    /// The id the next `send` or `emit` effect will get.
    pub next_message: u64,
    /// What each history state recorded when its parent was last exited.
    pub history: History,
    /// `null` while running and when cancelled.
    pub result: Option<RunResult>,
}

/// What the driver gives the core.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum Input {
    /// An external event, a tagged record: `{"_tag": "Cancel", …}`.
    #[serde(rename = "event")]
    Event {
        /// The event.
        value: Value,
    },
    /// A message: run `from` sent one of this machine's events with `send`.
    #[serde(rename = "message")]
    Message {
        /// The sender's run id.
        from: String,
        /// The message's id in the sender.
        message: u64,
        /// The event, a tagged record.
        value: Value,
    },
    /// An emitted event: run `from` emitted one of its machine's `emits`.
    #[serde(rename = "emitted")]
    Emitted {
        /// The sender's run id.
        from: String,
        /// The id of this run's invoke or spawned child that is the sender, when this run is
        /// its parent; `null` when this run holds a `ref` to it.
        invoke: Option<u64>,
        /// The message's id in the sender.
        message: u64,
        /// The event, a tagged record.
        value: Value,
    },
    /// A task's emit: the task of this run's invoke or spawned task `invoke` emitted one of
    /// its task's `emits` while it ran. Given only for an invoke with `as` or a spawned task,
    /// and only for an event the run's machine hears from that task.
    #[serde(rename = "task.emitted")]
    TaskEmitted {
        /// The id of the invoke or spawned task.
        invoke: u64,
        /// The event, a tagged record.
        value: Value,
    },
    /// A call: run `from` called this machine's call `call` with its invoke `invoke`.
    #[serde(rename = "call")]
    Call {
        /// The caller's run id.
        from: String,
        /// The id of the caller's invoke that made the call.
        invoke: u64,
        /// The call's name, a key of the machine's `calls`.
        call: String,
        /// Its arguments, keyed by the call's parameter names.
        args: Object,
    },
    /// A peer's end: run `from`, which this run holds a `ref` to, has closed. Given only to
    /// a run whose machine hears `ended` from `from`'s machine, at most once per peer.
    #[serde(rename = "ended")]
    Ended {
        /// The peer's run id.
        from: String,
        /// How it closed.
        status: Ended,
    },
    /// An invoke succeeded.
    #[serde(rename = "invoke.done")]
    InvokeDone {
        /// The invoke's id.
        invoke: u64,
        /// Its result, `null` for a task, machine or call that returns nothing.
        value: Value,
    },
    /// An invoke failed with an error, a tagged record: for a call, possibly
    /// `{"_tag": "Unanswered"}`.
    #[serde(rename = "invoke.fail")]
    InvokeFail {
        /// The invoke's id.
        invoke: u64,
        /// The error.
        error: Value,
    },
    /// An invoke ended with a defect.
    #[serde(rename = "invoke.defect")]
    InvokeDefect {
        /// The invoke's id.
        invoke: u64,
        /// The defect.
        defect: Defect,
    },
    /// A spawned child ended `done`.
    #[serde(rename = "spawn.done")]
    SpawnDone {
        /// The spawned child's id.
        spawn: u64,
        /// Its output, `null` for a machine that returns nothing.
        value: Value,
    },
    /// A spawned child failed with an error, a tagged record.
    #[serde(rename = "spawn.fail")]
    SpawnFail {
        /// The spawned child's id.
        spawn: u64,
        /// The error.
        error: Value,
    },
    /// A spawned child ended with a defect.
    #[serde(rename = "spawn.defect")]
    SpawnDefect {
        /// The spawned child's id.
        spawn: u64,
        /// The defect.
        defect: Defect,
    },
    /// A timer expired.
    #[serde(rename = "after")]
    After {
        /// The timer's id.
        timer: u64,
    },
    /// Cancel the run from outside.
    #[serde(rename = "cancel")]
    Cancel,
    /// The driver met a defect while carrying out this run's commands.
    #[serde(rename = "defect")]
    Defect {
        /// The defect.
        defect: Defect,
    },
}

impl Input {
    /// The id of the invoke whose outcome this is, if it is one.
    pub fn invoke_id(&self) -> Option<u64> {
        match self {
            Input::InvokeDone { invoke, .. }
            | Input::InvokeFail { invoke, .. }
            | Input::InvokeDefect { invoke, .. } => Some(*invoke),
            _ => None,
        }
    }

    /// The id of the spawned child whose outcome this is, if it is one.
    pub fn spawn_id(&self) -> Option<u64> {
        match self {
            Input::SpawnDone { spawn, .. }
            | Input::SpawnFail { spawn, .. }
            | Input::SpawnDefect { spawn, .. } => Some(*spawn),
            _ => None,
        }
    }

    /// The id of the timer whose expiry this is, if it is one.
    pub fn timer_id(&self) -> Option<u64> {
        match self {
            Input::After { timer } => Some(*timer),
            _ => None,
        }
    }
}

/// Whether an invoke runs a task or a machine, or makes a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SrcKind {
    /// A host task.
    Task,
    /// A machine.
    Machine,
    /// A call of a peer's `call`, which the driver makes itself: host code never sees one.
    Call,
}

/// What the core asks the driver to do, in order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum Command {
    /// Write this trace record, after adding `t`, `run` and (for a child run) `parent`.
    #[serde(rename = "trace")]
    Trace {
        /// The record.
        record: Record,
    },
    /// Run the action and wait for it to finish.
    #[serde(rename = "action")]
    Action {
        /// The action's name.
        action: String,
        /// Its arguments, keyed by parameter name.
        args: Object,
    },
    /// Start the task or machine, or make the call; queue its outcome as an `invoke.*` input
    /// when it ends.
    #[serde(rename = "invoke.start")]
    InvokeStart {
        /// The invoke's id.
        invoke: u64,
        /// The id of the state that owns it.
        state: String,
        /// The task's, machine's or call's name.
        src: String,
        /// Task, machine or call.
        #[serde(rename = "srcKind")]
        src_kind: SrcKind,
        /// For a call, the callee's run id; `None` for a task or a machine.
        to: Option<String>,
        /// The invoke's `as` name, or `null`: a child run invoked with one delivers what it
        /// emits to this run.
        #[serde(rename = "as")]
        alias: Option<String>,
        /// Its arguments, keyed by parameter name.
        args: Object,
        /// The saved snapshot of the invoke's child run, which the run that claims the invoke
        /// restores instead of starting: only in the commands of
        /// [`crate::Program::restore`], when the saved snapshot holds one.
        saved: Option<Box<Saved>>,
    },
    /// Answer the call that run `to` made with its invoke `invoke`, without waiting for the
    /// caller.
    #[serde(rename = "reply")]
    Reply {
        /// The caller's run id.
        to: String,
        /// The id of the caller's invoke.
        invoke: u64,
        /// The call's name.
        call: String,
        /// Done or fail.
        outcome: ReplyOutcome,
        /// The result (`null` for a call that returns nothing) or the error.
        value: Value,
    },
    /// Cancel the invoke and wait until it has stopped.
    #[serde(rename = "invoke.cancel")]
    InvokeCancel {
        /// The invoke's id.
        invoke: u64,
    },
    /// Start machine or task `src` as a spawned child, which delivers what it emits to this
    /// run; queue its outcome as a `spawn.*` input when it ends.
    #[serde(rename = "spawn.start")]
    SpawnStart {
        /// The spawned child's id.
        spawn: u64,
        /// The machine's or task's name.
        src: String,
        /// [`SrcKind::Machine`] or [`SrcKind::Task`], by the collection's kind.
        #[serde(rename = "srcKind")]
        src_kind: SrcKind,
        /// Its arguments, keyed by parameter name.
        args: Object,
        /// The saved snapshot of the child run, as for [`Command::InvokeStart`]; always `None`
        /// for a task.
        saved: Option<Box<Saved>>,
    },
    /// Stop the spawned child and wait until it has stopped, as `invoke.cancel` does.
    #[serde(rename = "spawn.stop")]
    SpawnStop {
        /// The spawned child's id.
        spawn: u64,
    },
    /// Start a timer that queues `{"kind": "after", "timer": id}` after `delay` milliseconds.
    #[serde(rename = "timer.start")]
    TimerStart {
        /// The timer's id.
        timer: u64,
        /// Milliseconds.
        delay: u64,
    },
    /// Cancel the timer.
    #[serde(rename = "timer.cancel")]
    TimerCancel {
        /// The timer's id.
        timer: u64,
    },
    /// Queue `{"kind": "after", "timer": id}` on this run's queue at once, without starting a
    /// timer: a restored timer that was already due. Only [`crate::Program::restore`] emits it.
    #[serde(rename = "timer.fire")]
    TimerFire {
        /// The timer's id.
        timer: u64,
    },
    /// Queue a message for run `to` without waiting for it: for the child run of this run's
    /// invoke or spawned child `invoke`, or, when `invoke` is `None`, for the peer `to`.
    #[serde(rename = "send")]
    Send {
        /// The receiver's run id.
        to: String,
        /// The invoke whose child run receives it, or `None` for a peer.
        invoke: Option<u64>,
        /// The message's id in this run.
        message: u64,
        /// The event, a tagged record.
        value: Value,
    },
    /// Queue the emitted event for this run's parent (if it invoked this run with `as`, or
    /// spawned it) and for every run that holds a `ref` to this run, of those whose machine has
    /// an `on` for it ([`Program::heard`](crate::Program::heard)), without waiting for them.
    #[serde(rename = "emit")]
    Emit {
        /// The message's id in this run.
        message: u64,
        /// The event, a tagged record.
        value: Value,
    },
}

/// A trace record as the core emits it, without `t`, `run` and `parent`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum Record {
    /// The run started.
    #[serde(rename = "started")]
    Started {
        /// The machine's name.
        machine: String,
        /// The machine's hash.
        ir: String,
        /// The arguments.
        input: Object,
    },
    /// The run was restored from a saved snapshot: first, instead of `started`.
    #[serde(rename = "restored")]
    Restored {
        /// The machine's name.
        machine: String,
        /// The machine's hash.
        ir: String,
        /// The hash the snapshot had before a migration, or `null` when there was none.
        migrated: Option<String>,
        /// The saved snapshot it was restored from, after any migration.
        snapshot: Box<Saved>,
    },
    /// A state was entered, before its entry effects.
    #[serde(rename = "entered")]
    Entered {
        /// The state's id.
        state: String,
    },
    /// A state was left, after its work was cancelled and its exit effects ran.
    #[serde(rename = "exited")]
    Exited {
        /// The state's id.
        state: String,
    },
    /// An input that enables a transition.
    #[serde(rename = "event")]
    Event {
        /// The input as the core received it.
        event: Input,
    },
    /// An input that enables no transition.
    #[serde(rename = "dropped")]
    Dropped {
        /// The input.
        event: Input,
    },
    /// The outcome or expiry of an invoke or timer that is no longer running.
    #[serde(rename = "stale")]
    Stale {
        /// The input.
        event: Input,
    },
    /// A transition was taken.
    #[serde(rename = "transition")]
    Transition {
        /// The id of the state it belongs to.
        from: String,
        /// Its index in that state's `transitions`.
        index: usize,
        /// The targets' ids, as the IR's `targets`: `[]` for a transition with no target.
        to: Vec<String>,
    },
    /// A variable was assigned.
    #[serde(rename = "set")]
    Set {
        /// The variable.
        var: String,
        /// Its new value.
        value: Value,
    },
    /// An action is about to run.
    #[serde(rename = "action")]
    Action {
        /// The action's name.
        action: String,
        /// Its arguments, keyed by parameter name.
        args: Object,
    },
    /// A `send` effect ran; the driver queues the message next.
    #[serde(rename = "sent")]
    Sent {
        /// The message's id in this run.
        message: u64,
        /// The receiver's run id.
        to: String,
        /// The event.
        value: Value,
    },
    /// An `emit` effect ran; the driver queues the event for its receivers next.
    #[serde(rename = "emitted")]
    Emitted {
        /// The message's id in this run.
        message: u64,
        /// The event.
        value: Value,
    },
    /// A `send` that reached no run: written by the core with `to` `null` when the child it
    /// is addressed to is not running, and by the driver after `sent` when the receiver has
    /// ended.
    #[serde(rename = "undelivered")]
    Undelivered {
        /// The message's id in the sending run.
        message: u64,
        /// The receiver's run id, or `null` for a child that is not running.
        to: Option<String>,
        /// The event.
        value: Value,
    },
    /// A task or child machine was started, or a call was made.
    #[serde(rename = "invoke.started")]
    InvokeStarted {
        /// The invoke's id.
        invoke: u64,
        /// The id of the state that owns it.
        state: String,
        /// The task's or machine's name, or for a call `inventory.reserve`.
        src: String,
        /// Its arguments.
        args: Object,
    },
    /// A `reply` effect ran, or a call that enabled no transition was answered `Unanswered`;
    /// the driver answers the caller next.
    #[serde(rename = "replied")]
    Replied {
        /// The caller's run id.
        to: String,
        /// The id of the caller's invoke.
        invoke: u64,
        /// The call's name.
        call: String,
        /// Done or fail.
        outcome: ReplyOutcome,
        /// The result (`null` for a call that returns nothing) or the error.
        value: Value,
    },
    /// A running invoke was cancelled because its state was left.
    #[serde(rename = "invoke.cancelled")]
    InvokeCancelled {
        /// The invoke's id.
        invoke: u64,
        /// The id of the state that owned it.
        state: String,
        /// The task's or machine's name.
        src: String,
    },
    /// A spawned child was started.
    #[serde(rename = "spawn.started")]
    SpawnStarted {
        /// Its id.
        spawn: u64,
        /// Its collection.
        children: String,
        /// Its key.
        key: Value,
        /// Its machine.
        src: String,
        /// Its arguments.
        args: Object,
    },
    /// A running spawned child was stopped.
    #[serde(rename = "spawn.stopped")]
    SpawnStopped {
        /// Its id.
        spawn: u64,
        /// Its collection.
        children: String,
        /// Its key.
        key: Value,
    },
    /// An `after` timer was started.
    #[serde(rename = "timer.started")]
    TimerStarted {
        /// The timer's id.
        timer: u64,
        /// The id of the state that owns it.
        state: String,
        /// The index of its block's first transition.
        index: usize,
        /// The evaluated delay, in milliseconds.
        delay: u64,
    },
    /// A running timer was cancelled because its state was left.
    #[serde(rename = "timer.cancelled")]
    TimerCancelled {
        /// The timer's id.
        timer: u64,
        /// The id of the state that owned it.
        state: String,
        /// The index of its block's first transition.
        index: usize,
    },
    /// The run reached a top-level final state with an output.
    #[serde(rename = "done")]
    Done {
        /// The final state's id.
        state: String,
        /// The output, `null` for a machine that returns nothing.
        output: Value,
    },
    /// The run reached a top-level final state with `fail`.
    #[serde(rename = "failed")]
    Failed {
        /// The final state's id.
        state: String,
        /// The error.
        error: Value,
    },
    /// The run was cancelled from outside.
    #[serde(rename = "cancelled")]
    Cancelled,
    /// The run ended with a defect.
    #[serde(rename = "defect")]
    Defect {
        /// The defect.
        defect: Defect,
    },
    /// Written by the driver, last, when it stopped an idle run it had saved. The run has not
    /// ended: it goes on, as another run, from its saved snapshot.
    #[serde(rename = "suspended")]
    Suspended,
}

impl Record {
    /// The record as a JSON object, without `t`, `run` and `parent`.
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

/// A running `after` timer in a saved snapshot: a [`TimerEntry`] with its due time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedTimer {
    /// Its id within the run.
    pub id: u64,
    /// The id of the state that owns it.
    pub state: String,
    /// The index in the state's `transitions` of its `after` block's first transition.
    pub index: usize,
    /// The evaluated delay, in milliseconds.
    pub delay: u64,
    /// When it expires, on the driver's clock, in integer milliseconds.
    pub due: u64,
}

/// A running invoke in a saved snapshot: an [`InvokeEntry`] with, when its work is a child run
/// that has claimed its invoke and is still open, that child's saved snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedInvoke {
    /// Its id within the run.
    pub id: u64,
    /// The id of the state that owns it.
    pub state: String,
    /// The task's or machine's name, or for a call the address and the call's name.
    pub src: String,
    /// The saved snapshot of its child run (version 4), if it is one that is saved with it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child: Option<Box<Saved>>,
}

/// A running spawned child in a saved snapshot: a [`SpawnedEntry`] with, when it is a child run
/// that is still open, its saved snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedSpawned {
    /// Its id within the run, from the sequence invokes take theirs from.
    pub id: u64,
    /// Its collection.
    pub children: String,
    /// Its key: a string or an integer.
    pub key: Value,
    /// The arguments it was spawned with, keyed by parameter name.
    pub args: Object,
    /// The saved snapshot of its child run (version 4), if it is one that is saved with it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child: Option<Box<Saved>>,
}

/// A saved snapshot of a top-level run, the form host code stores
/// (`spec/execution.md#the-saved-snapshot`, `spec/snapshot.schema.json`): the core's
/// [`Snapshot`] with the format version and each timer's due time, and the saved snapshots of
/// its running child runs nested in its `invokes` and `spawned` entries (`child`), to any
/// depth. Both runtimes save the same run as the same JSON, and either restores what the other
/// saved.
///
/// Read one with [`Saved::from_json`] or [`Saved::from_value`], which check it against the
/// schema; restoring checks the rest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Saved {
    /// The version of the format: `4`, or `3`, `2` or `1` for one written before child runs
    /// were saved, before keyed spawn or before history states. A nested snapshot is always
    /// version 4.
    pub snapshot_version: u64,
    /// The machine's name.
    pub machine: String,
    /// The id the run had when it was saved. A restored run gets a new one.
    pub run: String,
    /// The machine's hash in the IR it ran with.
    pub ir: String,
    /// The run's status.
    pub status: Status,
    /// The active states' ids, in document order.
    pub active: Vec<String>,
    /// The arguments, keyed by parameter name.
    pub input: Object,
    /// Every variable that has a value.
    pub vars: Object,
    /// The running invokes, in the order they were started.
    pub invokes: Vec<SavedInvoke>,
    /// The calls the run had taken and not answered, in the order it took them. Written
    /// only when there are some; a snapshot without the member has none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<CallEntry>,
    /// The running spawned children, in the order they were started. `None` only in a
    /// version 1 or 2 snapshot, which has no `spawned` member and restores with no children.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawned: Option<Vec<SavedSpawned>>,
    /// The running timers, in the order they were started, with their due times.
    pub timers: Vec<SavedTimer>,
    /// The id the next invoke will get.
    pub next_invoke: u64,
    /// The id the next timer will get.
    pub next_timer: u64,
    /// The id the next `send` or `emit` effect will get.
    pub next_message: u64,
    /// What each history state recorded. `None` only in a version 1 snapshot, which has no
    /// `history` member and restores as if it were empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history: Option<History>,
    /// `null` while running and when cancelled.
    pub result: Option<RunResult>,
}

impl Saved {
    /// The version of the format this runtime writes. It reads this one and versions 1 to 3.
    pub const VERSION: u64 = 4;

    /// `snap` saved, with each timer due at `due(id)`, and no child run nested in it.
    pub fn new(snap: &Snapshot, mut due: impl FnMut(&TimerEntry) -> u64) -> Saved {
        let mut saved = Saved {
            snapshot_version: Saved::VERSION,
            machine: snap.machine.clone(),
            run: snap.run.clone(),
            ir: snap.ir.clone(),
            status: snap.status,
            active: snap.active.clone(),
            input: snap.input.clone(),
            vars: snap.vars.clone(),
            invokes: snap
                .invokes
                .iter()
                .map(|i| SavedInvoke {
                    id: i.id,
                    state: i.state.clone(),
                    src: i.src.clone(),
                    child: None,
                })
                .collect(),
            calls: snap.calls.clone(),
            spawned: Some(
                snap.spawned
                    .iter()
                    .map(|c| SavedSpawned {
                        id: c.id,
                        children: c.children.clone(),
                        key: c.key.clone(),
                        args: c.args.clone(),
                        child: None,
                    })
                    .collect(),
            ),
            timers: snap
                .timers
                .iter()
                .map(|t| SavedTimer {
                    id: t.id,
                    state: t.state.clone(),
                    index: t.index,
                    delay: t.delay,
                    due: due(t),
                })
                .collect(),
            next_invoke: snap.next_invoke,
            next_timer: snap.next_timer,
            next_message: snap.next_message,
            history: Some(snap.history.clone()),
            result: snap.result.clone(),
        };
        // Integers in integer form: a float that holds one (`2.0`, `-0.0`) is saved as it.
        crate::saved::integral_numbers_in(&mut saved);
        saved
    }

    /// The core's snapshot: without the version, the due times and the nested child
    /// snapshots, as run `run`, with an empty `history` for a version 1 snapshot and no spawned
    /// children for a version 1 or 2 one.
    pub fn to_snapshot(&self, run: &str) -> Snapshot {
        Snapshot {
            machine: self.machine.clone(),
            run: run.to_owned(),
            ir: self.ir.clone(),
            status: self.status,
            active: self.active.clone(),
            input: self.input.clone(),
            vars: self.vars.clone(),
            invokes: self
                .invokes
                .iter()
                .map(|i| InvokeEntry {
                    id: i.id,
                    state: i.state.clone(),
                    src: i.src.clone(),
                })
                .collect(),
            calls: self.calls.clone(),
            spawned: self
                .spawned
                .iter()
                .flatten()
                .map(|c| SpawnedEntry {
                    id: c.id,
                    children: c.children.clone(),
                    key: c.key.clone(),
                    args: c.args.clone(),
                })
                .collect(),
            timers: self
                .timers
                .iter()
                .map(|t| TimerEntry {
                    id: t.id,
                    state: t.state.clone(),
                    index: t.index,
                    delay: t.delay,
                })
                .collect(),
            next_invoke: self.next_invoke,
            next_timer: self.next_timer,
            next_message: self.next_message,
            history: self.history.clone().unwrap_or_default(),
            result: self.result.clone(),
        }
    }

    /// The snapshot without the child runs' snapshots nested in it: what a `restored` record
    /// shows.
    pub fn without_children(&self) -> Saved {
        let mut shown = self.clone();
        for inv in &mut shown.invokes {
            inv.child = None;
        }
        for ch in shown.spawned.iter_mut().flatten() {
            ch.child = None;
        }
        shown
    }

    /// Whether a child run's snapshot is nested in it.
    pub fn has_children(&self) -> bool {
        self.invokes.iter().any(|i| i.child.is_some())
            || self.spawned.iter().flatten().any(|c| c.child.is_some())
    }

    /// The snapshot as JSON.
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    /// The snapshot in canonical JSON (RFC 8785), the form conformance compares.
    pub fn to_canonical(&self) -> String {
        roadbook_ir::jcs::to_canonical_string(&self.to_value())
    }

    /// Reads a saved snapshot from JSON, checking it against `spec/snapshot.schema.json`: a
    /// document that does not match is refused as `invalid`.
    ///
    /// A number's form does not matter: an integral one written as a float (`1.0`, `-0.0`) is
    /// read as the integer. A snapshot of a run that has ended whose result this runtime cannot
    /// hold (a defect of a kind it does not know, which the schema allows) is refused as
    /// `ended`, the reason restoring it would give.
    pub fn from_value(mut value: Value) -> Result<Saved, crate::RestoreError> {
        crate::saved::integral_numbers(&mut value);
        crate::saved::schema(&value).map_err(crate::RestoreError::invalid)?;
        let status = value["status"].clone();
        serde_json::from_value(value).map_err(|e| {
            if status == "running" {
                crate::RestoreError::invalid(e.to_string())
            } else {
                crate::RestoreError::new(
                    crate::Refusal::Ended,
                    format!(
                        "the run has ended ({status}), with a result this runtime cannot read: {e}"
                    ),
                )
            }
        })
    }

    /// Reads a saved snapshot from JSON text ([`Saved::from_value`]).
    pub fn from_json(json: &str) -> Result<Saved, crate::RestoreError> {
        let value = serde_json::from_str(json)
            .map_err(|e| crate::RestoreError::invalid(format!("not JSON: {e}")))?;
        Saved::from_value(value)
    }

    /// The earliest due time of its timers: when a host should wake the saved run.
    pub fn next_due(&self) -> Option<u64> {
        self.timers.iter().map(|t| t.due).min()
    }
}
