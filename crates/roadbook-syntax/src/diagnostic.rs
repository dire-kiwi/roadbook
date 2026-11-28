//! Diagnostics, shared by every Roadbook crate.
//!
//! A [`Diagnostic`] has a code, a severity, a message, a primary span, optional secondary
//! labels, an optional help line and optional machine-applicable fixes. It serializes to JSON
//! (for `roadbook check --json`) and renders in the rustc-like style shown in `language.md`:
//!
//! ```text
//! error[RB0204]: `invoke chargeCard` does not handle `GatewayUnavailable`
//!   --> examples/shop/checkout.road:13:5
//!    |
//! 13 |     invoke chargeCard(order, user) {
//!    |     ^^^^^^^^^^^^^^^^^ declared as `-> Receipt ! CardDeclined | GatewayUnavailable`
//!    |
//!    = help: add `fail GatewayUnavailable -> <State>` or `fail * -> <State>`
//! ```

use std::fmt::{self, Write as _};

use serde::Serialize;

use crate::span::{FileId, LineIndex, Span};

/// A diagnostic code, `RB` followed by four digits. Ranges: `RB00xx` syntax, `RB01xx` names,
/// `RB02xx` structure, `RB03xx` types, `RB04xx` analyses, `RB09xx` warnings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Code(pub u16);

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RB{:04}", self.0)
    }
}

impl Serialize for Code {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

/// A secondary span with a message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Label {
    pub span: Span,
    pub message: String,
}

/// Replace the text of `span` with `text`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TextEdit {
    pub span: Span,
    pub text: String,
}

/// A machine-applicable fix: a set of edits that together resolve the diagnostic.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Fix {
    pub message: String,
    pub edits: Vec<TextEdit>,
}

/// One diagnostic. The JSON form has the fields below in camelCase; spans carry their file id,
/// byte offsets and 1-based lines and columns (see [`Span`]'s `Serialize`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    pub code: Code,
    pub severity: Severity,
    pub message: String,
    /// The file the diagnostic is about. Always `span.file`.
    pub file: FileId,
    pub span: Span,
    /// The text printed next to the primary underline.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<Label>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fixes: Vec<Fix>,
}

impl Diagnostic {
    pub fn error(code: Code, span: Span, message: impl Into<String>) -> Diagnostic {
        Diagnostic::new(Severity::Error, code, span, message)
    }

    pub fn warning(code: Code, span: Span, message: impl Into<String>) -> Diagnostic {
        Diagnostic::new(Severity::Warning, code, span, message)
    }

    fn new(severity: Severity, code: Code, span: Span, message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            code,
            severity,
            message: message.into(),
            file: span.file,
            span,
            label: None,
            labels: Vec::new(),
            help: None,
            fixes: Vec::new(),
        }
    }

    /// Set the text printed next to the primary underline.
    pub fn with_label(mut self, label: impl Into<String>) -> Diagnostic {
        self.label = Some(label.into());
        self
    }

    /// Add a secondary label.
    pub fn with_secondary(mut self, span: Span, message: impl Into<String>) -> Diagnostic {
        self.labels.push(Label {
            span,
            message: message.into(),
        });
        self
    }

    pub fn with_help(mut self, help: impl Into<String>) -> Diagnostic {
        self.help = Some(help.into());
        self
    }

    /// Add a fix made of a single edit.
    pub fn with_fix(
        mut self,
        message: impl Into<String>,
        span: Span,
        text: impl Into<String>,
    ) -> Diagnostic {
        self.fixes.push(Fix {
            message: message.into(),
            edits: vec![TextEdit {
                span,
                text: text.into(),
            }],
        });
        self
    }

    /// Add a fix made of several edits, made together.
    pub fn with_fix_edits(
        mut self,
        message: impl Into<String>,
        edits: impl IntoIterator<Item = (Span, String)>,
    ) -> Diagnostic {
        self.fixes.push(Fix {
            message: message.into(),
            edits: edits
                .into_iter()
                .map(|(span, text)| TextEdit { span, text })
                .collect(),
        });
        self
    }

    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }

    /// Render in the human-readable style. `files` provides each file's path and text.
    pub fn render(&self, files: &impl Files) -> String {
        let mut out = String::new();
        let severity = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        let _ = writeln!(out, "{severity}[{}]: {}", self.code, self.message);

        // Group labels by file, the primary file first, then by line within each file.
        let mut labels: Vec<(Span, Option<&str>, bool)> =
            vec![(self.span, self.label.as_deref(), true)];
        labels.extend(
            self.labels
                .iter()
                .map(|l| (l.span, Some(l.message.as_str()), false)),
        );
        let mut file_order: Vec<FileId> = Vec::new();
        for (span, ..) in &labels {
            if !file_order.contains(&span.file) {
                file_order.push(span.file);
            }
        }
        let width = labels
            .iter()
            .map(|(s, ..)| digits(s.start.line))
            .max()
            .unwrap_or(1);
        let pad = " ".repeat(width);

        for (i, file) in file_order.iter().enumerate() {
            let src = files.source(*file);
            let index = LineIndex::new(src);
            let mut here: Vec<_> = labels.iter().filter(|(s, ..)| s.file == *file).collect();
            here.sort_by_key(|(s, _, primary)| (s.start.line, !*primary, s.start.col));
            let first = here
                .iter()
                .find(|(_, _, primary)| *primary)
                .unwrap_or(&here[0])
                .0;
            let arrow = if i == 0 { "-->" } else { ":::" };
            let _ = writeln!(
                out,
                "{pad}{arrow} {}:{}:{}",
                files.name(*file),
                first.start.line,
                first.start.col
            );
            let _ = writeln!(out, "{pad} |");
            let mut last_line = None;
            for (span, message, primary) in here {
                let line = span.start.line;
                if last_line != Some(line) {
                    if last_line.is_some_and(|l| line > l + 1) {
                        let _ = writeln!(out, "...");
                    }
                    let text = index.line_text(src, line).replace('\t', "    ");
                    let _ = writeln!(out, "{}", format!("{line:>width$} | {text}").trim_end());
                    last_line = Some(line);
                }
                let raw = index.line_text(src, line);
                let col = display_width(raw, span.start.col - 1);
                let end_col = if span.end.line == line {
                    display_width(raw, span.end.col - 1)
                } else {
                    display_width(raw, raw.chars().count() as u32)
                };
                let marker = if *primary { "^" } else { "-" };
                let len = end_col.saturating_sub(col).max(1);
                let mut row = format!("{pad} | {}{}", " ".repeat(col), marker.repeat(len));
                if let Some(m) = message {
                    row.push(' ');
                    row.push_str(m);
                }
                let _ = writeln!(out, "{row}");
            }
        }
        if let Some(help) = &self.help {
            let _ = writeln!(out, "{pad} |");
            let _ = writeln!(out, "{pad} = help: {help}");
        }
        out
    }
}

