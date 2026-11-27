//! What a run shares with the outside: its inbox, its cancel flag, whether it is open, the
//! runs subscribed to what it emits, the calls it waits for and those it was given, how it
//! finished, and the state it is in. For a child run this is also the cancel token of the
//! invoke context.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::Waker;

use roadbook_core::{Defect, Input, Program, ReplyOutcome, Saved, Snapshot};
use serde_json::Value;

use crate::env::Config;
use crate::listen::{Feed, Listeners, Subscription};

/// How a run reported that it finished.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Finished {
    /// It ended, however it ended.
    Ok,
    /// Its cancel step ended with this defect.
    Defect(Defect),
}

#[derive(Default)]
struct Inner {
    claimed: bool,
    inbox: VecDeque<Input>,
    cancel: bool,
    waker: Option<Waker>,
    finished: Option<Finished>,
    finish_wakers: Vec<Waker>,
    state_version: u64,
    state: Option<String>,
    state_wakers: Vec<Waker>,
    /// The run's core has returned a snapshot that is not `running`: nothing more is
    /// delivered to it.
    closed: bool,
    /// The open runs that hold a `ref` to this run, each with the events it emits that they
    /// receive: those their machine has an `on` for.
    subscribers: Vec<(Arc<Shared>, HashSet<String>)>,
    /// Host code listening to what this run emits (`spec/execution.md#host-listeners`),
    /// ended when the run finishes.
    listeners: Listeners,
    /// Host code asked to suspend the run once it is idle.
    suspend: bool,
    /// The run's saved snapshot as its last step left it, once it has one.
    saved: Option<Published>,
    saved_wakers: Vec<Waker>,
    /// The invokes of this run that made a call and still wait for its answer: an answer to
    /// one that is not here (cancelled, or answered already) is not pushed.
    calling: BTreeSet<u64>,
    /// The calls other runs made of this run that it has not answered yet, queued or taken,
    /// in the order they arrived: each caller's run id, its invoke, and the way to answer.
    answers: Vec<(String, u64, Answer)>,
}

/// The way to answer a call (`spec/execution.md#carrying-out-calls`): push an outcome for the
/// caller's invoke onto the caller's queue, while the caller is open and that invoke still
/// waits for it.
pub(crate) struct Answer {
    caller: Arc<Shared>,
    invoke: u64,
}

impl Answer {
    /// Answers the call with `invoke.done` and `value`, or `invoke.fail` and the error
    /// `value`. Pushes nothing if the caller has closed or no longer waits for the invoke
    /// (it cancelled it); the check and the push are one operation.
    pub fn answer(self, outcome: ReplyOutcome, value: Value) {
        let invoke = self.invoke;
        let input = match outcome {
            ReplyOutcome::Done => Input::InvokeDone { invoke, value },
            ReplyOutcome::Fail => Input::InvokeFail {
                invoke,
                error: value,
            },
        };
        let waker = {
            let mut g = self.caller.lock();
            if g.closed || g.finished.is_some() || !g.calling.remove(&invoke) {
                return;
            }
            g.inbox.push_back(input);
            g.waker.take()
        };
        if let Some(w) = waker {
            w.wake();
        }
    }

    /// Answers the call `fail` with `{"_tag": "Unanswered"}`.
    pub fn unanswered(self) {
        self.answer(
            ReplyOutcome::Fail,
            serde_json::json!({ "_tag": roadbook_ir::UNANSWERED }),
        );
    }
}

/// A published saved snapshot. The driver publishes one after every step, and few are ever
/// read, so a step's is built only when it is first read.
enum Published {
    Saved(Arc<Saved>),
    /// The core's snapshot, and each of its timers' due time, in order.
    Step {
        snap: Arc<Snapshot>,
        dues: Vec<u64>,
    },
}

