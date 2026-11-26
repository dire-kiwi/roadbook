//! `sensitive`: which fields, parameters and variables are marked, and the checks that keep a
//! sensitive value from reaching a place a trace shows as it is.
//!
//! A runtime redacts a value by its declared type (`spec/trace.md#redaction`), so a value is
//! safe wherever the place it goes is sensitive itself, or has a type whose sensitive fields
//! are known. Two moves would lose that, and are errors:
//!
//! - a value read from a sensitive place (`password`, `event.password`, `creds.user` for a
//!   sensitive `creds`) going to a place that is not sensitive: a variable, a parameter of an
//!   action, task or machine, a field, an `output`, a `fail` or an `after` delay (`RB0317`);
//! - a value whose type has sensitive fields going where `json` is expected, which has no
//!   fields to redact by (`RB0318`).
//!
//! Function arguments and guards are exempt: function calls are not traced. An operator's result
//! with a sensitive operand is sensitive; a function's result is not, since a `fn` is how a
//! chart derives what may be shown.

use std::collections::HashSet;

use roadbook_ir::Builtin;
use roadbook_syntax::Diagnostic;
use roadbook_syntax::ast::{self, DeclKind, Expr};

use crate::check::Checker;
use crate::codes;
use crate::machine::Cx;
use crate::types::{Ty, TypeDef};

/// Every member marked `sensitive`, as `(owner, member)`: the owner is a record or error type
/// (its fields), a function, action, task or machine (its parameters), a machine (its
/// variables), or `Machine.Event` (the fields of an event or emitted event). Declaration names
/// are unique in a compilation and event keys have a dot, so owners never collide.
#[derive(Default)]
pub(crate) struct Sensitive {
    marked: HashSet<(String, String)>,
}

impl Sensitive {
    pub fn build<'p>(decls: impl IntoIterator<Item = &'p ast::Decl>) -> Sensitive {
        let mut s = Sensitive::default();
        let fields = |s: &mut Sensitive, owner: &str, fs: &[ast::Field]| {
            for f in fs.iter().filter(|f| f.sensitive) {
                s.marked.insert((owner.to_owned(), f.name.name.clone()));
            }
        };
        for d in decls {
            let name = d.name.name.as_str();
            match &d.kind {
                DeclKind::Record { fields: fs } | DeclKind::Error { fields: fs } => {
                    fields(&mut s, name, fs)
                }
                DeclKind::Fn { params, .. }
                | DeclKind::Action { params }
                | DeclKind::Task { params, .. } => fields(&mut s, name, params),
                DeclKind::Machine(m) => {
                    fields(&mut s, name, &m.params);
                    for item in &m.body.items {
                        match item {
                            ast::Item::Event(e) | ast::Item::Emits(e) => {
                                fields(&mut s, &format!("{name}.{}", e.name.name), &e.fields)
                            }
                            _ => {}
                        }
                    }
                    for v in m.body.vars().filter(|v| v.sensitive) {
                        s.marked.insert((name.to_owned(), v.name.name.clone()));
                    }
                }
                DeclKind::Alias { .. } | DeclKind::Enum { .. } | DeclKind::Extern => {}
            }
        }
        s
    }

    /// Whether `member` of `owner` is marked.
    pub fn is(&self, owner: &str, member: &str) -> bool {
        self.marked.contains(&(owner.to_owned(), member.to_owned()))
    }
}

/// Where a checked value goes, for the sensitivity checks.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum Dest {
    /// A sensitive place: anything may go there.
    Sensitive,
    /// A place that could be marked `sensitive` and is not: a variable, a parameter, a field,
    /// named as the help names it (`` `note` ``, `` field `note` of `Box` ``).
    Plain(String),
    /// A place a trace shows and that cannot be marked: an `output`, a `fail`, an `after`
    /// delay.
    Shown,
}

