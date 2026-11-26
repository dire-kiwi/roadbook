//! Type references: the strings the IR uses wherever a type is named.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A built-in type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Builtin {
    /// `string`
    String,
    /// `int`, a 64-bit signed integer.
    Int,
    /// `float`, an IEEE 754 double.
    Float,
    /// `bool`
    Bool,
    /// `duration`, carried as integer milliseconds.
    Duration,
    /// `json`, any JSON value.
    Json,
}

impl Builtin {
    /// Every built-in, in the order the language page lists them.
    pub const ALL: [Builtin; 6] = [
        Builtin::String,
        Builtin::Int,
        Builtin::Float,
        Builtin::Bool,
        Builtin::Duration,
        Builtin::Json,
    ];

    /// The keyword that names this type in source and in the IR.
    pub fn name(self) -> &'static str {
        match self {
            Builtin::String => "string",
            Builtin::Int => "int",
            Builtin::Float => "float",
            Builtin::Bool => "bool",
            Builtin::Duration => "duration",
            Builtin::Json => "json",
        }
    }

    /// The built-in with this keyword, if any.
    pub fn from_name(name: &str) -> Option<Builtin> {
        Builtin::ALL.into_iter().find(|b| b.name() == name)
    }
}

/// A reference to a type, as written in canonical source form.
///
/// In JSON a `TypeRef` is a string: a built-in keyword or a declared
/// upper-case name, followed by any number of `[]` (list of) and `?`
/// (optional) suffixes, applied left to right. `LineItem[]` is a list of
/// `LineItem`; `string[]?` is an optional list of strings; `string?[]` is a
/// list of optional strings. There is no whitespace.
///
/// `Display` and `FromStr` convert to and from that string, and
/// `parse(display(t)) == t` for every `t`.
///
/// `ref Name` is a handle to a running machine `Name`, with the same suffix
/// rules: `ref Inventory[]` is a list of handles. It is the one form with a
/// space.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TypeRef {
    /// A built-in type such as `int`.
    Builtin(Builtin),
    /// A declared type, error or extern type, by name (`Order`).
    ///
    /// Event types are named here too. Inside a machine, the bare name of one
    /// of its own events or emitted events (`Cancel`, `Progress`) is that
    /// event: the type of `event` in `on Cancel`, of the value of
    /// `emit Progress { … }`. A dotted name `Machine.Event` (`Upload.Pause`)
    /// is an event of another machine: the type of `event` in
    /// `on upload.Progress`, of the value of `send upload.Pause`.
    Named(String),
    /// `ref Name`: a handle to a running machine, a key of `machines`.
    Ref(String),
    /// `T[]`
    List(Box<TypeRef>),
    /// `T?`
    Optional(Box<TypeRef>),
}

impl TypeRef {
    /// A declared type by name.
    pub fn named(name: impl Into<String>) -> TypeRef {
        TypeRef::Named(name.into())
    }

    /// `self[]`
    pub fn list(self) -> TypeRef {
        TypeRef::List(Box::new(self))
    }

    /// `self?`
    pub fn optional(self) -> TypeRef {
        TypeRef::Optional(Box::new(self))
    }

    /// The type with every `[]` and `?` suffix removed.
    pub fn base(&self) -> &TypeRef {
        match self {
            TypeRef::List(t) | TypeRef::Optional(t) => t.base(),
            t => t,
        }
    }

    /// The declared name this type refers to, if its base is a declared type
    /// (not a built-in or a `ref`).
    pub fn named_base(&self) -> Option<&str> {
        match self.base() {
            TypeRef::Named(n) => Some(n),
            _ => None,
        }
    }

    /// The machine a `ref` names, if the base of this type is a `ref`.
    pub fn ref_base(&self) -> Option<&str> {
        match self.base() {
            TypeRef::Ref(n) => Some(n),
            _ => None,
        }
    }
}

