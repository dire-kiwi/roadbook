//! Rust names for chart names.

use heck::{ToSnakeCase, ToUpperCamelCase};
use proc_macro2::{Ident, Span};

/// Words that can only be identifiers in their raw form (`r#type`).
const KEYWORDS: &[&str] = &[
    "abstract", "as", "async", "await", "become", "box", "break", "const", "continue", "do", "dyn",
    "else", "enum", "extern", "false", "final", "fn", "for", "gen", "if", "impl", "in", "let",
    "loop", "macro", "match", "mod", "move", "mut", "override", "priv", "pub", "ref", "return",
    "static", "struct", "trait", "true", "try", "type", "typeof", "unsafe", "unsized", "use",
    "virtual", "where", "while", "yield",
];

/// Keywords that cannot be raw identifiers; they get a trailing `_`.
const UNRAWABLE: &[&str] = &["self", "Self", "super", "crate", "_"];

/// The identifier for `name`: raw if it is a keyword, with a trailing `_` if it cannot be.
pub fn ident(name: &str) -> Ident {
    if UNRAWABLE.contains(&name) {
        Ident::new(&format!("{name}_"), Span::call_site())
    } else if KEYWORDS.contains(&name) {
        Ident::new_raw(name, Span::call_site())
    } else {
        Ident::new(name, Span::call_site())
    }
}

/// The `snake_case` form of a chart name (`totalCents` is `total_cents`).
pub fn snake(name: &str) -> String {
    name.to_snake_case()
}

/// The `UpperCamelCase` form of a chart name (`chargeCard` is `ChargeCard`).
pub fn camel(name: &str) -> String {
    name.to_upper_camel_case()
}

/// The identifier as Rust prints it, without `r#`: what a name is compared by.
pub fn plain(id: &Ident) -> String {
    let s = id.to_string();
    s.strip_prefix("r#").map(str::to_owned).unwrap_or(s)
}
