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

use std::cmp::Ordering;
use std::fmt::Write as _;
use std::ops::Range;

use serde::Serialize;
use serde::ser::{self, Error as _};
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

/// The canonical form of any serialisable value: the same text as
/// `to_canonical_string(&serde_json::to_value(value)?)`, written straight from
/// `value`'s `Serialize` with no `Value` in between.
///
/// Fails where `serde_json::to_value` does, for example on a map with
/// non-string keys. As there, a non-finite float is `null`. An object that
/// gets the same key twice (a `#[serde(flatten)]` that repeats one) keeps both
/// members, where `to_value` would keep the last.
pub fn to_canonical<T: Serialize + ?Sized>(value: &T) -> Result<String, serde_json::Error> {
    let mut out = String::new();
    write_canonical(&mut out, value)?;
    Ok(out)
}

/// Appends the canonical form of `value` to `out`, as [`to_canonical`] gives
/// it. On an error, `out` holds a partial value.
pub fn write_canonical<T: Serialize + ?Sized>(
    out: &mut String,
    value: &T,
) -> Result<(), serde_json::Error> {
    value.serialize(Canon { out })
}

/// Whether member names `a` and `b` are in canonical order (UTF-16 code
/// units). UTF-8 byte order is code point order, which is UTF-16 order unless
/// one of them holds a character at U+E000 or above (lead byte 0xEE or more),
/// where surrogate pairs sort below U+E000..U+FFFF.
fn key_cmp(a: &str, b: &str) -> Ordering {
    let low = |s: &str| s.bytes().all(|b| b < 0xEE);
    if low(a) && low(b) {
        a.as_bytes().cmp(b.as_bytes())
    } else {
        a.encode_utf16().cmp(b.encode_utf16())
    }
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
            if !entries.is_sorted_by(|(a, _), (b, _)| key_cmp(a, b) != Ordering::Greater) {
                entries.sort_by(|(a, _), (b, _)| key_cmp(a, b));
            }
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
    // Runs of characters written as they are go in one piece.
    let mut plain = 0;
    for (i, b) in s.bytes().enumerate() {
        let escape = match b {
            b'"' => "\\\"",
            b'\\' => "\\\\",
            0x08 => "\\b",
            0x0c => "\\f",
            b'\n' => "\\n",
            b'\r' => "\\r",
            b'\t' => "\\t",
            b if b < b' ' => "",
            _ => continue,
        };
        out.push_str(&s[plain..i]);
        if escape.is_empty() {
            // Writing to a String cannot fail.
            let _ = write!(out, "\\u{:04x}", b);
        } else {
            out.push_str(escape);
        }
        plain = i + 1;
    }
    out.push_str(&s[plain..]);
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

/// A number from `Serialize`, as `to_value` would hold it and [`write_value`]
/// write it: as a double, and `null` if it is not finite.
fn write_f64(out: &mut String, f: f64) {
    if f.is_finite() {
        write_number(out, f);
    } else {
        out.push_str("null");
    }
}

/// The serializer behind [`write_canonical`]: scalars and arrays are written
/// as they come; an object's members are written in the order they come, each
/// one's text noted, and put in canonical order at the end if they were not in
/// it already.
struct Canon<'a> {
    out: &'a mut String,
}

/// An array being written.
struct Seq<'a> {
    out: &'a mut String,
    first: bool,
    /// For a tuple variant, `{"Variant":[…]}`: the brace to close.
    close: bool,
}

/// One member of an object being written: where its name is (as written, in
/// quotes, when it has no escapes; else owned), and where the whole member is.
struct Member {
    name: Name,
    text: Range<usize>,
}

enum Name {
    Static(&'static str),
    /// The written name, quotes included, which has no escapes.
    Written(Range<usize>),
    Owned(String),
}

/// An object being written.
struct Obj<'a> {
    out: &'a mut String,
    /// Where the first member starts, just after `{`.
    start: usize,
    members: Vec<Member>,
    /// For a struct variant, `{"Variant":{…}}`: the brace to close.
    close: bool,
}

