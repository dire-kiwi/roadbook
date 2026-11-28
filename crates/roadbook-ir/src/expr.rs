//! Expressions.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::types::{Builtin, TypeRef};

/// Largest integer magnitude an `int` or `duration` literal may have in the
/// IR, `2^53 - 1`. Larger integers are not exact in canonical JSON (RFC 8785
/// treats every number as a double) or in JavaScript, so the compiler reports
/// them instead of emitting them.
pub const MAX_SAFE_INTEGER: i64 = (1 << 53) - 1;

/// An expression node.
///
/// In JSON: `{"kind": …, <kind fields>, "type": TypeRef, "text": string}`.
/// Every node carries its type (filled in by the type checker) and its
/// canonical source text, so tools can show it without a pretty-printer.
#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    /// What the node is.
    pub kind: ExprKind,
    /// Its type. For `record` and `enum` nodes this is also the name of the
    /// type being constructed, so it is always [`TypeRef::Named`].
    pub ty: TypeRef,
    /// Canonical source form, as the formatter would write it.
    pub text: String,
}

/// The kinds of expression node.
#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    /// A literal: `{"kind": "lit", "value": …}`.
    Lit(Literal),
    /// A variable, parameter or implicit name (`event`, `result`, `error`),
    /// followed by field names: `{"kind": "ref", "path": ["error", "issues"]}`.
    Ref {
        /// The name, then each field accessed on it. Never empty.
        path: Vec<String>,
        /// The id of the state whose parameter or variable `path[0]` names, or
        /// `None` for a machine parameter or variable, or an implicit name.
        state: Option<String>,
    },
    /// A field of a base that is not a variable, such as a call's result:
    /// `{"kind": "field", "of": Expr, "name": …}`.
    Field {
        /// The expression whose field is read.
        of: Box<Expr>,
        /// The field.
        name: String,
    },
    /// A call of a `fn`: `{"kind": "call", "fn": …, "args": [Expr]}`.
    Call {
        /// The function's name (the JSON key is `fn`).
        func: String,
        /// Arguments, in parameter order.
        args: Vec<Expr>,
    },
    /// An operator: `{"kind": "op", "op": …, "args": [Expr]}`. Binary
    /// operators have two arguments; `not` and `neg` have one.
    Op {
        /// The operator.
        op: Op,
        /// Its operands, left to right.
        args: Vec<Expr>,
    },
    /// A record or error value: `{"kind": "record", "fields": [{name, value}]}`.
    /// The constructed type is [`Expr::ty`]. A field-less error written bare
    /// (`GatewayUnavailable`) is a record with no fields.
    Record {
        /// Field values, in the order written.
        fields: Vec<FieldValue>,
    },
    /// An enum value: `{"kind": "enum", "variant": …}`. The enum is
    /// [`Expr::ty`].
    Enum {
        /// The variant's name.
        variant: String,
    },
    /// Whether a state is active: `{"kind": "in", "state": …}`. Its type is
    /// always `bool`.
    In {
        /// The state's id, such as `Player.Volume.Muted`.
        state: String,
    },
}

/// One field of a record value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldValue {
    /// The field's name.
    pub name: String,
    /// Its value.
    pub value: Expr,
}

/// A literal value. Which variant it is follows from the node's `type`.
#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    /// `int`: a JSON integer within ±[`MAX_SAFE_INTEGER`].
    Int(i64),
    /// `float`: a finite JSON number.
    Float(f64),
    /// `string`: a JSON string.
    String(String),
    /// `bool`: `true` or `false`.
    Bool(bool),
    /// `duration`: a JSON integer, milliseconds, at most
    /// [`MAX_SAFE_INTEGER`]. `30s` is `30000`.
    Duration(u64),
}

