//! The machine-readable form of diagnostics, as `roadbook check --json` prints it. The shape
//! is documented in `docs/design/language.md` ("Diagnostics as JSON") and is stable: members
//! are in the order below and always present, `null` or `[]` when empty. Lines and columns
//! are 1-based, columns count Unicode scalar values, and the end is exclusive, as in IR spans.
//!
//! ```json
//! {
//!   "diagnostics": [{
//!     "code": "RB0204", "severity": "error", "message": "…",
//!     "file": "examples/shop/checkout.road", "line": 13, "col": 5, "endLine": 13, "endCol": 22,
//!     "label": "declared as `…`",
//!     "labels": [{ "file": "…", "line": 1, "col": 1, "endLine": 1, "endCol": 2, "message": "…" }],
//!     "help": "…",
//!     "fixes": [{ "message": "…", "edits": [{ "file": "…", "line": 1, "col": 1, "endLine": 1, "endCol": 1, "text": "…" }] }]
//!   }],
//!   "errors": 1,
//!   "warnings": 0,
//!   "fatal": null,
//!   "checks": [{
//!     "machine": "Pay", "check": "reachable Charged", "doc": null,
//!     "file": "pay.road", "line": 30, "col": 3, "endLine": 30, "endCol": 26,
//!     "verdict": "conditional",
//!     "assumptions": ["`ok()` holds"],
//!     "path": [{ "from": "Pay.Idle", "index": 0, "trigger": "Go", "guard": "ok()", "to": "Pay.Charged" }]
//!   }]
//! }
//! ```
//!
//! `checks` lists every `check` line `roadbook check` evaluated, with its verdict:
//! `proved`, `conditional`, `failed` (also an error among the diagnostics) or `unchecked`
//! (with the warning `RB0910`). `assumptions` are what its path relies on that the chart does
//! not decide (guard outcomes, which timer expires first, what other machines do, and that a
//! step comes before another machine that could act then), and `path`
//! the transitions of that path from the initial configuration, shaped as the steps of
//! `roadbook paths --json`: the counterexample of a failed check, the witness of a
//! `reachable` that holds, and for a conditional `can finish`, the way to the first
//! configuration from which the host cannot force the end and on to a final state; `[]` when
//! there is none. It lists the transitions taken, leaving out the steps that take none. The
//! checks of an entry with compile errors are not evaluated, so not listed. Checks cover runs started from the chart, not runs restored from a snapshot of an
//! earlier version of it.

use roadbook_syntax::diagnostic::Files;

use crate::properties::CheckResult;
use roadbook_syntax::{Diagnostic, Severity, Span};
use serde::Serialize;
use serde_json::Value;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Range<'a> {
    file: &'a str,
    line: u32,
    col: u32,
    end_line: u32,
    end_col: u32,
}

fn range(span: Span, files: &[(String, String)]) -> Range<'_> {
    Range {
        file: files.name(span.file),
        line: span.start.line,
        col: span.start.col,
        end_line: span.end.line,
        end_col: span.end.col,
    }
}

#[derive(Serialize)]
struct Label<'a> {
    #[serde(flatten)]
    range: Range<'a>,
    message: &'a str,
}

#[derive(Serialize)]
struct Edit<'a> {
    #[serde(flatten)]
    range: Range<'a>,
    text: &'a str,
}

#[derive(Serialize)]
struct Fix<'a> {
    message: &'a str,
    edits: Vec<Edit<'a>>,
}

#[derive(Serialize)]
struct Diag<'a> {
    code: String,
    severity: &'static str,
    message: &'a str,
    #[serde(flatten)]
    range: Range<'a>,
    label: Option<&'a str>,
    labels: Vec<Label<'a>>,
    help: Option<&'a str>,
    fixes: Vec<Fix<'a>>,
}

