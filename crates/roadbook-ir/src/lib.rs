//! The Roadbook IR.
//!
//! The compiler turns `.road` files into one JSON document, the IR, and every
//! other tool (the viewer, both runtimes, the code generators) reads only
//! that. This crate holds:
//!
//! - the IR as serde types, starting at [`Ir`], which read and write the
//!   documented JSON exactly ([`Ir::from_json`], [`Ir::to_json`]);
//! - [`TypeRef`], the string form of types;
//! - canonical JSON (RFC 8785) in [`jcs`];
//! - the machine hash: [`machine_hash`] and [`fill_hashes`];
//! - the version check a runtime uses to refuse an IR it does not
//!   understand: [`check_version`].
//!
//! The field-by-field reference is `spec/ir.md` and the JSON Schema is
//! `spec/ir.schema.json`, both at the repository root.
//!
//! # Optional fields
//!
//! Every member the reference lists is always present, except that an
//! [`Address`] has exactly one of `machine` and `task`, by its kind, and no
//! member for the other. An absent value is
//! `null` (`returns`, `init`, `initial`, `invoke`, `guard`,
//! `output`, `fail`), a missing doc comment is `""`, and an empty list or
//! map is `[]` or `{}`. Nothing is ever omitted, so the output for a given
//! input is unique. The reader rejects unknown members.

mod expr;
mod hash;
pub mod jcs;
mod model;
mod types;

use std::fmt;

use serde::Deserialize;

pub use expr::{Expr, ExprKind, FieldValue, Literal, MAX_SAFE_INTEGER, Op};
pub use hash::{HASH_PREFIX, HashError, fill_hashes, machine_hash, semantic_view};
pub use model::{
    ActionDecl, Address, AddressKind, Call, ChildrenDecl, ChildrenKind, ENDED, ENDED_VARIANTS,
    Effect, EventDecl, Field, FunctionDecl, Invoke, InvokeKind, Ir, Machine, ReplyOutcome, Source,
    Span, State, StateArgs, StateKind, TaskDecl, Transition, Trigger, TypeDecl, TypeDef,
    UNANSWERED, Var,
};
pub use types::{Builtin, TypeRef, TypeRefError};

/// The IR version this crate reads and writes.
pub const IR_VERSION: &str = "0.12";

/// An `irVersion` this crate cannot read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionError {
    /// The version found, or `None` if the document has no string `irVersion`.
    pub found: Option<String>,
}

impl fmt::Display for VersionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.found {
            Some(v) => write!(
                f,
                "IR version {v:?} is not supported: this reader understands IR version {IR_VERSION}"
            ),
            None => write!(f, "not a Roadbook IR: no string `irVersion` member"),
        }
    }
}

impl std::error::Error for VersionError {}

fn parse_version(v: &str) -> Option<(u32, u32)> {
    let (major, minor) = v.split_once('.')?;
    let num = |s: &str| {
        let canonical = s == "0" || (!s.is_empty() && !s.starts_with('0'));
        if canonical && s.bytes().all(|b| b.is_ascii_digit()) {
            s.parse().ok()
        } else {
            None
        }
    };
    Some((num(major)?, num(minor)?))
}

/// Checks that an IR with `irVersion` equal to `found` can be read by this
/// crate.
///
/// Versions are `"MAJOR.MINOR"`. While the major version is 0 every change
/// is breaking, so only `"0.12"` is accepted. From `1.0` on, a reader accepts
/// its own major version with any minor version up to its own: a minor
/// version only adds to the IR, and an older reader must not silently ignore
/// what it does not know.
pub fn check_version(found: &str) -> Result<(), VersionError> {
    let err = || VersionError {
        found: Some(found.to_owned()),
    };
    let (major, minor) = parse_version(found).ok_or_else(err)?;
    let (our_major, our_minor) = parse_version(IR_VERSION).ok_or_else(err)?;
    let ok = major == our_major
        && if major == 0 {
            minor == our_minor
        } else {
            minor <= our_minor
        };
    if ok { Ok(()) } else { Err(err()) }
}

/// The deepest nesting of JSON arrays and objects [`Ir::from_json`] reads. The compiler nests
/// states and expressions at most 64 levels deep, each two JSON levels, below a handful of
/// levels of document structure.
pub const MAX_JSON_DEPTH: usize = 256;

