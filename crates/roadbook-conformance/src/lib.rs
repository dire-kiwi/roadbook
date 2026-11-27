//! The Roadbook conformance cases.
//!
//! `spec/conformance/` holds charts, scripts and expected traces that every
//! runtime must reproduce byte for byte (see `spec/conformance/README.md`).
//! This crate reads them, checks them against their schemas, and gives a
//! runtime's runner what it needs:
//!
//! - [`Case`] and [`Step`], the contents of a `case.json`;
//! - [`LoadedCase`], a case with its expected trace and, once the compiler
//!   has generated it, its IR ([`load_case`], [`load_all`]);
//! - [`FnTable`], the scripted results of the host's pure functions, and
//!   [`ActionTable`], the action calls that have a host defect;
//! - [`arrange`] and [`to_lines`], which put a runtime's trace records in the
//!   order and the canonical form the expected traces use;
//! - [`lint`], structural checks of a trace that need no runtime;
//! - the two schemas, compiled ([`case_validator`], [`trace_validator`]).

use std::cmp::Ordering;
use std::fmt;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use roadbook_ir::Ir;
use serde::Deserialize;
use serde::de::Error as _;
use serde_json::Value;

pub mod lint;

/// The schema of `case.json`, `spec/conformance/case.schema.json`.
pub const CASE_SCHEMA: &str = include_str!("../../../spec/conformance/case.schema.json");

/// The schema of one trace record, `spec/trace.schema.json`.
pub const TRACE_SCHEMA: &str = include_str!("../../../spec/trace.schema.json");

/// The schema of a saved snapshot, `spec/snapshot.schema.json`.
pub const SNAPSHOT_SCHEMA: &str = include_str!("../../../spec/snapshot.schema.json");

/// The id of the root run in every conformance run: the case's `machine`,
/// started first. Runs started by [`Step::Start`] are `r2`, `r3`, ….
pub const ROOT_RUN: &str = "r1";

/// The features a case can require beyond the first version's: a runner
/// skips a case that requires one it does not implement.
pub const FEATURES: &[&str] = &[
    "messaging",
    "parallel",
    "snapshots",
    "calls",
    "history",
    "spawn",
];

/// `spec/conformance` in this repository.
pub fn conformance_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spec/conformance")
}

/// A `case.json`. Every member is required.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    /// What the case checks.
    pub description: String,
    /// The features it needs beyond the first version's (see [`FEATURES`]).
    pub requires: Vec<String>,
    /// The entry chart, relative to `spec/conformance`.
    pub chart: String,
    /// The machine run as the root run.
    pub machine: String,
    /// The machine's arguments, keyed by parameter name.
    pub input: Value,
    /// Machines scripted like tasks instead of run from the chart.
    pub stub: Vec<String>,
    /// Scripted results of the host's pure functions.
    pub fns: FnTable,
    /// The action calls that have a host defect.
    pub actions: ActionTable,
    /// The steps, in order.
    pub script: Vec<Step>,
    /// The root run's status after the script.
    pub status: Status,
}

impl Case {
    /// Whether invoking machine `name` is scripted rather than run from the
    /// chart.
    pub fn is_stubbed(&self, name: &str) -> bool {
        self.stub.iter().any(|s| s == name)
    }

    /// Whether the case needs a feature that is not in `supported`: a
    /// runner that implements only `supported` skips it.
    pub fn needs_more_than(&self, supported: &[&str]) -> bool {
        self.requires
            .iter()
            .any(|f| !supported.contains(&f.as_str()))
    }

    /// Every top-level run the case starts from its machine: the root run
    /// `r1` first, then each [`Step::Start`] in order, as `(run, machine,
    /// input, status)`. Runs restored from snapshots are in
    /// [`Case::top_level_statuses`].
    pub fn top_level_runs(&self) -> Vec<(String, &str, &Value, Status)> {
        let mut runs = vec![(
            ROOT_RUN.to_owned(),
            self.machine.as_str(),
            &self.input,
            self.status,
        )];
        for step in &self.script {
            if let Step::Start(s) = step {
                runs.push((s.run.clone(), s.machine.as_str(), &s.input, s.status));
            }
        }
        runs
    }

