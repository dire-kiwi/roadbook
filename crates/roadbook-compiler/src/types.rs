//! Types as the checker sees them, and the rules for using one where another is expected.
//!
//! The rules (also in `language.md`): no implicit conversions; `int` and `float` are distinct;
//! `T` is assignable to `T?` but not the reverse; a list is assignable to a list whose element
//! type its own element type is assignable to, a map to a map whose value type its own is, and
//! an instance of a generic type to an instance of the same type whose arguments its own are;
//! any value but a `ref` is assignable to `json`; aliases are transparent.

use std::collections::HashMap;
use std::fmt;

use roadbook_ir::{Builtin, TypeRef};
use roadbook_syntax::ast;

/// A type. `Unknown` stands for a type that could not be determined because of an error that
/// was already reported; it is compatible with everything, so one mistake gives one
/// diagnostic.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Ty {
    Builtin(Builtin),
    /// A declared type by its project-wide name: record, alias, enum, error or extern.
    Named(String),
    /// An instance of a generic record or alias: `Page<Order>`.
    Generic(String, Vec<Ty>),
    /// `map<K, V>`
    Map(Box<Ty>, Box<Ty>),
    /// A type parameter, inside the declaration of a generic type.
    Param(String),
    List(Box<Ty>),
    Optional(Box<Ty>),
    /// An event's type: in `on Name`, the machine's event `Name`; for the value of an
    /// `emit Name`, its emitted event; `Machine.Name` for an event of another machine (in
    /// `on child.Name` and in `send child.Name`). Events are not declared types.
    Event(String),
    /// `ref Machine`: a handle to a running machine.
    Ref(String),
    Unknown,
}

pub(crate) const BOOL: Ty = Ty::Builtin(Builtin::Bool);
pub(crate) const DURATION: Ty = Ty::Builtin(Builtin::Duration);

impl Ty {
    pub fn to_ir(&self) -> TypeRef {
        match self {
            Ty::Builtin(b) => TypeRef::Builtin(*b),
            Ty::Named(n) | Ty::Event(n) | Ty::Param(n) => TypeRef::Named(n.clone()),
            Ty::Generic(n, args) => {
                TypeRef::Generic(n.clone(), args.iter().map(Ty::to_ir).collect())
            }
            Ty::Map(k, v) => TypeRef::Map(Box::new(k.to_ir()), Box::new(v.to_ir())),
            Ty::Ref(m) => TypeRef::Ref(m.clone()),
            Ty::List(t) => t.to_ir().list(),
            Ty::Optional(t) => t.to_ir().optional(),
            // Only reached when there are errors, and then the IR is not returned.
            Ty::Unknown => TypeRef::Builtin(Builtin::Json),
        }
    }

    pub fn is_unknown(&self) -> bool {
        *self == Ty::Unknown
    }

    /// The type with each of `params` replaced by the argument at the same index.
    pub fn substitute(&self, params: &[String], args: &[Ty]) -> Ty {
        if params.is_empty() {
            return self.clone();
        }
        match self {
            Ty::Param(p) => match params.iter().position(|x| x == p) {
                Some(i) => args.get(i).cloned().unwrap_or(Ty::Unknown),
                None => self.clone(),
            },
            Ty::Generic(n, a) => Ty::Generic(
                n.clone(),
                a.iter().map(|t| t.substitute(params, args)).collect(),
            ),
            Ty::Map(k, v) => Ty::Map(
                Box::new(k.substitute(params, args)),
                Box::new(v.substitute(params, args)),
            ),
            Ty::List(t) => Ty::List(Box::new(t.substitute(params, args))),
            Ty::Optional(t) => Ty::Optional(Box::new(t.substitute(params, args))),
            _ => self.clone(),
        }
    }

    /// Calls `f` with this type and every type inside it, outermost first.
    pub fn walk<'a>(&'a self, f: &mut impl FnMut(&'a Ty)) {
        f(self);
        match self {
            Ty::Generic(_, args) => args.iter().for_each(|a| a.walk(f)),
            Ty::Map(k, v) => {
                k.walk(f);
                v.walk(f);
            }
            Ty::List(t) | Ty::Optional(t) => t.walk(f),
            _ => {}
        }
    }

    /// Whether an event type appears anywhere in this type.
    pub fn has_event(&self) -> bool {
        let mut found = false;
        self.walk(&mut |t| found |= matches!(t, Ty::Event(_)));
        found
    }
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ty::Builtin(b) => f.write_str(b.name()),
            Ty::Named(n) | Ty::Param(n) => f.write_str(n),
            Ty::Generic(n, args) => {
                write!(f, "{n}<")?;
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{a}")?;
                }
                f.write_str(">")
            }
            Ty::Map(k, v) => write!(f, "map<{k}, {v}>"),
            Ty::Event(n) => write!(f, "event {n}"),
            Ty::Ref(m) => write!(f, "ref {m}"),
            Ty::List(t) => write!(f, "{t}[]"),
            Ty::Optional(t) => write!(f, "{t}?"),
            Ty::Unknown => f.write_str("_"),
        }
    }
}

