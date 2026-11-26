//! The Roadbook syntax crate: lexer, parser, syntax tree, diagnostics and the canonical
//! formatter for `.road` files.
//!
//! ```
//! use roadbook_syntax::{FileId, parse_file, format};
//!
//! let src = "machine M() {\n  initial A\n  final state A {}\n}\n";
//! let (file, diagnostics) = parse_file(FileId(0), src);
//! assert!(diagnostics.is_empty());
//! assert_eq!(file.machines().count(), 1);
//! assert_eq!(format(src).unwrap(), src);
//! ```

pub mod ast;
pub mod codes;
pub mod diagnostic;
mod format;
pub mod lexer;
mod parser;
pub mod span;

pub use diagnostic::{Diagnostic, Severity};
pub use format::format;
pub use parser::parse_file;
pub use span::{FileId, LineIndex, Pos, Span};