impl<'a> Canon<'a> {
    fn seq(self, close: bool) -> Seq<'a> {
        self.out.push('[');
        Seq {
            out: self.out,
            first: true,
            close,
        }
    }

    fn obj(self, len: Option<usize>, close: bool) -> Obj<'a> {
        self.out.push('{');
        Obj {
            start: self.out.len(),
            out: self.out,
            members: Vec::with_capacity(len.unwrap_or(0)),
            close,
        }
    }

    /// `{"variant":` before a variant's content.
    fn open_variant(&mut self, variant: &str) {
        self.out.push('{');
        write_string(self.out, variant);
        self.out.push(':');
    }
}

impl<'a> ser::Serializer for Canon<'a> {
    type Ok = ();
    type Error = serde_json::Error;
    type SerializeSeq = Seq<'a>;
    type SerializeTuple = Seq<'a>;
    type SerializeTupleStruct = Seq<'a>;
    type SerializeTupleVariant = Seq<'a>;
    type SerializeMap = Obj<'a>;
    type SerializeStruct = Obj<'a>;
    type SerializeStructVariant = Obj<'a>;

    fn serialize_bool(self, v: bool) -> Result<(), Self::Error> {
        self.out.push_str(if v { "true" } else { "false" });
        Ok(())
    }
    fn serialize_i8(self, v: i8) -> Result<(), Self::Error> {
        self.serialize_i64(v.into())
    }
    fn serialize_i16(self, v: i16) -> Result<(), Self::Error> {
        self.serialize_i64(v.into())
    }
    fn serialize_i32(self, v: i32) -> Result<(), Self::Error> {
        self.serialize_i64(v.into())
    }
    fn serialize_i64(self, v: i64) -> Result<(), Self::Error> {
        write_f64(self.out, v as f64);
        Ok(())
    }
    fn serialize_i128(self, v: i128) -> Result<(), Self::Error> {
        match i64::try_from(v) {
            Ok(v) => self.serialize_i64(v),
            Err(_) => self.serialize_u128(u128::try_from(v).map_err(|_| number_out_of_range())?),
        }
    }
    fn serialize_u8(self, v: u8) -> Result<(), Self::Error> {
        self.serialize_u64(v.into())
    }
    fn serialize_u16(self, v: u16) -> Result<(), Self::Error> {
        self.serialize_u64(v.into())
    }
    fn serialize_u32(self, v: u32) -> Result<(), Self::Error> {
        self.serialize_u64(v.into())
    }
    fn serialize_u64(self, v: u64) -> Result<(), Self::Error> {
        write_f64(self.out, v as f64);
        Ok(())
    }
    fn serialize_u128(self, v: u128) -> Result<(), Self::Error> {
        self.serialize_u64(u64::try_from(v).map_err(|_| number_out_of_range())?)
    }
    fn serialize_f32(self, v: f32) -> Result<(), Self::Error> {
        self.serialize_f64(v.into())
    }
    fn serialize_f64(self, v: f64) -> Result<(), Self::Error> {
        write_f64(self.out, v);
        Ok(())
    }
    fn serialize_char(self, v: char) -> Result<(), Self::Error> {
        write_string(self.out, v.encode_utf8(&mut [0; 4]));
        Ok(())
    }
    fn serialize_str(self, v: &str) -> Result<(), Self::Error> {
        write_string(self.out, v);
        Ok(())
    }
    fn serialize_bytes(self, v: &[u8]) -> Result<(), Self::Error> {
        let mut seq = self.seq(false);
        for b in v {
            ser::SerializeSeq::serialize_element(&mut seq, b)?;
        }
        ser::SerializeSeq::end(seq)
    }
    fn serialize_none(self) -> Result<(), Self::Error> {
        self.serialize_unit()
    }
    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<(), Self::Error> {
        value.serialize(self)
    }
    fn serialize_unit(self) -> Result<(), Self::Error> {
        self.out.push_str("null");
        Ok(())
    }
    fn serialize_unit_struct(self, _: &'static str) -> Result<(), Self::Error> {
        self.serialize_unit()
    }
    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
    ) -> Result<(), Self::Error> {
        self.serialize_str(variant)
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        value: &T,
    ) -> Result<(), Self::Error> {
        value.serialize(self)
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        mut self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<(), Self::Error> {
        self.open_variant(variant);
        value.serialize(Canon {
            out: &mut *self.out,
        })?;
        self.out.push('}');
        Ok(())
    }
    fn serialize_seq(self, _: Option<usize>) -> Result<Seq<'a>, Self::Error> {
        Ok(self.seq(false))
    }
    fn serialize_tuple(self, _: usize) -> Result<Seq<'a>, Self::Error> {
        Ok(self.seq(false))
    }
    fn serialize_tuple_struct(self, _: &'static str, _: usize) -> Result<Seq<'a>, Self::Error> {
        Ok(self.seq(false))
    }
    fn serialize_tuple_variant(
        mut self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        _: usize,
    ) -> Result<Seq<'a>, Self::Error> {
        self.open_variant(variant);
        Ok(self.seq(true))
    }
    fn serialize_map(self, len: Option<usize>) -> Result<Obj<'a>, Self::Error> {
        Ok(self.obj(len, false))
    }
    fn serialize_struct(self, _: &'static str, len: usize) -> Result<Obj<'a>, Self::Error> {
        Ok(self.obj(Some(len), false))
    }
    fn serialize_struct_variant(
        mut self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<Obj<'a>, Self::Error> {
        self.open_variant(variant);
        Ok(self.obj(Some(len), true))
    }
}