    /// The id and expected status of every top-level run, in the order the
    /// script starts them: the root run, then each [`Step::Start`] and each
    /// [`Step::Restore`] that is not refused.
    pub fn top_level_statuses(&self) -> Vec<(String, Status)> {
        let mut runs = vec![(ROOT_RUN.to_owned(), self.status)];
        for step in &self.script {
            match step {
                Step::Start(s) => runs.push((s.run.clone(), s.status)),
                Step::Restore(RestoreStep {
                    run: Some(run),
                    status: Some(status),
                    ..
                }) => runs.push((run.clone(), *status)),
                _ => {}
            }
        }
        runs
    }

    /// The names of the snapshot files the script uses, each once: the
    /// first member says whether a runtime writes it or a migration gives
    /// it (a `snapshot` or `suspend` step, a `migrate`), so that its hash is
    /// the chart's current one.
    pub fn snapshot_names(&self) -> Vec<(String, bool)> {
        let mut names: Vec<(String, bool)> = Vec::new();
        let mut add = |n: &str, current: bool| match names.iter_mut().find(|(m, _)| m == n) {
            Some(e) => e.1 |= current,
            None => names.push((n.to_owned(), current)),
        };
        for step in &self.script {
            match step {
                Step::Snapshot(s) | Step::Suspend(s) => add(&s.name, true),
                Step::Restore(r) => {
                    add(&r.name, false);
                    if let Some(m) = &r.migrate {
                        add(m, true);
                    }
                }
                _ => {}
            }
        }
        names
    }
}

/// A run's status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Still running when the script ends.
    Running,
    /// Reached a final state with an output.
    Done,
    /// Reached a final state with `fail`.
    Failed,
    /// Cancelled from outside.
    Cancelled,
    /// Ended with a defect.
    Defect,
}

impl Status {
    /// The trace record kind that ends a run with this status, or `None`
    /// for [`Status::Running`].
    pub fn final_record(self) -> Option<&'static str> {
        match self {
            Status::Running => None,
            Status::Done => Some("done"),
            Status::Failed => Some("failed"),
            Status::Cancelled => Some("cancelled"),
            Status::Defect => Some("defect"),
        }
    }
}

/// One step of a script.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// Advance virtual time by this many milliseconds, firing due timers one
    /// at a time.
    Advance(u64),
    /// Give a top-level run an external event, as host code does.
    Event(EventStep),
    /// End a running scripted invoke.
    Task(TaskStep),
    /// Cancel a top-level run from outside: the root run when `None`.
    Cancel(Option<String>),
    /// Start another top-level run.
    Start(StartStep),
    /// Queue all these inputs for one run before it takes the first. Only
    /// [`Step::Event`], [`Step::Task`] and at most one [`Step::Advance`]
    /// appear here; an advance queues the expiries of the timers due in its
    /// window.
    Together(Vec<Step>),
    /// Save a top-level run and compare its saved snapshot with a file.
    Snapshot(SnapshotStep),
    /// Restore a saved snapshot as a new top-level run, or check that the
    /// restore is refused.
    Restore(RestoreStep),
    /// Suspend a top-level run and, once it is idle or has ended, compare
    /// the saved snapshot it gives with a file.
    Suspend(SnapshotStep),
}

/// `{"snapshot": …}` or `{"suspend": …}`: a top-level run and the snapshot
/// file to compare its saved snapshot with.
#[derive(Debug, Clone, PartialEq)]
pub struct SnapshotStep {
    /// The snapshot's name: the file `<name>.snapshot.json` in the case's
    /// directory.
    pub name: String,
    /// The run, a top-level run id: the root run when `None`.
    pub run: Option<String>,
}

