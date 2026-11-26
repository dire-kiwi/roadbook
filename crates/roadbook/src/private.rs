//! What the code `roadbook::include!` generates calls. Not a stable interface.

use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

pub use std::sync::LazyLock;

pub use roadbook_core::{Defect, Object, Program, SrcKind};
pub use serde;
pub use serde_json;

pub use crate::actor::{Codec, spawn};
pub use crate::dynamic::{Host, Invocation, Outcome, run, run_with};
pub use crate::env::{BoxFuture, Config};

/// Reads the IR a macro embedded. It was produced by the compiler in the same build, so a
/// failure is a bug.
pub fn program(json: &str) -> Program {
    Program::from_json(json)
        .unwrap_or_else(|e| panic!("the IR embedded by roadbook::include! is invalid: {e}"))
}

/// A value in its JSON form, or a host defect if it cannot be encoded (a non-finite `float`
/// in a generated type is one: `serde_json` would write it as `null`).
pub fn encode<T: Serialize + ?Sized>(value: &T) -> Result<Value, Defect> {
    serde_json::to_value(value).map_err(|e| Defect::host(format!("could not encode a value: {e}")))
}

/// An argument or result decoded as its Rust type, or a host defect.
pub fn decode<T: DeserializeOwned>(value: Value, what: &str) -> Result<T, Defect> {
    serde_json::from_value(value).map_err(|e| Defect::host(format!("{what} does not decode: {e}")))
}

/// Takes argument `name` out of `args` and decodes it.
pub fn arg<T: DeserializeOwned>(args: &mut Object, name: &str) -> Result<T, Defect> {
    decode(
        args.remove(name).unwrap_or(Value::Null),
        &format!("argument `{name}`"),
    )
}

/// Encodes a type built from `int`, `float` and `duration` ([`wire::Wire`]), or gives a host
/// defect.
pub fn encode_wire<T: wire::Wire>(value: &T) -> Result<Value, Defect> {
    value
        .to_wire()
        .map_err(|e| Defect::host(format!("could not encode a value: {e}")))
}

/// Decodes a type built from `int`, `float` and `duration` ([`wire::Wire`]), or gives a host
/// defect.
pub fn decode_wire<T: wire::Wire>(value: Value, what: &str) -> Result<T, Defect> {
    T::from_wire(value).map_err(|e| Defect::host(format!("{what} does not decode: {e}")))
}

/// Takes argument `name` out of `args` and decodes it as a [`wire::Wire`] type.
pub fn arg_wire<T: wire::Wire>(args: &mut Object, name: &str) -> Result<T, Defect> {
    decode_wire(
        args.remove(name).unwrap_or(Value::Null),
        &format!("argument `{name}`"),
    )
}

/// A machine's output or error, decoded. The core produced it from the chart's own
/// expressions, so it has the declared type, but a host type's `Deserialize` (an extern
/// type's, say) can still refuse it: that is a host defect, raised as a [`Defect`] panic
/// payload like any defect that ends a run.
pub fn expect<T: DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).unwrap_or_else(|e| result_defect(e.to_string()))
}

/// [`expect`] for a [`wire::Wire`] type.
pub fn expect_wire<T: wire::Wire>(value: Value) -> T {
    T::from_wire(value).unwrap_or_else(|e| result_defect(e))
}

fn result_defect(e: String) -> ! {
    std::panic::resume_unwind(Box::new(Defect::host(format!(
        "a machine's result does not decode: {e}"
    ))))
}

/// The machine's input, or the host defect that encoding it met, raised as a [`Defect`]
/// panic payload: a run that cannot start ends with a defect before its first record.
pub fn input(input: Result<Object, Defect>) -> Object {
    input.unwrap_or_else(|d| std::panic::resume_unwind(Box::new(d)))
}

/// The defect for a name the chart does not declare (the driver never asks for one).
pub fn unknown(what: &str, name: &str) -> Defect {
    Defect::host(format!("the chart has no {what} `{name}`"))
}

/// A future that is ready at once.
pub fn ready<'a, T: Send + 'a>(value: T) -> BoxFuture<'a, T> {
    Box::pin(std::future::ready(value))
}

