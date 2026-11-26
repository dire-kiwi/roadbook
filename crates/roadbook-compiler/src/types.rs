//! Types as the checker sees them, and the rules for using one where another is expected.
//!
//! The rules (also in `language.md`): no implicit conversions; `int` and `float` are distinct;
//! `T` is assignable to `T?` but not the reverse; a list is assignable to a list whose element
//! type its own element type is assignable to; any value is assignable to `json`; aliases are
//! transparent.

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
    List(Box<Ty>),
    Optional(Box<Ty>),
    /// The type of `event` in `on Name`: the machine's event `Name`, which is not a declared
    /// type (an event may share its name with one).
    Event(String),
    Unknown,
}

pub(crate) const BOOL: Ty = Ty::Builtin(Builtin::Bool);
pub(crate) const DURATION: Ty = Ty::Builtin(Builtin::Duration);

impl Ty {
    pub fn to_ir(&self) -> TypeRef {
        match self {
            Ty::Builtin(b) => TypeRef::Builtin(*b),
            Ty::Named(n) | Ty::Event(n) => TypeRef::Named(n.clone()),
            Ty::List(t) => t.to_ir().list(),
            Ty::Optional(t) => t.to_ir().optional(),
            // Only reached when there are errors, and then the IR is not returned.
            Ty::Unknown => TypeRef::Builtin(Builtin::Json),
        }
    }

    pub fn is_unknown(&self) -> bool {
        *self == Ty::Unknown
    }
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ty::Builtin(b) => f.write_str(b.name()),
            Ty::Named(n) => f.write_str(n),
            Ty::Event(n) => write!(f, "event {n}"),
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
        ast::TypeKind::List(t) => ir_type(t).list(),
        ast::TypeKind::Optional(t) => ir_type(t).optional(),
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
}

impl TypeEnv {
    /// The type with every alias replaced by what it stands for, at every level.
    pub fn expand(&self, t: &Ty) -> Ty {
        self.expand_n(t, 0)
    }

    fn expand_n(&self, t: &Ty, depth: usize) -> Ty {
        // Alias cycles are reported and broken when the environment is built; the bound is a
        // safety net.
        if depth > 64 {
            return Ty::Unknown;
        }
        match t {
            Ty::Named(n) => match self.defs.get(n) {
                Some(TypeDef::Alias(target)) => self.expand_n(target, depth + 1),
                _ => t.clone(),
            },
            Ty::List(e) => Ty::List(Box::new(self.expand_n(e, depth + 1))),
            Ty::Optional(e) => Ty::Optional(Box::new(self.expand_n(e, depth + 1))),
            _ => t.clone(),
        }
    }

    /// Whether a value of type `from` can be used where `to` is expected.
    pub fn assignable(&self, from: &Ty, to: &Ty) -> bool {
        assignable(&self.expand(from), &self.expand(to))
    }

    /// The fields of a record or error type (through aliases), or `None` if it has none.
    pub fn fields(&self, t: &Ty) -> Option<&[(String, Ty)]> {
        match self.expand(t) {
            Ty::Named(n) => match self.defs.get(&n) {
                Some(TypeDef::Record(f) | TypeDef::Error(f)) => Some(f),
                _ => None,
            },
            _ => None,
        }
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

fn assignable(from: &Ty, to: &Ty) -> bool {
    match (from, to) {
        (Ty::Unknown, _) | (_, Ty::Unknown) => true,
        (_, Ty::Builtin(Builtin::Json)) => true,
        (Ty::Optional(f), Ty::Optional(t)) => assignable(f, t),
        (f, Ty::Optional(t)) => assignable(f, t),
        (Ty::List(f), Ty::List(t)) => assignable(f, t),
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
    }
}
