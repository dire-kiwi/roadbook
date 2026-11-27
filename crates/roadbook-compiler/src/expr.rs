//! Expressions: type checking and lowering to IR expression nodes.

use std::collections::HashMap;

use roadbook_ir::{self as ir, Builtin, ExprKind as K, Literal, MAX_SAFE_INTEGER, Op};
use roadbook_syntax::ast::{BinaryOp, DeclKind, Expr, ExprKind, FieldValue, UnaryOp};
use roadbook_syntax::{Diagnostic, expr_text};

use crate::check::SigKind;
use crate::codes;
use crate::index::SymbolId;
use crate::machine::{Cx, Implicit, Scope};
use crate::names::{Found, is_type, suggest};
use crate::sensitive::{Dest, Sensitive};
use crate::types::{BOOL, Ty, TypeDef};

const INT: Ty = Ty::Builtin(Builtin::Int);
const FLOAT: Ty = Ty::Builtin(Builtin::Float);

fn node(kind: K, ty: &Ty, e: &Expr) -> ir::Expr {
    ir::Expr {
        kind,
        ty: ty.to_ir(),
        text: expr_text(e),
    }
}

fn op(o: BinaryOp) -> Op {
    match o {
        BinaryOp::Or => Op::Or,
        BinaryOp::And => Op::And,
        BinaryOp::Eq => Op::Eq,
        BinaryOp::Ne => Op::Ne,
        BinaryOp::Lt => Op::Lt,
        BinaryOp::Le => Op::Le,
        BinaryOp::Gt => Op::Gt,
        BinaryOp::Ge => Op::Ge,
        BinaryOp::Add => Op::Add,
        BinaryOp::Sub => Op::Sub,
        BinaryOp::Mul => Op::Mul,
    }
}

impl Cx<'_, '_> {
    /// Checks `e` against an expected type. `what` names the position in the diagnostic: "a
    /// guard", "argument `x` of `f`".
    pub(crate) fn expect(&mut self, e: &Expr, scope: &Scope, want: &Ty, what: &str) -> ir::Expr {
        self.expect_typed(e, scope, want, what).0
    }

    /// [`Cx::expect`], also giving the type `e` was found to have.
    pub(crate) fn expect_typed(
        &mut self,
        e: &Expr,
        scope: &Scope,
        want: &Ty,
        what: &str,
    ) -> (ir::Expr, Ty) {
        let (ie, got) = match &e.kind {
            ExprKind::Record { .. } => {
                let out = self.record(e, scope, Some(want));
                // A record value is not sensitive itself: its sensitive fields are redacted by
                // its type.
                self.sens = false;
                out
            }
            _ => self.expr(e, scope),
        };
        self.check_assignable(e, &got, want, what);
        (ie, got)
    }

    /// Reports `e`, of type `got`, if it cannot be used where `want` is expected.
    fn check_assignable(&mut self, e: &Expr, got: &Ty, want: &Ty, what: &str) {
        if !self.ck.env.assignable(got, want) {
            let mut d = Diagnostic::error(
                codes::TYPE_MISMATCH,
                e.span,
                format!("mismatched types: expected `{want}`, found `{got}`"),
            )
            .with_label(format!("{what} must be `{want}`"));
            if let ExprKind::Int(n) = e.kind
                && self.ck.env.assignable(&FLOAT, want)
            {
                d = d
                    .with_help("`int` and `float` do not mix; write a float literal")
                    .with_fix(format!("write `{n}.0`"), e.span, format!("{n}.0"));
            } else if self.ck.env.expand(got) == Ty::Optional(Box::new(self.ck.env.expand(want))) {
                d = d.with_help(format!(
                    "`{got}` may be absent, so it cannot be used where `{want}` is expected"
                ));
            }
            self.ck.diags.push(d);
        }
    }

    /// The IR node for `e` and its type. Leaves in [`Cx::sens`] whether `e`'s value is
    /// sensitive: a sensitive parameter or variable, a sensitive field of any value, a field of
    /// a sensitive value, or an operator's result with a sensitive operand (`pin + 0`,
    /// `not flag`, `pin == 1`). A function's result is not: a `fn` is how a chart derives what
    /// may be shown.
    pub(crate) fn expr(&mut self, e: &Expr, scope: &Scope) -> (ir::Expr, Ty) {
        let out = self.expr_node(e, scope);
        if !matches!(
            e.kind,
            ExprKind::Var(_) | ExprKind::Field(..) | ExprKind::Unary(..) | ExprKind::Binary(..)
        ) {
            self.sens = false;
        }
        out
    }

