//! Types as the checker sees them, and the rules for using one where another is expected.
//!
//! The rules (also in `language.md`): no implicit conversions; `int` and `float` are distinct;
//! `T` is assignable to `T?` but not the reverse; a list is assignable to a list whose element
//! type its own element type is assignable to, a map to a map whose value type its own is, and
//! an instance of a generic type to an instance of the same type whose arguments its own are;
//! any value but a `ref` or a pending call is assignable to `json`; aliases are transparent.

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
    /// `call name`: a pending call of the machine's own call `name`.
    Call(String),
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
            Ty::Call(c) => TypeRef::Call(c.clone()),
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
            Ty::Call(c) => write!(f, "call {c}"),
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
        ast::TypeKind::Call(c) => TypeRef::Call(c.name.clone()),
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

/// The fields of an event type, by its name, or `None` for one that is not an event.
pub(crate) type EventFields<'a> = dyn Fn(&str) -> Option<Vec<(String, Ty)>> + 'a;

/// The part of a type that `==` cannot compare ([`TypeEnv::incomparable`]).
#[derive(Debug)]
pub(crate) struct Incomparable {
    /// The innermost record field that holds it, `Order.extra`.
    pub field: Option<String>,
    /// The field's type as declared, or the part itself when no field holds it.
    pub ty: Ty,
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

    /// Why `==` and `!=` are not defined on the type, or `None` if they are. They are defined
    /// on scalars and enums, and on records, errors, events, lists, maps and optionals whose
    /// parts all are, recursively; not on `json`, an `extern type`, a `ref` or a pending call.
    /// `events` gives the fields of an event type by its name. What is returned is the part
    /// that cannot be compared: the innermost record field holding it (`Order.extra`) with
    /// the field's type, or, when no field holds it, the part itself.
    pub fn incomparable(&self, t: &Ty, events: &EventFields<'_>) -> Option<Incomparable> {
        self.incomparable_in(t, events, &mut Vec::new())
    }

    /// `seen`: the records already being compared, which a recursive type reaches again.
    fn incomparable_in(
        &self,
        t: &Ty,
        events: &EventFields<'_>,
        seen: &mut Vec<Ty>,
    ) -> Option<Incomparable> {
        let x = self.expand(t);
        let fields = match &x {
            Ty::Unknown => return None,
            Ty::Builtin(Builtin::Json) | Ty::Ref(_) | Ty::Call(_) | Ty::Param(_) => {
                return Some(Incomparable { field: None, ty: x });
            }
            Ty::Builtin(_) => return None,
            Ty::List(e) | Ty::Optional(e) | Ty::Map(_, e) => {
                return self.incomparable_in(e, events, seen);
            }
            Ty::Named(n) if matches!(self.defs.get(n), Some(TypeDef::Enum(_))) => return None,
            _ if seen.contains(&x) => return None,
            Ty::Event(n) => events(n),
            Ty::Named(_) | Ty::Generic(..) => self.fields(&x),
        };
        let Some(fields) = fields else {
            return Some(Incomparable { field: None, ty: x });
        };
        seen.push(x.clone());
        fields.into_iter().find_map(|(name, ft)| {
            let part = self.incomparable_in(&ft, events, seen)?;
            Some(match part.field {
                Some(_) => part,
                None => Incomparable {
                    field: Some(format!(
                        "{}.{name}",
                        x.to_string().trim_start_matches("event ")
                    )),
                    ty: ft,
                },
            })
        })
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
        // A handle or a pending call is not data: it has no JSON form.
        (f, Ty::Builtin(Builtin::Json)) => !has_ref(f) && !matches!(f, Ty::Call(_)),
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
        let incomparable = |env: &TypeEnv, t: &Ty| env.incomparable(t, &|_| None).is_some();
        assert!(incomparable(&env, &inv));
        let pending = Ty::Call("reserve".into());
        assert!(env.assignable(&pending, &pending));
        assert!(!env.assignable(&pending, &Ty::Call("count".into())));
        assert!(!env.assignable(&pending, &json));
        assert!(incomparable(&env, &pending));
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

    #[test]
    fn equatability() {
        let mut env = TypeEnv::default();
        let json = b(Builtin::Json);
        let int = b(Builtin::Int);
        let list = |t: &Ty| Ty::List(Box::new(t.clone()));
        let opt = |t: &Ty| Ty::Optional(Box::new(t.clone()));
        let named = |n: &str| Ty::Named(n.into());
        env.defs.insert(
            "Order".into(),
            TypeDef::Record(vec![
                ("id".into(), int.clone()),
                ("extra".into(), list(&json)),
            ]),
        );
        env.defs.insert(
            "Line".into(),
            TypeDef::Record(vec![("order".into(), opt(&named("Order")))]),
        );
        env.defs.insert(
            "Tree".into(),
            TypeDef::Record(vec![
                ("value".into(), int.clone()),
                ("kids".into(), list(&named("Tree"))),
            ]),
        );
        env.defs.insert(
            "Box".into(),
            TypeDef::Record(vec![("value".into(), Ty::Param("T".into()))]),
        );
        env.params.insert("Box".into(), vec!["T".into()]);
        env.defs.insert("Handle".into(), TypeDef::Extern);
        env.defs
            .insert("Colour".into(), TypeDef::Enum(vec!["Red".into()]));
        let no_events = |_: &str| None;
        let part = |t: &Ty| {
            env.incomparable(t, &no_events)
                .map(|p| (p.field, p.ty.to_string()))
        };
        assert_eq!(part(&list(&opt(&int))), None);
        assert_eq!(part(&named("Colour")), None);
        assert_eq!(part(&named("Tree")), None);
        assert_eq!(part(&json), Some((None, "json".into())));
        assert_eq!(part(&named("Handle")), Some((None, "Handle".into())));
        assert_eq!(
            part(&list(&named("Line"))),
            Some((Some("Order.extra".into()), "json[]".into()))
        );
        let boxed = |t: &Ty| Ty::Generic("Box".into(), vec![t.clone()]);
        assert_eq!(part(&boxed(&named("Tree"))), None);
        assert_eq!(
            part(&boxed(&json)),
            Some((Some("Box<json>.value".into()), "json".into()))
        );
        let events = |n: &str| (n == "Go").then(|| vec![("payload".into(), b(Builtin::Json))]);
        assert_eq!(
            env.incomparable(&Ty::Event("Go".into()), &events)
                .map(|p| p.field),
            Some(Some("Go.payload".into()))
        );
    }
}
