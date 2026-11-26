//! Evaluating IR expressions.

use roadbook_ir::{Builtin, Expr, ExprKind, Literal, Op, TypeDef, TypeRef};
use serde_json::{Map, Number, Value};

use crate::Defect;
use crate::program::Program;

/// Host functions as the core sees them: name and positional arguments, giving the value or
/// a host defect.
pub type Fns<'a> = dyn FnMut(&str, Vec<Value>) -> Result<Value, Defect> + 'a;

/// What an expression can read besides the parameters: the variables, and the implicit name
/// bound by the transition being tried (`event`, `result` or `error`).
pub(crate) struct Scope<'a> {
    pub program: &'a Program,
    pub machine: &'a roadbook_ir::Machine,
    pub input: &'a Map<String, Value>,
    pub vars: &'a Map<String, Value>,
    pub implicit: Option<(&'static str, &'a Value)>,
}

pub(crate) fn eval(e: &Expr, s: &Scope<'_>, fns: &mut Fns<'_>) -> Result<Value, Defect> {
    Ok(match &e.kind {
        ExprKind::Lit(lit) => match lit {
            Literal::Int(i) => Value::from(*i),
            Literal::Float(f) => float(*f, e)?,
            Literal::String(v) => Value::from(v.as_str()),
            Literal::Bool(b) => Value::from(*b),
            Literal::Duration(ms) => Value::from(*ms),
        },
        ExprKind::Ref { path } => {
            let (first, rest) = path.split_first().expect("a ref path is never empty");
            let mut v = lookup(first, s)?;
            for f in rest {
                v = field(v, f);
            }
            v
        }
        ExprKind::Field { of, name } => field(eval(of, s, fns)?, name),
        ExprKind::Call { func, args } => {
            let mut vals = Vec::with_capacity(args.len());
            for a in args {
                vals.push(eval(a, s, fns)?);
            }
            fns(func, vals)?
        }
        ExprKind::Record { fields } => {
            let mut obj = Map::new();
            let name = match s.program.resolve(&e.ty) {
                TypeRef::Named(n) => n.as_str(),
                _ => "",
            };
            let decl = s.program.ir().types.get(name).map(|t| &t.def);
            if let Some(TypeDef::Error { .. }) = decl {
                obj.insert("_tag".into(), Value::from(name));
            }
            for f in fields {
                let v = eval(&f.value, s, fns)?;
                obj.insert(f.name.clone(), v);
            }
            // An optional field with no value is `null`, never omitted.
            if let Some(TypeDef::Record { fields: decl } | TypeDef::Error { fields: decl }) = decl {
                for f in decl {
                    obj.entry(f.name.clone()).or_insert(Value::Null);
                }
            }
            Value::Object(obj)
        }
        ExprKind::Enum { variant } => Value::from(variant.as_str()),
        ExprKind::Op { op, args } => op_value(*op, args, e, s, fns)?,
    })
}

fn lookup(name: &str, s: &Scope<'_>) -> Result<Value, Defect> {
    if let Some((n, v)) = s.implicit {
        if n == name {
            return Ok(v.clone());
        }
    }
    if s.machine.vars.iter().any(|v| v.name == name) {
        return s.vars.get(name).cloned().ok_or_else(|| Defect::Unassigned {
            var: name.to_owned(),
        });
    }
    Ok(s.input.get(name).cloned().unwrap_or(Value::Null))
}