    fn expr_node(&mut self, e: &Expr, scope: &Scope) -> (ir::Expr, Ty) {
        self.sens = false;
        match &e.kind {
            ExprKind::Int(n) => {
                if n.unsigned_abs() > MAX_SAFE_INTEGER as u64 {
                    self.out_of_range(e);
                }
                (node(K::Lit(Literal::Int(*n)), &INT, e), INT)
            }
            ExprKind::Float(f) => (node(K::Lit(Literal::Float(*f)), &FLOAT, e), FLOAT),
            ExprKind::Duration(d) => {
                let ms = d.millis();
                if ms > MAX_SAFE_INTEGER as u64 {
                    self.out_of_range(e);
                }
                let t = Ty::Builtin(Builtin::Duration);
                (node(K::Lit(Literal::Duration(ms)), &t, e), t)
            }
            ExprKind::String(s) => {
                let t = Ty::Builtin(Builtin::String);
                (node(K::Lit(Literal::String(s.clone())), &t, e), t)
            }
            ExprKind::Bool(b) => (node(K::Lit(Literal::Bool(*b)), &BOOL, e), BOOL),
            ExprKind::Var(n) => {
                let implicit = match n.name.as_str() {
                    "event" => Some((&scope.event, "an `on` transition")),
                    "result" => Some((&scope.result, "an invoke's `done` outcome")),
                    "error" => Some((&scope.error, "an invoke's `fail` outcome")),
                    "call" => Some((&scope.call, "an `on name` transition that takes a call")),
                    // A parameter or variable named `key` (already reported) is read as one
                    // where no child's key is in scope.
                    "key"
                        if !(matches!(scope.key, Implicit::NotHere)
                            && self.vars.contains_key("key")) =>
                    {
                        Some((
                            &scope.key,
                            "a transition taken for a spawned child: `on name.Event`, `on name.done` or `on name.fail` through a `children` collection",
                        ))
                    }
                    _ => None,
                };
                let ty = match implicit {
                    Some((Implicit::Has(t), _)) => t.clone(),
                    Some((Implicit::NotHere, place)) => {
                        self.ck.diags.push(Diagnostic::error(
                            codes::IMPLICIT_NAME_OUT_OF_SCOPE,
                            n.span,
                            format!("`{}` is only in scope in {place}", n.name),
                        ));
                        Ty::Unknown
                    }
                    Some((Implicit::Bad(code, msg), _)) => {
                        self.ck
                            .diags
                            .push(Diagnostic::error(*code, n.span, msg.clone()));
                        Ty::Unknown
                    }
                    Some((Implicit::CallArgs(c), _)) => {
                        let example = self
                            .calls
                            .get(c)
                            .and_then(|s| s.params.first())
                            .map_or("event.<name>".to_string(), |(p, _)| format!("event.{p}"));
                        self.ck.diags.push(
                            Diagnostic::error(
                                codes::UNTYPED_EVENT,
                                n.span,
                                format!("`event` is the arguments of call `{c}`, which are read only through their fields"),
                            )
                            .with_help(format!("read one argument: `{example}`")),
                        );
                        Ty::Unknown
                    }
                    None => match self.vars.get(&n.name) {
                        Some(v) => {
                            self.sens = v.sensitive;
                            self.ck.refer_member(self.file, self.symbol, n);
                            if !v.param {
                                self.reads.push((n.name.clone(), n.span));
                            }
                            v.ty.clone()
                        }
                        None => {
                            let d = self.unknown_variable(n, true);
                            self.ck.diags.push(d);
                            Ty::Unknown
                        }
                    },
                };
                let path = vec![n.name.clone()];
                (node(K::Ref { path }, &ty, e), ty)
            }
            ExprKind::Field(base, f)
                if matches!(&base.kind, ExprKind::Var(n) if n.name == "event")
                    && matches!(scope.event, Implicit::CallArgs(_)) =>
            {
                let Implicit::CallArgs(c) = &scope.event else {
                    unreachable!("matched above")
                };
                let ty = self.call_argument(c, f);
                self.sens = self
                    .ck
                    .sensitive
                    .is(&Sensitive::call(&self.machine, c), &f.name);
                let path = vec!["event".to_string(), f.name.clone()];
                (node(K::Ref { path }, &ty, e), ty)
            }
            ExprKind::Field(base, f) => {
                let (be, bt) = self.expr(base, scope);
                let sens = self.sens || self.field_is_sensitive(&bt, &f.name);
                let ty = self.field(&bt, f);
                self.sens = sens;
                let kind = match be.kind {
                    K::Ref { mut path } => {
                        path.push(f.name.clone());
                        K::Ref { path }
                    }
                    other => K::Field {
                        of: Box::new(ir::Expr {
                            kind: other,
                            ty: be.ty,
                            text: be.text,
                        }),
                        name: f.name.clone(),
                    },
                };
                (node(kind, &ty, e), ty)
            }
            ExprKind::Call(c) => {
                let sig = self.callee(&c.name, SigKind::Fn);
                let args = self.args(
                    &c.name,
                    c.span,
                    &c.args,
                    sig.as_ref(),
                    SigKind::Fn,
                    scope,
                    None,
                );
                let ty = sig.and_then(|s| s.returns).unwrap_or(Ty::Unknown);
                let kind = K::Call {
                    func: c.name.name.clone(),
                    args,
                };
                (node(kind, &ty, e), ty)
            }
            ExprKind::Enum(t, v) => {
                let ty = match self.type_value(t, "an enum") {
                    Some((name, TypeDef::Enum(variants), _)) => {
                        let owner = self.ck.decl_symbol(&name);
                        self.ck.refer_member(self.file, owner, v);
                        if !variants.contains(&v.name) {
                            let mut d = Diagnostic::error(
                                codes::UNKNOWN_VARIANT,
                                v.span,
                                format!("enum `{name}` has no variant `{}`", v.name),
                            )
                            .with_label(format!("its variants are {}", variants.join(", ")));
                            if let Some(s) = suggest(&v.name, variants.iter().map(String::as_str)) {
                                d = d.with_help(format!("did you mean `{s}`?")).with_fix(
                                    format!("replace with `{s}`"),
                                    v.span,
                                    s,
                                );
                            }
                            self.ck.diags.push(d);
                        }
                        Ty::Named(name)
                    }
                    Some((name, _, _)) => {
                        self.ck.diags.push(Diagnostic::error(
                            codes::WRONG_KIND,
                            t.span,
                            format!(
                                "`{name}` is not an enum, so `{}.{}` is not a value",
                                t.name, v.name
                            ),
                        ));
                        Ty::Unknown
                    }
                    None => Ty::Unknown,
                };
                let kind = K::Enum {
                    variant: v.name.clone(),
                };
                (node(kind, &ty, e), ty)
            }
            ExprKind::Record { .. } => self.record(e, scope, None),
            ExprKind::Unary(UnaryOp::Not, x) => {
                // `self.sens` stays the operand's: nothing below evaluates another expression.
                let (xe, xt) = self.expr(x, scope);
                if !self.ck.env.same(&xt, &BOOL) {
                    self.bad_operands(e, format!("`not` needs a `bool`, found `{xt}`"));
                }
                (
                    node(
                        K::Op {
                            op: Op::Not,
                            args: vec![xe],
                        },
                        &BOOL,
                        e,
                    ),
                    BOOL,
                )
            }
            ExprKind::Unary(UnaryOp::Neg, x) => {
                // `self.sens` stays the operand's: nothing below evaluates another expression.
                let (xe, xt) = self.expr(x, scope);
                let ty = match self.ck.env.expand(&xt) {
                    t @ (Ty::Unknown | Ty::Builtin(Builtin::Int | Builtin::Float)) => t,
                    _ => {
                        self.bad_operands(
                            e,
                            format!("unary `-` needs an `int` or a `float`, found `{xt}`"),
                        );
                        Ty::Unknown
                    }
                };
                (
                    node(
                        K::Op {
                            op: Op::Neg,
                            args: vec![xe],
                        },
                        &ty,
                        e,
                    ),
                    ty,
                )
            }
            ExprKind::Binary(o, a, b) => {
                let (ae, at) = self.expr(a, scope);
                let sens = self.sens;
                let (be, bt) = self.expr(b, scope);
                // Either operand's: nothing below evaluates another expression.
                self.sens |= sens;
                let (xa, xb) = (self.ck.env.expand(&at), self.ck.env.expand(&bt));
                let unknown = xa.is_unknown() || xb.is_unknown();
                let sym = o.as_str();
                let ty = match o {
                    BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul => match (&xa, &xb) {
                        _ if unknown => Ty::Unknown,
                        (Ty::Builtin(Builtin::Int), Ty::Builtin(Builtin::Int)) => INT,
                        (Ty::Builtin(Builtin::Float), Ty::Builtin(Builtin::Float)) => FLOAT,
                        _ => {
                            let msg = format!(
                                "`{sym}` needs two `int`s or two `float`s, found `{at}` and `{bt}`"
                            );
                            self.arith_mismatch(e, a, b, &xa, &xb, msg);
                            Ty::Unknown
                        }
                    },
                    BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => {
                        let ordered = matches!(
                            xa,
                            Ty::Builtin(Builtin::Int | Builtin::Float | Builtin::Duration)
                        ) && xa == xb;
                        if !unknown && !ordered {
                            let msg = format!(
                                "`{sym}` compares two `int`s, two `float`s or two `duration`s, found `{at}` and `{bt}`"
                            );
                            self.arith_mismatch(e, a, b, &xa, &xb, msg);
                        }
                        BOOL
                    }
                    BinaryOp::Eq | BinaryOp::Ne => {
                        if !unknown {
                            if xa != xb {
                                self.bad_operands(e, format!("`{sym}` compares two values of the same type, found `{at}` and `{bt}`"));
                            } else if !self.ck.env.is_equatable(&xa) {
                                self.bad_operands(
                                    e,
                                    format!(
                                        "`{sym}` is defined on scalars and enums, not on `{at}`"
                                    ),
                                );
                            }
                        }
                        BOOL
                    }
                    BinaryOp::And | BinaryOp::Or => {
                        if !unknown && (xa != BOOL || xb != BOOL) {
                            self.bad_operands(
                                e,
                                format!("`{sym}` needs two `bool`s, found `{at}` and `{bt}`"),
                            );
                        }
                        BOOL
                    }
                };
                let kind = K::Op {
                    op: op(*o),
                    args: vec![ae, be],
                };
                (node(kind, &ty, e), ty)
            }
        }
    }