impl Literal {
    /// The built-in type of this literal.
    pub fn builtin(&self) -> Builtin {
        match self {
            Literal::Int(_) => Builtin::Int,
            Literal::Float(_) => Builtin::Float,
            Literal::String(_) => Builtin::String,
            Literal::Bool(_) => Builtin::Bool,
            Literal::Duration(_) => Builtin::Duration,
        }
    }

    fn to_value(&self) -> Value {
        match self {
            Literal::Int(i) => Value::from(*i),
            // A non-finite float cannot be written in source; serde_json
            // would write it as null, which the reader rejects.
            Literal::Float(f) => Value::from(*f),
            Literal::String(s) => Value::from(s.as_str()),
            Literal::Bool(b) => Value::from(*b),
            Literal::Duration(ms) => Value::from(*ms),
        }
    }

    fn from_value(ty: &TypeRef, value: Value) -> Result<Literal, String> {
        let bad = |what: &str| format!("a `{ty}` literal needs {what}, found {value}");
        let safe = |i: i64| (-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(&i);
        Ok(match ty {
            TypeRef::Builtin(Builtin::Int) => match value.as_i64() {
                Some(i) if value.is_i64() || value.is_u64() => {
                    if !safe(i) {
                        return Err(bad("an integer within ±(2^53 - 1)"));
                    }
                    Literal::Int(i)
                }
                _ => return Err(bad("a JSON integer within ±(2^53 - 1)")),
            },
            TypeRef::Builtin(Builtin::Float) => match value.as_f64() {
                Some(f) => Literal::Float(f),
                None => return Err(bad("a JSON number")),
            },
            TypeRef::Builtin(Builtin::Duration) => match value.as_u64() {
                Some(ms) if ms <= MAX_SAFE_INTEGER as u64 => Literal::Duration(ms),
                _ => return Err(bad("an integer number of milliseconds within 0..=2^53 - 1")),
            },
            TypeRef::Builtin(Builtin::String) => match value {
                Value::String(s) => Literal::String(s),
                _ => return Err(bad("a JSON string")),
            },
            TypeRef::Builtin(Builtin::Bool) => match value {
                Value::Bool(b) => Literal::Bool(b),
                _ => return Err(bad("true or false")),
            },
            _ => return Err(format!("a literal cannot have type `{ty}`")),
        })
    }
}

/// Operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Op {
    /// `==`
    #[serde(rename = "==")]
    Eq,
    /// `!=`
    #[serde(rename = "!=")]
    Ne,
    /// `<`
    #[serde(rename = "<")]
    Lt,
    /// `<=`
    #[serde(rename = "<=")]
    Le,
    /// `>`
    #[serde(rename = ">")]
    Gt,
    /// `>=`
    #[serde(rename = ">=")]
    Ge,
    /// `+`
    #[serde(rename = "+")]
    Add,
    /// binary `-`
    #[serde(rename = "-")]
    Sub,
    /// `*`
    #[serde(rename = "*")]
    Mul,
    /// `and`
    #[serde(rename = "and")]
    And,
    /// `or`
    #[serde(rename = "or")]
    Or,
    /// `not`, unary
    #[serde(rename = "not")]
    Not,
    /// unary `-`
    #[serde(rename = "neg")]
    Neg,
}

impl Op {
    /// The number of operands the operator takes.
    pub fn arity(self) -> usize {
        match self {
            Op::Not | Op::Neg => 1,
            _ => 2,
        }
    }
}