impl SnapshotStep {
    /// The run the step is about.
    pub fn run(&self) -> &str {
        self.run.as_deref().unwrap_or(ROOT_RUN)
    }
}

/// `{"restore": …}`: restore a saved snapshot as a new top-level run.
#[derive(Debug, Clone, PartialEq)]
pub struct RestoreStep {
    /// The snapshot to restore: the file `<name>.snapshot.json`.
    pub name: String,
    /// The new run's id; `None` exactly when `refused` is set.
    pub run: Option<String>,
    /// The new run's status after the last step; `None` exactly when
    /// `refused` is set.
    pub status: Option<Status>,
    /// The snapshot the host's migration returns, if the host gives one.
    pub migrate: Option<String>,
    /// The reason the restore must be refused for, if it must be.
    pub refused: Option<String>,
}

/// `{"event": …}`: give a top-level run an external event.
#[derive(Debug, Clone, PartialEq)]
pub struct EventStep {
    /// The run, a top-level run id: the root run when `None`.
    pub run: Option<String>,
    /// The event, a tagged record.
    pub event: Value,
}

impl EventStep {
    /// The run the event is for.
    pub fn run(&self) -> &str {
        self.run.as_deref().unwrap_or(ROOT_RUN)
    }
}

/// `{"start": …}`: start another top-level run, as host code spawns one.
#[derive(Debug, Clone, PartialEq)]
pub struct StartStep {
    /// The machine.
    pub machine: String,
    /// Its id: `r2` for the first start step, `r3` for the next, and so on.
    pub run: String,
    /// Its arguments. A `ref` argument is the id of a top-level run already
    /// started.
    pub input: Value,
    /// Its status after the last step.
    pub status: Status,
}

/// `{"task": …}`: end the running scripted invoke of a task or stubbed
/// machine.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskStep {
    /// The task's or stubbed machine's name.
    pub task: String,
    /// The run whose invoke is meant, when several runs have one.
    pub run: Option<String>,
    /// How it ends.
    pub outcome: Outcome,
}

/// How a scripted invoke ends.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Success with this value: the input `invoke.done`.
    Done(Value),
    /// This declared error, a tagged record: the input `invoke.fail`.
    Fail(Value),
    /// A host defect with this message: the input `invoke.defect` with
    /// `{"kind": "host", "message": …}`.
    Defect(String),
}

impl<'de> Deserialize<'de> for Step {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        step_from_value(Value::deserialize(d)?).map_err(D::Error::custom)
    }
}

