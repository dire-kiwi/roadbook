//! Roadbook statecharts in Rust.
//!
//! `roadbook::include!("path/to/chart.road")` compiles a chart at build time and expands to
//! its types, a module per machine with a `Handlers` trait, `State` and `Event` enums, and
//! `run` and `spawn` functions. This crate is the driver behind them: it interprets the
//! embedded IR with [`roadbook_core`], carries out the core's commands with the host's
//! concurrency, and writes the trace. The design is in `docs/design/rust.md`, the rules in
//! `spec/execution.md`.
//!
//! - A run is one future that owns everything it started, with one FIFO that every input is
//!   pushed onto. The driver never spawns tasks, so `run` borrows the handlers and works on
//!   any executor; only `after` timers need a runtime, through the [`Timer`] trait
//!   ([`TokioTimer`] by default).
//! - A machine invoke's future is polled inside an ambient invoke context. A `run` that
//!   finds one becomes the parent's child run, writing to the same [`TraceSink`] on the same
//!   timer, and the parent cancels it gracefully, waiting for its exit effects.
//! - Panics in host code become defects; a run that ends with a defect resumes unwinding
//!   with a [`Defect`] payload.
//! - [`Actor`] (from a generated `spawn`) takes events, shows its state, and can be
//!   cancelled gracefully. Its [`Actor::handle`], a [`Ref`], is how host code and other
//!   machines reach the run: a `ref` argument, and a way to send it events.
//! - Host code receives what a spawned run emits through [`Actor::subscribe`], a
//!   [`Subscription`] of the machine's `Emitted` enum: every event from then on, unbounded,
//!   until the run ends. Tools subscribe to any run, as JSON, with [`RunHandle::subscribe`].
//! - Messages between runs go through the runs' queues: a run is open until its core gives a
//!   snapshot that is not `running`, a `send` to a closed run is `undelivered`, and a run is
//!   subscribed to what its peers emit while it is open.
//! - A peer's end (`on inventory.ended`) is an [`Input::Ended`] with an [`Ended`] status. When
//!   a run closes, however it closes (a hard stop is `Cancelled`, suspending `Suspended`), its
//!   driver pushes one onto the queue of each subscriber whose machine has an `on name.ended`
//!   for it, after its last step's commands and its `Unanswered` answers. A run that
//!   subscribes to a peer that has already closed is told by its own driver, right after its
//!   start step. Either way, once per peer.
//! - A call (`invoke inventory.reserve(…)`) is made by the driver itself, with no `Handlers`
//!   method: it pushes the call onto the peer's queue with a way to answer it, and the
//!   callee's `reply` pushes the answer onto the caller's queue as the invoke's outcome. A
//!   call to a closed peer, and every call a run has not answered when it ends (or is
//!   dropped), is answered `Unanswered`.
//! - A top-level run can be saved ([`Saved`], `actor.save()`), suspended once idle
//!   (`actor.suspend()`), and restored from a saved snapshot as a new run (a generated
//!   `restore` or `resume`, [`dynamic::restore`], [`dynamic::resume`]); a snapshot that does
//!   not fit is refused with a [`RestoreError`].
//! - [`dynamic`] is the erased interface the generated code adapts to, for hosts that call
//!   host code by name.

mod actor;
mod defect;
mod driver;
pub mod dynamic;
mod env;
mod handle;
mod listen;
mod shared;

#[doc(hidden)]
#[path = "private.rs"]
pub mod __private;

pub use actor::{Actor, Stopped};
pub use driver::RunEnd;
#[cfg(feature = "tokio")]
pub use env::TokioTimer;
pub use env::{
    BoxFuture, Config, JsonLines, MemorySink, Migrate, NoTrace, RunObserver, Timer, TraceRecord,
    TraceSink,
};
pub use handle::Ref;
pub use listen::Subscription;
pub use roadbook_core::{
    Defect, Ended, Input, InvokeEntry, Program, RUN_ID_PATTERN, Record, Refusal, RestoreError,
    RunResult, Saved, SavedTimer, Status, is_run_id,
};
pub use roadbook_macros::include;
pub use shared::RunHandle;

#[cfg(test)]
mod send_check {
    use super::*;

    fn assert_send<T: Send>(_: &T) {}

    #[test]
    fn run_futures_are_send() {
        struct Nothing;
        impl dynamic::Host for Nothing {
            fn function(
                &self,
                name: &str,
                _: Vec<serde_json::Value>,
            ) -> Result<serde_json::Value, Defect> {
                Err(Defect::host(name))
            }
            fn action<'a>(
                &'a self,
                _: &str,
                _: roadbook_core::Object,
            ) -> BoxFuture<'a, Result<(), Defect>> {
                Box::pin(async { Ok(()) })
            }
            fn invoke<'a>(&'a self, _: dynamic::Invocation) -> BoxFuture<'a, dynamic::Outcome> {
                Box::pin(std::future::pending())
            }
        }
        let ir = roadbook_ir::Ir {
            ir_version: roadbook_ir::IR_VERSION.into(),
            sources: Vec::new(),
            types: Default::default(),
            functions: Default::default(),
            actions: Default::default(),
            tasks: Default::default(),
            machines: Default::default(),
        };
        let program = Program::new(ir).unwrap();
        let f = dynamic::run(&program, "X", Default::default(), &[], &Nothing);
        assert_send(&f);
    }
}
