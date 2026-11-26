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
//! Every member the reference lists is always present. An absent value is
//! `null` (`returns`, `init`, `initial`, `invoke`, `guard`, `target`,
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
    ActionDecl, Effect, EventDecl, Field, FunctionDecl, Invoke, InvokeKind, Ir, Machine, Source,
    Span, State, StateKind, TaskDecl, Transition, Trigger, TypeDecl, TypeDef, Var,
};
pub use types::{Builtin, TypeRef, TypeRefError};

/// The IR version this crate reads and writes.
pub const IR_VERSION: &str = "0.1";

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
/// is breaking, so only `"0.1"` is accepted. From `1.0` on, a reader accepts
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
        let probe: Probe = serde_json::from_str(json).map_err(ReadError::Json)?;
        match probe.ir_version {
            Some(serde_json::Value::String(v)) => check_version(&v).map_err(ReadError::Version)?,
            _ => return Err(ReadError::Version(VersionError { found: None })),
        }
        serde_json::from_str(json).map_err(ReadError::Json)
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
        assert!(check_version("0.1").is_ok());
        for v in [
            "0.2", "0.0", "1.0", "0.01", "00.1", "0.1.0", "0", "", "v0.1", "0.1 ",
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
            Ir::from_json(r#"{"irVersion":"0.1"}"#),
            Err(ReadError::Json(_))
        ));
    }
}