fn step_from_value(v: Value) -> Result<Step, String> {
    let Value::Object(mut m) = v else {
        return Err("a script step is an object".into());
    };
    let only = |m: &serde_json::Map<String, Value>, allowed: &[&str]| -> Result<(), String> {
        match m.keys().find(|k| !allowed.contains(&k.as_str())) {
            Some(k) => Err(format!("unexpected member `{k}` in a script step")),
            None => Ok(()),
        }
    };
    if let Some(a) = m.remove("advance") {
        only(&m, &[])?;
        return a
            .as_u64()
            .map(Step::Advance)
            .ok_or_else(|| "`advance` is a non-negative integer".into());
    }
    let take_run = |m: &mut serde_json::Map<String, Value>| match m.remove("run") {
        None => Ok(None),
        Some(Value::String(r)) => Ok(Some(r)),
        Some(_) => Err("`run` is a run id".to_owned()),
    };
    if let Some(event) = m.remove("event") {
        only(&m, &["run"])?;
        let run = take_run(&mut m)?;
        return Ok(Step::Event(EventStep { run, event }));
    }
    if let Some(c) = m.remove("cancel") {
        only(&m, &["run"])?;
        let run = take_run(&mut m)?;
        return match c {
            Value::Bool(true) => Ok(Step::Cancel(run)),
            _ => Err("`cancel` is `true`".into()),
        };
    }
    if let Some(machine) = m.remove("start") {
        only(&m, &["run", "input", "status"])?;
        let Value::String(machine) = machine else {
            return Err("`start` is a machine name".into());
        };
        let run = take_run(&mut m)?.ok_or("a start step has a `run`")?;
        let input = m.remove("input").ok_or("a start step has an `input`")?;
        let status = m.remove("status").ok_or("a start step has a `status`")?;
        let status = Status::deserialize(status).map_err(|e| e.to_string())?;
        return Ok(Step::Start(StartStep {
            machine,
            run,
            input,
            status,
        }));
    }
    let take_name = |v: Value, what: &str| match v {
        Value::String(n) => Ok(n),
        _ => Err(format!("`{what}` is a snapshot name")),
    };
    for what in ["snapshot", "suspend"] {
        if let Some(n) = m.remove(what) {
            only(&m, &["run"])?;
            let name = take_name(n, what)?;
            let run = take_run(&mut m)?;
            let step = SnapshotStep { name, run };
            return Ok(if what == "snapshot" {
                Step::Snapshot(step)
            } else {
                Step::Suspend(step)
            });
        }
    }
    if let Some(n) = m.remove("restore") {
        only(&m, &["run", "status", "migrate", "refused"])?;
        let name = take_name(n, "restore")?;
        let run = take_run(&mut m)?;
        let status = m
            .remove("status")
            .map(|s| Status::deserialize(s).map_err(|e| e.to_string()))
            .transpose()?;
        let migrate = m
            .remove("migrate")
            .map(|v| take_name(v, "migrate"))
            .transpose()?;
        let refused = m
            .remove("refused")
            .map(|v| match v {
                Value::String(r) => Ok(r),
                _ => Err("`refused` is a reason".to_owned()),
            })
            .transpose()?;
        if refused.is_some() != (run.is_none() && status.is_none())
            || run.is_some() != status.is_some()
        {
            return Err("a restore step has `run` and `status`, or `refused` and neither".into());
        }
        return Ok(Step::Restore(RestoreStep {
            name,
            run,
            status,
            migrate,
            refused,
        }));
    }
    if let Some(t) = m.remove("together") {
        only(&m, &[])?;
        let Value::Array(items) = t else {
            return Err("`together` is a list of steps".into());
        };
        let steps = items
            .into_iter()
            .map(step_from_value)
            .collect::<Result<Vec<_>, _>>()?;
        if steps.len() < 2 {
            return Err("`together` has at least two steps".into());
        }
        if !steps
            .iter()
            .all(|s| matches!(s, Step::Event(_) | Step::Task(_) | Step::Advance(_)))
        {
            return Err("`together` holds only `event`, `task` and `advance` steps".into());
        }
        if steps
            .iter()
            .filter(|s| matches!(s, Step::Advance(_)))
            .count()
            > 1
        {
            return Err("`together` holds at most one `advance`".into());
        }
        return Ok(Step::Together(steps));
    }
    if let Some(task) = m.remove("task") {
        only(&m, &["run", "done", "fail", "defect"])?;
        let Value::String(task) = task else {
            return Err("`task` is a name".into());
        };
        let run = take_run(&mut m)?;
        let mut outcomes = Vec::new();
        if let Some(v) = m.remove("done") {
            outcomes.push(Outcome::Done(v));
        }
        if let Some(v) = m.remove("fail") {
            outcomes.push(Outcome::Fail(v));
        }
        if let Some(v) = m.remove("defect") {
            match v {
                Value::String(s) => outcomes.push(Outcome::Defect(s)),
                _ => return Err("`defect` is a message".into()),
            }
        }
        return match <[Outcome; 1]>::try_from(outcomes) {
            Ok([outcome]) => Ok(Step::Task(TaskStep { task, run, outcome })),
            Err(_) => Err("a task step has exactly one of `done`, `fail` and `defect`".into()),
        };
    }
    Err("unknown script step".into())
}