fn number_out_of_range() -> serde_json::Error {
    serde_json::Error::custom("number out of range")
}

impl Seq<'_> {
    fn element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), serde_json::Error> {
        if !self.first {
            self.out.push(',');
        }
        self.first = false;
        value.serialize(Canon {
            out: &mut *self.out,
        })
    }

    fn finish(self) -> Result<(), serde_json::Error> {
        self.out.push(']');
        if self.close {
            self.out.push('}');
        }
        Ok(())
    }
}

impl ser::SerializeSeq for Seq<'_> {
    type Ok = ();
    type Error = serde_json::Error;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Self::Error> {
        self.element(value)
    }
    fn end(self) -> Result<(), Self::Error> {
        self.finish()
    }
}

impl ser::SerializeTuple for Seq<'_> {
    type Ok = ();
    type Error = serde_json::Error;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Self::Error> {
        self.element(value)
    }
    fn end(self) -> Result<(), Self::Error> {
        self.finish()
    }
}

impl ser::SerializeTupleStruct for Seq<'_> {
    type Ok = ();
    type Error = serde_json::Error;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Self::Error> {
        self.element(value)
    }
    fn end(self) -> Result<(), Self::Error> {
        self.finish()
    }
}

impl ser::SerializeTupleVariant for Seq<'_> {
    type Ok = ();
    type Error = serde_json::Error;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Self::Error> {
        self.element(value)
    }
    fn end(self) -> Result<(), Self::Error> {
        self.finish()
    }
}