/// The IR form of a type as written in source.
pub(crate) fn ir_type(t: &ast::TypeRef) -> TypeRef {
    match &t.kind {
        ast::TypeKind::Builtin(b) => TypeRef::Builtin(builtin(*b)),
        ast::TypeKind::Named(n) => TypeRef::Named(n.name.clone()),
        ast::TypeKind::Generic(n, args) => {
            TypeRef::Generic(n.name.clone(), args.iter().map(ir_type).collect())
        }
        ast::TypeKind::Map(k, v) => TypeRef::Map(Box::new(ir_type(k)), Box::new(ir_type(v))),
        ast::TypeKind::List(t) => ir_type(t).list(),
        ast::TypeKind::Optional(t) => ir_type(t).optional(),
        ast::TypeKind::Ref(m) => TypeRef::Ref(m.name.clone()),
    }
}

pub(crate) fn builtin(b: ast::Builtin) -> Builtin {
    match b {
        ast::Builtin::String => Builtin::String,
        ast::Builtin::Int => Builtin::Int,
        ast::Builtin::Float => Builtin::Float,
        ast::Builtin::Bool => Builtin::Bool,
        ast::Builtin::Duration => Builtin::Duration,
        ast::Builtin::Json => Builtin::Json,
    }
}

/// A declared type, with its member types resolved.
#[derive(Clone, Debug)]
pub(crate) enum TypeDef {
    Record(Vec<(String, Ty)>),
    Error(Vec<(String, Ty)>),
    Alias(Ty),
    Enum(Vec<String>),
    Extern,
}

/// Every declared type of the project, by name.
#[derive(Default)]
pub(crate) struct TypeEnv {
    pub defs: HashMap<String, TypeDef>,
    /// The type parameters of each generic record and alias. A type that is not generic has
    /// no entry.
    pub params: HashMap<String, Vec<String>>,
}

impl TypeEnv {
    /// The type parameters of the declaration `name`: empty unless it is generic.
    pub fn params_of(&self, name: &str) -> &[String] {
        self.params.get(name).map_or(&[], Vec::as_slice)
    }

    /// The type with every alias replaced by what it stands for, at every level, and the
    /// arguments of a generic alias substituted into it.
    pub fn expand(&self, t: &Ty) -> Ty {
        self.expand_n(t, 0)
    }

    /// `depth` counts the aliases expanded on the way here: a type as written is finite, so
    /// only alias expansion can go on, and alias cycles are reported and broken when the
    /// environment is built. The bound is a safety net.
    fn expand_n(&self, t: &Ty, depth: usize) -> Ty {
        if depth > 256 {
            return Ty::Unknown;
        }
        match t {
            Ty::Named(n) => match self.defs.get(n) {
                Some(TypeDef::Alias(target)) if self.params_of(n).is_empty() => {
                    self.expand_n(target, depth + 1)
                }
                _ => t.clone(),
            },
            Ty::Generic(n, args) => match self.defs.get(n) {
                Some(TypeDef::Alias(target)) => {
                    let target = target.substitute(self.params_of(n), args);
                    self.expand_n(&target, depth + 1)
                }
                _ => Ty::Generic(
                    n.clone(),
                    args.iter().map(|a| self.expand_n(a, depth)).collect(),
                ),
            },
            Ty::Map(k, v) => Ty::Map(
                Box::new(self.expand_n(k, depth)),
                Box::new(self.expand_n(v, depth)),
            ),
            Ty::List(e) => Ty::List(Box::new(self.expand_n(e, depth))),
            Ty::Optional(e) => Ty::Optional(Box::new(self.expand_n(e, depth))),
            _ => t.clone(),
        }
    }

    /// Whether a value of type `from` can be used where `to` is expected.
    pub fn assignable(&self, from: &Ty, to: &Ty) -> bool {
        assignable(&self.expand(from), &self.expand(to))
    }

    /// The fields of a record or error type (through aliases), with the type arguments of a
    /// generic record substituted, or `None` if it has none.
    pub fn fields(&self, t: &Ty) -> Option<Vec<(String, Ty)>> {
        let (n, args) = match self.expand(t) {
            Ty::Named(n) => (n, Vec::new()),
            Ty::Generic(n, args) => (n, args),
            _ => return None,
        };
        match self.defs.get(&n) {
            Some(TypeDef::Record(f) | TypeDef::Error(f)) => {
                let params = self.params_of(&n);
                Some(
                    f.iter()
                        .map(|(name, t)| (name.clone(), t.substitute(params, &args)))
                        .collect(),
                )
            }
            _ => None,
        }
    }

    /// Whether the type is a `ref`, or holds one in a list, optional or map, through aliases.
    pub fn has_ref(&self, t: &Ty) -> bool {
        has_ref(&self.expand(t))
    }