/// `json` read as a `T`, without serde_json's own nesting limit, 128, which is below what the
/// compiler can produce. The caller has checked the depth against [`MAX_JSON_DEPTH`].
fn read<'a, T: Deserialize<'a>>(json: &'a str) -> Result<T, serde_json::Error> {
    let mut de = serde_json::Deserializer::from_str(json);
    de.disable_recursion_limit();
    let value = T::deserialize(&mut de)?;
    de.end()?;
    Ok(value)
}

/// The deepest nesting of arrays and objects in a JSON text, found without recursing.
fn json_depth(json: &str) -> usize {
    let (mut depth, mut max, mut in_string, mut escaped) = (0usize, 0usize, false, false);
    for b in json.bytes() {
        if in_string {
            match b {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'[' | b'{' => {
                depth += 1;
                max = max.max(depth);
            }
            b']' | b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    max
}

/// Why an IR document could not be read.
#[derive(Debug)]
pub enum ReadError {
    /// The document's `irVersion` is missing or not supported.
    Version(VersionError),
    /// The document is not valid JSON or does not have the IR's shape.
    Json(serde_json::Error),
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReadError::Version(e) => e.fmt(f),
            ReadError::Json(e) => write!(f, "invalid IR: {e}"),
        }
    }
}

impl std::error::Error for ReadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ReadError::Version(e) => Some(e),
            ReadError::Json(e) => Some(e),
        }
    }
}

impl Ir {
    /// Reads an IR document, checking its version before its shape, so an IR
    /// from a newer compiler is reported as a version mismatch rather than as
    /// an unknown field.
    pub fn from_json(json: &str) -> Result<Ir, ReadError> {
        #[derive(Deserialize)]
        struct Probe {
            #[serde(rename = "irVersion")]
            ir_version: Option<serde_json::Value>,
        }
        if json_depth(json) > MAX_JSON_DEPTH {
            let e = <serde_json::Error as serde::de::Error>::custom(format!(
                "nested more than {MAX_JSON_DEPTH} levels deep"
            ));
            return Err(ReadError::Json(e));
        }
        let probe: Probe = read(json).map_err(ReadError::Json)?;
        match probe.ir_version {
            Some(serde_json::Value::String(v)) => check_version(&v).map_err(ReadError::Version)?,
            _ => return Err(ReadError::Version(VersionError { found: None })),
        }
        read(json).map_err(ReadError::Json)
    }

    /// Writes the IR in its file form: JSON indented by two spaces, members
    /// in the documented order, with a final newline. This is the form the
    /// compiler writes and that is committed; it is deterministic. For
    /// hashing or byte-for-byte comparison use [`jcs`] instead.
    pub fn to_json(&self) -> String {
        // The IR has only string keys and finite numbers, so this cannot fail.
        let mut s = serde_json::to_string_pretty(self).unwrap_or_default();
        s.push('\n');
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(check_version("0.12").is_ok());
        for v in [
            "0.1", "0.2", "0.3", "0.4", "0.5", "0.6", "0.7", "0.8", "0.9", "0.10", "0.11", "0.0",
            "1.0", "0.012", "00.12", "0.12.0", "0", "", "v0.12", "0.12 ",
        ] {
            assert!(check_version(v).is_err(), "{v:?}");
        }
        assert!(matches!(
            Ir::from_json(r#"{"irVersion":"9.0","whatever":1}"#),
            Err(ReadError::Version(_))
        ));
        assert!(matches!(
            Ir::from_json(r#"{"irVersion":1}"#),
            Err(ReadError::Version(VersionError { found: None }))
        ));
        assert!(matches!(
            Ir::from_json(r#"{"irVersion":"0.12"}"#),
            Err(ReadError::Json(_))
        ));
    }

    #[test]
    fn depth_is_bounded_without_recursing() {
        assert_eq!(json_depth(r#"{"a": "[[{", "b": [[1], {"c": "\"]"}]}"#), 3);
        let deep = format!(
            r#"{{"irVersion":"0.12","x":{}{}}}"#,
            "[".repeat(MAX_JSON_DEPTH),
            "]".repeat(MAX_JSON_DEPTH)
        );
        let e = Ir::from_json(&deep).unwrap_err().to_string();
        assert!(e.contains("nested more than"), "{e}");
    }
}