    /// A record or error value, checked against `want` when it is known. A value of a generic
    /// record takes its type arguments from an alias it is built through, else from `want`,
    /// else from its field values.
    fn record(&mut self, e: &Expr, scope: &Scope, want: Option<&Ty>) -> (ir::Expr, Ty) {
        let ExprKind::Record { ty: t, fields } = &e.kind else {
            unreachable!("record() is called on a record value")
        };
        let def = self.type_value(t, "a record or error type");
        let (name, decl_fields, args) = match def {
            Some((name, TypeDef::Record(fs) | TypeDef::Error(fs), args)) => (name, fs, args),
            Some((name, other, _)) => {
                let why = match other {
                    TypeDef::Enum(vs) => format!(
                        "`{name}` is an enum; its values are written `{name}.{}`",
                        vs.first().map_or("Variant", String::as_str)
                    ),
                    TypeDef::Extern => {
                        format!("`{name}` is an extern type; a chart cannot build its values")
                    }
                    TypeDef::Alias(_) if !self.ck.env.params_of(&name).is_empty() => format!(
                        "`{name}` is a generic alias; build the value with the name of the record it stands for"
                    ),
                    _ => format!("`{name}` is not a record or error type"),
                };
                self.ck
                    .diags
                    .push(Diagnostic::error(codes::WRONG_KIND, t.span, why));
                let out = self.record_fields(&t.name, &Ty::Unknown, None, fields, e.span, scope);
                return (
                    node(K::Record { fields: out }, &Ty::Unknown, e),
                    Ty::Unknown,
                );
            }
            None => {
                let out = self.record_fields(&t.name, &Ty::Unknown, None, fields, e.span, scope);
                return (
                    node(K::Record { fields: out }, &Ty::Unknown, e),
                    Ty::Unknown,
                );
            }
        };
        let params = self.ck.env.params_of(&name).to_vec();
        if params.is_empty() {
            let ty = Ty::Named(name);
            let out = self.record_fields(&t.name, &ty, Some(&decl_fields), fields, e.span, scope);
            return (node(K::Record { fields: out }, &ty, e), ty);
        }
        let wanted = want.and_then(|w| match self.ck.env.expand(w) {
            Ty::Optional(inner) => match *inner {
                Ty::Generic(n, a) if n == name => Some(a),
                _ => None,
            },
            Ty::Generic(n, a) if n == name => Some(a),
            _ => None,
        });
        let (args, values) = match args.or(wanted) {
            Some(args) => (args, None),
            None => {
                // Infer from the field values: each is computed once, here.
                // A field whose type names no parameter is checked against it, as anywhere a
                // type is expected; the others are what the arguments are inferred from.
                let mut values: Vec<(ir::Expr, Ty, bool)> = Vec::new();
                for fv in fields {
                    let fixed = decl_fields
                        .iter()
                        .find(|(n, _)| *n == fv.name.name)
                        .map(|(_, ft)| ft)
                        .filter(|ft| {
                            let mut generic = false;
                            ft.walk(&mut |x| generic |= matches!(x, Ty::Param(_)));
                            !generic
                        })
                        .cloned();
                    let (value, got) = match fixed {
                        Some(ft) => {
                            let what = format!("field `{}` of `{}`", fv.name.name, t.name);
                            self.expect_typed(&fv.value, scope, &ft, &what)
                        }
                        None => self.expr(&fv.value, scope),
                    };
                    values.push((value, got, self.sens));
                }
                let mut candidates: HashMap<String, Vec<Ty>> = HashMap::new();
                for (fv, (_, got, _)) in fields.iter().zip(&values) {
                    if let Some((_, ft)) = decl_fields.iter().find(|(n, _)| *n == fv.name.name) {
                        // Both sides with their aliases expanded (a generic alias's arguments
                        // substituted, its parameters kept), so that `Dict<T>` meets
                        // `map<string, int>` as `map<string, T>`.
                        let (ft, got) = (self.ck.env.expand(ft), self.ck.env.expand(got));
                        unify(&ft, &got, &mut candidates);
                    }
                }
                let mut args = Vec::new();
                for p in &params {
                    let found = candidates.get(p).map(Vec::as_slice).unwrap_or_default();
                    let pick = found
                        .iter()
                        .find(|c| found.iter().all(|o| self.ck.env.assignable(o, c)));
                    match pick {
                        Some(c) if c.has_event() => {
                            self.ck.diags.push(
                                Diagnostic::error(
                                    codes::UNINFERRED_TYPE_ARGUMENT,
                                    e.span,
                                    format!("`{p}` of `{name}` would be `{c}`, and an event cannot be a type argument"),
                                )
                                .with_help(format!("give the value a declared type: `set` it to a variable of type `{name}<…>`, or pass it where one is expected")),
                            );
                            args.push(Ty::Unknown);
                        }
                        Some(c) => args.push(c.clone()),
                        None => {
                            let seen: Vec<String> =
                                found.iter().map(|c| format!("`{c}`")).collect();
                            let message = if seen.is_empty() {
                                format!(
                                    "cannot tell what `{p}` of `{name}` is from the fields of this value"
                                )
                            } else {
                                format!(
                                    "cannot tell what `{p}` of `{name}` is: the fields give {}",
                                    seen.join(" and ")
                                )
                            };
                            self.ck.diags.push(
                                Diagnostic::error(codes::UNINFERRED_TYPE_ARGUMENT, e.span, message)
                                    .with_help(format!("use the value where a `{name}<…>` is expected, such as a `set` of a variable of that type, so that its type arguments are known")),
                            );
                            args.push(Ty::Unknown);
                        }
                    }
                }
                let inferred = Ty::Generic(name.clone(), args.clone());
                self.ck.check_instance(e.span, &inferred);
                (args, Some(values))
            }
        };
        let ty = Ty::Generic(name, args);
        let substituted = self.ck.env.fields(&ty).unwrap_or_default();
        let out = match values {
            None => self.record_fields(&t.name, &ty, Some(&substituted), fields, e.span, scope),
            Some(values) => self.record_fields_of(
                &t.name,
                &ty,
                Some(&substituted),
                fields,
                e.span,
                scope,
                Some(values),
            ),
        };
        (node(K::Record { fields: out }, &ty, e), ty)
    }