// The JSON shape keeps `type` and `text` at the end of every node, after the
// kind's own fields. These mirror enums give that shape to serde while the
// public type keeps `ty` and `text` in one place.

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum ExprSer<'a> {
    Lit {
        value: Value,
        #[serde(rename = "type")]
        ty: &'a TypeRef,
        text: &'a str,
    },
    Ref {
        path: &'a [String],
        state: &'a Option<String>,
        #[serde(rename = "type")]
        ty: &'a TypeRef,
        text: &'a str,
    },
    Field {
        of: &'a Expr,
        name: &'a str,
        #[serde(rename = "type")]
        ty: &'a TypeRef,
        text: &'a str,
    },
    Call {
        #[serde(rename = "fn")]
        func: &'a str,
        args: &'a [Expr],
        #[serde(rename = "type")]
        ty: &'a TypeRef,
        text: &'a str,
    },
    Op {
        op: Op,
        args: &'a [Expr],
        #[serde(rename = "type")]
        ty: &'a TypeRef,
        text: &'a str,
    },
    Record {
        fields: &'a [FieldValue],
        #[serde(rename = "type")]
        ty: &'a TypeRef,
        text: &'a str,
    },
    Enum {
        variant: &'a str,
        #[serde(rename = "type")]
        ty: &'a TypeRef,
        text: &'a str,
    },
    In {
        state: &'a str,
        #[serde(rename = "type")]
        ty: &'a TypeRef,
        text: &'a str,
    },
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
enum ExprDe {
    Lit {
        value: Value,
        #[serde(rename = "type")]
        ty: TypeRef,
        text: String,
    },
    Ref {
        path: Vec<String>,
        state: Option<String>,
        #[serde(rename = "type")]
        ty: TypeRef,
        text: String,
    },
    Field {
        of: Box<Expr>,
        name: String,
        #[serde(rename = "type")]
        ty: TypeRef,
        text: String,
    },
    Call {
        #[serde(rename = "fn")]
        func: String,
        args: Vec<Expr>,
        #[serde(rename = "type")]
        ty: TypeRef,
        text: String,
    },
    Op {
        op: Op,
        args: Vec<Expr>,
        #[serde(rename = "type")]
        ty: TypeRef,
        text: String,
    },
    Record {
        fields: Vec<FieldValue>,
        #[serde(rename = "type")]
        ty: TypeRef,
        text: String,
    },
    Enum {
        variant: String,
        #[serde(rename = "type")]
        ty: TypeRef,
        text: String,
    },
    In {
        state: String,
        #[serde(rename = "type")]
        ty: TypeRef,
        text: String,
    },
}

