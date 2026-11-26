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

/// The id of the root run in every conformance run.
pub const ROOT_RUN: &str = "r1";

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
    /// Give the root run this external event, a tagged record.
    Event(Value),
    /// End a running scripted invoke.
    Task(TaskStep),
    /// Cancel the root run from outside.
    Cancel,
    /// Queue all these inputs for one run before it takes the first. Only
    /// [`Step::Event`], [`Step::Task`] and at most one [`Step::Advance`]
    /// appear here; an advance queues the expiries of the timers due in its
    /// window.
    Together(Vec<Step>),
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
    if let Some(e) = m.remove("event") {
        only(&m, &[])?;
        return Ok(Step::Event(e));
    }
    if let Some(c) = m.remove("cancel") {
        only(&m, &[])?;
        return match c {
            Value::Bool(true) => Ok(Step::Cancel),
            _ => Err("`cancel` is `true`".into()),
        };
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
        let run = match m.remove("run") {
            None => None,
            Some(Value::String(r)) => Some(r),
            Some(_) => return Err("`run` is a run id".into()),
        };
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
}

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

/// The schema errors of `doc`, one string each, with where they are.
pub fn schema_errors(v: &jsonschema::Validator, doc: &Value) -> Vec<String> {
    v.iter_errors(doc)
        .map(|e| format!("{e} at {}", e.instance_path))
        .collect()
}

/// `trace` with the `ir` member of every `started` record replaced by that
/// machine's hash in `ir`. This is the only edit `regen-ir.sh` makes to the
/// expected traces. Records of machines missing from `ir` are left alone.
pub fn restamp_hashes(records: &[Value], ir: &Ir) -> Vec<Value> {
    records
        .iter()
        .map(|r| {
            let mut r = r.clone();
            if r.get("kind").and_then(Value::as_str) == Some("started") {
                let machine = r.get("machine").and_then(Value::as_str).unwrap_or("");
                if let Some(m) = ir.machines.get(machine) {
                    r["ir"] = Value::String(m.hash.clone());
                }
            }
            r
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn run_order_is_preorder() {
        let mut ids = vec!["r1.10", "r1.4.2", "r1", "r1.4", "r1.7", "r1.4.10"];
        ids.sort_by(|a, b| compare_run_ids(a, b));
        assert_eq!(ids, ["r1", "r1.4", "r1.4.2", "r1.4.10", "r1.7", "r1.10"]);
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
            {"together": [{"event": {"_tag": "Go"}}, {"advance": 10}]}
        ]))
        .unwrap();
        assert_eq!(s[0], Step::Advance(5));
        assert_eq!(
            s[2],
            Step::Task(TaskStep {
                task: "t".into(),
                run: Some("r1.2".into()),
                outcome: Outcome::Fail(json!({"_tag": "E"}))
            })
        );
        assert!(matches!(&s[4], Step::Together(v) if v.len() == 2));
        for bad in [
            json!({"task": "t"}),
            json!({"task": "t", "done": 1, "fail": {"_tag": "E"}}),
            json!({"advance": -1}),
            json!({"advance": 1, "event": {}}),
            json!({"together": [{"advance": 1}, {"cancel": true}]}),
            json!({"together": [{"advance": 1}, {"advance": 2}]}),
            json!({"cancel": false}),
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