    /// Checks the fields of a record, error or event value against the declared ones, if
    /// known (every field, once, and no others), and lowers them. `name` is the type as
    /// written; `span` is the whole value.
    pub(crate) fn record_fields(
        &mut self,
        name: &str,
        ty: &Ty,
        decl_fields: Option<&[(String, Ty)]>,
        fields: &[FieldValue],
        span: roadbook_syntax::Span,
        scope: &Scope,
    ) -> Vec<ir::FieldValue> {
        self.record_fields_of(name, ty, decl_fields, fields, span, scope, None)
    }

    /// [`Cx::record_fields`], with the field values already computed, in the order written,
    /// if `values` is given.
    #[allow(clippy::too_many_arguments)]
    fn record_fields_of(
        &mut self,
        name: &str,
        ty: &Ty,
        decl_fields: Option<&[(String, Ty)]>,
        fields: &[FieldValue],
        span: roadbook_syntax::Span,
        scope: &Scope,
        values: Option<Vec<(ir::Expr, Ty, bool)>>,
    ) -> Vec<ir::FieldValue> {
        let mut values = values.map(Vec::into_iter);
        let mut seen: Vec<&str> = Vec::new();
        let mut out = Vec::new();
        let owner = self.type_symbol(ty);
        for fv in fields {
            self.ck.refer_member(self.file, owner, &fv.name);
            let declared = decl_fields
                .and_then(|fs| fs.iter().find(|(n, _)| *n == fv.name.name))
                .map(|(_, t)| t.clone());
            if seen.contains(&fv.name.name.as_str()) {
                self.ck.diags.push(Diagnostic::error(
                    codes::DUPLICATE_NAME,
                    fv.name.span,
                    format!("field `{}` is given twice", fv.name.name),
                ));
            }
            seen.push(&fv.name.name);
            if let Some((value, got, sens)) = values.as_mut().and_then(Iterator::next) {
                if let Some(ft) = &declared {
                    let what = format!("field `{}` of `{name}`", fv.name.name);
                    self.check_assignable(&fv.value, &got, ft, &what);
                    let target = if self.field_is_sensitive(ty, &fv.name.name) {
                        Dest::Sensitive
                    } else {
                        Dest::Plain(format!("field `{}` of `{name}`", fv.name.name))
                    };
                    self.sens = sens;
                    self.check_dest(&fv.value, &value.text, &got, ft, &what, &target);
                } else if let Some(fs) = decl_fields {
                    let names: Vec<&str> = fs.iter().map(|(n, _)| n.as_str()).collect();
                    let d = self.no_such_field(ty, &fv.name, &names);
                    self.ck.diags.push(d);
                }
                out.push(ir::FieldValue {
                    name: fv.name.name.clone(),
                    value,
                });
                continue;
            }
            let value = match (&declared, decl_fields) {
                (Some(ft), _) => {
                    let what = format!("field `{}` of `{name}`", fv.name.name);
                    let target = if self.field_is_sensitive(ty, &fv.name.name) {
                        Dest::Sensitive
                    } else {
                        Dest::Plain(format!("field `{}` of `{name}`", fv.name.name))
                    };
                    self.expect_to(&fv.value, scope, ft, &what, target)
                }
                (None, Some(fs)) => {
                    let names: Vec<&str> = fs.iter().map(|(n, _)| n.as_str()).collect();
                    let d = self.no_such_field(ty, &fv.name, &names);
                    self.ck.diags.push(d);
                    self.expr(&fv.value, scope).0
                }
                (None, None) => self.expr(&fv.value, scope).0,
            };
            out.push(ir::FieldValue {
                name: fv.name.name.clone(),
                value,
            });
        }
        if let Some(fs) = decl_fields {
            let missing: Vec<&(String, Ty)> = fs
                .iter()
                .filter(|(n, _)| !seen.contains(&n.as_str()))
                .collect();
            if !missing.is_empty() {
                let list: Vec<String> = missing.iter().map(|(n, _)| format!("`{n}`")).collect();
                let example: Vec<String> =
                    missing.iter().map(|(n, t)| format!("{n}: <{t}>")).collect();
                self.ck.diags.push(
                    Diagnostic::error(
                        codes::MISSING_FIELD,
                        span,
                        format!(
                            "`{name}` value is missing field{} {}",
                            if missing.len() == 1 { "" } else { "s" },
                            list.join(", ")
                        ),
                    )
                    .with_help(format!(
                        "give every field: `{name} {{ {} }}`",
                        example.join(", ")
                    )),
                );
            }
        }
        out
    }

