//! The core's data: snapshots, inputs, commands, trace records and defects, each serialising
//! to exactly the JSON form `spec/execution.md` and `spec/trace.md` give.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A JSON object: arguments keyed by parameter name, variables keyed by name.
pub type Object = Map<String, Value>;

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

/// Why a run ended with a defect. In JSON, `{"kind": "host", "message": …}` and so on.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
        }
    }
}

impl std::error::Error for Defect {}

/// A run's result, once it has one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// The task's or machine's name.
    pub src: String,
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
    /// The running timers, in the order they were started.
    pub timers: Vec<TimerEntry>,
    /// The id the next invoke will get.
    pub next_invoke: u64,
    /// The id the next timer will get.
    pub next_timer: u64,
    /// `null` while running and when cancelled.
    pub result: Option<RunResult>,
}

/// What the driver gives the core.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum Input {
    /// An external event, a tagged record: `{"_tag": "Cancel", …}`.
    #[serde(rename = "event")]
    Event {
        /// The event.
        value: Value,
    },
    /// An invoke succeeded.
    #[serde(rename = "invoke.done")]
    InvokeDone {
        /// The invoke's id.
        invoke: u64,
        /// Its result, `null` for a task that returns nothing.
        value: Value,
    },
    /// An invoke failed with an error, a tagged record.
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

    /// The id of the timer whose expiry this is, if it is one.
    pub fn timer_id(&self) -> Option<u64> {
        match self {
            Input::After { timer } => Some(*timer),
            _ => None,
        }
    }
}

/// Whether an invoke runs a task or a machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SrcKind {
    /// A host task.
    Task,
    /// A machine.
    Machine,
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
    /// Start the task or machine; queue its outcome as an `invoke.*` input when it ends.
    #[serde(rename = "invoke.start")]
    InvokeStart {
        /// The invoke's id.
        invoke: u64,
        /// The id of the state that owns it.
        state: String,
        /// The task's or machine's name.
        src: String,
        /// Task or machine.
        #[serde(rename = "srcKind")]
        src_kind: SrcKind,
        /// Its arguments, keyed by parameter name.
        args: Object,
    },
    /// Cancel the invoke and wait until it has stopped.
    #[serde(rename = "invoke.cancel")]
    InvokeCancel {
        /// The invoke's id.
        invoke: u64,
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
        /// The target's id, `null` for an internal transition.
        to: Option<String>,
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
    /// A task or child machine was started.
    #[serde(rename = "invoke.started")]
    InvokeStarted {
        /// The invoke's id.
        invoke: u64,
        /// The id of the state that owns it.
        state: String,
        /// The task's or machine's name.
        src: String,
        /// Its arguments.
        args: Object,
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
}

impl Record {
    /// The record as a JSON object, without `t`, `run` and `parent`.
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}
