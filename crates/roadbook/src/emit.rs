//! The emitter a task's host implementation emits its events through
//! (`spec/execution.md#the-emitter`): typed, [`Emitter`], and erased,
//! [`crate::dynamic::Emitter`].

use std::collections::HashSet;
use std::fmt;
use std::marker::PhantomData;
use std::sync::{Arc, Mutex};

use roadbook_core::Input;
use serde::Serialize;
use serde_json::Value;

use crate::shared::Shared;

/// What a live emitter holds: the run's queue, the invoke's (or spawned task's) id, the events
/// of the task's `emits` that the run's machine hears from it, and whether it is still open.
pub(crate) struct Live {
    /// Closed once the driver has cancelled, stopped or dropped the task's work, or found its
    /// end. Held while pushing, so that an emit and the close are one operation.
    open: Mutex<bool>,
    queue: Arc<Shared>,
    invoke: u64,
    heard: HashSet<String>,
}

/// The erased emitter: an event in its JSON form, a tagged record, pushed onto the invoking
/// run's queue as a `task.emitted` input when the run hears it.
///
/// The driver gives one to every task it starts, in [`crate::dynamic::Invocation::emitter`]. It
/// is **live** for an invoke with `as` and for a spawned task, and **inert** otherwise (an
/// invoke without `as`, a machine invoke): emitting through an inert one does nothing. A live
/// one is closed once the driver has cancelled or stopped the task, found that its future has
/// ended, or dropped it, and when the run closes; emitting through a closed one does nothing
/// too. It is cheap to clone, `Send` and `Sync`, and may be kept and used from anywhere.
#[derive(Clone, Default)]
pub struct DynEmitter(Option<Arc<Live>>);

impl DynEmitter {
    /// An emitter that queues nothing.
    pub fn inert() -> Self {
        DynEmitter(None)
    }

    /// A live emitter of invoke (or spawned task) `invoke` onto `queue`, for the events in
    /// `heard`.
    pub(crate) fn live(queue: Arc<Shared>, invoke: u64, heard: HashSet<String>) -> Self {
        DynEmitter(Some(Arc::new(Live {
            open: Mutex::new(true),
            queue,
            invoke,
            heard,
        })))
    }

    /// Closes it for good: nothing it emits from now on is queued. Idempotent.
    pub(crate) fn close(&self) {
        if let Some(live) = &self.0 {
            *live.open.lock().unwrap_or_else(|e| e.into_inner()) = false;
        }
    }

    /// Emits `event`, one of the task's `emits` in its JSON form: pushes it onto the run's
    /// queue as `{"kind": "task.emitted", "invoke": id, "value": event}` if the emitter is live
    /// and open, the run is open, and the run's machine hears the event's `_tag` from the task.
    /// Otherwise it does nothing, and nothing is written. It never waits.
    pub fn emit(&self, event: Value) {
        let Some(live) = &self.0 else { return };
        let open = live.open.lock().unwrap_or_else(|e| e.into_inner());
        if *open && live.hears(&event) {
            live.queue.deliver(Input::TaskEmitted {
                invoke: live.invoke,
                value: event,
            });
        }
    }

    /// The input [`DynEmitter::emit`] would push for `event` now, without pushing it: `None`
    /// if it would push nothing. For a caller that queues it with other inputs in one
    /// operation ([`crate::RunHandle::push`]), as a conformance runner's `together` does.
    pub fn input(&self, event: &Value) -> Option<Input> {
        let live = self.0.as_ref()?;
        let open = live.open.lock().unwrap_or_else(|e| e.into_inner());
        (*open && live.hears(event)).then(|| Input::TaskEmitted {
            invoke: live.invoke,
            value: event.clone(),
        })
    }

    /// Whether it is live (an invoke with `as`, or a spawned task) and not closed yet.
    pub fn is_open(&self) -> bool {
        self.0
            .as_ref()
            .is_some_and(|l| *l.open.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

impl Live {
    fn hears(&self, event: &Value) -> bool {
        event
            .get("_tag")
            .and_then(Value::as_str)
            .is_some_and(|t| self.heard.contains(t))
    }
}

impl fmt::Debug for DynEmitter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            None => f.write_str("Emitter(inert)"),
            Some(l) => f
                .debug_struct("Emitter")
                .field("invoke", &l.invoke)
                .field("open", &self.is_open())
                .finish(),
        }
    }
}

/// Two emitters are equal when they are the same emitter (clones of one), or both inert.
impl PartialEq for DynEmitter {
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (None, None) => true,
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

/// How a task's host implementation emits the events its task declares with `emits`
/// (`spec/execution.md#the-emitter`). `T` is the task's generated `{Task}Emitted` enum.
///
/// The `Handlers` method of a task that declares `emits` takes one as its last parameter. It
/// is cheap to clone, `Send`, `Sync` and `'static`, so it can be moved into another task or
/// thread, and kept after the method's future has ended.
///
/// [`Emitter::emit`] queues the event for the run that invoked the task with `as`, or spawned
/// it, if that run's machine has an `on` for it (`on up.Progress`, `on uploads.Progress`). An
/// invoke without `as` has an inert emitter, and once the task's work has been cancelled,
/// stopped or has ended, the emitter is closed: emitting through either does nothing.
pub struct Emitter<T> {
    inner: DynEmitter,
    _event: PhantomData<fn(T)>,
}

impl<T> Emitter<T> {
    /// The typed emitter over an erased one ([`crate::dynamic::Invocation::emitter`]).
    pub fn new(inner: DynEmitter) -> Self {
        Emitter {
            inner,
            _event: PhantomData,
        }
    }

    /// The erased emitter, for the [`crate::dynamic`] interface.
    pub fn erased(&self) -> &DynEmitter {
        &self.inner
    }

    /// Whether what it emits can still be queued: it is live (an invoke with `as`, or a
    /// spawned task) and not closed yet. A stream task may stop when this is `false`, though
    /// it is dropped soon after its work is cancelled anyway.
    pub fn is_open(&self) -> bool {
        self.inner.is_open()
    }
}

impl<T: Serialize> Emitter<T> {
    /// Emits `event`. Synchronous, and it never waits: the run's queue is unbounded.
    ///
    /// An event that cannot be encoded (a non-finite `float`) is the task's host defect:
    /// nothing is queued, and `emit` panics with the [`crate::Defect`] as its payload
    /// (`std::panic::resume_unwind`), which the driver, catching it around the task's poll,
    /// makes the task's outcome (`invoke.defect` or `spawn.defect`). Called from another
    /// thread, the panic is that thread's.
    pub fn emit(&self, event: T) {
        let value = crate::__private::encode(&event)
            .unwrap_or_else(|d| std::panic::resume_unwind(Box::new(d)));
        self.inner.emit(value);
    }
}

impl<T> Clone for Emitter<T> {
    fn clone(&self) -> Self {
        Emitter::new(self.inner.clone())
    }
}

impl<T> fmt::Debug for Emitter<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.inner.fmt(f)
    }
}