impl Obj<'_> {
    /// Starts a member: its separator, and its name and `:`.
    fn begin(&mut self, name: Name) -> usize {
        if !self.members.is_empty() {
            self.out.push(',');
        }
        let at = self.out.len();
        let name = match name {
            Name::Static(s) => {
                write_string(self.out, s);
                Name::Static(s)
            }
            n => n,
        };
        self.members.push(Member { name, text: at..at });
        at
    }

    fn field<T: Serialize + ?Sized>(
        &mut self,
        name: &'static str,
        value: &T,
    ) -> Result<(), serde_json::Error> {
        let at = self.begin(Name::Static(name));
        self.out.push(':');
        self.value_from(at, value)
    }

    /// Writes a member's value and notes where the member is.
    fn value_from<T: Serialize + ?Sized>(
        &mut self,
        at: usize,
        value: &T,
    ) -> Result<(), serde_json::Error> {
        value.serialize(Canon {
            out: &mut *self.out,
        })?;
        let end = self.out.len();
        if let Some(m) = self.members.last_mut() {
            m.text = at..end;
        }
        Ok(())
    }

    /// A member's name. `text` is what was written from position `offset` of
    /// the output on.
    fn name<'n>(text: &'n str, offset: usize, name: &'n Name) -> &'n str {
        match name {
            Name::Static(s) => s,
            Name::Written(r) => &text[r.start - offset + 1..r.end - offset - 1],
            Name::Owned(s) => s,
        }
    }

    fn finish(self) -> Result<(), serde_json::Error> {
        let Obj {
            out,
            start,
            mut members,
            close,
        } = self;
        let sorted = members.is_sorted_by(|a, b| {
            key_cmp(Self::name(out, 0, &a.name), Self::name(out, 0, &b.name)) != Ordering::Greater
        });
        if !sorted {
            let written = out.split_off(start);
            members.sort_by(|a, b| {
                key_cmp(
                    Self::name(&written, start, &a.name),
                    Self::name(&written, start, &b.name),
                )
            });
            for (i, m) in members.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&written[m.text.start - start..m.text.end - start]);
            }
        }
        out.push('}');
        if close {
            out.push('}');
        }
        Ok(())
    }
}

impl ser::SerializeMap for Obj<'_> {
    type Ok = ();
    type Error = serde_json::Error;
    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), Self::Error> {
        let at = self.begin(Name::Written(0..0));
        key.serialize(Key {
            out: &mut *self.out,
        })?;
        let end = self.out.len();
        let written = &self.out[at..end];
        let name = if written[1..written.len() - 1].contains('\\') {
            let unescaped: String = serde_json::from_str(written)?;
            Name::Owned(unescaped)
        } else {
            Name::Written(at..end)
        };
        if let Some(m) = self.members.last_mut() {
            m.name = name;
        }
        self.out.push(':');
        Ok(())
    }
    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Self::Error> {
        let at = self.members.last().map_or(self.out.len(), |m| m.text.start);
        self.value_from(at, value)
    }
    fn end(self) -> Result<(), Self::Error> {
        self.finish()
    }
}

impl ser::SerializeStruct for Obj<'_> {
    type Ok = ();
    type Error = serde_json::Error;
    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        name: &'static str,
        value: &T,
    ) -> Result<(), Self::Error> {
        self.field(name, value)
    }
    fn end(self) -> Result<(), Self::Error> {
        self.finish()
    }
}

impl ser::SerializeStructVariant for Obj<'_> {
    type Ok = ();
    type Error = serde_json::Error;
    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        name: &'static str,
        value: &T,
    ) -> Result<(), Self::Error> {
        self.field(name, value)
    }
    fn end(self) -> Result<(), Self::Error> {
        self.finish()
    }
}

/// An object member's name: a string, or an integer or `bool` written as one,
/// as `serde_json::to_value` takes them.
struct Key<'a> {
    out: &'a mut String,
}

impl Key<'_> {
    fn display(self, v: impl std::fmt::Display) -> Result<(), serde_json::Error> {
        write_string(self.out, &v.to_string());
        Ok(())
    }
}

fn key_must_be_a_string() -> serde_json::Error {
    serde_json::Error::custom("key must be a string")
}

