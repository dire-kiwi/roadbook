//! One module per command. Each has an `Args` type and a `run` function returning an
//! [`Exit`](crate::Exit).

pub mod check;
pub mod export;
pub mod fmt;
pub mod r#gen;
pub mod lsp;
pub mod paths;
pub mod viz;
