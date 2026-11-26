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
//!   "fatal": null
//! }
//! ```

use roadbook_syntax::diagnostic::Files;
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

/// Every diagnostic, with the error and warning counts, as one JSON object.
pub fn diagnostics(diags: &[Diagnostic], files: &[(String, String)]) -> Value {
    serde_json::to_value(report(diags, files)).unwrap_or(Value::Null)
}

/// The same as [`diagnostics`], as text with members in the documented order.
pub fn diagnostics_string(diags: &[Diagnostic], files: &[(String, String)]) -> String {
    serde_json::to_string_pretty(&report(diags, files)).unwrap_or_default()
}

fn report<'a>(diags: &'a [Diagnostic], files: &'a [(String, String)]) -> Report<'a> {
    let errors = diags.iter().filter(|d| d.is_error()).count();
    Report {
        diagnostics: diags.iter().map(|d| diag(d, files)).collect(),
        errors,
        warnings: diags.len() - errors,
        fatal: None,
    }
}

/// The object printed when a usage or I/O error stops the check before anything is
/// compiled: no diagnostics, and the error's message in `fatal`.
pub fn fatal_string(message: &str) -> String {
    let report = Report {
        diagnostics: Vec::new(),
        errors: 0,
        warnings: 0,
        fatal: Some(message),
    };
    serde_json::to_string_pretty(&report).unwrap_or_default()
}