impl Inner {
    /// The published snapshot, built now if it was not yet.
    fn saved(&mut self) -> Option<Arc<Saved>> {
        let saved = match self.saved.take()? {
            Published::Saved(saved) => saved,
            Published::Step { snap, dues } => {
                let mut dues = dues.into_iter();
                Arc::new(Saved::new(&snap, |_| dues.next().unwrap_or(0)))
            }
        };
        self.saved = Some(Published::Saved(saved.clone()));
        Some(saved)
    }

    fn wake_saved(mut g: MutexGuard<'_, Inner>) {
        let wakers = std::mem::take(&mut g.saved_wakers);
        drop(g);
        for w in wakers {
            w.wake();
        }
    }
}

#[derive(Default)]
pub(crate) struct Shared {
    inner: Mutex<Inner>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn claimed() -> Arc<Shared> {
        let s = Shared::default();
        s.lock().claimed = true;
        Arc::new(s)
    }

    /// Claims an unclaimed token. Only the first caller succeeds.
    pub fn try_claim(&self) -> bool {
        let mut g = self.lock();
        !std::mem::replace(&mut g.claimed, true)
    }

    pub fn is_claimed(&self) -> bool {
        self.lock().claimed
    }

    /// Appends `inputs` to the inbox. After the run has closed nobody takes them, so they
    /// are dropped.
    pub fn push(&self, inputs: impl IntoIterator<Item = Input>) {
        let waker = {
            let mut g = self.lock();
            if g.closed || g.finished.is_some() {
                return;
            }
            g.inbox.extend(inputs);
            g.waker.take()
        };
        if let Some(w) = waker {
            w.wake();
        }
    }

    /// Delivers a message or an emitted event from another run: appends it to the inbox and
    /// gives `true` if the run is open, else gives `false` and drops it. The check and the
    /// push are one operation with respect to [`Shared::close`]. An unclaimed token is open:
    /// what is pushed onto it is kept for the run that claims it.
    pub fn deliver(&self, input: Input) -> bool {
        let waker = {
            let mut g = self.lock();
            if g.closed || g.finished.is_some() {
                return false;
            }
            g.inbox.push_back(input);
            g.waker.take()
        };
        if let Some(w) = waker {
            w.wake();
        }
        true
    }

    /// Makes a call with this run's invoke `invoke`: from now until it is answered or
    /// [`Shared::forget_call`], an answer is pushed onto this run's queue.
    pub fn make_call(self: &Arc<Self>, invoke: u64) -> Answer {
        self.lock().calling.insert(invoke);
        Answer {
            caller: self.clone(),
            invoke,
        }
    }

    /// This run cancelled its call invoke `invoke`, or took its outcome: an answer that comes
    /// now is not pushed.
    pub fn forget_call(&self, invoke: u64) {
        self.lock().calling.remove(&invoke);
    }

    /// Delivers the call `input` (an [`Input::Call`]) to this run, keeping `answer` with it
    /// until the run answers it, and gives `Ok` if the run is open; else gives the way to
    /// answer back. The check and the push are one operation with respect to
    /// [`Shared::close`], as for a message.
    pub fn deliver_call(&self, input: Input, answer: Answer) -> Result<(), Answer> {
        let Input::Call { from, invoke, .. } = &input else {
            return Err(answer);
        };
        let key = (from.clone(), *invoke);
        let waker = {
            let mut g = self.lock();
            if g.closed || g.finished.is_some() {
                return Err(answer);
            }
            g.answers.push((key.0, key.1, answer));
            g.inbox.push_back(input);
            g.waker.take()
        };
        if let Some(w) = waker {
            w.wake();
        }
        Ok(())
    }

    /// The way to answer the call that run `from` made with its invoke `invoke`, which this
    /// run then forgets, if it has it.
    pub fn take_answer(&self, from: &str, invoke: u64) -> Option<Answer> {
        let mut g = self.lock();
        let i = g
            .answers
            .iter()
            .position(|(f, i, _)| f == from && *i == invoke)?;
        Some(g.answers.remove(i).2)
    }

