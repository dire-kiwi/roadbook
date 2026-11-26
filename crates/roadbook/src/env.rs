//! What a run needs from its surroundings: a clock and timer, a trace sink, a run id.

use std::fmt;
use std::future::Future;
use std::io::Write;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use roadbook_core::Record;
use serde_json::Value;

/// A boxed, `Send` future, the shape every erased host call returns.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The clock and timer a run reads time from and waits with.
///
/// The driver reads the time only through [`Timer::now`], when it writes a trace record and
/// when it starts a timer, and waits only through [`Timer::start`]. It never completes a
/// timer itself, not even one with delay 0, so a test can give it a virtual clock and fire
/// timers itself.
pub trait Timer: Send + Sync {
    /// The time in integer milliseconds: since the Unix epoch in production, virtual
    /// milliseconds in tests.
    fn now(&self) -> u64;

    /// Starts timer `timer` of run `run`. The future completes when it is due, `delay`
    /// milliseconds from now. Dropping the future cancels it.
    fn start(&self, run: &str, timer: u64, delay: u64) -> BoxFuture<'static, ()>;

    /// Tells the timer that the driver cancelled timer `timer` of run `run`. The driver also
    /// drops the timer's future; the default does nothing more.
    fn cancel(&self, run: &str, timer: u64) {
        let _ = (run, timer);
    }
}

/// The default [`Timer`]: the system clock and `tokio::time::sleep`.
#[cfg(feature = "tokio")]
#[derive(Debug, Clone, Copy, Default)]
pub struct TokioTimer;

#[cfg(feature = "tokio")]
impl Timer for TokioTimer {
    fn now(&self) -> u64 {
        system_now()
    }

    fn start(&self, _run: &str, _timer: u64, delay: u64) -> BoxFuture<'static, ()> {
        Box::pin(tokio::time::sleep(std::time::Duration::from_millis(delay)))
    }
}

fn system_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The timer used without the `tokio` feature: it reads the system clock and refuses to
/// start a timer.
#[cfg(not(feature = "tokio"))]
#[derive(Debug, Clone, Copy, Default)]
struct NoTimer;

#[cfg(not(feature = "tokio"))]
impl Timer for NoTimer {
    fn now(&self) -> u64 {
        system_now()
    }

    fn start(&self, _run: &str, _timer: u64, _delay: u64) -> BoxFuture<'static, ()> {
        panic!(
            "a roadbook machine started an `after` timer, but no Timer was given: enable the \
             `tokio` feature of `roadbook` or pass one with `Config::timer`"
        )
    }
}

/// One trace record as the driver writes it: the core's record plus `t`, `run` and, for a
/// child run, `parent`.
#[derive(Debug, Clone, PartialEq)]
pub struct TraceRecord {
    /// The clock when the record was written, in milliseconds.
    pub t: u64,
    /// The id of the run that wrote it.
    pub run: String,
    /// The parent run's id, for a child run.
    pub parent: Option<String>,
    /// The record.
    pub record: Record,
}

impl TraceRecord {
    /// The record as the JSON object of `spec/trace.md`.
    pub fn to_json(&self) -> Value {
        let mut v = self.record.to_value();
        if let Value::Object(m) = &mut v {
            m.insert("t".into(), Value::from(self.t));
            m.insert("run".into(), Value::from(self.run.as_str()));
            if let Some(p) = &self.parent {
                m.insert("parent".into(), Value::from(p.as_str()));
            }
        }
        v
    }

    /// The record in canonical JSON (RFC 8785), the form traces are compared in: the same
    /// text as [`TraceRecord::to_json`]'s, written with no JSON value in between.
    pub fn to_canonical(&self) -> String {
        let mut out = String::new();
        self.write_canonical(&mut out);
        out
    }

    /// Appends [`TraceRecord::to_canonical`] to `out`.
    pub fn write_canonical(&self, out: &mut String) {
        /// The record with `t`, `run` and `parent` among its members.
        #[derive(serde::Serialize)]
        struct Line<'a> {
            t: u64,
            run: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            parent: Option<&'a str>,
            #[serde(flatten)]
            record: &'a Record,
        }
        let start = out.len();
        let line = Line {
            t: self.t,
            run: &self.run,
            parent: self.parent.as_deref(),
            record: &self.record,
        };
        if roadbook_ir::jcs::write_canonical(out, &line).is_err() {
            // As `to_json` gives for a record that does not serialise.
            out.truncate(start);
            out.push_str("null");
        }
    }
}

/// Where a run's trace records go.
pub trait TraceSink: Send + Sync {
    /// Writes one record. Called in the order the run emitted them.
    fn record(&self, record: &TraceRecord);

    /// Whether the sink wants records at all. A sink that says `false` is never given one,
    /// and the driver saves itself building them. Asked once per record, so it may change.
    fn enabled(&self) -> bool {
        true
    }
}

/// The default sink: drops every record.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoTrace;

impl TraceSink for NoTrace {
    fn record(&self, _record: &TraceRecord) {}

    fn enabled(&self) -> bool {
        false
    }
}

/// Writes each record as one line of canonical JSON ([JSON Lines](https://jsonlines.org/)).
pub struct JsonLines<W> {
    /// The writer, and the line being written, kept to reuse its buffer.
    out: Mutex<(W, String)>,
}