/// The outcome of a task or machine invoke from its encoded result.
pub fn outcome(result: Result<Result<Value, Value>, Defect>) -> Outcome {
    match result {
        Ok(Ok(v)) => Outcome::Done(v),
        Ok(Err(e)) => Outcome::Fail(e),
        Err(d) => Outcome::Defect(d),
    }
}

/// A `duration` in its JSON form: whole milliseconds, finer parts truncated.
pub fn millis(d: Duration) -> Value {
    Value::from(u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// An integer from a JSON number in any form: `2`, `2.0` and `2e0` are all 2, since a
/// number's form does not matter (`spec/execution.md#values`). `None` for a number that is
/// not integral or does not fit.
fn integer(v: &Value) -> Option<i64> {
    if let Some(i) = v.as_i64() {
        return Some(i);
    }
    let f = v.as_f64()?;
    // i64::MIN is exactly -2^63; 2^63 itself does not fit.
    (f.fract() == 0.0 && (i64::MIN as f64..-(i64::MIN as f64)).contains(&f)).then_some(f as i64)
}

/// `#[serde(with = "…::wire")]` for fields whose type is built from `int`, `float` and
/// `duration` with `Option` and `Vec`: an `int` accepts any integral JSON number, a
/// `duration` is whole milliseconds, and a non-finite `float` is refused rather than
/// written as `null`.
pub mod wire {
    use super::*;

    /// A type built from `i64`, `f64` and `Duration` with `Option` and `Vec`.
    pub trait Wire: Sized {
        /// The JSON form, or why there is none.
        fn to_wire(&self) -> Result<Value, String>;
        /// The value of a JSON form, or why it is not one.
        fn from_wire(v: Value) -> Result<Self, String>;
    }

    impl Wire for i64 {
        fn to_wire(&self) -> Result<Value, String> {
            Ok(Value::from(*self))
        }
        fn from_wire(v: Value) -> Result<Self, String> {
            integer(&v).ok_or_else(|| format!("{v} is not an int"))
        }
    }

    impl Wire for f64 {
        fn to_wire(&self) -> Result<Value, String> {
            serde_json::Number::from_f64(*self)
                .map(Value::Number)
                .ok_or_else(|| format!("{self} is not a finite float"))
        }
        fn from_wire(v: Value) -> Result<Self, String> {
            v.as_f64().ok_or_else(|| format!("{v} is not a float"))
        }
    }

    impl Wire for Duration {
        fn to_wire(&self) -> Result<Value, String> {
            Ok(millis(*self))
        }
        fn from_wire(v: Value) -> Result<Self, String> {
            integer(&v)
                .and_then(|i| u64::try_from(i).ok())
                .or_else(|| v.as_u64())
                .map(Duration::from_millis)
                .ok_or_else(|| format!("{v} is not a duration in milliseconds"))
        }
    }

    impl<T: Wire> Wire for Option<T> {
        fn to_wire(&self) -> Result<Value, String> {
            self.as_ref().map_or(Ok(Value::Null), T::to_wire)
        }
        fn from_wire(v: Value) -> Result<Self, String> {
            match v {
                Value::Null => Ok(None),
                v => T::from_wire(v).map(Some),
            }
        }
    }

    impl<T: Wire> Wire for Vec<T> {
        fn to_wire(&self) -> Result<Value, String> {
            self.iter()
                .map(T::to_wire)
                .collect::<Result<_, _>>()
                .map(Value::Array)
        }
        fn from_wire(v: Value) -> Result<Self, String> {
            match v {
                Value::Array(items) => items.into_iter().map(T::from_wire).collect(),
                v => Err(format!("{v} is not a list")),
            }
        }
    }

    pub fn serialize<T: Wire, S: Serializer>(v: &T, s: S) -> Result<S::Ok, S::Error> {
        v.to_wire().map_err(serde::ser::Error::custom)?.serialize(s)
    }

    pub fn deserialize<'de, T: Wire, D: Deserializer<'de>>(d: D) -> Result<T, D::Error> {
        let v = Value::deserialize(d)?;
        T::from_wire(v).map_err(serde::de::Error::custom)
    }
}