    /// Answers `Unanswered` every call this run was given and has not answered, in the order
    /// they arrived: those it took first (pending, or taken by a step a defect cut short),
    /// then those still queued. It forgets them all. For a run that has ended, after its last
    /// step's commands.
    pub fn answer_unanswered(&self) {
        let answers = std::mem::take(&mut self.lock().answers);
        for (_, _, answer) in answers {
            answer.unanswered();
        }
    }

    /// Closes the run for good: nothing more is delivered to it, and what its inbox holds is
    /// discarded.
    pub fn close(&self) {
        let mut g = self.lock();
        g.closed = true;
        g.inbox.clear();
    }

    /// Closes the run if its inbox is empty and no cancel was requested, and says whether it
    /// did: the last check of a run about to suspend, made under the lock that pushing takes,
    /// so that nothing pushed while the run was open is discarded.
    pub fn close_if_idle(&self) -> bool {
        let mut g = self.lock();
        if !g.inbox.is_empty() || g.cancel {
            return false;
        }
        g.closed = true;
        true
    }

    /// Subscribes `run` to the events in `events` that this run emits.
    pub fn subscribe(&self, run: Arc<Shared>, events: HashSet<String>) {
        self.lock().subscribers.push((run, events));
    }

    /// Ends the subscription of `run`.
    pub fn unsubscribe(&self, run: &Arc<Shared>) {
        self.lock()
            .subscribers
            .retain(|(s, _)| !Arc::ptr_eq(s, run));
    }

    /// The runs subscribed to `event` when this run emits it. Runs that have closed without
    /// unsubscribing (one stopped before its driver was first polled) are forgotten here.
    pub fn subscribers(&self, event: &str) -> Vec<Arc<Shared>> {
        // Not holding this run's lock while taking theirs.
        let (open, gone): (Vec<_>, Vec<_>) = self
            .lock()
            .subscribers
            .iter()
            .filter(|(_, events)| events.contains(event))
            .map(|(s, _)| s.clone())
            .partition(|s| s.is_open());
        if !gone.is_empty() {
            self.lock()
                .subscribers
                .retain(|(s, _)| !gone.iter().any(|g| Arc::ptr_eq(g, s)));
        }
        open
    }

    /// A new host listener of what this run emits, ended at once if the run has finished.
    /// With `after_start` (an actor's), it hears nothing the start step emits.
    pub fn listen(&self, after_start: bool) -> Arc<Feed> {
        self.lock().listeners.add(after_start)
    }

    /// The run has carried out its start (or restore) step.
    pub fn listeners_started(&self) {
        self.lock().listeners.start();
    }

    /// Gives an event this run emits to its host listeners.
    pub fn tell_listeners(&self, value: &Value) {
        let feeds = self.lock().listeners.live();
        for feed in feeds {
            feed.push(value.clone());
        }
    }

    /// Neither closed nor finished.
    fn is_open(&self) -> bool {
        let g = self.lock();
        !g.closed && g.finished.is_none()
    }

    /// Asks the run to suspend once it is idle (`spec/execution.md#suspending-and-resume`).
    pub fn request_suspend(&self) {
        let waker = {
            let mut g = self.lock();
            g.suspend = true;
            g.waker.take()
        };
        if let Some(w) = waker {
            w.wake();
        }
    }

    pub fn suspend_requested(&self) -> bool {
        self.lock().suspend
    }

    /// Publishes the run's saved snapshot.
    pub fn set_saved(&self, saved: Saved) {
        let mut g = self.lock();
        g.saved = Some(Published::Saved(Arc::new(saved)));
        Inner::wake_saved(g);
    }

    /// Publishes the run's saved snapshot as `Saved::new(snap, …)` with `dues`, each timer's
    /// due time in order, built when it is first read.
    pub fn publish(&self, snap: Arc<Snapshot>, dues: Vec<u64>) {
        let mut g = self.lock();
        g.saved = Some(Published::Step { snap, dues });
        Inner::wake_saved(g);
    }