/// The scripted results of the host's pure functions: for each function, a
/// list of argument lists and results.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(transparent)]
pub struct FnTable(pub IndexMap<String, Vec<FnResult>>);

/// One entry of an [`FnTable`].
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FnResult {
    /// Positional argument values.
    pub args: Vec<Value>,
    /// The function's value for them.
    pub returns: Value,
}

impl FnTable {
    /// The result of calling `name` with `args`, comparing argument lists as
    /// canonical JSON. `None` means the call is not in the table, which fails
    /// the case.
    pub fn call(&self, name: &str, args: &[Value]) -> Option<&Value> {
        let want = roadbook_ir::jcs::to_canonical_string(&Value::Array(args.to_vec()));
        self.0.get(name)?.iter().find_map(|r| {
            let have = roadbook_ir::jcs::to_canonical_string(&Value::Array(r.args.clone()));
            (have == want).then_some(&r.returns)
        })
    }
}

/// The action calls that have a host defect: for each action, a list of
/// argument objects and defect messages. Every other call does nothing.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(transparent)]
pub struct ActionTable(pub IndexMap<String, Vec<ActionDefect>>);

/// One entry of an [`ActionTable`].
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionDefect {
    /// The arguments, keyed by parameter name.
    pub args: Value,
    /// The message of the host defect a matching call has.
    pub defect: String,
}

impl ActionTable {
    /// The message of the host defect that calling action `name` with `args`
    /// (keyed by parameter name) has, comparing arguments as canonical JSON.
    /// `None` means the call does nothing and returns at once.
    pub fn defect(&self, name: &str, args: &Value) -> Option<&str> {
        let want = roadbook_ir::jcs::to_canonical_string(args);
        self.0.get(name)?.iter().find_map(|a| {
            (roadbook_ir::jcs::to_canonical_string(&a.args) == want).then_some(a.defect.as_str())
        })
    }
}

/// Compares run ids in trace order: segment by segment at the dots, the
/// first segment as a string and the others as numbers, a prefix first.
/// This is a pre-order walk of the run tree.
pub fn compare_run_ids(a: &str, b: &str) -> Ordering {
    let mut a = a.split('.');
    let mut b = b.split('.');
    let (a0, b0) = (a.next().unwrap_or(""), b.next().unwrap_or(""));
    a0.cmp(b0).then_with(|| {
        let num = |s: &str| s.parse::<u64>().unwrap_or(u64::MAX);
        a.map(num).cmp(b.map(num))
    })
}

/// Puts trace records in the order conformance compares: grouped by run,
/// the groups in [`compare_run_ids`] order, each group in its original
/// order. A record without a string `run` sorts as the empty id.
pub fn arrange(records: Vec<Value>) -> Vec<Value> {
    let mut indexed: Vec<(usize, Value)> = records.into_iter().enumerate().collect();
    indexed.sort_by(|(i, a), (j, b)| compare_run_ids(run_of(a), run_of(b)).then_with(|| i.cmp(j)));
    indexed.into_iter().map(|(_, r)| r).collect()
}

fn run_of(record: &Value) -> &str {
    record.get("run").and_then(Value::as_str).unwrap_or("")
}

/// The trace file form of `records`: each in canonical JSON followed by a
/// line feed.
pub fn to_lines(records: &[Value]) -> String {
    let mut out = String::new();
    for r in records {
        out.push_str(&roadbook_ir::jcs::to_canonical_string(r));
        out.push('\n');
    }
    out
}

/// A case read from disk.
#[derive(Debug, Clone)]
pub struct LoadedCase {
    /// The case directory's name.
    pub name: String,
    /// The case directory.
    pub dir: PathBuf,
    /// The parsed `case.json`.
    pub case: Case,
    /// `case.json` as JSON, for schema validation.
    pub case_json: Value,
    /// Where the chart's IR is (or will be once generated).
    pub ir_path: PathBuf,
    /// The chart's IR, or `None` if it has not been generated yet.
    pub ir: Option<Ir>,
    /// `trace.jsonl` exactly as committed.
    pub expected: String,
    /// Its records, one per line.
    pub records: Vec<Value>,
    /// The snapshot files in the case's directory, by name (the file name
    /// without `.snapshot.json`), exactly as committed.
    pub snapshots: std::collections::BTreeMap<String, String>,
}

