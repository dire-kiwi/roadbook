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
//! - Messages between runs go through the runs' queues: a run is open until its core gives a
//!   snapshot that is not `running`, a `send` to a closed run is `undelivered`, and a run is
//!   subscribed to what its peers emit while it is open.
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
pub use roadbook_core::{Defect, Input, Program, Record, Refusal, RestoreError, Saved, SavedTimer};
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