fn field(v: Value, name: &str) -> Value {
    match v {
        Value::Object(mut m) => m.remove(name).unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

fn float(f: f64, e: &Expr) -> Result<Value, Defect> {
    Number::from_f64(f)
        .map(Value::Number)
        .ok_or_else(|| Defect::Overflow {
            expr: e.text.clone(),
        })
}

/// Whether a value of type `ty` is a `b`, aliases expanded.
fn is(s: &Scope<'_>, ty: &TypeRef, b: Builtin) -> bool {
    *s.program.scalar(ty) == TypeRef::Builtin(b)
}

fn as_bool(v: &Value) -> bool {
    v.as_bool().unwrap_or(false)
}

/// An `int` value. The binding layer decodes host values against their types, so an `int`
/// is an integer; an integral number written as a float (`2.0`) is read as one all the same,
/// since a JSON number's form does not matter.
fn int(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| {
        let f = v.as_f64()?;
        (f.fract() == 0.0 && (i64::MIN as f64..-(i64::MIN as f64)).contains(&f)).then_some(f as i64)
    })
}

fn as_i64(v: &Value) -> i64 {
    int(v).unwrap_or(0)
}

/// A `duration` value, in milliseconds, read like [`int`].
fn millis(v: &Value) -> Option<u64> {
    int(v)
        .and_then(|i| u64::try_from(i).ok())
        .or_else(|| v.as_u64())
}

fn as_f64(v: &Value) -> f64 {
    v.as_f64().unwrap_or(0.0)
}

fn op_value(
    op: Op,
    args: &[Expr],
    e: &Expr,
    s: &Scope<'_>,
    fns: &mut Fns<'_>,
) -> Result<Value, Defect> {
    let overflow = || Defect::Overflow {
        expr: e.text.clone(),
    };
    match op {
        Op::And | Op::Or => {
            let left = as_bool(&eval(&args[0], s, fns)?);
            if left == (op == Op::Or) {
                return Ok(Value::from(left));
            }
            Ok(Value::from(as_bool(&eval(&args[1], s, fns)?)))
        }
        Op::Not => Ok(Value::from(!as_bool(&eval(&args[0], s, fns)?))),
        Op::Neg => {
            let v = eval(&args[0], s, fns)?;
            if is(s, &e.ty, Builtin::Float) {
                float(-as_f64(&v), e)
            } else {
                as_i64(&v)
                    .checked_neg()
                    .map(Value::from)
                    .ok_or_else(overflow)
            }
        }
        _ => {
            let a = eval(&args[0], s, fns)?;
            let b = eval(&args[1], s, fns)?;
            let ty = s.program.scalar(&args[0].ty);
            match op {
                Op::Eq => Ok(Value::from(equal(ty, &a, &b))),
                Op::Ne => Ok(Value::from(!equal(ty, &a, &b))),
                Op::Lt | Op::Le | Op::Gt | Op::Ge => {
                    let ord = compare(ty, &a, &b);
                    let r = match op {
                        Op::Lt => ord == Some(std::cmp::Ordering::Less),
                        Op::Le => matches!(
                            ord,
                            Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
                        ),
                        Op::Gt => ord == Some(std::cmp::Ordering::Greater),
                        _ => matches!(
                            ord,
                            Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)
                        ),
                    };
                    Ok(Value::from(r))
                }
                Op::Add | Op::Sub | Op::Mul => {
                    if is(s, &e.ty, Builtin::Float) {
                        let (x, y) = (as_f64(&a), as_f64(&b));
                        let r = match op {
                            Op::Add => x + y,
                            Op::Sub => x - y,
                            _ => x * y,
                        };
                        float(r, e)
                    } else {
                        let (x, y) = (as_i64(&a), as_i64(&b));
                        let r = match op {
                            Op::Add => x.checked_add(y),
                            Op::Sub => x.checked_sub(y),
                            _ => x.checked_mul(y),
                        };
                        r.map(Value::from).ok_or_else(overflow)
                    }
                }
                _ => unreachable!("every operator is handled above"),
            }
        }
    }
}

/// `==` on the operands' type, aliases expanded: numbers compare numerically (floats as
/// IEEE 754 doubles, so `0.0 == -0.0`), everything else as JSON.
fn equal(ty: &TypeRef, a: &Value, b: &Value) -> bool {
    let same = match ty {
        TypeRef::Builtin(Builtin::Float) => a.as_f64().zip(b.as_f64()).map(|(x, y)| x == y),
        TypeRef::Builtin(Builtin::Int) => int(a).zip(int(b)).map(|(x, y)| x == y),
        TypeRef::Builtin(Builtin::Duration) => millis(a).zip(millis(b)).map(|(x, y)| x == y),
        _ => None,
    };
    same.unwrap_or_else(|| match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        _ => a == b,
    })
}

/// The order of the operands, on their type with aliases expanded.
fn compare(ty: &TypeRef, a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    match ty {
        TypeRef::Builtin(Builtin::Float) => as_f64(a).partial_cmp(&as_f64(b)),
        TypeRef::Builtin(Builtin::Int) => Some(as_i64(a).cmp(&as_i64(b))),
        TypeRef::Builtin(Builtin::Duration) => millis(a).partial_cmp(&millis(b)),
        TypeRef::Builtin(Builtin::String) => a.as_str().partial_cmp(&b.as_str()),
        _ => None,
    }
}
