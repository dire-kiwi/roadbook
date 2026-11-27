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
/// In JSON a `TypeRef` is a string: a base followed by any number of `[]`
/// (list of) and `?` (optional) suffixes, applied left to right. `LineItem[]`
/// is a list of `LineItem`; `string[]?` is an optional list of strings;
/// `string?[]` is a list of optional strings. The base is a built-in keyword,
/// a declared upper-case name with optional type arguments (`Page<Order>`),
/// an event type `Machine.Event`, a handle `ref Machine`, a pending call
/// `call name`, or a map `map<string, Order>`. Type arguments are separated
/// by `, `; there is no other whitespace, except the one space after `ref`
/// or `call`.
///
/// `call name` is a pending call of the machine's own call `name`: only the
/// type of a machine variable and of the implicit `call` in `on name`, never
/// with a suffix.
///
/// `Display` and `FromStr` convert to and from that string, and
/// `parse(display(t)) == t` for every `t`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TypeRef {
    /// A built-in type such as `int`.
    Builtin(Builtin),
    /// A declared type, error or extern type, by name (`Order`); or, inside a
    /// generic declaration, one of its type parameters (`T`).
    ///
    /// Event types are named here too. Inside a machine, the bare name of one
    /// of its own events or emitted events (`Cancel`, `Progress`) is that
    /// event: the type of `event` in `on Cancel`, of the value of
    /// `emit Progress { … }`. A dotted name `Machine.Event` (`Upload.Pause`)
    /// is an event of another machine: the type of `event` in
    /// `on upload.Progress`, of the value of `send upload.Pause`.
    Named(String),
    /// `Name<A, B>`: a generic declared type with its type arguments, at
    /// least one.
    Generic(String, Vec<TypeRef>),
    /// `map<K, V>`: a map from keys of type `K` (`string` or an alias of it)
    /// to values of type `V`.
    Map(Box<TypeRef>, Box<TypeRef>),
    /// `ref Name`: a handle to a running machine, a key of `machines`.
    Ref(String),
    /// `call name`: a pending call of the machine's call `name`, a key of its
    /// `calls`.
    Call(String),
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

    /// The declared name this type refers to, if its base is a declared type,
    /// generic or not (not a built-in, a map or a `ref`).
    pub fn named_base(&self) -> Option<&str> {
        match self.base() {
            TypeRef::Named(n) | TypeRef::Generic(n, _) => Some(n),
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

    /// Calls `f` with every name this type mentions as a declared type (or a
    /// type parameter, or an event type), outermost first: `map<UserId,
    /// Page<Order>>` gives `UserId`, `Page` and `Order`. `ref` machines and
    /// `call` names are not names of types and are not given.
    pub fn for_each_name<'a>(&'a self, f: &mut impl FnMut(&'a str)) {
        match self {
            TypeRef::Builtin(_) | TypeRef::Ref(_) | TypeRef::Call(_) => {}
            TypeRef::Named(n) => f(n),
            TypeRef::Generic(n, args) => {
                f(n);
                for a in args {
                    a.for_each_name(f);
                }
            }
            TypeRef::Map(k, v) => {
                k.for_each_name(f);
                v.for_each_name(f);
            }
            TypeRef::List(t) | TypeRef::Optional(t) => t.for_each_name(f),
        }
    }

    /// Whether a `ref` appears anywhere in this type (not through aliases).
    pub fn mentions_ref(&self) -> bool {
        match self {
            TypeRef::Ref(_) => true,
            TypeRef::Builtin(_) | TypeRef::Named(_) | TypeRef::Call(_) => false,
            TypeRef::Generic(_, args) => args.iter().any(TypeRef::mentions_ref),
            TypeRef::Map(k, v) => k.mentions_ref() || v.mentions_ref(),
            TypeRef::List(t) | TypeRef::Optional(t) => t.mentions_ref(),
        }
    }

    /// The type with each of `params` replaced by the argument at the same
    /// index: the member types of an instance `G<args>` of a generic
    /// declaration with these parameters. Other names are left alone.
    pub fn substitute(&self, params: &[String], args: &[TypeRef]) -> TypeRef {
        if params.is_empty() {
            return self.clone();
        }
        match self {
            TypeRef::Named(n) => match params.iter().position(|p| p == n) {
                Some(i) => args.get(i).cloned().unwrap_or_else(|| self.clone()),
                None => self.clone(),
            },
            TypeRef::Generic(n, a) => TypeRef::Generic(
                n.clone(),
                a.iter().map(|t| t.substitute(params, args)).collect(),
            ),
            TypeRef::Map(k, v) => TypeRef::Map(
                Box::new(k.substitute(params, args)),
                Box::new(v.substitute(params, args)),
            ),
            TypeRef::List(t) => t.substitute(params, args).list(),
            TypeRef::Optional(t) => t.substitute(params, args).optional(),
            TypeRef::Builtin(_) | TypeRef::Ref(_) | TypeRef::Call(_) => self.clone(),
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
            TypeRef::Generic(n, args) => {
                write!(f, "{n}<")?;
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{a}")?;
                }
                f.write_str(">")
            }
            TypeRef::Map(k, v) => write!(f, "map<{k}, {v}>"),
            TypeRef::Ref(n) => write!(f, "ref {n}"),
            TypeRef::Call(n) => write!(f, "call {n}"),
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
             an upper-case name with optional type arguments (`Page<Order>`), `Machine.Event`, \
             `ref Machine`, `call name` or `map<K, V>`, followed by any number of `[]` and `?`",
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

/// The deepest nesting of type arguments read, so that a hostile IR cannot
/// exhaust the stack. The source grammar allows at most 64 levels.
const MAX_TYPE_DEPTH: usize = 128;

/// A recursive descent reader of one type reference.
struct TypeReader<'s> {
    rest: &'s str,
}

impl TypeReader<'_> {
    fn eat(&mut self, s: &str) -> bool {
        match self.rest.strip_prefix(s) {
            Some(r) => {
                self.rest = r;
                true
            }
            None => false,
        }
    }

    /// A run of `[A-Za-z0-9.]`.
    fn word(&mut self) -> &str {
        let len = self
            .rest
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '.')
            .unwrap_or(self.rest.len());
        let (w, r) = self.rest.split_at(len);
        self.rest = r;
        w
    }

    fn ty(&mut self, depth: usize) -> Option<TypeRef> {
        if depth > MAX_TYPE_DEPTH {
            return None;
        }
        let mut t = if self.eat("ref ") {
            let m = self.word();
            if !is_upper_name(m) {
                return None;
            }
            TypeRef::Ref(m.to_owned())
        } else if self.eat("call ") {
            let c = self.word();
            if !is_lower_name(c) {
                return None;
            }
            TypeRef::Call(c.to_owned())
        } else {
            let base = self.word().to_owned();
            if base == "map" && self.eat("<") {
                let k = self.ty(depth + 1)?;
                if !self.eat(", ") {
                    return None;
                }
                let v = self.ty(depth + 1)?;
                if !self.eat(">") {
                    return None;
                }
                TypeRef::Map(Box::new(k), Box::new(v))
            } else if let Some(b) = Builtin::from_name(&base) {
                TypeRef::Builtin(b)
            } else if is_upper_name(&base) {
                if self.eat("<") {
                    let mut args = vec![self.ty(depth + 1)?];
                    while self.eat(", ") {
                        args.push(self.ty(depth + 1)?);
                    }
                    if !self.eat(">") {
                        return None;
                    }
                    TypeRef::Generic(base, args)
                } else {
                    TypeRef::Named(base)
                }
            } else if let Some((m, e)) = base.split_once('.')
                && is_upper_name(m)
                && is_upper_name(e)
            {
                TypeRef::Named(base)
            } else {
                return None;
            }
        };
        // Each suffix is a level too, counted against the same bound.
        let mut depth = depth;
        loop {
            if self.eat("[]") {
                t = t.list();
            } else if self.eat("?") {
                t = t.optional();
            } else {
                return Some(t);
            }
            depth += 1;
            if depth > MAX_TYPE_DEPTH {
                return None;
            }
        }
    }
}