    fn out_of_range(&mut self, e: &Expr) {
        self.ck.diags.push(
            Diagnostic::error(
                codes::LITERAL_OUT_OF_RANGE,
                e.span,
                format!(
                    "`{}` is outside the range the IR carries exactly, ±(2^53 − 1)",
                    expr_text(e)
                ),
            )
            .with_help("the IR is JSON, and JavaScript reads integers exactly only up to 2^53 − 1"),
        );
    }

    fn bad_operands(&mut self, e: &Expr, msg: String) {
        self.ck
            .diags
            .push(Diagnostic::error(codes::BAD_OPERANDS, e.span, msg));
    }

    /// An arithmetic or ordering operator on mismatched operands. When one side is an `int`
    /// literal and the other a `float`, the fix writes the literal as a float.
    fn arith_mismatch(&mut self, e: &Expr, a: &Expr, b: &Expr, xa: &Ty, xb: &Ty, msg: String) {
        let mut d = Diagnostic::error(codes::BAD_OPERANDS, e.span, msg);
        let lit = match (&a.kind, &b.kind) {
            (ExprKind::Int(n), _) if *xb == FLOAT => Some((a, *n)),
            (_, ExprKind::Int(n)) if *xa == FLOAT => Some((b, *n)),
            _ => None,
        };
        if let Some((l, n)) = lit {
            d = d
                .with_help("`int` and `float` do not mix; write a float literal")
                .with_fix(format!("write `{n}.0`"), l.span, format!("{n}.0"));
        } else if (*xa == INT && *xb == FLOAT) || (*xa == FLOAT && *xb == INT) {
            d = d.with_help("`int` and `float` do not mix; convert one side with a `fn`");
        }
        self.ck.diags.push(d);
    }