    pub fn saved(&self) -> Option<Arc<Saved>> {
        self.lock().saved()
    }

    /// The saved snapshot, `Some(None)` if the run finished without one, or `None` after
    /// registering `waker` for it.
    pub fn poll_saved(&self, waker: &Waker) -> Option<Option<Arc<Saved>>> {
        let mut g = self.lock();
        if g.saved.is_some() || g.finished.is_some() {
            return Some(g.saved());
        }
        remember(&mut g.saved_wakers, waker);
        None
    }

    pub fn request_cancel(&self) {
        let waker = {
            let mut g = self.lock();
            g.cancel = true;
            g.waker.take()
        };
        if let Some(w) = waker {
            w.wake();
        }
    }

    /// Moves the inbox into `queue`, remembers `waker` for the next push, and says whether a
    /// cancel was requested.
    pub fn drain(&self, queue: &mut VecDeque<Input>, waker: &Waker) -> bool {
        let mut g = self.lock();
        queue.extend(g.inbox.drain(..));
        if !g.waker.as_ref().is_some_and(|w| w.will_wake(waker)) {
            g.waker = Some(waker.clone());
        }
        g.cancel
    }

    pub fn finish(&self, how: Finished) {
        let mut g = self.lock();
        if g.finished.is_none() {
            g.finished = Some(how);
        }
        g.inbox.clear();
        let mut wakers = std::mem::take(&mut g.finish_wakers);
        wakers.append(&mut g.state_wakers);
        wakers.append(&mut g.saved_wakers);
        let feeds = g.listeners.end();
        drop(g);
        for feed in feeds {
            feed.end();
        }
        for w in wakers {
            w.wake();
        }
    }

    pub fn finished(&self) -> Option<Finished> {
        self.lock().finished.clone()
    }

    /// The finish report, or `None` after registering `waker` for it.
    pub fn poll_finished(&self, waker: &Waker) -> Option<Finished> {
        let mut g = self.lock();
        if g.finished.is_none() {
            remember(&mut g.finish_wakers, waker);
        }
        g.finished.clone()
    }

    pub fn set_state(&self, leaf: &str) {
        let mut g = self.lock();
        if g.state.as_deref() != Some(leaf) {
            g.state = Some(leaf.to_owned());
            g.state_version += 1;
            let wakers = std::mem::take(&mut g.state_wakers);
            drop(g);
            for w in wakers {
                w.wake();
            }
        }
    }

    pub fn state(&self) -> (u64, Option<String>) {
        let g = self.lock();
        (g.state_version, g.state.clone())
    }

    /// The state if its version is newer than `seen` or the run has finished (so that waiting
    /// for a change of a finished run does not hang), else `None` after registering `waker`.
    pub fn poll_state(&self, seen: u64, waker: &Waker) -> Option<(u64, Option<String>)> {
        let mut g = self.lock();
        if g.state_version > seen || g.finished.is_some() {
            return Some((g.state_version, g.state.clone()));
        }
        remember(&mut g.state_wakers, waker);
        None
    }
}

/// Adds `waker` to `wakers` unless one there already wakes the same task, so that a future
/// polled again and again leaves one waker, not one per poll.
fn remember(wakers: &mut Vec<Waker>, waker: &Waker) {
    if !wakers.iter().any(|w| w.will_wake(waker)) {
        wakers.push(waker.clone());
    }
}