/// True for `[a-z][A-Za-z0-9]*`, the grammar's `Lower`.
fn is_lower_name(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_alphanumeric())
}

impl FromStr for TypeRef {
    type Err = TypeRefError;

    fn from_str(s: &str) -> Result<TypeRef, TypeRefError> {
        let mut r = TypeReader { rest: s };
        match r.ty(0) {
            Some(t) if r.rest.is_empty() => Ok(t),
            _ => Err(TypeRefError {
                input: s.to_owned(),
            }),
        }
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
            "map<string, int>",
            "map<UserId, Order[]>?",
            "Page<Order>",
            "Pair<string, map<string, Tree<int>>>[]",
            "Page<Order?>[]?",
            "call reserve",
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
        let g = "Page<T[]>".parse::<TypeRef>().unwrap();
        assert_eq!(
            g.substitute(&["T".into()], &[TypeRef::named("Order")]),
            "Page<Order[]>".parse().unwrap()
        );
        let mut names = Vec::new();
        "map<UserId, Page<Order>>"
            .parse::<TypeRef>()
            .unwrap()
            .for_each_name(&mut |n| names.push(n.to_owned()));
        assert_eq!(names, ["UserId", "Page", "Order"]);
        let deep = format!("{}int{}", "Page<".repeat(200), ">".repeat(200));
        assert!(deep.parse::<TypeRef>().is_err());
        assert!(
            format!("int{}", "[]".repeat(100_000))
                .parse::<TypeRef>()
                .is_err()
        );
        assert!(
            format!("int{}", "[]".repeat(100))
                .parse::<TypeRef>()
                .is_ok()
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
            "ref order",
            "ref  Order",
            "ref int",
            "ref Order.Event",
            "Order.",
            "Order.event",
            "A.B.C",
            "int!",
            "Or_der",
            "map<string,int>",
            "map<string>",
            "map<string, int, int>",
            "Page<>",
            "Page<Order",
            "Page <Order>",
            "page<Order>",
            "ref Page<Order>",
            "Upload.Progress<int>",
            "call Reserve",
            "call  reserve",
            "call re.serve",
        ] {
            assert!(s.parse::<TypeRef>().is_err(), "{s:?} should be rejected");
        }
    }
}