    /// The type of field `f` of a value of type `t`.
    fn field(&mut self, t: &Ty, f: &roadbook_syntax::ast::Ident) -> Ty {
        let expanded = self.ck.env.expand(t);
        let fields: Vec<(String, Ty)> = match &expanded {
            Ty::Unknown => return Ty::Unknown,
            Ty::Event(name) => match name.split_once('.') {
                Some((machine, event)) => self
                    .ck
                    .interfaces
                    .get(machine)
                    .and_then(|i| i.events.get(event).or_else(|| i.emits.get(event)))
                    .cloned()
                    .unwrap_or_default(),
                None => self
                    .events
                    .get(name)
                    .or_else(|| self.emits.get(name))
                    .cloned()
                    .unwrap_or_default(),
            },
            Ty::Named(_) | Ty::Generic(..) if self.ck.env.fields(&expanded).is_some() => {
                self.ck.env.fields(&expanded).unwrap_or_default()
            }
            Ty::Optional(_) => {
                self.ck.diags.push(
                    Diagnostic::error(
                        codes::NO_FIELDS,
                        f.span,
                        format!(
                            "cannot read field `{}` of an optional value: `{t}` may be absent",
                            f.name
                        ),
                    )
                    .with_help("pass the value to a `fn` that handles the absent case"),
                );
                return Ty::Unknown;
            }
            other => {
                self.ck.diags.push(Diagnostic::error(
                    codes::NO_FIELDS,
                    f.span,
                    format!("`{other}` has no fields, so it has no field `{}`", f.name),
                ));
                return Ty::Unknown;
            }
        };
        if fields.is_empty() {
            self.ck.diags.push(
                Diagnostic::error(
                    codes::NO_FIELDS,
                    f.span,
                    format!("`{t}` has no fields, so it has no field `{}`", f.name),
                )
                .with_help(format!(
                    "declare the field on `{}`, or do not read it",
                    t.to_string().trim_start_matches("event ")
                )),
            );
            return Ty::Unknown;
        }
        match fields.iter().find(|(n, _)| *n == f.name) {
            Some((_, ft)) => {
                let owner = self.type_symbol(&expanded);
                self.ck.refer_member(self.file, owner, f);
                ft.clone()
            }
            None => {
                let names: Vec<&str> = fields.iter().map(|(n, _)| n.as_str()).collect();
                let d = self.no_such_field(t, f, &names);
                self.ck.diags.push(d);
                Ty::Unknown
            }
        }
    }

