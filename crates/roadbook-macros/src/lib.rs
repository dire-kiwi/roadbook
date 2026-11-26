//! The `roadbook::include!` macro: compiles a chart at build time and expands to its Rust
//! types, a module per machine with a `Handlers` trait, and the functions that run it.
//!
//! The generator is in [`codegen`]; this file finds and compiles the chart.

use std::path::{Component, Path, PathBuf};

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as Tokens};
use quote::quote;
use roadbook_compiler::{FsLoader, Options, compile};

mod codegen;
mod names;

/// Compiles a chart at build time and expands to its Rust types, traits and runners.
///
/// ```ignore
/// roadbook::include!("charts/place_order.road");
/// ```
///
/// The path is relative to the package's directory (`CARGO_MANIFEST_DIR`). The project root,
/// which the chart's imports and diagnostics are relative to, is the nearest directory above
/// the chart that holds a `roadbook.toml`, else the package's directory. A chart error is a
/// compile error at the macro call, with the chart's file, line and column; warnings are not
/// reported (run `roadbook check` for those). Cargo rebuilds the crate when any of the
/// chart's files, or the project's `roadbook.toml`, changes; creating a `roadbook.toml` that
/// would move the project root is not seen until something else changes.
///
/// Call it at module level, at most once per module: it defines items next to the call.
///
/// - For every `type`, a `struct` with `serde` derives (fields in `snake_case`, renamed to the
///   chart's names in JSON); for every `enum`, a fieldless `enum`; for every `error`, a
///   `struct` (a unit struct for a bare error) whose JSON carries `"_tag": "Name"`, with
///   `Display` and `std::error::Error`; for every alias, a `type` alias. The user crate needs
///   no `serde` dependency for them.
/// - An `extern type Name` generates nothing: define (or import) `Name` next to the call. It
///   must implement `Debug`, `Clone`, `PartialEq`, `Send`, `Sync`, `serde::Serialize` and
///   `serde::Deserialize`, and its JSON form is whatever its `Serialize` produces.
/// - For every task and machine that declares errors, an enum of exactly those errors,
///   `<Name>Error` (`ChargeCardError`), with `From` for each so `?` converts into it. If that
///   name is taken by a type or another such enum, a task's is `<Name>TaskError` and a
///   machine's `<Name>MachineError`.
/// - For every machine, a module named in `snake_case` (`PlaceOrder` is `place_order`) with
///   a `Handlers` trait (one method per function, action, task and child machine the machine
///   uses directly), `State` and `Event` enums, and `run`, `run_with`, `spawn` and
///   `spawn_with`. A compound state's variant carries its active child as an enum named after
///   the state; if that name is taken (by `State`, `Event`, `Handlers`, a type of the chart
///   or another such enum), it is prefixed with the names of the states above it.
/// - Types map as `string` to `String`, `int` to `i64`, `float` to `f64`, `bool` to `bool`,
///   `duration` to `std::time::Duration` (whole milliseconds in JSON), `json` to
///   `serde_json::Value`, `T[]` to `Vec<T>` and `T?` to `Option<T>`. `Handlers` methods take
///   strings, lists, records, errors, extern types, `json` and aliases of those by
///   reference (`&str`, `&[T]`, `&T`), and numbers, `bool`, `duration` and enums by value.
///
/// Parallel states are not supported until milestone 6.
#[proc_macro]
pub fn include(input: TokenStream) -> TokenStream {
    let lit = match syn::parse::<syn::LitStr>(input) {
        Ok(lit) => lit,
        Err(_) => {
            return error(
                "roadbook::include! takes one string literal: the path of a `.road` file, \
                 relative to the package directory",
            )
            .into();
        }
    };
    expand(&lit.value()).into()
}

fn expand(path: &str) -> Tokens {
    let Some(manifest_dir) = std::env::var_os("CARGO_MANIFEST_DIR") else {
        return error("roadbook::include!: CARGO_MANIFEST_DIR is not set; build with Cargo");
    };
    let manifest_dir = PathBuf::from(manifest_dir);
    let entry = normalize(&manifest_dir.join(path));
    let found = project_root(&entry);
    let root = found.clone().unwrap_or_else(|| normalize(&manifest_dir));
    let project_path = relative(&root, &entry);

    let compilation = match compile(
        &mut FsLoader::new(&root),
        &project_path,
        &Options::default(),
    ) {
        Ok(c) => c,
        Err(e) => {
            return error(&format!(
                "roadbook::include!: cannot read `{path}` (relative to the package directory, \
                 CARGO_MANIFEST_DIR): {}",
                e.reason
            ));
        }
    };

    // Every file the chart is made of, and the `roadbook.toml` that decided the project root,
    // so that Cargo rebuilds when one changes or goes away, even when the chart has errors.
    // A file that does not exist cannot be tracked on stable Rust: creating a `roadbook.toml`
    // that would move the root is not seen. A missing import is a compile error, which Cargo
    // retries on every build, so creating it is.
    let config = found.map(|d| d.join("roadbook.toml"));
    let tracked = compilation
        .files
        .iter()
        .map(|(p, _)| root.join(p))
        .chain(config)
        .map(|abs| {
            let abs = abs.to_string_lossy().into_owned();
            quote! { const _: &[u8] = ::core::include_bytes!(#abs); }
        });
    let tracked = quote! { #(#tracked)* };

    if compilation.has_errors() {
        let rendered: Vec<String> = compilation
            .diagnostics
            .iter()
            .filter(|d| d.is_error())
            .map(|d| d.render(&compilation.files))
            .collect();
        let message = error(&rendered.join("\n"));
        return quote! { #tracked #message };
    }
    let Some(ir) = compilation.ir else {
        return quote! { #tracked };
    };
    match codegen::generate(&ir) {
        Ok(items) => {
            let export = export_macro(&ir, &project_path);
            quote! { #tracked #items #export }
        }
        Err(message) => {
            let message = error(&format!("roadbook::include!: {message}"));
            quote! { #tracked #message }
        }
    }
}

/// Milestone 5 (cross-crate imports): emits the hidden
/// `#[macro_export] macro_rules! __roadbook_chart_<mangled project path>` that hands this
/// chart's IR and `$crate` to an importing crate's `include!`, which continues in it (see
/// `spike/cross-crate/README.md`, "Recommendation"). Until then it emits nothing.
fn export_macro(_ir: &roadbook_ir::Ir, _project_path: &str) -> Tokens {
    Tokens::new()
}

/// `compile_error!(message)` at the macro call.
fn error(message: &str) -> Tokens {
    quote::quote_spanned! { Span::call_site() => ::core::compile_error!(#message); }
}

/// The nearest directory above `entry` that holds a `roadbook.toml`.
fn project_root(entry: &Path) -> Option<PathBuf> {
    entry
        .ancestors()
        .skip(1)
        .find(|d| d.join("roadbook.toml").is_file())
        .map(Path::to_path_buf)
}

/// `path` with `.` and `..` segments resolved lexically.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            c => out.push(c),
        }
    }
    out
}

/// `path` relative to `base` (both normalized and absolute), `/`-separated.
fn relative(base: &Path, path: &Path) -> String {
    let base: Vec<_> = base.components().collect();
    let path: Vec<_> = path.components().collect();
    let common = base.iter().zip(&path).take_while(|(a, b)| a == b).count();
    let mut segments: Vec<String> = vec!["..".to_owned(); base.len() - common];
    segments.extend(
        path[common..]
            .iter()
            .map(|c| c.as_os_str().to_string_lossy().into_owned()),
    );
    segments.join("/")
}