#[derive(Serialize)]
struct Report<'a> {
    diagnostics: Vec<Diag<'a>>,
    errors: usize,
    warnings: usize,
    /// A usage or I/O error that stopped the check, such as a file that cannot be read.
    fatal: Option<&'a str>,
    checks: Vec<CheckJson<'a>>,
}

#[derive(Serialize)]
struct CheckJson<'a> {
    machine: &'a str,
    check: &'a str,
    doc: Option<&'a str>,
    #[serde(flatten)]
    range: Range<'a>,
    verdict: &'static str,
    assumptions: &'a [String],
    path: Vec<StepJson<'a>>,
}

#[derive(Serialize)]
struct StepJson<'a> {
    from: &'a str,
    index: usize,
    trigger: &'a str,
    guard: Option<&'a str>,
    to: Option<&'a str>,
}

fn check<'a>(c: &'a CheckResult, files: &'a [(String, String)]) -> CheckJson<'a> {
    CheckJson {
        machine: &c.machine,
        check: &c.check,
        doc: (!c.doc.is_empty()).then_some(c.doc.as_str()),
        range: range(c.span, files),
        verdict: c.verdict.as_str(),
        assumptions: &c.assumptions,
        path: c
            .path
            .iter()
            .map(|s| StepJson {
                from: &s.from,
                index: s.index,
                trigger: &s.trigger,
                guard: s.guard.as_deref(),
                to: s.to.as_deref(),
            })
            .collect(),
    }
}

fn diag<'a>(d: &'a Diagnostic, files: &'a [(String, String)]) -> Diag<'a> {
    Diag {
        code: d.code.to_string(),
        severity: match d.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        },
        message: &d.message,
        range: range(d.span, files),
        label: d.label.as_deref(),
        labels: d
            .labels
            .iter()
            .map(|l| Label {
                range: range(l.span, files),
                message: &l.message,
            })
            .collect(),
        help: d.help.as_deref(),
        fixes: d
            .fixes
            .iter()
            .map(|f| Fix {
                message: &f.message,
                edits: f
                    .edits
                    .iter()
                    .map(|e| Edit {
                        range: range(e.span, files),
                        text: &e.text,
                    })
                    .collect(),
            })
            .collect(),
    }
}

/// One diagnostic as a JSON object.
pub fn diagnostic(d: &Diagnostic, files: &[(String, String)]) -> Value {
    serde_json::to_value(diag(d, files)).unwrap_or(Value::Null)
}

/// Every diagnostic, with the error and warning counts, and the verdict of every check, as
/// one JSON object.
pub fn diagnostics(
    diags: &[Diagnostic],
    checks: &[CheckResult],
    files: &[(String, String)],
) -> Value {
    serde_json::to_value(report(diags, checks, files)).unwrap_or(Value::Null)
}

/// The same as [`diagnostics`], as text with members in the documented order.
pub fn diagnostics_string(
    diags: &[Diagnostic],
    checks: &[CheckResult],
    files: &[(String, String)],
) -> String {
    serde_json::to_string_pretty(&report(diags, checks, files)).unwrap_or_default()
}

fn report<'a>(
    diags: &'a [Diagnostic],
    checks: &'a [CheckResult],
    files: &'a [(String, String)],
) -> Report<'a> {
    let errors = diags.iter().filter(|d| d.is_error()).count();
    Report {
        diagnostics: diags.iter().map(|d| diag(d, files)).collect(),
        errors,
        warnings: diags.len() - errors,
        fatal: None,
        checks: checks.iter().map(|c| check(c, files)).collect(),
    }
}

/// The object printed when a usage or I/O error stops the check before anything is
/// compiled: no diagnostics and no checks, and the error's message in `fatal`.
pub fn fatal_string(message: &str) -> String {
    let report = Report {
        diagnostics: Vec::new(),
        errors: 0,
        warnings: 0,
        fatal: Some(message),
        checks: Vec::new(),
    };
    serde_json::to_string_pretty(&report).unwrap_or_default()
}