impl From<Builtin> for TypeRef {
    fn from(b: Builtin) -> TypeRef {
        TypeRef::Builtin(b)
    }
}

impl fmt::Display for TypeRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TypeRef::Builtin(b) => f.write_str(b.name()),
            TypeRef::Named(n) => f.write_str(n),
            TypeRef::Ref(n) => write!(f, "ref {n}"),
            TypeRef::List(t) => write!(f, "{t}[]"),
            TypeRef::Optional(t) => write!(f, "{t}?"),
        }
    }
}

/// Why a string is not a [`TypeRef`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeRefError {
    input: String,
}

impl fmt::Display for TypeRefError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "`{}` is not a type: expected a built-in (string, int, float, bool, duration, json), \
             an upper-case name, `Machine.Event` or `ref Machine`, followed by any number of \
             `[]` and `?`",
            self.input
        )
    }
}

impl std::error::Error for TypeRefError {}

/// True for `[A-Z][A-Za-z0-9]*`, the grammar's `Upper`.
pub(crate) fn is_upper_name(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_uppercase())
        && chars.all(|c| c.is_ascii_alphanumeric())
}

impl FromStr for TypeRef {
    type Err = TypeRefError;

    fn from_str(s: &str) -> Result<TypeRef, TypeRefError> {
        let err = || TypeRefError {
            input: s.to_owned(),
        };
        let (is_ref, body) = match s.strip_prefix("ref ") {
            Some(rest) => (true, rest),
            None => (false, s),
        };
        let base_len = body
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '.')
            .unwrap_or(body.len());
        let (base, mut rest) = body.split_at(base_len);
        let mut t = if is_ref {
            if !is_upper_name(base) {
                return Err(err());
            }
            TypeRef::Ref(base.to_owned())
        } else if let Some(b) = Builtin::from_name(base) {
            TypeRef::Builtin(b)
        } else if is_upper_name(base) {
            TypeRef::Named(base.to_owned())
        } else if let Some((m, e)) = base.split_once('.')
            && is_upper_name(m)
            && is_upper_name(e)
        {
            TypeRef::Named(base.to_owned())
        } else {
            return Err(err());
        };
        while !rest.is_empty() {
            if let Some(r) = rest.strip_prefix("[]") {
                t = t.list();
                rest = r;
            } else if let Some(r) = rest.strip_prefix('?') {
                t = t.optional();
                rest = r;
            } else {
                return Err(err());
            }
        }
        Ok(t)
    }
}

impl Serialize for TypeRef {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for TypeRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<TypeRef, D::Error> {
        let s = std::borrow::Cow::<str>::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_displays() {
        for s in [
            "string",
            "int",
            "json",
            "LineItem",
            "LineItem[]",
            "string?",
            "string[]?",
            "string?[]",
            "A1[][]",
            "ref Inventory",
            "ref Inventory[]?",
            "Upload.Progress",
        ] {
            let t: TypeRef = s.parse().unwrap();
            assert_eq!(t.to_string(), s);
        }
        assert_eq!(
            "string[]?".parse::<TypeRef>().unwrap(),
            TypeRef::Builtin(Builtin::String).list().optional()
        );
        assert_eq!(
            "Order[]".parse::<TypeRef>().unwrap().named_base(),
            Some("Order")
        );
        let r = "ref Inventory[]".parse::<TypeRef>().unwrap();
        assert_eq!(r, TypeRef::Ref("Inventory".into()).list());
        assert_eq!(r.ref_base(), Some("Inventory"));
        assert_eq!(r.named_base(), None);
    }

    #[test]
    fn rejects() {
        for s in [
            "",
            "order",
            "Order []",
            "Order[",
            "[]",
            "?",
            "ref order",
            "ref  Order",
            "ref int",
            "ref Order.Event",
            "Order.",
            "Order.event",
            "A.B.C",
            "int!",
            "Or_der",
        ] {
            assert!(s.parse::<TypeRef>().is_err(), "{s:?} should be rejected");
        }
    }
}