impl Checker<'_> {
    /// Whether a value of type `t` can hold a value of a sensitive field: a record, error or
    /// event with a sensitive field, or with a field whose type can, through lists, optionals
    /// and aliases. A bare event name is one of `machine`'s.
    pub(crate) fn carries_sensitive(&self, t: &Ty, machine: &str) -> bool {
        self.carries(t, machine, &mut HashSet::new())
    }

    fn carries(&self, t: &Ty, machine: &str, seen: &mut HashSet<String>) -> bool {
        match self.env.expand(t) {
            Ty::List(e) | Ty::Optional(e) => self.carries(&e, machine, seen),
            Ty::Named(n) => {
                if !seen.insert(n.clone()) {
                    return false;
                }
                match self.env.defs.get(&n) {
                    Some(TypeDef::Record(fs) | TypeDef::Error(fs)) => fs
                        .iter()
                        .any(|(f, ft)| self.sensitive.is(&n, f) || self.carries(ft, machine, seen)),
                    _ => false,
                }
            }
            Ty::Event(n) => {
                let (m, e) = n.split_once('.').unwrap_or((machine, n.as_str()));
                let key = format!("{m}.{e}");
                if !seen.insert(key.clone()) {
                    return false;
                }
                let Some(iface) = self.interfaces.get(m) else {
                    return false;
                };
                let fields = iface.events.get(e).or_else(|| iface.emits.get(e));
                fields.is_some_and(|fs| {
                    fs.iter().any(|(f, ft)| {
                        self.sensitive.is(&key, f) || self.carries(ft, machine, seen)
                    })
                })
            }
            _ => false,
        }
    }

    /// Whether using a `got` where a `want` is expected puts a value that carries sensitive
    /// fields where `json` is expected, at any depth of `[]` and `?`.
    fn leaks_into_json(&self, got: &Ty, want: &Ty, machine: &str) -> bool {
        match (self.env.expand(got), self.env.expand(want)) {
            (_, Ty::Builtin(Builtin::Json)) => self.carries_sensitive(got, machine),
            (Ty::List(g), Ty::List(w)) | (Ty::Optional(g), Ty::Optional(w)) => {
                self.leaks_into_json(&g, &w, machine)
            }
            (g, Ty::Optional(w)) => self.leaks_into_json(&g, &w, machine),
            _ => false,
        }
    }
}

impl Cx<'_, '_> {
    /// The owner key of the fields of a value of type `t`, for [`Sensitive::is`]: the record or
    /// error type, or `Machine.Event`.
    pub(crate) fn field_owner(&self, t: &Ty) -> Option<String> {
        match self.ck.env.expand(t) {
            Ty::Named(n) => Some(n),
            Ty::Event(n) if n.contains('.') => Some(n),
            Ty::Event(n) => Some(format!("{}.{n}", self.machine)),
            _ => None,
        }
    }

    /// Whether field `name` of a value of type `t` is sensitive.
    pub(crate) fn field_is_sensitive(&self, t: &Ty, name: &str) -> bool {
        self.field_owner(t)
            .is_some_and(|o| self.ck.sensitive.is(&o, name))
    }

    /// Checks `e` against `want` like [`Cx::expect`], then that no sensitive value goes to
    /// `target` where a trace would show it.
    pub(crate) fn expect_to(
        &mut self,
        e: &Expr,
        scope: &crate::machine::Scope,
        want: &Ty,
        what: &str,
        target: Dest,
    ) -> roadbook_ir::Expr {
        let (ie, got) = self.expect_typed(e, scope, want, what);
        if target == Dest::Sensitive {
            return ie;
        }
        if self.sens {
            let help = match &target {
                Dest::Plain(place) => format!(
                    "traces redact only what is marked: mark {place} `sensitive` too, or derive what may be shown with a `fn`"
                ),
                _ => format!(
                    "traces show {what} as it is: carry the value in a field marked `sensitive`, or derive what may be shown with a `fn`"
                ),
            };
            self.ck.diags.push(
                Diagnostic::error(
                    codes::SENSITIVE_VALUE,
                    e.span,
                    format!("`{}` is sensitive, and {what} is not", ie.text),
                )
                .with_label("a trace would show this value")
                .with_help(help),
            );
        } else if self.ck.leaks_into_json(&got, want, &self.machine) {
            self.ck.diags.push(
                Diagnostic::error(
                    codes::SENSITIVE_INTO_JSON,
                    e.span,
                    format!("`{got}` has sensitive fields, and {what} is `{want}`"),
                )
                .with_label("a trace would show these fields")
                .with_help(match &target {
                    Dest::Plain(place) => format!(
                        "traces redact a value by its type, and `json` has no fields to redact by: give {place} the value's own type, or mark it `sensitive`"
                    ),
                    _ => "traces redact a value by its type, and `json` has no fields to redact by: use the value's own type".to_owned(),
                }),
            );
        }
        ie
    }
}
