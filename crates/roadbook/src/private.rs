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

/// A value in its JSON form, or a host defect if it cannot be encoded.
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

/// Decodes a value the core produced from the chart's own expressions (a machine's output
/// or error), which always has the declared type.
pub fn expect<T: DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value)
        .unwrap_or_else(|e| panic!("a machine's result does not decode: {e}"))
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

/// `#[serde(with = "…::ms")]` for fields whose type is built from `Duration`
/// (`Duration`, `Option<Duration>`, `Vec<Duration>`, …): milliseconds in JSON.
pub mod ms {
    use super::*;

    /// A type built from `Duration` with `Option` and `Vec`.
    pub trait Millis: Sized {
        fn to_ms(&self) -> Value;
        fn from_ms(v: Value) -> Result<Self, String>;
    }

    impl Millis for Duration {
        fn to_ms(&self) -> Value {
            millis(*self)
        }
        fn from_ms(v: Value) -> Result<Self, String> {
            v.as_u64()
                .map(Duration::from_millis)
                .ok_or_else(|| format!("{v} is not a duration in milliseconds"))
        }
    }

    impl<T: Millis> Millis for Option<T> {
        fn to_ms(&self) -> Value {
            self.as_ref().map_or(Value::Null, T::to_ms)
        }
        fn from_ms(v: Value) -> Result<Self, String> {
            match v {
                Value::Null => Ok(None),
                v => T::from_ms(v).map(Some),
            }
        }
    }

    impl<T: Millis> Millis for Vec<T> {
        fn to_ms(&self) -> Value {
            Value::Array(self.iter().map(T::to_ms).collect())
        }
        fn from_ms(v: Value) -> Result<Self, String> {
            match v {
                Value::Array(items) => items.into_iter().map(T::from_ms).collect(),
                v => Err(format!("{v} is not a list")),
            }
        }
    }

    pub fn serialize<T: Millis, S: Serializer>(v: &T, s: S) -> Result<S::Ok, S::Error> {
        v.to_ms().serialize(s)
    }

    pub fn deserialize<'de, T: Millis, D: Deserializer<'de>>(d: D) -> Result<T, D::Error> {
        let v = Value::deserialize(d)?;
        T::from_ms(v).map_err(serde::de::Error::custom)
    }
}
