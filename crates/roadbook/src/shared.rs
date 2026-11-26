//! What a run shares with the outside: its inbox, its cancel flag, how it finished, and the
//! state it is in. For a child run this is also the cancel token of the invoke context.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::Waker;

use roadbook_core::{Defect, Input};
use serde_json::Value;

use crate::env::Config;

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

    /// Appends `inputs` to the inbox. After the run has finished nobody drains it, so they
    /// are dropped.
    pub fn push(&self, inputs: impl IntoIterator<Item = Input>) {
        let waker = {
            let mut g = self.lock();
            if g.finished.is_some() {
                return;
            }
            g.inbox.extend(inputs);
            g.waker.take()
        };
        if let Some(w) = waker {
            w.wake();
        }
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
        drop(g);
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

/// A handle to a run's queue, for a host or a test that feeds inputs to it directly.
#[derive(Clone)]
pub struct RunHandle {
    pub(crate) shared: Arc<Shared>,
}

impl std::fmt::Debug for RunHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunHandle").finish_non_exhaustive()
    }
}

impl RunHandle {
    /// Queues an external event, a tagged record.
    pub fn send(&self, event: Value) {
        self.shared.push([Input::Event { value: event }]);
    }

    /// Appends `inputs` to the run's queue in one operation, so that the run takes no step
    /// between them. They are taken after whatever was queued before.
    pub fn push(&self, inputs: Vec<Input>) {
        self.shared.push(inputs);
    }

    /// Asks the run to cancel. It takes the request between steps, before anything queued.
    pub fn cancel(&self) {
        self.shared.request_cancel();
    }

    /// Whether the run has ended.
    pub fn is_finished(&self) -> bool {
        self.shared.finished().is_some()
    }
}

/// The ambient context the driver sets while it polls a machine invoke's future.
pub(crate) struct InvokeContext {
    pub config: Config,
    pub parent: String,
    pub invoke: u64,
    pub token: Arc<Shared>,
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
