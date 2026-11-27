//! Host listeners of a run's emitted events (`spec/execution.md#host-listeners`): a
//! [`Subscription`] from [`crate::Actor::subscribe`] or [`crate::RunHandle::subscribe`].
//!
//! A listener is not a run. The driver pushes each event the run emits onto every live
//! listener's buffer as it carries out the `emit`, never waits for one, and writes no record.
//! It ends them when the run finishes, after the commands of its last step. Each buffer is
//! unbounded, as queues are.

use std::collections::VecDeque;
use std::fmt;
use std::future::poll_fn;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::task::{Poll, Waker};

use serde_json::Value;

#[derive(Default)]
struct FeedInner {
    buffer: VecDeque<Value>,
    ended: bool,
    waker: Option<Waker>,
}

/// One listener's buffer, shared by the run that pushes onto it and the [`Subscription`]
/// that takes from it.
#[derive(Default)]
pub(crate) struct Feed(Mutex<FeedInner>);

impl Feed {
    fn lock(&self) -> MutexGuard<'_, FeedInner> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Appends an event, unless the feed has ended.
    pub(crate) fn push(&self, value: Value) {
        let waker = {
            let mut g = self.lock();
            if g.ended {
                return;
            }
            g.buffer.push_back(value);
            g.waker.take()
        };
        if let Some(w) = waker {
            w.wake();
        }
    }

    /// Ends the feed: what it holds can still be taken, and nothing more is appended.
    pub(crate) fn end(&self) {
        let waker = {
            let mut g = self.lock();
            g.ended = true;
            g.waker.take()
        };
        if let Some(w) = waker {
            w.wake();
        }
    }

    /// The next event, `Ready(None)` once ended and empty, or `Pending` after registering
    /// `waker`.
    fn poll(&self, waker: Option<&Waker>) -> Poll<Option<Value>> {
        let mut g = self.lock();
        if let Some(v) = g.buffer.pop_front() {
            return Poll::Ready(Some(v));
        }
        if g.ended {
            return Poll::Ready(None);
        }
        if let Some(w) = waker {
            g.waker = Some(w.clone());
        }
        Poll::Pending
    }
}

/// The listeners of one run. A listener whose [`Subscription`] was dropped is gone: the run
/// holds only a weak reference to its feed.
#[derive(Default)]
pub(crate) struct Listeners {
    feeds: Vec<Weak<Feed>>,
    /// An actor's listeners made before the run's start (or restore) step was carried out:
    /// they hear from the step after it, as in TypeScript, where the actor exists only then.
    waiting: Vec<Weak<Feed>>,
    started: bool,
    ended: bool,
}

impl Listeners {
    /// A new listener's feed; ended at once if the run has finished. With `after_start`, it
    /// hears nothing the run's start step emits.
    pub(crate) fn add(&mut self, after_start: bool) -> Arc<Feed> {
        let feed = Arc::new(Feed::default());
        if self.ended {
            feed.end();
        } else if after_start && !self.started {
            self.waiting.retain(|f| f.strong_count() > 0);
            self.waiting.push(Arc::downgrade(&feed));
        } else {
            self.feeds.retain(|f| f.strong_count() > 0);
            self.feeds.push(Arc::downgrade(&feed));
        }
        feed
    }

    /// The run has carried out its start (or restore) step.
    pub(crate) fn start(&mut self) {
        self.started = true;
        self.feeds.append(&mut self.waiting);
    }

    /// The feeds still listened to, to push an event onto outside the run's lock.
    pub(crate) fn live(&mut self) -> Vec<Arc<Feed>> {
        self.feeds.retain(|f| f.strong_count() > 0);
        self.feeds.iter().filter_map(Weak::upgrade).collect()
    }

    /// Marks the run finished and gives the feeds to end, outside the run's lock.
    pub(crate) fn end(&mut self) -> Vec<Arc<Feed>> {
        self.ended = true;
        self.feeds.append(&mut self.waiting);
        std::mem::take(&mut self.feeds)
            .iter()
            .filter_map(Weak::upgrade)
            .collect()
    }
}

/// A host listener of a run's emitted events, typed `T`: the machine's `Emitted` enum from
/// [`crate::Actor::subscribe`], JSON values from [`crate::RunHandle::subscribe`].
///
/// It hears every event the run emits from when it was made, whatever any machine hears,
/// in the order the run emitted them; what the run emitted before is not in it. Nothing
/// waits for it, and what it has not taken yet is kept without bound. Dropping it
/// unsubscribes. Listening writes nothing to the trace (`spec/execution.md#host-listeners`).
pub struct Subscription<T> {
    feed: Arc<Feed>,
    decode: fn(Value) -> T,
}

impl<T> fmt::Debug for Subscription<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Subscription").finish_non_exhaustive()
    }
}

impl<T> Subscription<T> {
    pub(crate) fn new(feed: Arc<Feed>, decode: fn(Value) -> T) -> Self {
        Subscription { feed, decode }
    }

    /// Waits for the next event. `None` once the run has ended (or been suspended) and every
    /// event it emitted before that has been taken. An event that does not decode as `T` is a
    /// host defect: it panics with the [`crate::Defect`] as its payload.
    pub async fn next(&mut self) -> Option<T> {
        let value = poll_fn(|cx| self.feed.poll(Some(cx.waker()))).await;
        value.map(self.decode)
    }

    /// The next event if one is there now, without waiting. `None` both when none has
    /// arrived yet and when the run has ended; [`Subscription::next`] tells them apart.
    pub fn try_next(&mut self) -> Option<T> {
        match self.feed.poll(None) {
            Poll::Ready(v) => v.map(self.decode),
            Poll::Pending => None,
        }
    }
}
