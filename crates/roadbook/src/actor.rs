//! `spawn`: a long-lived run that takes events, shows its state and can be cancelled.

use std::fmt;
use std::future::poll_fn;
use std::panic::resume_unwind;
use std::sync::Arc;
use std::task::Poll;

use roadbook_core::{Defect, Input, Object, Program, RestoreError, Saved};
use serde_json::Value;

use crate::driver::{Driver, RunEnd};
use crate::dynamic::Host;
use crate::env::Config;
use crate::handle::Ref;
use crate::listen::Subscription;
use crate::shared::{RunHandle, Shared};

/// How a spawned run ended without an output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stopped<E> {
    /// It reached a final state with `fail`.
    Failed(E),
    /// It was cancelled from outside ([`Actor::cancel`]).
    Cancelled,
    /// It was suspended through its [`crate::RunHandle`] (from [`Config::on_run`]) once idle,
    /// giving its saved snapshot. It has not ended: it goes on from the snapshot when that is
    /// restored.
    Suspended(Box<Saved>),
}

impl<E: fmt::Display> fmt::Display for Stopped<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Stopped::Failed(e) => write!(f, "the machine failed: {e}"),
            Stopped::Cancelled => f.write_str("the machine was cancelled"),
            Stopped::Suspended(_) => f.write_str("the machine was suspended"),
        }
    }
}

impl<E: fmt::Debug + fmt::Display> std::error::Error for Stopped<E> {}

/// The conversions between a machine's Rust types and the driver's values, which the
/// generated `spawn` supplies.
#[doc(hidden)]
pub struct Codec<Event, State, Output, Error, Emitted, Tag> {
    pub event: fn(&Event) -> Result<Value, Defect>,
    pub state: fn(&str) -> Option<State>,
    pub tag: fn(&str) -> Option<Tag>,
    pub output: fn(Value) -> Output,
    pub error: fn(Value) -> Error,
    pub emitted: fn(Value) -> Emitted,
}

/// A running machine instance that takes events, made by a generated `spawn`.
///
/// It runs on its own Tokio task. Dropping the actor stops it at once, like dropping the
/// future of `run`: no exit effects run and the trace has no final record. So does dropping
/// the future of [`Actor::result`] before it is ready, since it owns the actor.
/// [`Actor::cancel`] stops it gracefully instead.
pub struct Actor<Event, State, Output, Error, Emitted, Tag> {
    shared: Arc<Shared>,
    id: String,
    program: &'static Program,
    machine: &'static str,
    #[cfg(feature = "tokio")]
    task: Option<tokio::task::JoinHandle<RunEnd>>,
    codec: Codec<Event, State, Output, Error, Emitted, Tag>,
    initial: State,
    seen: u64,
}

impl<Event, State, Output, Error, Emitted, Tag> fmt::Debug
    for Actor<Event, State, Output, Error, Emitted, Tag>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Actor").finish_non_exhaustive()
    }
}

impl<Event, State, Output, Error, Emitted, Tag> Actor<Event, State, Output, Error, Emitted, Tag> {
    /// A handle to the run, which host code can send through and pass to other runs as the
    /// chart's `ref Machine` argument. It can be cloned and outlives the actor; once the run
    /// has ended, what is sent through it is ignored (or, from a machine, `undelivered`).
    pub fn handle(&self) -> Ref<Event> {
        Ref::new(RunHandle::new(
            self.shared.clone(),
            &self.id,
            self.program,
            self.machine,
            false,
        ))
    }

    /// The run's id.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Listens to the events the run emits (the machine's `Emitted`), every one of them
    /// whatever any machine hears, from now until the run ends (after its last step's
    /// commands, so a final state's or a cancel step's exit `emit`s are in it) or is
    /// suspended (`spec/execution.md#host-listeners`). What the run emitted before is not in
    /// it, nor is what its start step emits, even if the run's task has not taken that step
    /// yet when this is called. Nothing waits for the subscription, and what it has not
    /// taken yet is kept without bound; dropping it unsubscribes. Listening writes nothing to
    /// the trace.
    pub fn subscribe(&self) -> Subscription<Emitted> {
        Subscription::new(self.shared.listen(true), self.codec.emitted)
    }
}