    /// Whether `==` and `!=` are defined on the type: scalars and enums.
    pub fn is_equatable(&self, t: &Ty) -> bool {
        match self.expand(t) {
            Ty::Builtin(b) => b != Builtin::Json,
            Ty::Named(n) => matches!(self.defs.get(&n), Some(TypeDef::Enum(_))),
            Ty::Unknown => true,
            _ => false,
        }
    }

    pub fn same(&self, a: &Ty, b: &Ty) -> bool {
        let (a, b) = (self.expand(a), self.expand(b));
        a.is_unknown() || b.is_unknown() || a == b
    }
}

fn has_ref(t: &Ty) -> bool {
    let mut found = false;
    t.walk(&mut |t| found |= matches!(t, Ty::Ref(_)));
    found
}

fn assignable(from: &Ty, to: &Ty) -> bool {
    match (from, to) {
        (Ty::Unknown, _) | (_, Ty::Unknown) => true,
        // A handle is not data: it has no JSON form.
        (f, Ty::Builtin(Builtin::Json)) => !has_ref(f),
        (Ty::Optional(f), Ty::Optional(t)) => assignable(f, t),
        (f, Ty::Optional(t)) => assignable(f, t),
        (Ty::List(f), Ty::List(t)) => assignable(f, t),
        // Keys are strings, whatever alias names them.
        (Ty::Map(_, f), Ty::Map(_, t)) => assignable(f, t),
        (Ty::Generic(a, fs), Ty::Generic(b, ts)) => {
            a == b && fs.len() == ts.len() && fs.iter().zip(ts).all(|(f, t)| assignable(f, t))
        }
        (f, t) => f == t,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(b: Builtin) -> Ty {
        Ty::Builtin(b)
    }

    #[test]
    fn assignability() {
        let mut env = TypeEnv::default();
        env.defs
            .insert("UserId".into(), TypeDef::Alias(b(Builtin::String)));
        env.defs.insert("Order".into(), TypeDef::Record(vec![]));
        let int = b(Builtin::Int);
        let float = b(Builtin::Float);
        let string = b(Builtin::String);
        let json = b(Builtin::Json);
        let opt = |t: &Ty| Ty::Optional(Box::new(t.clone()));
        let list = |t: &Ty| Ty::List(Box::new(t.clone()));
        assert!(env.assignable(&int, &int));
        assert!(!env.assignable(&int, &float));
        assert!(!env.assignable(&float, &int));
        assert!(env.assignable(&int, &opt(&int)));
        assert!(!env.assignable(&opt(&int), &int));
        assert!(env.assignable(&list(&int), &json));
        assert!(env.assignable(&list(&int), &list(&json)));
        assert!(env.assignable(&Ty::Named("UserId".into()), &string));
        assert!(env.assignable(&string, &Ty::Named("UserId".into())));
        assert!(!env.assignable(&Ty::Named("Order".into()), &string));
        assert!(!env.assignable(&Ty::Event("Order".into()), &Ty::Named("Order".into())));
        assert!(env.assignable(&Ty::Event("Order".into()), &json));
        let inv = Ty::Ref("Inventory".into());
        assert!(env.assignable(&inv, &inv));
        assert!(env.assignable(&inv, &opt(&inv)));
        assert!(!env.assignable(&inv, &Ty::Ref("Other".into())));
        assert!(!env.assignable(&inv, &json));
        assert!(!env.assignable(&list(&inv), &list(&json)));
        assert!(!env.is_equatable(&inv));
        let map = |t: &Ty| Ty::Map(Box::new(string.clone()), Box::new(t.clone()));
        assert!(env.assignable(&map(&int), &map(&opt(&int))));
        assert!(env.assignable(&map(&int), &json));
        assert!(!env.assignable(&map(&int), &map(&float)));
        assert!(!env.assignable(&map(&int), &list(&int)));
        env.defs.insert(
            "Page".into(),
            TypeDef::Record(vec![("items".into(), list(&Ty::Param("T".into())))]),
        );
        env.params.insert("Page".into(), vec!["T".into()]);
        env.defs
            .insert("Dict".into(), TypeDef::Alias(map(&Ty::Param("T".into()))));
        env.params.insert("Dict".into(), vec!["T".into()]);
        let page = |t: &Ty| Ty::Generic("Page".into(), vec![t.clone()]);
        assert!(env.assignable(&page(&int), &page(&opt(&int))));
        assert!(!env.assignable(&page(&opt(&int)), &page(&int)));
        assert!(!env.assignable(&page(&int), &Ty::Named("Page".into())));
        let dict = Ty::Generic("Dict".into(), vec![int.clone()]);
        assert!(env.assignable(&dict, &map(&int)));
        assert!(env.assignable(&map(&int), &dict));
        assert_eq!(
            env.fields(&page(&string)),
            Some(vec![("items".into(), list(&string))])
        );
    }
}
