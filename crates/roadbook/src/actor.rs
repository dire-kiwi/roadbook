//! `spawn`: a long-lived run that takes events, shows its state and can be cancelled.

use std::fmt;
use std::future::poll_fn;
use std::sync::Arc;
use std::task::Poll;

use roadbook_core::{Input, Program};
use serde_json::Value;

use crate::driver::{Driver, RunEnd};
use crate::dynamic::Host;
use crate::env::Config;
use crate::shared::Shared;

/// How a spawned run ended without an output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stopped<E> {
    /// It reached a final state with `fail`.
    Failed(E),
    /// It was cancelled from outside ([`Actor::cancel`]).
    Cancelled,
}

impl<E: fmt::Display> fmt::Display for Stopped<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Stopped::Failed(e) => write!(f, "the machine failed: {e}"),
            Stopped::Cancelled => f.write_str("the machine was cancelled"),
        }
    }
}

impl<E: fmt::Debug + fmt::Display> std::error::Error for Stopped<E> {}

/// The conversions between a machine's Rust types and the driver's values, which the
/// generated `spawn` supplies.
#[doc(hidden)]
pub struct Codec<Event, State, Output, Error> {
    pub event: fn(&Event) -> Value,
    pub state: fn(&str) -> Option<State>,
    pub output: fn(Value) -> Output,
    pub error: fn(Value) -> Error,
}

/// A running machine instance that takes events, made by a generated `spawn`.
///
/// It runs on its own Tokio task. Dropping the actor stops it at once, like dropping the
/// future of `run`: no exit effects run and the trace has no final record. [`Actor::cancel`]
/// stops it gracefully instead.
pub struct Actor<Event, State, Output, Error> {
    shared: Arc<Shared>,
    #[cfg(feature = "tokio")]
    task: Option<tokio::task::JoinHandle<RunEnd>>,
    codec: Codec<Event, State, Output, Error>,
    initial: State,
    seen: u64,
}

impl<Event, State, Output, Error> fmt::Debug for Actor<Event, State, Output, Error> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Actor").finish_non_exhaustive()
    }
}

impl<Event, State: Clone, Output, Error> Actor<Event, State, Output, Error> {
    /// Queues an event. Events are taken one at a time, in the order sent. An event sent
    /// after the run ended is ignored.
    pub async fn send(&self, event: Event) {
        self.shared.push([Input::Event {
            value: (self.codec.event)(&event),
        }]);
    }

    /// The state the run is in: its innermost active state, or the final state it ended in.
    pub fn state(&self) -> State {
        let (_, leaf) = self.shared.state();
        self.decode_state(leaf)
    }

    fn decode_state(&self, leaf: Option<String>) -> State {
        leaf.and_then(|l| (self.codec.state)(&l))
            .unwrap_or_else(|| self.initial.clone())
    }

    /// Waits until the state changes, and returns the new state. Once the run has ended it
    /// returns the last state at once.
    pub async fn changed(&mut self) -> State {
        let (version, leaf) = poll_fn(|cx| match self.shared.poll_state(self.seen, cx.waker()) {
            Some(s) => Poll::Ready(s),
            None => Poll::Pending,
        })
        .await;
        self.seen = version;
        self.decode_state(leaf)
    }

    /// Cancels the run from outside and returns once its cancel step has finished: its
    /// exit effects have run and its trace ends with `cancelled` (or, if an exit action had
    /// a host defect, with `defect`). [`Actor::result`] then reports how it ended.
    pub async fn cancel(&self) {
        self.shared.request_cancel();
        poll_fn(|cx| match self.shared.poll_finished(cx.waker()) {
            Some(_) => Poll::Ready(()),
            None => Poll::Pending,
        })
        .await;
    }

    /// Waits for the run to end. A run that ended with a defect resumes unwinding with its
    /// [`crate::Defect`] here.
    pub async fn result(mut self) -> Result<Output, Stopped<Error>> {
        let end = self.join().await;
        match end {
            RunEnd::Done(v) => Ok((self.codec.output)(v)),
            RunEnd::Failed(e) => Err(Stopped::Failed((self.codec.error)(e))),
            RunEnd::Cancelled => Err(Stopped::Cancelled),
            RunEnd::Defect(d) => std::panic::resume_unwind(Box::new(d)),
        }
    }

    #[cfg(feature = "tokio")]
    async fn join(&mut self) -> RunEnd {
        let task = self.task.take().expect("result is called once");
        match task.await {
            Ok(end) => end,
            Err(e) => match e.try_into_panic() {
                Ok(p) => std::panic::resume_unwind(p),
                Err(e) => panic!("the machine's task was stopped: {e}"),
            },
        }
    }

    #[cfg(not(feature = "tokio"))]
    async fn join(&mut self) -> RunEnd {
        unreachable!("an actor is only made with the `tokio` feature")
    }
}

#[cfg(feature = "tokio")]
impl<Event, State, Output, Error> Drop for Actor<Event, State, Output, Error> {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// The ids of the states entered when `machine` starts, down to its first leaf.
fn initial_leaf(program: &Program, machine: &str) -> Option<String> {
    let m = program.ir().machines.get(machine)?;
    let mut s = &m.root;
    while let Some(init) = &s.initial {
        s = s.states.iter().find(|c| &c.id == init)?;
    }
    Some(s.id.clone())
}

/// Starts `machine` as a root run on a new Tokio task and gives its actor.
#[doc(hidden)]
#[cfg_attr(not(feature = "tokio"), allow(unused_variables))]
pub fn spawn<Event, State, Output, Error>(
    config: Config,
    program: &'static Program,
    machine: &'static str,
    input: serde_json::Map<String, Value>,
    host: Box<dyn Host + 'static>,
    codec: Codec<Event, State, Output, Error>,
) -> Actor<Event, State, Output, Error> {
    let shared = Shared::claimed();
    let leaf = initial_leaf(program, machine).expect("the program has the machine");
    // Starting is not a change of state.
    shared.set_state(&leaf);
    let seen = shared.state().0;
    let initial = (codec.state)(&leaf).expect("a machine's initial state has a State variant");
    #[cfg(feature = "tokio")]
    {
        let run_shared = shared.clone();
        let task = tokio::spawn(async move {
            let host = host;
            Driver::root(program, machine, input, &*host, &config, run_shared)
                .drive()
                .await
        });
        Actor {
            shared,
            task: Some(task),
            codec,
            initial,
            seen,
        }
    }
    #[cfg(not(feature = "tokio"))]
    panic!("`spawn` needs the `tokio` feature of `roadbook`")
}