impl<Event, State: Clone, Output, Error, Emitted, Tag: PartialEq>
    Actor<Event, State, Output, Error, Emitted, Tag>
{
    /// Queues an event. Events are taken one at a time, in the order sent. An event sent
    /// after the run ended is ignored.
    ///
    /// An event that cannot be encoded (one holding a non-finite `float`) is the sender's
    /// host defect, not the run's: nothing is queued, and `send` panics with the
    /// [`crate::Defect`] as its payload (`std::panic::resume_unwind`). Inside a `Handlers`
    /// method that ends the run calling it, as any panic there does.
    pub async fn send(&self, event: Event) {
        let value = (self.codec.event)(&event).unwrap_or_else(|d| resume_unwind(Box::new(d)));
        self.shared.push([Input::Event { value }]);
    }

    /// The state the run is in: its innermost active state, or the final state it ended in.
    pub fn state(&self) -> State {
        let (_, leaf) = self.shared.state();
        self.decode_state(leaf)
    }

    /// The tags of the run's active states, each once, in document order: each state's tags in
    /// the order written, outermost state first (`spec/execution.md#tags`). Like
    /// [`Actor::state`], what the run's last step left; before its first step, the tags of the
    /// states down to the state it starts in. Empty once the run has ended.
    pub fn tags(&self) -> Vec<Tag> {
        self.shared
            .tags()
            .iter()
            .filter_map(|t| (self.codec.tag)(t))
            .collect()
    }

    /// Whether `tag` is one of [`Actor::tags`]: whether some active state has it.
    pub fn has_tag(&self, tag: Tag) -> bool {
        self.tags().contains(&tag)
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

    /// Waits for the run to end, or to be suspended through its [`crate::RunHandle`]
    /// ([`Stopped::Suspended`]). A run that ended with a defect resumes unwinding with its
    /// [`crate::Defect`] here, and so does a result that does not decode as `Output` or
    /// `Error` (a host defect). The future owns the actor: dropping it before it is ready
    /// stops the run, as dropping the actor does.
    pub async fn result(mut self) -> Result<Output, Stopped<Error>> {
        let end = self.join().await;
        match end {
            RunEnd::Done(v) => Ok((self.codec.output)(v)),
            RunEnd::Failed(e) => Err(Stopped::Failed((self.codec.error)(e))),
            RunEnd::Cancelled => Err(Stopped::Cancelled),
            RunEnd::Defect(d) => std::panic::resume_unwind(Box::new(d)),
            // Only host code holding the run's `RunHandle` can suspend it that way.
            RunEnd::Suspended(saved) => Err(Stopped::Suspended(saved)),
        }
    }

    /// The run's saved snapshot (`spec/execution.md#the-saved-snapshot`) as its last step
    /// left it: the run carries on. It waits only until the run has carried out its start
    /// (or restore) step; a run that ended gives its final snapshot.
    pub async fn save(&self) -> Saved {
        let saved = poll_fn(|cx| match self.shared.poll_saved(cx.waker()) {
            Some(s) => Poll::Ready(s),
            None => Poll::Pending,
        })
        .await;
        match saved {
            Some(s) => (*s).clone(),
            None => std::panic::resume_unwind(Box::new(Defect::host(
                "the run stopped before its first step",
            ))),
        }
    }

    /// Waits until the run and its child runs are idle (between steps, nothing queued, no
    /// running task, call or stub, every child run waiting the same way), taking inputs
    /// meanwhile as always, then stops them without running any exit effect, writes
    /// `suspended` to the trace for each, and gives its saved snapshot, with its child runs'
    /// nested in it: the request handler's way to keep a run as data. A run that ends first gives its final snapshot
    /// (`done`, `failed` or `cancelled`) and writes nothing; one that ends with a defect
    /// resumes unwinding with the [`crate::Defect`].
    pub async fn suspend(mut self) -> Saved {
        self.shared.request_suspend();
        let end = self.join().await;
        crate::dynamic::suspended(end, &self.shared)
    }

    /// Waits for the run's task. The handle stays in the actor until the task has ended, so
    /// that dropping this future aborts the task.
    #[cfg(feature = "tokio")]
    async fn join(&mut self) -> RunEnd {
        let task = self.task.as_mut().expect("result is called once");
        let joined = task.await;
        self.task = None;
        match joined {
            Ok(end) => end,
            Err(e) => match e.try_into_panic() {
                Ok(p) => std::panic::resume_unwind(p),
                Err(e) => std::panic::resume_unwind(Box::new(Defect::host(format!(
                    "the machine's task was stopped: {e}"
                )))),
            },
        }
    }

    #[cfg(not(feature = "tokio"))]
    async fn join(&mut self) -> RunEnd {
        unreachable!("an actor is only made with the `tokio` feature")
    }
}

#[cfg(feature = "tokio")]
impl<Event, State, Output, Error, Emitted, Tag> Drop
    for Actor<Event, State, Output, Error, Emitted, Tag>
{
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// Closes and finishes a run when dropped, telling its holders it was cancelled if its driver
/// has not told them how it ended (idempotent). The run's id is the second member.
#[cfg(feature = "tokio")]
struct Closer(Arc<Shared>, String);

#[cfg(feature = "tokio")]
impl Drop for Closer {
    fn drop(&mut self) {
        self.0.close();
        self.0.answer_unanswered();
        self.0.tell_ended(&self.1, roadbook_core::Ended::Cancelled);
        self.0.finish(crate::shared::Finished::Ok);
    }
}

/// The state a run of `machine` is in once started: down its initial chain to a leaf or a
/// parallel state. A run never rests in a choice, which has no `State` variant, so one on the
/// way stands for where its `else` goes, and a history state there (which has recorded
/// nothing yet) for its default or its parent's default entry. The run's first step
/// publishes the state it is really in.
fn initial_leaf(program: &Program, machine: &str) -> Option<String> {
    use roadbook_ir::{State, StateKind};
    /// The state `id` under `s`, with its parent.
    fn find<'a>(s: &'a State, id: &str) -> Option<(&'a State, &'a State)> {
        s.states.iter().find_map(|c| {
            if c.id == id {
                Some((c, s))
            } else {
                find(c, id)
            }
        })
    }
    let m = program.ir().machines.get(machine)?;
    let mut s = &m.root;
    // Each turn goes down or follows an `else`, so a loop of `else`s is the only way to
    // turn more often than this; it ends the walk where it is.
    for _ in 0..64 * (m.root.states.len() + 1) {
        let next = if let Some(init) = &s.initial {
            init
        } else if s.kind == StateKind::Choice {
            s.transitions.last()?.targets.first()?
        } else if s.kind.is_history() {
            let parent = find(&m.root, &s.id)?.1;
            match (s.default.first(), &parent.initial) {
                (Some(d), _) => d,
                (None, Some(init)) => init,
                // A parallel parent: the state to stop at.
                (None, None) => return Some(parent.id.clone()),
            }
        } else {
            break;
        };
        s = find(&m.root, next)?.0;
    }
    Some(s.id.clone())
}

/// Restores `saved` as a root run on a new Tokio task and gives its actor, or refuses it
/// before starting anything ([`crate::dynamic::restore`]).
#[doc(hidden)]
#[cfg_attr(not(feature = "tokio"), allow(unused_variables))]
pub fn spawn_restored<Event, State, Output, Error, Emitted, Tag>(
    config: Config,
    program: &'static Program,
    machine: &'static str,
    saved: &Saved,
    host: Box<dyn Host + 'static>,
    codec: Codec<Event, State, Output, Error, Emitted, Tag>,
) -> Result<Actor<Event, State, Output, Error, Emitted, Tag>, RestoreError> {
    let checked = crate::driver::check(program, Some(machine), saved, &config)?;
    let shared = Shared::claimed();
    let id = config.root_id();
    let leaf = program
        .current_state(machine, &checked.0.active)
        .expect("a running snapshot has active states")
        .to_owned();
    shared.set_state(&leaf);
    let tags = program.tags(machine, &checked.0.active);
    shared.set_tags(tags.into_iter().map(str::to_owned).collect());
    let seen = shared.state().0;
    let initial = (codec.state)(&leaf).expect("a state has a State variant");
    #[cfg(feature = "tokio")]
    {
        let run_shared = shared.clone();
        let run_id = id.clone();
        let guard = Closer(run_shared.clone(), run_id.clone());
        let task = tokio::spawn(async move {
            let _guard = guard;
            let host = host;
            Driver::restored(program, checked, &*host, &config, run_id, run_shared)
                .drive()
                .await
        });
        Ok(Actor {
            shared,
            id,
            program,
            machine,
            task: Some(task),
            codec,
            initial,
            seen,
        })
    }
    #[cfg(not(feature = "tokio"))]
    panic!("`restore` needs the `tokio` feature of `roadbook`")
}

/// Starts `machine` as a root run on a new Tokio task and gives its actor. An input that
/// could not be encoded gives an actor whose run ended with that defect before it started.
/// `refs` are the handles of the runs the input's `ref`s name. The run is open, and
/// subscribed to what those runs emit, before this returns, so an event a peer emits after
/// it reaches the run even if its task has not started yet.
#[doc(hidden)]
#[cfg_attr(not(feature = "tokio"), allow(unused_variables))]
pub fn spawn<Event, State, Output, Error, Emitted, Tag>(
    config: Config,
    program: &'static Program,
    machine: &'static str,
    input: Result<Object, Defect>,
    refs: Vec<RunHandle>,
    host: Box<dyn Host + 'static>,
    codec: Codec<Event, State, Output, Error, Emitted, Tag>,
) -> Actor<Event, State, Output, Error, Emitted, Tag> {
    let shared = Shared::claimed();
    let id = config.root_id();
    let leaf = initial_leaf(program, machine).expect("the program has the machine");
    // Starting is not a change of state.
    shared.set_state(&leaf);
    // The states down to it: what its first step will have entered, in a run that starts as
    // it seems to.
    let path: Vec<String> = leaf
        .match_indices('.')
        .map(|(i, _)| leaf[..i].to_owned())
        .chain([leaf.clone()])
        .collect();
    shared.set_tags(
        program
            .tags(machine, &path)
            .into_iter()
            .map(str::to_owned)
            .collect(),
    );
    let seen = shared.state().0;
    let initial = (codec.state)(&leaf).expect("a machine's initial state has a State variant");
    #[cfg(feature = "tokio")]
    {
        let run_shared = shared.clone();
        let run_id = id.clone();
        let task = match input {
            Ok(input) => {
                let peers = crate::driver::open(program, machine, &input, &refs, &run_shared);
                // Closes the run if the task is stopped before its driver exists.
                let guard = Closer(run_shared.clone(), run_id.clone());
                tokio::spawn(async move {
                    let _guard = guard;
                    let host = host;
                    Driver::opened(
                        program, machine, input, peers, &*host, &config, run_id, run_shared,
                    )
                    .drive()
                    .await
                })
            }
            Err(defect) => {
                run_shared.set_saved(crate::driver::early_defect(
                    program,
                    machine,
                    &id,
                    Object::new(),
                    defect.clone(),
                ));
                run_shared.close();
                run_shared.tell_ended(&id, roadbook_core::Ended::Defect);
                run_shared.finish(crate::shared::Finished::Ok);
                tokio::spawn(async move { RunEnd::Defect(defect) })
            }
        };
        Actor {
            shared,
            id,
            program,
            machine,
            task: Some(task),
            codec,
            initial,
            seen,
        }
    }
    #[cfg(not(feature = "tokio"))]
    panic!("`spawn` needs the `tokio` feature of `roadbook`")
}
