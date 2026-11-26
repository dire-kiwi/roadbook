//! The `roadbook::include!` macro. Placeholder until the generator lands.

use proc_macro::TokenStream;

/// Compiles a chart at build time and expands to its Rust types, traits and runners.
#[proc_macro]
pub fn include(_input: TokenStream) -> TokenStream {
    TokenStream::new()
}