/// A handle to a run: its id and its queue. Host code and tests feed inputs to a run
/// through it, and it is what a `ref` to the run is in the erased interface
/// ([`crate::dynamic`]): a run's id in a value, and this handle beside it. The typed handle,
/// a machine's `Ref`, wraps one ([`crate::Ref`]).
///
/// A handle knows the machine its run runs, so a run given it as a `ref M` checks that it
/// is a run of `M` (by name and machine hash). The handle of a child run, which only
/// [`crate::Config::on_run`] hands out, cannot be passed as a `ref` at all: a child run is
/// reached only through its invoke (`spec/execution.md#refs-and-handles`).
#[derive(Clone)]
pub struct RunHandle {
    pub(crate) shared: Arc<Shared>,
    pub(crate) id: Arc<str>,
    pub(crate) machine: Arc<str>,
    /// The machine's hash in the program the run runs.
    pub(crate) hash: Arc<str>,
    /// Whether this is a child run's handle.
    pub(crate) child: bool,
    /// Whether [`RunHandle::cancel`] reaches the run: not for a run that cannot report a
    /// cancellation (a child run, a root run of `run` or `run_with`).
    pub(crate) cancellable: bool,
}

impl std::fmt::Debug for RunHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("RunHandle").field(&&*self.id).finish()
    }
}

/// Two handles are equal when they are handles to the same run.
impl PartialEq for RunHandle {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.shared, &other.shared)
    }
}

impl Eq for RunHandle {}

impl RunHandle {
    pub(crate) fn new(
        shared: Arc<Shared>,
        id: &str,
        program: &Program,
        machine: &str,
        child: bool,
    ) -> RunHandle {
        let hash = program
            .ir()
            .machines
            .get(machine)
            .map_or("", |m| m.hash.as_str());
        RunHandle {
            shared,
            id: Arc::from(id),
            machine: Arc::from(machine),
            hash: Arc::from(hash),
            child,
            cancellable: !child,
        }
    }

    /// The handle of a root run of `run` or `run_with`, which goes on to its end.
    pub(crate) fn runs_to_the_end(mut self) -> Self {
        self.cancellable = false;
        self
    }

    /// The run's id: what a `ref` to it is in snapshots and traces.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The name of the machine the run runs.
    pub fn machine(&self) -> &str {
        &self.machine
    }

    /// Whether this is the handle of a child run (from [`crate::Config::on_run`]), which can
    /// be sent inputs but not passed to another run as a `ref`.
    pub fn is_child(&self) -> bool {
        self.child
    }

    /// Queues an external event, a tagged record.
    pub fn send(&self, event: Value) {
        self.shared.push([Input::Event { value: event }]);
    }

    /// Appends `inputs` to the run's queue in one operation, so that the run takes no step
    /// between them. They are taken after whatever was queued before.
    ///
    /// # Panics
    ///
    /// If `inputs` holds [`Input::Cancel`] or [`Input::Defect`], which are not inputs a queue
    /// holds: a cancel request goes through [`RunHandle::cancel`], and a defect comes only from
    /// the run's own driver.
    pub fn push(&self, inputs: Vec<Input>) {
        assert!(
            !inputs
                .iter()
                .any(|i| matches!(i, Input::Cancel | Input::Defect { .. })),
            "RunHandle::push: a cancel request or a defect is not an input to queue; \
             use RunHandle::cancel to cancel a run"
        );
        self.shared.push(inputs);
    }

    /// Asks the run to cancel. It takes the request between steps, before anything queued.
    ///
    /// A run that cannot report a cancellation ignores the request, as it ignores
    /// [`RunHandle::suspend`]: a child run, which only its parent cancels, by leaving the
    /// invoking state (a child run has no handle of its own, `spec/execution.md#refs-and-handles`,
    /// and its parent learns how it ended only through its invoke), and a root run of
    /// [`crate::dynamic::run`] or [`crate::dynamic::run_with`] (or a generated `run`), whose
    /// caller gets an output or an error. Cancel a root run gracefully through a run of
    /// [`crate::dynamic::start`] or a spawned actor; stop one of `run` by dropping its future.
    pub fn cancel(&self) {
        if self.cancellable {
            self.shared.request_cancel();
        }
    }

    /// Asks a top-level run to suspend: once it is idle (between steps, nothing queued, no
    /// running invoke, no pending call) it stops without exit effects, writes `suspended`, and its future
    /// ends with [`crate::RunEnd::Suspended`]. A run that ends first ends as it would have.
    /// A child run ignores the request.
    pub fn suspend(&self) {
        self.shared.request_suspend();
    }