impl Serialize for Expr {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let (ty, text) = (&self.ty, self.text.as_str());
        match &self.kind {
            ExprKind::Lit(l) => ExprSer::Lit {
                value: l.to_value(),
                ty,
                text,
            },
            ExprKind::Ref { path, state } => ExprSer::Ref {
                path,
                state,
                ty,
                text,
            },
            ExprKind::Field { of, name } => ExprSer::Field { of, name, ty, text },
            ExprKind::Call { func, args } => ExprSer::Call {
                func,
                args,
                ty,
                text,
            },
            ExprKind::Op { op, args } => ExprSer::Op {
                op: *op,
                args,
                ty,
                text,
            },
            ExprKind::Record { fields } => ExprSer::Record { fields, ty, text },
            ExprKind::Enum { variant } => ExprSer::Enum { variant, ty, text },
            ExprKind::In { state } => ExprSer::In { state, ty, text },
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Expr {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Expr, D::Error> {
        let named = |ty: &TypeRef, kind: &str| match ty {
            TypeRef::Named(_) => Ok(()),
            _ => Err(D::Error::custom(format!(
                "a `{kind}` expression's type must be a declared name, found `{ty}`"
            ))),
        };
        Ok(match ExprDe::deserialize(deserializer)? {
            ExprDe::Lit { value, ty, text } => Expr {
                kind: ExprKind::Lit(Literal::from_value(&ty, value).map_err(D::Error::custom)?),
                ty,
                text,
            },
            ExprDe::Ref {
                path,
                state,
                ty,
                text,
            } => {
                if path.is_empty() {
                    return Err(D::Error::custom("a `ref` expression's path is empty"));
                }
                Expr {
                    kind: ExprKind::Ref { path, state },
                    ty,
                    text,
                }
            }
            ExprDe::Field { of, name, ty, text } => Expr {
                kind: ExprKind::Field { of, name },
                ty,
                text,
            },
            ExprDe::Call {
                func,
                args,
                ty,
                text,
            } => Expr {
                kind: ExprKind::Call { func, args },
                ty,
                text,
            },
            ExprDe::Op { op, args, ty, text } => {
                if args.len() != op.arity() {
                    return Err(D::Error::custom(format!(
                        "operator `{}` takes {} operand(s), found {}",
                        serde_json::to_value(op).map_err(D::Error::custom)?,
                        op.arity(),
                        args.len()
                    )));
                }
                Expr {
                    kind: ExprKind::Op { op, args },
                    ty,
                    text,
                }
            }
            ExprDe::Record { fields, ty, text } => {
                // A value of a generic record has the instance as its type.
                if !matches!(ty, TypeRef::Generic(..)) {
                    named(&ty, "record")?;
                }
                Expr {
                    kind: ExprKind::Record { fields },
                    ty,
                    text,
                }
            }
            ExprDe::Enum { variant, ty, text } => {
                named(&ty, "enum")?;
                Expr {
                    kind: ExprKind::Enum { variant },
                    ty,
                    text,
                }
            }
            ExprDe::In { state, ty, text } => {
                if ty != TypeRef::Builtin(Builtin::Bool) {
                    return Err(D::Error::custom(format!(
                        "an `in` expression's type must be `bool`, found `{ty}`"
                    )));
                }
                Expr {
                    kind: ExprKind::In { state },
                    ty,
                    text,
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(json: &str) -> Expr {
        let e: Expr = serde_json::from_str(json).unwrap();
        assert_eq!(serde_json::to_string(&e).unwrap(), json);
        e
    }

    #[test]
    fn shapes() {
        round_trip(r#"{"kind":"lit","value":3,"type":"int","text":"3"}"#);
        round_trip(r#"{"kind":"lit","value":1.5,"type":"float","text":"1.5"}"#);
        round_trip(r#"{"kind":"lit","value":30000,"type":"duration","text":"30s"}"#);
        round_trip(r#"{"kind":"enum","variant":"Admin","type":"Role","text":"Role.Admin"}"#);
        round_trip(
            r#"{"kind":"in","state":"Player.Volume.Muted","type":"bool","text":"in(Volume.Muted)"}"#,
        );
        assert!(
            serde_json::from_str::<Expr>(
                r#"{"kind":"in","state":"M.A","type":"int","text":"in(A)"}"#
            )
            .is_err()
        );
        let e = round_trip(
            r#"{"kind":"op","op":"<","args":[{"kind":"ref","path":["attempts"],"state":null,"type":"int","text":"attempts"},{"kind":"lit","value":3,"type":"int","text":"3"}],"type":"bool","text":"attempts < 3"}"#,
        );
        assert!(matches!(e.kind, ExprKind::Op { op: Op::Lt, .. }));
        let e = round_trip(
            r#"{"kind":"ref","path":["tries"],"state":"M.Confirming","type":"int","text":"tries"}"#,
        );
        assert!(matches!(e.kind, ExprKind::Ref { state: Some(s), .. } if s == "M.Confirming"));
    }

    #[test]
    fn rejects() {
        for bad in [
            r#"{"kind":"lit","value":1.5,"type":"int","text":"1"}"#,
            r#"{"kind":"lit","value":9007199254740992,"type":"int","text":"9007199254740992"}"#,
            r#"{"kind":"lit","value":"x","type":"json","text":"x"}"#,
            r#"{"kind":"ref","path":[],"type":"int","text":""}"#,
            r#"{"kind":"ref","path":["a"],"type":"int","text":"a","span":null}"#,
            r#"{"kind":"op","op":"not","args":[],"type":"bool","text":"not"}"#,
            r#"{"kind":"enum","variant":"A","type":"int","text":"A"}"#,
            r#"{"kind":"nope","type":"int","text":""}"#,
        ] {
            assert!(serde_json::from_str::<Expr>(bad).is_err(), "{bad}");
        }
    }
}
