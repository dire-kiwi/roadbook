//! The Roadbook interpreter: the pure core of `spec/execution.md`.
//!
//! Two synchronous functions do all the work, with no I/O, no clock and no async:
//!
//! - [`Program::start`] starts a machine and returns its first [`Snapshot`] and the
//!   [`Command`]s to carry out;
//! - [`Program::step`] takes one [`Input`] and returns the next snapshot and commands.
//!
//! A snapshot is never modified in place. Host functions are called through the `fns`
//! callback ([`Fns`]), which returns a value or a [`Defect`]; the binding layer catches
//! panics, so nothing unwinds through the core. Trace records are commands too
//! ([`Command::Trace`]), so they interleave with the work exactly as the spec says. The
//! driver, the part with queues, futures and timers, is the `roadbook` crate.
//!
//! Values are `serde_json::Value`s in the JSON form of `spec/execution.md#values`. `int` is
//! `i64`, and arithmetic outside it is an `overflow` defect.
//!
//! Messages between runs are commands and inputs like the rest: a `send` or `emit` effect
//! gives the driver a [`Command::Send`] or [`Command::Emit`], and what other runs send and
//! emit comes back as [`Input::Message`] and [`Input::Emitted`]. A peer's end, which the
//! driver tells the runs that hold a `ref` to it and have an `on name.ended` for it, is an
//! [`Input::Ended`]. A `ref` is the id of the run it refers to; delivering is the driver's
//! business.
//!
//! Calls between runs are commands and inputs too: a call invoke's [`Command::InvokeStart`]
//! has [`SrcKind::Call`] and the callee's run id, the callee takes the call as an
//! [`Input::Call`], keeps it in [`Snapshot::calls`] while it is pending, and a `reply` gives
//! the driver a [`Command::Reply`], whose answer reaches the caller as the outcome of its
//! invoke.
//!
//! Parallel states take one input in several regions at once, in one microstep. A run can be
//! saved as a [`Saved`] snapshot and started again from it: [`Program::check_saved`] checks a
//! saved snapshot against the IR (and applies a migration), and [`Program::restore`] starts
//! the new run from it.
//!
//! ```
//! use roadbook_core::{Command, Input, Program, Record, Status};
//! use serde_json::{Map, Value, json};
//!
//! let ir = roadbook_compiler::compile_source(
//!     "tick.road",
//!     "machine Tick() -> int {\n  event Go\n  initial A\n  state A { on Go -> B }\n  final state B { output 1 }\n}\n",
//!     &Default::default(),
//! )
//! .ir
//! .unwrap();
//! let program = Program::new(ir).unwrap();
//! let mut no_fns = |_: &str, _: Vec<Value>| unreachable!();
//! let (snap, _) = program.start("Tick", "r1", Map::new(), &mut no_fns).unwrap();
//! assert_eq!(snap.active, ["Tick", "Tick.A"]);
//! let go = Input::Event { value: json!({"_tag": "Go"}) };
//! let (snap, commands) = program.step(&snap, go, &mut no_fns);
//! assert_eq!(snap.status, Status::Done);
//! assert!(matches!(commands.last(), Some(Command::Trace { record: Record::Done { .. } })));
//! ```

mod eval;
mod program;
mod redact;
mod run;
mod saved;
mod types;

pub use eval::Fns;
pub use program::{Program, ProgramError};
pub use redact::REDACTED;
pub use run::{StepResult, UnknownMachine};
pub use saved::{Migration, RUN_ID_PATTERN, Refusal, RestoreError, is_run_id};
pub use types::{
    CallEntry, Command, Defect, Ended, History, Input, InvokeEntry, Object, Record, ReplyOutcome,
    RunResult, Saved, SavedTimer, Snapshot, SpawnedEntry, SrcKind, Status, TimerEntry,
};

/// The most microsteps settling may take for one input before it is a `limit` defect.
pub const SETTLE_LIMIT: usize = 1000;