    /// The run's saved snapshot as its last step left it (`spec/execution.md#saving-and-restoring`),
    /// or `None` before it has carried out its start (or restore) step.
    pub fn save(&self) -> Option<Saved> {
        self.shared.saved().map(|s| (*s).clone())
    }

    /// Listens to what the run emits, as JSON values, from now until it ends or is suspended
    /// (`spec/execution.md#host-listeners`). Called from [`crate::Config::on_run`], which
    /// runs before the run's start step, it hears what that step emits too, also for a child
    /// run or a run of `run`, which have no actor. Dropping the subscription unsubscribes.
    pub fn subscribe(&self) -> Subscription<Value> {
        Subscription::new(self.shared.listen(false), |v| v)
    }

    /// Whether the run has ended: it finished, was cancelled, or its future was dropped (a
    /// hard stop).
    pub fn is_finished(&self) -> bool {
        self.shared.finished().is_some()
    }

    /// Whether the run is closed: its core has given a snapshot that is not `running`, so
    /// messages sent to it now are undelivered. A run closes before it has finished
    /// carrying out its last step.
    pub fn is_closed(&self) -> bool {
        self.shared.lock().closed
    }
}

/// The ambient context the driver sets while it polls a machine invoke's future.
pub(crate) struct InvokeContext {
    pub config: Config,
    pub parent: String,
    pub invoke: u64,
    /// The child run's [`Shared`], once a run claims it. Until then its inbox holds the
    /// messages the parent sends to the invoke.
    pub token: Arc<Shared>,
    /// The parent's own, which the child pushes what it emits onto when the parent receives it.
    pub parent_queue: Arc<Shared>,
    /// The child's emitted events that the parent receives: those its machine has an `on`
    /// for, and none when the invoke has no `as` name (a spawned child always has them).
    pub heard: HashSet<String>,
}

thread_local! {
    static CONTEXT: RefCell<Option<Arc<InvokeContext>>> = const { RefCell::new(None) };
}

/// Runs `f` with `ctx` as the ambient invoke context (or none), restoring the previous one
/// afterwards, even if `f` panics.
pub(crate) fn with_context<R>(ctx: Option<&Arc<InvokeContext>>, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<Arc<InvokeContext>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let prev = self.0.take();
            CONTEXT.with(|c| *c.borrow_mut() = prev);
        }
    }
    let prev = CONTEXT.with(|c| c.replace(ctx.cloned()));
    let _restore = Restore(prev);
    f()
}

/// The ambient invoke context, if there is one whose token this call claims.
pub(crate) fn claim_context() -> Option<Arc<InvokeContext>> {
    CONTEXT
        .with(|c| c.borrow().clone())
        .filter(|ctx| ctx.token.try_claim())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::Wake;

    use super::*;

    /// Counts its wake-ups; two of them are two tasks.
    #[derive(Default)]
    struct Count(AtomicUsize);

    impl Wake for Count {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn inputs_pushed_after_the_run_finished_are_dropped() {
        let s = Shared::default();
        s.push([Input::Cancel]);
        assert_eq!(s.lock().inbox.len(), 1);
        s.finish(Finished::Ok);
        for _ in 0..3 {
            s.push([Input::Cancel]);
        }
        assert!(s.lock().inbox.is_empty());
    }

    #[test]
    fn polling_again_and_again_keeps_one_waker() {
        let s = Shared::default();
        let waker = Waker::from(Arc::new(Count::default()));
        let other = Waker::from(Arc::new(Count::default()));
        for _ in 0..5 {
            assert!(s.poll_state(0, &waker).is_none());
            assert!(s.poll_finished(&waker).is_none());
        }
        assert!(s.poll_state(0, &other).is_none());
        let g = s.lock();
        assert_eq!(g.state_wakers.len(), 2);
        assert_eq!(g.finish_wakers.len(), 1);
    }
}
