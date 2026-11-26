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
/// Milestone 5 adds `ref Name` (a handle to a running machine). It will be a
/// new variant, written `ref Name` with the same suffix rules.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TypeRef {
    /// A built-in type such as `int`.
    Builtin(Builtin),
    /// A declared type, error or extern type, by name (`Order`). Inside a
    /// machine the bare `event` value of an `on` transition has the event's
    /// name as its type, which names an entry in the machine's `events`.
    Named(String),
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

    /// The declared name this type refers to, if its base is not a built-in.
    pub fn named_base(&self) -> Option<&str> {
        match self.base() {
            TypeRef::Named(n) => Some(n),
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
            "`{}` is not a type: expected a built-in (string, int, float, bool, duration, json) \
             or an upper-case name, followed by any number of `[]` and `?`",
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
        let base_len = s
            .find(|c: char| !c.is_ascii_alphanumeric())
            .unwrap_or(s.len());
        let (base, mut rest) = s.split_at(base_len);
        let mut t = if let Some(b) = Builtin::from_name(base) {
            TypeRef::Builtin(b)
        } else if is_upper_name(base) {
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
            "ref Order",
            "int!",
            "Or_der",
        ] {
            assert!(s.parse::<TypeRef>().is_err(), "{s:?} should be rejected");
        }
    }
}
