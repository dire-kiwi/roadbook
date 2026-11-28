//! Evaluating IR expressions.

use roadbook_ir::{Builtin, Expr, ExprKind, Literal, Op, TypeDef, TypeRef};
use serde_json::{Map, Number, Value};

use crate::Defect;
use crate::program::Program;

/// Host functions as the core sees them: name and positional arguments, giving the value or
/// a host defect.
pub type Fns<'a> = dyn FnMut(&str, Vec<Value>) -> Result<Value, Defect> + 'a;

/// What an expression can read besides the parameters: the variables, the implicit names
/// bound by the transition being tried (`event`, `result` or `error`, and `call` with `event`
/// in an `on c` transition), and `key`, the key of the spawned child the input came from.
pub(crate) struct Scope<'a> {
    pub program: &'a Program,
    pub machine: &'a roadbook_ir::Machine,
    pub input: &'a Map<String, Value>,
    pub vars: &'a Map<String, Value>,
    /// The locals of the active states (and of those being exited), keyed by state id.
    pub locals: &'a crate::Locals,
    pub implicit: &'a [(&'static str, Value)],
    pub key: Option<&'a Value>,
    /// The machine's index, and the active states by index in document order, as they are
    /// at the moment of evaluation: what `in` reads.
    pub idx: &'a crate::program::MachineIndex,
    pub active: &'a [usize],
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
        ExprKind::Ref { path, state } => {
            let (first, rest) = path.split_first().expect("a ref path is never empty");
            let mut v = match state {
                Some(state) => local(state, first, s)?,
                None => lookup(first, s)?,
            };
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
            let resolved = s.program.resolve(&e.ty);
            let name = match &*resolved {
                TypeRef::Named(n) | TypeRef::Generic(n, _) => n.as_str(),
                _ => "",
            };
            let decl = s.program.ir().types.get(name).map(|t| &t.def);
            // An event (the value of a `send`, `emit` or `raise`): `Machine.Name`, or one of
            // this machine's own `emits`, `events` or `internalEvents` by its bare name.
            let event = match decl {
                Some(_) => None,
                None => match name.split_once('.') {
                    Some((m, ev)) => s
                        .program
                        .ir()
                        .machines
                        .get(m)
                        .and_then(|m| m.events.get(ev).or_else(|| m.emits.get(ev)))
                        .map(|d| (ev, d)),
                    None => s
                        .machine
                        .emits
                        .get(name)
                        .or_else(|| s.machine.own_event(name))
                        .map(|d| (name, d)),
                },
            };
            if let Some(TypeDef::Error { .. }) = decl {
                obj.insert("_tag".into(), Value::from(name));
            }
            if let Some((tag, _)) = event {
                obj.insert("_tag".into(), Value::from(tag));
            }
            for f in fields {
                let v = eval(&f.value, s, fns)?;
                obj.insert(f.name.clone(), v);
            }
            // An optional field with no value is `null`, never omitted.
            let declared = match (decl, event) {
                (Some(TypeDef::Record { fields, .. } | TypeDef::Error { fields }), _) => {
                    Some(fields)
                }
                (None, Some((_, d))) => Some(&d.fields),
                _ => None,
            };
            for f in declared.into_iter().flatten() {
                obj.entry(f.name.clone()).or_insert(Value::Null);
            }
            Value::Object(obj)
        }
        ExprKind::Enum { variant } => Value::from(variant.as_str()),
        ExprKind::In { state } => Value::from(
            s.idx
                .by_id
                .get(state)
                .is_some_and(|i| s.active.binary_search(i).is_ok()),
        ),
        ExprKind::Op { op, args } => op_value(*op, args, e, s, fns)?,
    })
}

pub(crate) fn lookup(name: &str, s: &Scope<'_>) -> Result<Value, Defect> {
    if let Some((_, v)) = s.implicit.iter().find(|(n, _)| *n == name) {
        return Ok(v.clone());
    }
    if let Some(k) = s.key
        && name == "key"
    {
        return Ok(k.clone());
    }
    if s.machine.vars.iter().any(|v| v.name == name) {
        return s.vars.get(name).cloned().ok_or_else(|| Defect::Unassigned {
            var: name.to_owned(),
        });
    }
    Ok(s.input.get(name).cloned().unwrap_or(Value::Null))
}

