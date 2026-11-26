//! Handles: how host code holds a `ref` to a run, and how the binding layer turns handles
//! into run ids and back (`spec/execution.md#refs-and-handles`).
//!
//! A typed handle encodes as its run's id. While the generated code encodes a machine's
//! arguments it collects every handle it meets ([`collect`]), so the driver gets the handles
//! along with the ids. While the driver calls host code it sets the handles of the run as the
//! ambient scope ([`with_scope`]), so that decoding a run id in an argument gives back the
//! handle.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;
use std::marker::PhantomData;
use std::sync::Arc;

use roadbook_core::Input;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::shared::RunHandle;

/// The handles a run holds, by run id.
pub(crate) type Peers = BTreeMap<String, RunHandle>;

thread_local! {
    static COLLECTED: RefCell<Option<Vec<RunHandle>>> = const { RefCell::new(None) };
    static SCOPE: RefCell<Option<Arc<Peers>>> = const { RefCell::new(None) };
}

/// Runs `f`, giving its result and every handle encoded while it ran.
pub(crate) fn collect<R>(f: impl FnOnce() -> R) -> (R, Vec<RunHandle>) {
    struct Restore(Option<Vec<RunHandle>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let prev = self.0.take();
            COLLECTED.with(|c| *c.borrow_mut() = prev);
        }
    }
    let prev = COLLECTED.with(|c| c.replace(Some(Vec::new())));
    let restore = Restore(prev);
    let r = f();
    let got = COLLECTED
        .with(|c| c.borrow_mut().take())
        .unwrap_or_default();
    drop(restore);
    (r, got)
}

/// Runs `f` with `peers` as the handles a run id in a decoded value can name.
pub(crate) fn with_scope<R>(peers: Option<&Arc<Peers>>, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<Arc<Peers>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let prev = self.0.take();
            SCOPE.with(|c| *c.borrow_mut() = prev);
        }
    }
    let prev = SCOPE.with(|c| c.replace(peers.cloned()));
    let _restore = Restore(prev);
    f()
}

fn note(handle: &RunHandle) {
    COLLECTED.with(|c| {
        if let Some(v) = c.borrow_mut().as_mut() {
            v.push(handle.clone());
        }
    });
}

fn resolve(id: &str) -> Option<RunHandle> {
    SCOPE.with(|c| c.borrow().as_ref().and_then(|p| p.get(id).cloned()))
}

/// A typed handle to a running machine whose accepted events are `E`: what a chart's
/// `ref Machine` is in Rust. Each machine's module names its own as `Ref`
/// (`inventory::Ref` is `roadbook::Ref<inventory::Event>`), so a handle to one machine is
/// not a handle to another.
///
/// Get one from a spawned run's actor ([`crate::Actor::handle`]) and pass it as a machine
/// argument of type `ref Machine`; a `Handlers` method gets one where the chart passes a
/// `ref` to host code. It is cheap to clone and `Send`. In JSON (snapshots, traces) it is
/// the run's id. It has no `==`, as the chart's `ref` has none.
///
/// A handle decodes only inside the driver, from a run id that the run it is decoded for
/// holds: deserialising one anywhere else is an error.
pub struct Ref<E> {
    handle: RunHandle,
    _event: PhantomData<fn(E)>,
}

impl<E> Ref<E> {
    pub(crate) fn new(handle: RunHandle) -> Self {
        Ref {
            handle,
            _event: PhantomData,
        }
    }

    /// The run's id: `r1`, `r2`, … unless a `roadbook::Config::run_id` named it.
    pub fn id(&self) -> &str {
        self.handle.id()
    }

    /// The erased handle, for the [`crate::dynamic`] interface.
    pub fn run_handle(&self) -> &RunHandle {
        &self.handle
    }

    /// Whether the run is closed: it has ended, or is carrying out its last step. A message
    /// a machine sends it now is `undelivered`.
    pub fn is_closed(&self) -> bool {
        self.handle.is_closed()
    }
}

impl<E: Serialize> Ref<E> {
    /// Queues an event for the run, as [`crate::Actor::send`] does: an ordinary event with no
    /// sender, taken in order after what is queued. An event sent after the run closed is
    /// ignored.
    ///
    /// An event that cannot be encoded (a non-finite `float`) is the sender's host defect,
    /// not the receiver's: nothing is queued, and `send` panics with the [`crate::Defect`] as
    /// its payload (`std::panic::resume_unwind`). Inside an action or a task that ends the run
    /// carrying it out with that host defect, as any panic there does.
    pub async fn send(&self, event: E) {
        let value = crate::__private::encode(&event)
            .unwrap_or_else(|d| std::panic::resume_unwind(Box::new(d)));
        self.handle.shared.push([Input::Event { value }]);
    }
}

impl<E> Clone for Ref<E> {
    fn clone(&self) -> Self {
        Ref::new(self.handle.clone())
    }
}

impl<E> fmt::Debug for Ref<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Ref").field(&self.id()).finish()
    }
}

impl<E> Serialize for Ref<E> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        note(&self.handle);
        serializer.serialize_str(self.id())
    }
}

impl<'de, E> Deserialize<'de> for Ref<E> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let id = String::deserialize(deserializer)?;
        resolve(&id).map(Ref::new).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "`{id}` is not a run this run holds a `ref` to (a `ref` decodes only inside \
                 the driver)"
            ))
        })
    }
}
