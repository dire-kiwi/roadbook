//! Canonical JSON: the JSON Canonicalization Scheme of RFC 8785.
//!
//! The output is what ECMAScript's `JSON.stringify` writes for the same value
//! once object members are sorted, so a TypeScript implementation that sorts
//! keys and calls `JSON.stringify` on each scalar produces identical bytes:
//!
//! - no whitespace;
//! - object members sorted by their names compared as arrays of UTF-16 code
//!   units;
//! - strings escaped as `JSON.stringify` escapes them: `\"`, `\\`, `\b`,
//!   `\f`, `\n`, `\r`, `\t`, other control characters below U+0020 as
//!   `\u00xx` with lowercase hex, and everything else literally;
//! - every number treated as an IEEE 754 double and written in the shortest
//!   form that round-trips, with ECMAScript's `Number.prototype.toString`
//!   layout: `2.0` is `2`, `-0` is `0`, `1e21` is `1e+21`, `0.000001` stays
//!   decimal and `1e-7` is `1e-7`.
//!
//! Integers beyond ±2^53 are rounded to the nearest double, as RFC 8785
//! requires; the IR keeps its own integers within that range.

use std::fmt::Write as _;

use serde::Serialize;
use serde_json::Value;

/// The canonical form of `value` as a string.
pub fn to_canonical_string(value: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, value);
    out
}

/// The canonical form of `value` as UTF-8 bytes, the input for hashing.
pub fn to_canonical_vec(value: &Value) -> Vec<u8> {
    to_canonical_string(value).into_bytes()
}

/// The canonical form of any serialisable value, via [`serde_json::Value`].
///
/// Fails where `serde_json::to_value` does, for example on a map with
/// non-string keys.
pub fn to_canonical<T: Serialize + ?Sized>(value: &T) -> Result<String, serde_json::Error> {
    Ok(to_canonical_string(&serde_json::to_value(value)?))
}

fn write_value(out: &mut String, value: &Value) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => {
            // Without serde_json's `arbitrary_precision` every number has an
            // f64 view, and `Value` never holds a non-finite one.
            let f = n.as_f64().unwrap_or(f64::NAN);
            write_number(out, f);
        }
        Value::String(s) => write_string(out, s),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value(out, item);
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by(|(a, _), (b, _)| a.encode_utf16().cmp(b.encode_utf16()));
            out.push('{');
            for (i, (k, v)) in entries.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_string(out, k);
                out.push(':');
                write_value(out, v);
            }
            out.push('}');
        }
    }
}

/// Writes `f` as ECMAScript's `Number.prototype.toString` would.
///
/// # Panics
///
/// If `f` is NaN or infinite, which JSON cannot represent.
fn write_number(out: &mut String, f: f64) {
    assert!(f.is_finite(), "JSON numbers are finite");
    if f == 0.0 {
        // Both zeros are written `0`.
        out.push('0');
    } else {
        out.push_str(ryu_js::Buffer::new().format_finite(f));
    }
}

fn write_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c < ' ' => {
                // Writing to a String cannot fail.
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// The ECMAScript string form of a finite double, as used for numbers in
/// canonical JSON. Exposed for tests and for writers of other canonical
/// formats; most callers want [`to_canonical_string`].
pub fn format_number(f: f64) -> Option<String> {
    if !f.is_finite() {
        return None;
    }
    let mut s = String::new();
    write_number(&mut s, f);
    Some(s)
}