impl<W: Write + Send> JsonLines<W> {
    /// Writes to `out`.
    pub fn new(out: W) -> Self {
        JsonLines {
            out: Mutex::new((out, String::new())),
        }
    }

    /// The writer back.
    pub fn into_inner(self) -> W {
        self.out.into_inner().unwrap_or_else(|e| e.into_inner()).0
    }
}

impl<W: Write + Send> TraceSink for JsonLines<W> {
    fn record(&self, record: &TraceRecord) {
        let mut guard = self.out.lock().unwrap_or_else(|e| e.into_inner());
        let (out, line) = &mut *guard;
        line.clear();
        record.write_canonical(line);
        line.push('\n');
        // A trace that cannot be written must not stop the machine.
        let _ = out.write_all(line.as_bytes());
    }
}

/// Keeps every record in memory, for tests.
#[derive(Debug, Default)]
pub struct MemorySink {
    records: Mutex<Vec<TraceRecord>>,
}

impl MemorySink {
    /// An empty sink.
    pub fn new() -> Self {
        Self::default()
    }

    /// The records written so far, in the order written.
    pub fn records(&self) -> Vec<TraceRecord> {
        self.records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

impl TraceSink for MemorySink {
    fn record(&self, record: &TraceRecord) {
        self.records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(record.clone());
    }
}

/// Called with the id and handle of every run as it starts, root and children.
pub type RunObserver = dyn Fn(&str, crate::RunHandle) + Send + Sync;

/// A migration of saved snapshots: the old snapshot to a new one, both plain JSON
/// ([`Config::migrate`]).
pub type Migrate = dyn Fn(Value) -> Result<Value, String> + Send + Sync;

/// How a root run is set up: its trace sink, timer and run id. Child runs inherit their
/// parent's sink and timer.
#[derive(Clone)]
pub struct Config {
    pub(crate) sink: Arc<dyn TraceSink>,
    pub(crate) timer: Arc<dyn Timer>,
    pub(crate) run_id: Option<String>,
    pub(crate) observer: Option<Arc<RunObserver>>,
    pub(crate) migrate: Option<Arc<Migrate>>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            sink: Arc::new(NoTrace),
            #[cfg(feature = "tokio")]
            timer: Arc::new(TokioTimer),
            #[cfg(not(feature = "tokio"))]
            timer: Arc::new(NoTimer),
            run_id: None,
            observer: None,
            migrate: None,
        }
    }
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("run_id", &self.run_id)
            .finish_non_exhaustive()
    }
}

impl Config {
    /// No trace, the Tokio timer (without the `tokio` feature, none), generated run ids.
    pub fn new() -> Self {
        Self::default()
    }

    /// Writes the trace to `sink`.
    pub fn sink(mut self, sink: Arc<dyn TraceSink>) -> Self {
        self.sink = sink;
        self
    }

    /// Reads time from and waits with `timer`.
    pub fn timer(mut self, timer: Arc<dyn Timer>) -> Self {
        self.timer = timer;
        self
    }

    /// Names the root run `id`. Without it the driver names root runs `r1`, `r2`, … in the
    /// order they start in this process.
    ///
    /// # Panics
    ///
    /// If `id` is not a run id ([`crate::is_run_id`]), which the snapshot schema writes as
    /// [`crate::RUN_ID_PATTERN`]: letters, digits, `_` and `-`, optionally followed by `.` and
    /// an invoke id, as in a child run's. A run named `order:42` would save a snapshot that
    /// `restore` refuses as `invalid`, so the name is refused where it is given, rather than
    /// when the saved state is lost. Check an id from outside with `is_run_id` first.
    pub fn run_id(mut self, id: impl Into<String>) -> Self {
        let id = id.into();
        assert!(
            roadbook_core::is_run_id(&id),
            "Config::run_id: {id:?} is not a run id: it must match {}",
            roadbook_core::RUN_ID_PATTERN
        );
        self.run_id = Some(id);
        self
    }

    /// Calls `f` with the id and handle of every run as it starts, the root and every child
    /// run below it. A test uses this to give inputs to a child run directly. A child run's
    /// handle, and the root's for `run` and `run_with`, ignore [`crate::RunHandle::cancel`]
    /// and [`crate::RunHandle::suspend`]: those runs end only by themselves (or, for a child,
    /// when its parent leaves the invoking state).
    pub fn on_run(mut self, f: impl Fn(&str, crate::RunHandle) + Send + Sync + 'static) -> Self {
        self.observer = Some(Arc::new(f));
        self
    }

    /// Restores a saved snapshot whose hash is not the machine's by migrating it with `f`
    /// first: `f` gets the old snapshot as JSON and gives the new one (with the current
    /// hash), or `Err` if it cannot. It is called at most once per restore, and only when the
    /// hashes differ; its result is checked like any snapshot, without a second migration. A
    /// migration that fails (or panics) refuses the snapshot with the reason `migration`.
    pub fn migrate(
        mut self,
        f: impl Fn(Value) -> Result<Value, String> + Send + Sync + 'static,
    ) -> Self {
        self.migrate = Some(Arc::new(f));
        self
    }

    pub(crate) fn root_id(&self) -> String {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        self.run_id
            .clone()
            .unwrap_or_else(|| format!("r{}", NEXT.fetch_add(1, Ordering::Relaxed)))
    }
}