    /// The type of argument `f` of the machine's call `c`, read as `event.f` in `on c`.
    fn call_argument(&mut self, c: &str, f: &roadbook_syntax::ast::Ident) -> Ty {
        let Some(sig) = self.calls.get(c).cloned() else {
            return Ty::Unknown;
        };
        if let Some((_, t)) = sig.params.iter().find(|(n, _)| *n == f.name) {
            let call = self.ck.call_symbol(self.symbol, c);
            self.ck.refer_member(self.file, call, f);
            return t.clone();
        }
        let names: Vec<&str> = sig.params.iter().map(|(n, _)| n.as_str()).collect();
        let mut d = if names.is_empty() {
            Diagnostic::error(
                codes::NO_FIELDS,
                f.span,
                format!(
                    "call `{c}` takes no arguments, so `event` has no field `{}`",
                    f.name
                ),
            )
        } else {
            Diagnostic::error(
                codes::UNKNOWN_FIELD,
                f.span,
                format!("call `{c}` has no argument `{}`", f.name),
            )
        };
        d = d.with_secondary(sig.name_span, "declared here");
        if let Some(s) = suggest(&f.name, names.iter().copied()) {
            d = d.with_help(format!("did you mean `{s}`?")).with_fix(
                format!("replace with `{s}`"),
                f.span,
                s,
            );
        } else if !names.is_empty() {
            d = d.with_help(format!("its arguments are {}", names.join(", ")));
        }
        self.ck.diags.push(d);
        Ty::Unknown
    }