/// The suffix of a snapshot file's name.
pub const SNAPSHOT_SUFFIX: &str = ".snapshot.json";

impl LoadedCase {
    /// The chart's path.
    pub fn chart_path(&self) -> PathBuf {
        self.dir
            .parent()
            .and_then(Path::parent)
            .unwrap_or(&self.dir)
            .join(&self.case.chart)
    }
}

/// Why a case could not be loaded.
#[derive(Debug)]
pub struct LoadError {
    /// The file concerned.
    pub path: PathBuf,
    /// What is wrong.
    pub message: String,
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path.display(), self.message)
    }
}

impl std::error::Error for LoadError {}

fn read(path: &Path) -> Result<String, LoadError> {
    std::fs::read_to_string(path).map_err(|e| LoadError {
        path: path.to_owned(),
        message: e.to_string(),
    })
}

/// The IR file of a chart: the chart's path with `.road` replaced by
/// `.ir.json`.
pub fn ir_path_for(chart: &Path) -> PathBuf {
    chart.with_extension("ir.json")
}

/// Loads one case directory under `spec/conformance/cases`. A missing IR
/// file is not an error: [`LoadedCase::ir`] is then `None`.
pub fn load_case(dir: &Path) -> Result<LoadedCase, LoadError> {
    let err = |path: &Path, message: String| LoadError {
        path: path.to_owned(),
        message,
    };
    let case_path = dir.join("case.json");
    let text = read(&case_path)?;
    let case_json: Value =
        serde_json::from_str(&text).map_err(|e| err(&case_path, e.to_string()))?;
    let case: Case =
        serde_json::from_value(case_json.clone()).map_err(|e| err(&case_path, e.to_string()))?;

    let root = dir.parent().and_then(Path::parent).unwrap_or(dir);
    let chart = root.join(&case.chart);
    let ir_path = ir_path_for(&chart);
    let ir = match std::fs::read_to_string(&ir_path) {
        Ok(json) => Some(Ir::from_json(&json).map_err(|e| err(&ir_path, e.to_string()))?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(err(&ir_path, e.to_string())),
    };

    let trace_path = dir.join("trace.jsonl");
    let expected = read(&trace_path)?;
    let records = expected
        .lines()
        .enumerate()
        .map(|(i, line)| {
            serde_json::from_str(line).map_err(|e| err(&trace_path, format!("line {}: {e}", i + 1)))
        })
        .collect::<Result<Vec<Value>, _>>()?;

    let mut snapshots = std::collections::BTreeMap::new();
    let entries = std::fs::read_dir(dir).map_err(|e| err(dir, e.to_string()))?;
    for entry in entries.filter_map(Result::ok) {
        let file = entry.file_name().to_string_lossy().into_owned();
        if let Some(name) = file.strip_suffix(SNAPSHOT_SUFFIX) {
            snapshots.insert(name.to_owned(), read(&entry.path())?);
        }
    }

    Ok(LoadedCase {
        name: dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        dir: dir.to_owned(),
        case,
        case_json,
        ir_path,
        ir,
        expected,
        records,
        snapshots,
    })
}

/// Loads every case under `conformance_dir/cases`, in name order.
pub fn load_all(conformance_dir: &Path) -> Result<Vec<LoadedCase>, LoadError> {
    let cases = conformance_dir.join("cases");
    let entries = std::fs::read_dir(&cases).map_err(|e| LoadError {
        path: cases.clone(),
        message: e.to_string(),
    })?;
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs.iter().map(|d| load_case(d)).collect()
}

fn compile_schema(text: &str) -> jsonschema::Validator {
    let schema: Value = serde_json::from_str(text).expect("the committed schema is valid JSON");
    jsonschema::draft202012::new(&schema).expect("the committed schema compiles")
}

/// The compiled `case.schema.json`.
pub fn case_validator() -> jsonschema::Validator {
    compile_schema(CASE_SCHEMA)
}

/// The compiled `trace.schema.json`.
pub fn trace_validator() -> jsonschema::Validator {
    compile_schema(TRACE_SCHEMA)
}

/// The compiled `snapshot.schema.json`.
pub fn snapshot_validator() -> jsonschema::Validator {
    compile_schema(SNAPSHOT_SCHEMA)
}

/// The file form of a saved snapshot: canonical JSON and a line feed.
pub fn snapshot_text(snapshot: &Value) -> String {
    let mut s = roadbook_ir::jcs::to_canonical_string(snapshot);
    s.push('\n');
    s
}

/// The schema errors of `doc`, one string each, with where they are.
pub fn schema_errors(v: &jsonschema::Validator, doc: &Value) -> Vec<String> {
    v.iter_errors(doc)
        .map(|e| format!("{e} at {}", e.instance_path))
        .collect()
}

/// `trace` with the `ir` member of every `started` and `restored` record,
/// and the `snapshot.ir` member of every `restored` record, replaced by that
/// machine's hash in `ir`. This is the only edit `regen-ir.sh` makes to the
/// expected traces. Records of machines missing from `ir` are left alone.
pub fn restamp_hashes(records: &[Value], ir: &Ir) -> Vec<Value> {
    records
        .iter()
        .map(|r| {
            let mut r = r.clone();
            let kind = r
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            if kind == "started" || kind == "restored" {
                let machine = r.get("machine").and_then(Value::as_str).unwrap_or("");
                if let Some(m) = ir.machines.get(machine) {
                    r["ir"] = Value::String(m.hash.clone());
                    if kind == "restored" {
                        r["snapshot"]["ir"] = Value::String(m.hash.clone());
                    }
                }
            }
            r
        })
        .collect()
}

/// A saved snapshot with its `ir` member replaced by its machine's hash in
/// `ir`, for the snapshot files a runtime writes or a migration gives.
pub fn restamp_snapshot(snapshot: &Value, ir: &Ir) -> Value {
    let mut s = snapshot.clone();
    let machine = s.get("machine").and_then(Value::as_str).unwrap_or("");
    if let Some(m) = ir.machines.get(machine) {
        s["ir"] = Value::String(m.hash.clone());
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn run_order_is_preorder() {
        let mut ids = vec![
            "r1.10", "r2", "r1.4.2", "r1", "r2.1", "r1.4", "r1.7", "r1.4.10",
        ];
        ids.sort_by(|a, b| compare_run_ids(a, b));
        assert_eq!(
            ids,
            [
                "r1", "r1.4", "r1.4.2", "r1.4.10", "r1.7", "r1.10", "r2", "r2.1"
            ]
        );
    }

    #[test]
    fn arrange_groups_by_run_and_keeps_order() {
        let recs = vec![
            json!({"run": "r1", "n": 1}),
            json!({"run": "r1.2", "n": 2}),
            json!({"run": "r1", "n": 3}),
            json!({"run": "r1.2", "n": 4}),
        ];
        let n: Vec<_> = arrange(recs).iter().map(|r| r["n"].clone()).collect();
        assert_eq!(n, [1, 3, 2, 4]);
    }

    #[test]
    fn steps() {
        let s: Vec<Step> = serde_json::from_value(json!([
            {"advance": 5},
            {"event": {"_tag": "Go"}},
            {"task": "t", "fail": {"_tag": "E"}, "run": "r1.2"},
            {"cancel": true},
            {"together": [{"event": {"_tag": "Go"}}, {"task": "t", "done": null}]},
            {"together": [{"event": {"_tag": "Go"}}, {"advance": 10}]},
            {"event": {"_tag": "Go"}, "run": "r2"},
            {"cancel": true, "run": "r2"},
            {"start": "Order", "run": "r2", "input": {"inventory": "r1"}, "status": "done"}
        ]))
        .unwrap();
        assert_eq!(s[0], Step::Advance(5));
        assert_eq!(
            s[6],
            Step::Event(EventStep {
                run: Some("r2".into()),
                event: json!({"_tag": "Go"})
            })
        );
        assert_eq!(s[7], Step::Cancel(Some("r2".into())));
        assert_eq!(
            s[8],
            Step::Start(StartStep {
                machine: "Order".into(),
                run: "r2".into(),
                input: json!({"inventory": "r1"}),
                status: Status::Done
            })
        );
        assert_eq!(
            s[2],
            Step::Task(TaskStep {
                task: "t".into(),
                run: Some("r1.2".into()),
                outcome: Outcome::Fail(json!({"_tag": "E"}))
            })
        );
        assert!(matches!(&s[4], Step::Together(v) if v.len() == 2));
        let saved: Vec<Step> = serde_json::from_value(json!([
            {"snapshot": "s1"},
            {"suspend": "s2", "run": "r2"},
            {"restore": "s1", "run": "r3", "status": "running", "migrate": "s3"},
            {"restore": "s1", "refused": "ir"}
        ]))
        .unwrap();
        assert_eq!(
            saved[1],
            Step::Suspend(SnapshotStep {
                name: "s2".into(),
                run: Some("r2".into())
            })
        );
        assert_eq!(
            saved[2],
            Step::Restore(RestoreStep {
                name: "s1".into(),
                run: Some("r3".into()),
                status: Some(Status::Running),
                migrate: Some("s3".into()),
                refused: None
            })
        );
        assert!(matches!(&saved[3], Step::Restore(r) if r.refused.as_deref() == Some("ir")));
        for bad in [
            json!({"task": "t"}),
            json!({"task": "t", "done": 1, "fail": {"_tag": "E"}}),
            json!({"advance": -1}),
            json!({"advance": 1, "event": {}}),
            json!({"together": [{"advance": 1}, {"cancel": true}]}),
            json!({"together": [{"advance": 1}, {"advance": 2}]}),
            json!({"cancel": false}),
            json!({"start": "Order", "input": {}, "status": "done"}),
            json!({"start": "Order", "run": "r2", "input": {}, "status": "over"}),
            json!({"together": [{"advance": 1}, {"start": "A", "run": "r2", "input": {}, "status": "done"}]}),
            json!({"restore": "s1"}),
            json!({"restore": "s1", "run": "r2"}),
            json!({"restore": "s1", "run": "r2", "status": "done", "refused": "ir"}),
            json!({"snapshot": 1}),
            json!({"suspend": "s1", "status": "done"}),
        ] {
            assert!(
                serde_json::from_value::<Step>(bad.clone()).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn fn_table_matches_canonically() {
        let t: FnTable = serde_json::from_value(json!({
            "f": [{"args": [{"b": 1, "a": 2.0}], "returns": "x"}]
        }))
        .unwrap();
        assert_eq!(t.call("f", &[json!({"a": 2, "b": 1})]), Some(&json!("x")));
        assert_eq!(t.call("f", &[json!({"a": 3, "b": 1})]), None);
        assert_eq!(t.call("g", &[]), None);
    }

    #[test]
    fn action_table_matches_canonically() {
        let t: ActionTable = serde_json::from_value(json!({
            "log": [{"args": {"what": "x", "n": 1.0}, "defect": "boom"}]
        }))
        .unwrap();
        assert_eq!(t.defect("log", &json!({"n": 1, "what": "x"})), Some("boom"));
        assert_eq!(t.defect("log", &json!({"n": 2, "what": "x"})), None);
        assert_eq!(t.defect("audit", &json!({})), None);
    }
}