/// Parameter or variable `name` of state `state`, from the locals: one with no value is
/// `unassigned`, named `<state id>.<name>` (`spec/execution.md#data-local-to-a-state`).
pub(crate) fn local(state: &str, name: &str, s: &Scope<'_>) -> Result<Value, Defect> {
    s.locals
        .get(state)
        .and_then(|l| l.get(name))
        .cloned()
        .ok_or_else(|| Defect::Unassigned {
            var: format!("{state}.{name}"),
        })
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
            match op {
                Op::Eq => return Ok(Value::from(equal(s, &args[0].ty, &a, &b))),
                Op::Ne => return Ok(Value::from(!equal(s, &args[0].ty, &a, &b))),
                _ => {}
            }
            let ty = s.program.scalar(&args[0].ty);
            match op {
                Op::Lt | Op::Le | Op::Gt | Op::Ge => {
                    let ord = compare(&ty, &a, &b);
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

/// `==` on values of type `ty`, walking the type (`spec/execution.md#equality`): numbers
/// compare numerically by their type (floats as IEEE 754 doubles, so `0.0 == -0.0`), records,
/// errors and events field by field (a missing field is `null`), lists element by element,
/// maps by their keys and the values at each, whatever the order, an optional's `null` only
/// with `null`, and anything else (a value not of its type, or a type the compiler does not
/// compare) as JSON ([`json_equal`]).
fn equal(s: &Scope<'_>, ty: &TypeRef, a: &Value, b: &Value) -> bool {
    let resolved = s.program.resolve(ty);
    match &*resolved {
        TypeRef::Builtin(Builtin::Float) => {
            if let (Some(x), Some(y)) = (a.as_f64(), b.as_f64()) {
                return x == y;
            }
        }
        TypeRef::Builtin(Builtin::Int) => {
            if let (Some(x), Some(y)) = (int(a), int(b)) {
                return x == y;
            }
        }
        TypeRef::Builtin(Builtin::Duration) => {
            if let (Some(x), Some(y)) = (millis(a), millis(b)) {
                return x == y;
            }
        }
        TypeRef::Optional(t) => {
            return match (a.is_null(), b.is_null()) {
                (true, true) => true,
                (false, false) => equal(s, t, a, b),
                _ => false,
            };
        }
        TypeRef::List(t) => {
            if let (Some(x), Some(y)) = (a.as_array(), b.as_array()) {
                return x.len() == y.len() && x.iter().zip(y).all(|(x, y)| equal(s, t, x, y));
            }
        }
        TypeRef::Map(_, t) => {
            if let (Some(x), Some(y)) = (a.as_object(), b.as_object()) {
                return x.len() == y.len()
                    && x.iter()
                        .all(|(k, x)| y.get(k).is_some_and(|y| equal(s, t, x, y)));
            }
        }
        TypeRef::Named(_) | TypeRef::Generic(..) => {
            if let Some((_, _, fields)) = s.program.ir().fields_of(&resolved) {
                return fields
                    .iter()
                    .all(|f| equal(s, &f.ty, field_of(a, &f.name), field_of(b, &f.name)));
            }
            if let Some(d) = event_decl(s, &resolved) {
                return d
                    .fields
                    .iter()
                    .all(|f| equal(s, &f.ty, field_of(a, &f.name), field_of(b, &f.name)));
            }
        }
        _ => {}
    }
    json_equal(a, b)
}

/// The declaration of the event type `ty`: `Machine.Name` (or `task.Name`, a task's emitted
/// event), or one of the current machine's own or emitted events by its bare name.
fn event_decl<'s>(s: &Scope<'s>, ty: &TypeRef) -> Option<&'s roadbook_ir::EventDecl> {
    let TypeRef::Named(n) = ty else { return None };
    let ir = s.program.ir();
    match n.split_once('.') {
        Some((owner, ev)) => ir
            .machines
            .get(owner)
            .and_then(|m| m.own_event(ev).or_else(|| m.emits.get(ev)))
            .or_else(|| ir.tasks.get(owner)?.emits.get(ev)),
        None => s.machine.own_event(n).or_else(|| s.machine.emits.get(n)),
    }
}

/// Field `name` of `v`, `null` if it has none.
fn field_of<'v>(v: &'v Value, name: &str) -> &'v Value {
    const NULL: &Value = &Value::Null;
    v.get(name).unwrap_or(NULL)
}

/// Equality of two JSON values, for a type `==` does not walk: numbers compare numerically,
/// arrays element by element, objects by their keys and the values at each, whatever the
/// order.
fn json_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(x, y)| json_equal(x, y))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, x)| y.get(k).is_some_and(|y| json_equal(x, y)))
        }
        _ => a == b,
    }
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