impl ser::Serializer for Key<'_> {
    type Ok = ();
    type Error = serde_json::Error;
    type SerializeSeq = ser::Impossible<(), serde_json::Error>;
    type SerializeTuple = ser::Impossible<(), serde_json::Error>;
    type SerializeTupleStruct = ser::Impossible<(), serde_json::Error>;
    type SerializeTupleVariant = ser::Impossible<(), serde_json::Error>;
    type SerializeMap = ser::Impossible<(), serde_json::Error>;
    type SerializeStruct = ser::Impossible<(), serde_json::Error>;
    type SerializeStructVariant = ser::Impossible<(), serde_json::Error>;

    fn serialize_str(self, v: &str) -> Result<(), Self::Error> {
        write_string(self.out, v);
        Ok(())
    }
    fn serialize_char(self, v: char) -> Result<(), Self::Error> {
        self.serialize_str(v.encode_utf8(&mut [0; 4]))
    }
    fn serialize_bool(self, v: bool) -> Result<(), Self::Error> {
        self.display(v)
    }
    fn serialize_i8(self, v: i8) -> Result<(), Self::Error> {
        self.display(v)
    }
    fn serialize_i16(self, v: i16) -> Result<(), Self::Error> {
        self.display(v)
    }
    fn serialize_i32(self, v: i32) -> Result<(), Self::Error> {
        self.display(v)
    }
    fn serialize_i64(self, v: i64) -> Result<(), Self::Error> {
        self.display(v)
    }
    fn serialize_i128(self, v: i128) -> Result<(), Self::Error> {
        self.display(v)
    }
    fn serialize_u8(self, v: u8) -> Result<(), Self::Error> {
        self.display(v)
    }
    fn serialize_u16(self, v: u16) -> Result<(), Self::Error> {
        self.display(v)
    }
    fn serialize_u32(self, v: u32) -> Result<(), Self::Error> {
        self.display(v)
    }
    fn serialize_u64(self, v: u64) -> Result<(), Self::Error> {
        self.display(v)
    }
    fn serialize_u128(self, v: u128) -> Result<(), Self::Error> {
        self.display(v)
    }
    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
    ) -> Result<(), Self::Error> {
        self.serialize_str(variant)
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        value: &T,
    ) -> Result<(), Self::Error> {
        value.serialize(self)
    }
    fn serialize_f32(self, _: f32) -> Result<(), Self::Error> {
        Err(key_must_be_a_string())
    }
    fn serialize_f64(self, _: f64) -> Result<(), Self::Error> {
        Err(key_must_be_a_string())
    }
    fn serialize_bytes(self, _: &[u8]) -> Result<(), Self::Error> {
        Err(key_must_be_a_string())
    }
    fn serialize_none(self) -> Result<(), Self::Error> {
        Err(key_must_be_a_string())
    }
    fn serialize_some<T: Serialize + ?Sized>(self, _: &T) -> Result<(), Self::Error> {
        Err(key_must_be_a_string())
    }
    fn serialize_unit(self) -> Result<(), Self::Error> {
        Err(key_must_be_a_string())
    }
    fn serialize_unit_struct(self, _: &'static str) -> Result<(), Self::Error> {
        Err(key_must_be_a_string())
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: &T,
    ) -> Result<(), Self::Error> {
        Err(key_must_be_a_string())
    }
    fn serialize_seq(self, _: Option<usize>) -> Result<Self::SerializeSeq, Self::Error> {
        Err(key_must_be_a_string())
    }
    fn serialize_tuple(self, _: usize) -> Result<Self::SerializeTuple, Self::Error> {
        Err(key_must_be_a_string())
    }
    fn serialize_tuple_struct(
        self,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeTupleStruct, Self::Error> {
        Err(key_must_be_a_string())
    }
    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeTupleVariant, Self::Error> {
        Err(key_must_be_a_string())
    }
    fn serialize_map(self, _: Option<usize>) -> Result<Self::SerializeMap, Self::Error> {
        Err(key_must_be_a_string())
    }
    fn serialize_struct(
        self,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeStruct, Self::Error> {
        Err(key_must_be_a_string())
    }
    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeStructVariant, Self::Error> {
        Err(key_must_be_a_string())
    }
}