    fn no_such_field(&self, t: &Ty, f: &roadbook_syntax::ast::Ident, names: &[&str]) -> Diagnostic {
        let mut d = Diagnostic::error(
            codes::UNKNOWN_FIELD,
            f.span,
            format!("`{t}` has no field `{}`", f.name),
        );
        if let Some(s) = suggest(&f.name, names.iter().copied()) {
            d = d.with_help(format!("did you mean `{s}`?")).with_fix(
                format!("replace with `{s}`"),
                f.span,
                s,
            );
        } else if names.is_empty() {
            d = d.with_help(format!("`{t}` has no fields"));
        } else {
            d = d.with_help(format!("its fields are {}", names.join(", ")));
        }
        d
    }

    /// The symbol that declares the fields of values of type `t`: a record or error type, or
    /// an event.
    fn type_symbol(&self, t: &Ty) -> Option<SymbolId> {
        match self.ck.env.expand(t) {
            Ty::Named(n) | Ty::Generic(n, _) => self.ck.decl_symbol(&n),
            Ty::Event(name) => match name.split_once('.') {
                Some((machine, event)) => {
                    let m = self.ck.decl_symbol(machine)?;
                    self.ck.member_symbol(m, event)
                }
                None => self.ck.member_symbol(self.symbol?, &name),
            },
            _ => None,
        }
    }

    /// The declared type an enum or record value names, resolved through aliases: its name
    /// and definition. Reports a missing name or one that is not a type.
    fn type_value(
        &mut self,
        t: &roadbook_syntax::ast::Ident,
        wanted: &str,
    ) -> Option<(String, TypeDef, Option<Vec<Ty>>)> {
        self.ck.refer_decl(self.file, t);
        match self.ck.names.lookup(self.file, &t.name) {
            Found::Decl(id) if is_type(&self.ck.names.decl(id).kind) => {
                match self.ck.env.expand(&Ty::Named(t.name.clone())) {
                    Ty::Named(n) => self.ck.env.defs.get(&n).cloned().map(|d| (n, d, None)),
                    Ty::Generic(n, args) => self
                        .ck
                        .env
                        .defs
                        .get(&n)
                        .cloned()
                        .map(|d| (n, d, Some(args))),
                    Ty::Unknown => None,
                    other => {
                        self.ck.diags.push(Diagnostic::error(
                            codes::WRONG_KIND,
                            t.span,
                            format!("`{}` is an alias of `{other}`, not {wanted}", t.name),
                        ));
                        None
                    }
                }
            }
            Found::Decl(id) => {
                let d = self.ck.names.wrong_kind(t, id, wanted);
                self.ck.diags.push(d);
                None
            }
            Found::Poisoned => None,
            Found::Missing if t.name == crate::check::UNANSWERED => {
                self.ck.diags.push(crate::check::unanswered_misused(
                    t.span,
                    "`Unanswered` is not a value a chart can make: a call fails with it only when it is not answered",
                ));
                None
            }
            Found::Missing => {
                let accept = |k: &DeclKind| is_type(k);
                let d = self
                    .ck
                    .names
                    .unknown(self.file, t, codes::UNKNOWN_TYPE, "type", accept);
                self.ck.diags.push(d);
                None
            }
        }
    }
}

/// Collects, for each type parameter `pattern` mentions, the types `got` has in its place.
/// `got` has its aliases expanded. Parts that do not match are left to the type check that
/// follows.
fn unify(pattern: &Ty, got: &Ty, out: &mut HashMap<String, Vec<Ty>>) {
    match (pattern, got) {
        (Ty::Param(p), g) => out.entry(p.clone()).or_default().push(g.clone()),
        (Ty::List(f), Ty::List(g)) => unify(f, g, out),
        (Ty::Optional(f), Ty::Optional(g)) => unify(f, g, out),
        (Ty::Optional(f), g) => unify(f, g, out),
        (Ty::Map(_, f), Ty::Map(_, g)) => unify(f, g, out),
        (Ty::Generic(a, fs), Ty::Generic(b, gs)) if a == b => {
            for (f, g) in fs.iter().zip(gs) {
                unify(f, g, out);
            }
        }
        _ => {}
    }
}