fn digits(n: u32) -> usize {
    n.to_string().len()
}

/// The display column of the first `chars` characters of `line`, with tabs as four columns.
fn display_width(line: &str, chars: u32) -> usize {
    line.chars()
        .take(chars as usize)
        .map(|c| if c == '\t' { 4 } else { 1 })
        .sum()
}

/// Gives the renderer the path and text of each file a diagnostic mentions.
pub trait Files {
    fn name(&self, file: FileId) -> &str;
    fn source(&self, file: FileId) -> &str;
}

/// A single file, for callers that work on one file at a time.
pub struct SingleFile<'a> {
    pub name: &'a str,
    pub source: &'a str,
}

impl Files for SingleFile<'_> {
    fn name(&self, _: FileId) -> &str {
        self.name
    }
    fn source(&self, _: FileId) -> &str {
        self.source
    }
}

/// A list of `(path, text)` pairs indexed by [`FileId`].
impl Files for [(String, String)] {
    fn name(&self, file: FileId) -> &str {
        &self[file.0 as usize].0
    }
    fn source(&self, file: FileId) -> &str {
        &self[file.0 as usize].1
    }
}

impl Files for Vec<(String, String)> {
    fn name(&self, file: FileId) -> &str {
        self.as_slice().name(file)
    }
    fn source(&self, file: FileId) -> &str {
        self.as_slice().source(file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span_of(src: &str, needle: &str) -> Span {
        let start = src.find(needle).unwrap();
        LineIndex::new(src).span(FileId(0), src, start, start + needle.len())
    }

    #[test]
    fn renders_the_documented_style() {
        let src = "line\n".repeat(12) + "    invoke chargeCard(order, user) {\n";
        let d = Diagnostic::error(
            Code(204),
            span_of(&src, "invoke chargeCard"),
            "`invoke chargeCard` does not handle `GatewayUnavailable`",
        )
        .with_label("declared as `-> Receipt ! CardDeclined | GatewayUnavailable`")
        .with_help("add `fail GatewayUnavailable -> <State>` or `fail * -> <State>`");
        let files = SingleFile {
            name: "examples/shop/checkout.road",
            source: &src,
        };
        let expected = "\
error[RB0204]: `invoke chargeCard` does not handle `GatewayUnavailable`
  --> examples/shop/checkout.road:13:5
   |
13 |     invoke chargeCard(order, user) {
   |     ^^^^^^^^^^^^^^^^^ declared as `-> Receipt ! CardDeclined | GatewayUnavailable`
   |
   = help: add `fail GatewayUnavailable -> <State>` or `fail * -> <State>`
";
        assert_eq!(d.render(&files), expected);
    }

    #[test]
    fn renders_secondary_labels() {
        let src = "state A {\n  on X -> B\n\n\n  on X -> C\n}\n";
        let first = span_of(src, "on X -> B");
        let second = Span {
            start: LineIndex::new(src).pos(src, 26),
            end: LineIndex::new(src).pos(src, 30),
            ..first
        };
        let d = Diagnostic::warning(Code(901), second, "same trigger")
            .with_secondary(first, "first here");
        let files = SingleFile {
            name: "a.road",
            source: src,
        };
        let expected = "\
warning[RB0901]: same trigger
 --> a.road:5:3
  |
2 |   on X -> B
  |   --------- first here
...
5 |   on X -> C
  |   ^^^^
";
        assert_eq!(d.render(&files), expected);
    }

    #[test]
    fn serializes_to_json() {
        let src = "abc";
        let d = Diagnostic::error(Code(1), span_of(src, "b"), "bad").with_fix(
            "remove it",
            span_of(src, "b"),
            "",
        );
        let json = serde_json::to_value(&d).unwrap();
        assert_eq!(json["code"], "RB0001");
        assert_eq!(json["severity"], "error");
        assert_eq!(json["span"]["col"], 2);
        assert_eq!(json["fixes"][0]["edits"][0]["text"], "");
        assert!(json.get("help").is_none());
    }
}
