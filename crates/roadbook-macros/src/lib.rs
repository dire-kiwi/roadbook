//! The `roadbook::include!` macro: compiles a chart at build time and expands to its Rust
//! types, a module per machine with a `Handlers` trait, and the functions that run it.
//!
//! The generator is in [`codegen`]; this file finds and compiles the chart, and gets the
//! charts of other crates from those crates' hidden macros.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use proc_macro::TokenStream;
use proc_macro2::{Delimiter, Ident, Span, TokenStream as Tokens, TokenTree};
use quote::quote;
use roadbook_compiler::loader::{FileRef, resolve_spec};
use roadbook_compiler::{FsLoader, Loaded, Loader, Options, compile};

mod codegen;
mod names;

/// Compiles a chart at build time and expands to its Rust types, traits and runners.
///
/// ```ignore
/// roadbook::include!("charts/place_order.road");
/// roadbook::include!("orders.road", crates(inventory = inv));
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
/// Call it at module level: it defines items next to the call. Two charts can be included in
/// one module if they declare no name in common (charts that import the same file both
/// declare what it declares, so give each its own module).
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
///   uses directly), `State`, `Event` (what it accepts) and `Emitted` (what it `emits`) enums,
///   `Ref` (`roadbook::Ref<Event>`, a handle to a run of it, the chart's `ref Machine`), and
///   `run`, `run_with`, `spawn` and `spawn_with`, and for saved snapshots `restore`,
///   `restore_with`, `resume` and `resume_with`. A compound state's variant carries its
///   active child as an enum named after the state; if that name is taken (by `State`,
///   `Event`, `Handlers`, a type of the chart or another such enum), it is prefixed with the
///   names of the states above it. A parallel state's variant carries nothing: a run in it is
///   in all its regions at once, and `State` names the parallel state.
/// - Types map as `string` to `String`, `int` to `i64`, `float` to `f64`, `bool` to `bool`,
///   `duration` to `std::time::Duration` (whole milliseconds in JSON), `json` to
///   `serde_json::Value`, `T[]` to `Vec<T>` and `T?` to `Option<T>`; but a `T?` that closes
///   a cycle of types (a record's or error's field, or an alias's target, where `T` holds the
///   declaring type: `type Node { next: Node? }`) is `Option<Box<T>>`, with the same JSON. An
///   `int` or `duration` reads any integral JSON number (`2.0` as well as `2`), and a
///   non-finite `float` does not encode: from host code it is a host defect, not `null`.
///   `Handlers` methods take strings, lists, records, errors, extern types, `json` and
///   aliases of those by reference (`&str`, `&[T]`, `&T`), and numbers, `bool`, `duration`
///   and enums by value.
///   `ref M` is `m::Ref`, taken by reference too.
///
/// **Charts of other crates.** An import of another package's file,
/// `import { Inventory } from "inventory/inventory.road"`, is answered by the crate of that
/// name (`-` read as `_`): it must be a dependency whose own `roadbook::include!` includes
/// that file (`roadbook::include!("inventory.road")` in package `inventory`), at its crate
/// root. An `include!` exports a hidden macro that hands its chart's IR, `$crate` and the
/// names it gave the error enums to an importing crate's `include!`, so the importer reads no
/// file of the dependency, always gets the version that is compiled, and is rebuilt when it
/// changes. The importer generates nothing for the declarations from the other package: it
/// re-exports the owning crate's (`pub use ::inventory::Item;`,
/// `pub use ::inventory::inventory;`, and the error enums under the owner's names), so there
/// is one Rust type for each. Imports chain (a crate can import a chart that imports another
/// crate's) and can form diamonds.
///
/// - A dependency renamed in `Cargo.toml` is named with `crates(package = name)`, the
///   package's name as the import writes it, bare (`crates(my-stock = stock)`) or quoted
///   (`crates("my-stock" = stock)`, which any package name can be).
/// - A crate may include the same chart more than once (in two modules, each with types of
///   its own). Only the first `include!` of it exports the hidden macro, and importers find
///   the chart's items at the crate root, so one of them must be there. If none is, an
///   importer's build fails with an error saying so.
#[proc_macro]
pub fn include(input: TokenStream) -> TokenStream {
    expand(input.into()).into()
}

/// Where an importing chart's `include!` continues once the crate that owns an imported
/// chart has handed over its IR: the original arguments, then one `__dep { … }` group per
/// chart handed over so far. Not for direct use.
#[doc(hidden)]
#[proc_macro]
pub fn __include_continue(input: TokenStream) -> TokenStream {
    expand(input.into()).into()
}

/// A chart of another crate, handed over by that crate's hidden macro.
struct Dep {
    /// The package, as the importing chart's import names it.
    package: String,
    /// The chart's path in the package.
    path: String,
    /// The owning crate, as a path: the `$crate` of its hidden macro.
    krate: Tokens,
    ir: roadbook_ir::Ir,
    /// The names the owning crate gave the error enums of the chart's fallible tasks
    /// (`false`) and machines (`true`). They depend on the names the owner took from the
    /// crates it imports from, so the IR alone does not tell them.
    errors: HashMap<(bool, String), String>,
}

/// The macro's arguments.
struct Args {
    path: String,
    /// The span of the path literal: written in the crate that calls `include!`, even when
    /// the call reaches this macro again through another crate's hidden macro.
    span: Span,
    /// `crates(package = alias, …)`: the name a package's crate has in this crate.
    crates: Vec<(String, Ident)>,
    deps: Vec<Dep>,
}

const USAGE: &str = "roadbook::include! takes one string literal, the path of a `.road` file relative \
    to the package directory, and optionally `crates(package = name, …)` naming the crates of \
    renamed dependencies, with the package's name as written in its import, bare or quoted: \
    `roadbook::include!(\"orders.road\", crates(inventory = inv, \"my-stock\" = stock))`";

/// The arguments, split at the commas outside groups.
fn split(input: Tokens) -> Vec<Vec<TokenTree>> {
    let mut parts = vec![Vec::new()];
    for t in input {
        match &t {
            TokenTree::Punct(p) if p.as_char() == ',' => parts.push(Vec::new()),
            // Tokens passed through `macro_rules!` may arrive in an invisible group.
            TokenTree::Group(g) if g.delimiter() == Delimiter::None => {
                let inner = split(g.stream());
                let mut inner = inner.into_iter();
                if let Some(first) = inner.next() {
                    parts.last_mut().expect("never empty").extend(first);
                }
                parts.extend(inner);
            }
            _ => parts.last_mut().expect("never empty").push(t),
        }
    }
    if parts.last().is_some_and(Vec::is_empty) {
        parts.pop();
    }
    parts
}

fn string(t: &TokenTree) -> Option<String> {
    match t {
        TokenTree::Literal(l) => syn::parse2::<syn::LitStr>(TokenTree::Literal(l.clone()).into())
            .ok()
            .map(|s| s.value()),
        TokenTree::Group(g) if g.delimiter() == Delimiter::None => {
            let inner: Vec<TokenTree> = g.stream().into_iter().collect();
            match &inner[..] {
                [t] => string(t),
                _ => None,
            }
        }
        _ => None,
    }
}

/// The span of a token, looking inside an invisible group of one token.
fn span_of(t: &TokenTree) -> Span {
    match t {
        TokenTree::Group(g) if g.delimiter() == Delimiter::None => {
            let inner: Vec<TokenTree> = g.stream().into_iter().collect();
            match &inner[..] {
                [t] => span_of(t),
                _ => t.span(),
            }
        }
        t => t.span(),
    }
}

/// A package name in `crates(…)`: a string literal (`"@scope/x"`), or identifiers joined by
/// `-` (`inventory`, `my-stock`).
fn package_name(tokens: &[TokenTree]) -> Option<String> {
    if let [t] = tokens {
        if let Some(s) = string(t) {
            return Some(s);
        }
    }
    let mut name = String::new();
    for (i, t) in tokens.iter().enumerate() {
        match t {
            TokenTree::Ident(id) if i % 2 == 0 => name.push_str(&id.to_string()),
            TokenTree::Punct(p) if i % 2 == 1 && p.as_char() == '-' => name.push('-'),
            _ => return None,
        }
    }
    (tokens.len() % 2 == 1).then_some(name)
}

fn parse_args(input: Tokens) -> Result<Args, String> {
    let parts = split(input);
    let mut parts = parts.into_iter();
    let (path, span) = match parts.next().as_deref() {
        Some([t]) => (string(t).ok_or(USAGE)?, span_of(t)),
        _ => return Err(USAGE.into()),
    };
    let mut args = Args {
        path,
        span,
        crates: Vec::new(),
        deps: Vec::new(),
    };
    for part in parts {
        match &part[..] {
            [] => {}
            [TokenTree::Ident(kw), TokenTree::Group(g)]
                if kw == "crates" && g.delimiter() == Delimiter::Parenthesis =>
            {
                for item in split(g.stream()) {
                    match &item[..] {
                        [package @ .., TokenTree::Punct(eq), TokenTree::Ident(name)]
                            if eq.as_char() == '=' =>
                        {
                            let package = package_name(package).ok_or(USAGE)?;
                            args.crates.push((package, name.clone()));
                        }
                        _ => return Err(USAGE.into()),
                    }
                }
            }
            [TokenTree::Ident(kw), TokenTree::Group(g)]
                if kw == "__dep" && g.delimiter() == Delimiter::Brace =>
            {
                args.deps.push(parse_dep(g.stream())?);
            }
            _ => return Err(USAGE.into()),
        }
    }
    Ok(args)
}

/// `package = "…", path = "…", krate = $crate, ir = "…", errors = "…"`, as the hidden macros
/// write it.
fn parse_dep(input: Tokens) -> Result<Dep, String> {
    let malformed = || "roadbook::include!: a malformed hand-over from another crate".to_owned();
    let (mut package, mut path, mut krate, mut ir, mut errors) = (None, None, None, None, None);
    for field in split(input) {
        let [TokenTree::Ident(key), TokenTree::Punct(eq), value @ ..] = &field[..] else {
            return Err(malformed());
        };
        if eq.as_char() != '=' {
            return Err(malformed());
        }
        let text = || match value {
            [t] => string(t),
            _ => None,
        };
        match key.to_string().as_str() {
            "package" => package = text(),
            "path" => path = text(),
            "ir" => ir = text(),
            "errors" => errors = text(),
            "krate" => krate = Some(value.iter().cloned().collect::<Tokens>()),
            _ => {}
        }
    }
    let (Some(package), Some(path), Some(krate), Some(ir), Some(errors)) =
        (package, path, krate, ir, errors)
    else {
        return Err(malformed());
    };
    let errors = decode_errors(&errors).ok_or_else(malformed)?;
    let ir = roadbook_ir::Ir::from_json(&ir).map_err(|e| {
        format!(
            "roadbook::include!: the IR of `{package}/{path}` that crate `{package}` handed over \
             cannot be read ({e}); it was built with another version of roadbook"
        )
    })?;
    Ok(Dep {
        package,
        path,
        krate,
        ir,
        errors,
    })
}

/// The error enums' names as a hand-over carries them: a line per fallible task or machine,
/// `task <name> <enum>` or `machine <name> <enum>`. Chart names and Rust identifiers have no
/// spaces or line breaks.
fn encode_errors(errors: &HashMap<(bool, String), String>) -> String {
    let mut lines: Vec<String> = errors
        .iter()
        .map(|((machine, name), e)| {
            let kind = if *machine { "machine" } else { "task" };
            format!("{kind} {name} {e}")
        })
        .collect();
    lines.sort();
    lines.join("\n")
}

fn decode_errors(text: &str) -> Option<HashMap<(bool, String), String>> {
    text.lines()
        .map(|line| match line.split(' ').collect::<Vec<_>>()[..] {
            ["task", name, e] => Some(((false, name.to_owned()), e.to_owned())),
            ["machine", name, e] => Some(((true, name.to_owned()), e.to_owned())),
            _ => None,
        })
        .collect()
}

/// Reads project files from disk, and answers each import of another package's file with
/// the IR that the owning crate handed over, if it has. The first file of a package that has
/// not been handed over yet is noted: the expansion then asks its crate for it.
struct MacroLoader<'d> {
    fs: FsLoader,
    deps: &'d [Dep],
    missing: Option<FileRef>,
    /// The project files read, to track.
    read: Vec<PathBuf>,
}

impl Loader for MacroLoader<'_> {
    fn resolve(&mut self, importer: &FileRef, spec: &str) -> Result<FileRef, String> {
        resolve_spec(importer, spec)
    }

    fn load(&mut self, file: &FileRef) -> Result<Loaded, String> {
        let Some(package) = &file.package else {
            let loaded = self.fs.load(file)?;
            self.read.push(self.fs.root.join(&file.path));
            return Ok(loaded);
        };
        if let Some(d) = self
            .deps
            .iter()
            .find(|d| d.package == *package && d.path == file.path)
        {
            return Ok(Loaded::Compiled(Box::new(d.ir.clone())));
        }
        self.missing.get_or_insert_with(|| file.clone());
        Err(format!(
            "crate `{package}` has not handed it over: it must be a dependency that calls \
             `roadbook::include!(\"{}\")`",
            file.path
        ))
    }
}

/// `path` as part of an identifier, one-to-one: every character that is not an ASCII letter
/// or digit is written as `_`, its hex code and `_` (`a/b.road` is `a_2f_b_2e_road`).
fn encode_path(path: &str) -> String {
    let mut out = String::new();
    for c in path.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else {
            out.push_str(&format!("_{:x}_", c as u32));
        }
    }
    out
}

/// The name of the hidden macro through which a crate hands over the chart at `path`,
/// relative to its package directory: `__roadbook_chart_` and the path, encoded.
fn chart_macro(path: &str) -> Ident {
    Ident::new(
        &format!("__roadbook_chart_{}", encode_path(path)),
        Span::call_site(),
    )
}

/// Whether the `include!` whose path literal has `span` is the one that exports the hidden
/// macro of the chart at `package_path` in the crate being compiled: the first one expanded.
/// A crate may include a chart more than once (in two modules), but can export only one
/// macro of a name.
fn exports(package_path: &str, span: Span) -> bool {
    static EXPORTERS: LazyLock<Mutex<HashMap<String, String>>> = LazyLock::new(Default::default);
    let var = |k| std::env::var(k).unwrap_or_default();
    let key = format!(
        "{}\0{}\0{package_path}",
        var("CARGO_MANIFEST_DIR"),
        var("CARGO_CRATE_NAME")
    );
    let site = format!("{span:?}");
    let mut exporters = EXPORTERS.lock().unwrap_or_else(|e| e.into_inner());
    *exporters.entry(key).or_insert_with(|| site.clone()) == site
}

fn expand(input: Tokens) -> Tokens {
    let args = match parse_args(input.clone()) {
        Ok(a) => a,
        Err(e) => return error(&e),
    };
    let path = args.path.as_str();
    let Some(manifest_dir) = std::env::var_os("CARGO_MANIFEST_DIR") else {
        return error("roadbook::include!: CARGO_MANIFEST_DIR is not set; build with Cargo");
    };
    let manifest_dir = normalize(&PathBuf::from(manifest_dir));
    let entry = normalize(&manifest_dir.join(path));
    let found = project_root(&entry);
    let root = found.clone().unwrap_or_else(|| manifest_dir.clone());
    let project_path = relative(&root, &entry);

    let mut loader = MacroLoader {
        fs: FsLoader::new(&root),
        deps: &args.deps,
        missing: None,
        read: Vec::new(),
    };
    let compilation = match compile(&mut loader, &project_path, &Options::default()) {
        Ok(c) => c,
        Err(e) => {
            return error(&format!(
                "roadbook::include!: cannot read `{path}` (relative to the package directory, \
                 CARGO_MANIFEST_DIR): {}",
                e.reason
            ));
        }
    };

    // An import of another crate's chart that has not been handed over yet: ask that
    // crate's hidden macro for it, which calls `__include_continue` back with the IR added.
    if let Some(missing) = loader.missing {
        let package = missing.package.clone().unwrap_or_default();
        let crate_name = |p: &str| p.replace('-', "_");
        let krate = match args
            .crates
            .iter()
            .find(|(p, _)| *p == package || crate_name(p) == crate_name(&package))
        {
            Some((_, name)) => name.clone(),
            None => {
                let name = crate_name(&package);
                if syn::parse_str::<Ident>(&name).is_err() {
                    return error(&format!(
                        "roadbook::include!: package `{package}` is not a crate name; name its \
                         crate with `crates(\"{package}\" = name)`"
                    ));
                }
                // Resolved where the `include!` was written, not inside the hidden macro of
                // the crate that handed over the last chart.
                Ident::new(&name, args.span)
            }
        };
        let chart = chart_macro(&missing.path);
        let file = missing.path.as_str();
        return quote! {
            ::#krate::#chart! {
                ::roadbook::__private::include_continue { #input }
                { package = #package, path = #file, }
            }
        };
    }

    // Every file the chart is made of, and the `roadbook.toml` that decided the project root,
    // so that Cargo rebuilds when one changes or goes away, even when the chart has errors.
    // A file that does not exist cannot be tracked on stable Rust: creating a `roadbook.toml`
    // that would move the root is not seen. A missing import is a compile error, which Cargo
    // retries on every build, so creating it is. Another crate's charts are not files here:
    // when one changes, Cargo rebuilds that crate and then this one.
    let config = found.map(|d| d.join("roadbook.toml"));
    let tracked = loader.read.iter().cloned().chain(config).map(|abs| {
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
    let foreign = match foreign(&ir, &args.deps) {
        Ok(f) => f,
        Err(message) => {
            let message = error(&format!("roadbook::include!: {message}"));
            return quote! { #tracked #message };
        }
    };
    let package_path = relative(&manifest_dir, &entry);
    let generated = codegen::generate(&ir, &foreign, &encode_path(&package_path))
        .and_then(|items| Ok((items, codegen::error_enum_names(&ir, &foreign)?)));
    match generated {
        Ok((items, errors)) => {
            let export = if exports(&package_path, args.span) {
                export_macro(&ir, &errors, &package_path, args.span)
            } else {
                Tokens::new()
            };
            quote! { #tracked #items #export }
        }
        Err(message) => {
            let message = error(&format!("roadbook::include!: {message}"));
            quote! { #tracked #message }
        }
    }
}

/// The declarations of `ir` from another package, each with the crate it is re-exported
/// from: the crate that handed over that package's chart, or else one whose chart has the
/// declaration (and so re-exports it).
fn foreign(ir: &roadbook_ir::Ir, deps: &[Dep]) -> Result<codegen::Foreign, String> {
    let mut out = codegen::Foreign::default();
    let package_of = |span: &roadbook_ir::Span| {
        ir.sources
            .get(span.file as usize)
            .and_then(|s| s.package.clone())
    };
    let has = |d: &Dep, name: &str| {
        d.ir.types.contains_key(name)
            || d.ir.functions.contains_key(name)
            || d.ir.actions.contains_key(name)
            || d.ir.tasks.contains_key(name)
            || d.ir.machines.contains_key(name)
    };
    let spans = ir
        .types
        .iter()
        .map(|(n, d)| (n, d.span))
        .chain(ir.functions.iter().map(|(n, d)| (n, d.span)))
        .chain(ir.actions.iter().map(|(n, d)| (n, d.span)))
        .chain(ir.tasks.iter().map(|(n, d)| (n, d.span)))
        .chain(ir.machines.iter().map(|(n, d)| (n, d.span)));
    for (name, span) in spans {
        let Some(package) = package_of(&span) else {
            continue;
        };
        let Some(i) = deps
            .iter()
            .position(|d| d.package == package && has(d, name))
            .or_else(|| deps.iter().position(|d| has(d, name)))
        else {
            return Err(format!(
                "`{name}` comes from package `{package}`, but no crate handed over a chart \
                 that declares it"
            ));
        };
        out.krate.insert(name.clone(), deps[i].krate.clone());
        for machine in [false, true] {
            let key = (machine, name.clone());
            if let Some(e) = deps[i].errors.get(&key) {
                out.error_enums.insert(key, e.clone());
            }
        }
    }
    Ok(out)
}

/// The hidden `#[macro_export] macro_rules!` through which this chart is handed to the
/// `include!` of a crate whose chart imports it: called with the importer's continuation and
/// what it asked for, it calls the continuation back with this chart's IR and `$crate`, and
/// the names this crate gave the chart's error enums (`errors`).
///
/// `$crate` gets `span`, the span of the path literal of this crate's `include!`, so that it
/// always names this crate. With the macro's call site it would name the crate whose hidden
/// macro called this expansion back, when this chart imports one.
///
/// Importers name this chart's items at the crate root (`$crate::Item`), so the macro first
/// checks that one: its `__ROADBOOK_IR_…` is found by a glob import of the crate root,
/// which shadows the empty one beside it only if it is there.
fn export_macro(
    ir: &roadbook_ir::Ir,
    errors: &HashMap<(bool, String), String>,
    package_path: &str,
    span: Span,
) -> Tokens {
    let name = chart_macro(package_path);
    let json = ir.to_json();
    let errors = encode_errors(errors);
    let dollar_crate = quote::quote_spanned!(span=> $crate);
    let marker = codegen::ir_const(&encode_path(package_path));
    let package = std::env::var("CARGO_PKG_NAME").unwrap_or_default();
    let message = format!(
        "roadbook::include!(\"{package_path}\") is not at the crate root of package \
         `{package}`, so other crates cannot import its chart: move it out of any `mod`, into \
         the crate root (src/lib.rs)"
    );
    quote! {
        #[doc(hidden)]
        #[macro_export]
        macro_rules! #name {
            ($cb:path { $($args:tt)* } { $($dep:tt)* }) => {
                const _: () = {
                    #[allow(dead_code, non_upper_case_globals)]
                    const #marker: &str = "";
                    {
                        #[allow(unused_imports)]
                        use #dollar_crate::*;
                        ::core::assert!(!#marker.is_empty(), #message);
                    }
                };
                $cb! { $($args)* , __dep { $($dep)* krate = #dollar_crate, ir = #json, errors = #errors } }
            };
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_path_encoding_is_one_to_one() {
        assert_eq!(encode_path("a/b.road"), "a_2f_b_2e_road");
        assert_ne!(encode_path("a/b.road"), encode_path("a\u{2fb}.road"));
        assert_ne!(encode_path("a_b"), encode_path("a\u{b}"));
        assert_eq!(encode_path("_"), "_5f_");
    }

    #[test]
    fn the_error_enums_names_survive_a_hand_over() {
        let errors = HashMap::from([
            ((false, "charge".to_owned()), "ChargeError".to_owned()),
            ((true, "Charge".to_owned()), "ChargeMachineError".to_owned()),
        ]);
        assert_eq!(decode_errors(&encode_errors(&errors)), Some(errors));
        assert_eq!(decode_errors(""), Some(HashMap::new()));
        assert_eq!(decode_errors("task charge"), None);
    }

    fn tokens(s: &str) -> Vec<TokenTree> {
        s.parse::<Tokens>().unwrap().into_iter().collect()
    }

    #[test]
    fn a_package_in_crates_is_bare_or_quoted() {
        assert_eq!(
            package_name(&tokens("inventory")).as_deref(),
            Some("inventory")
        );
        assert_eq!(package_name(&tokens("my-inv")).as_deref(), Some("my-inv"));
        assert_eq!(package_name(&tokens("a-b-c")).as_deref(), Some("a-b-c"));
        assert_eq!(
            package_name(&tokens("\"@scope/x\"")).as_deref(),
            Some("@scope/x")
        );
        assert_eq!(package_name(&tokens("my-")), None);
        assert_eq!(package_name(&tokens("a b")), None);
        assert_eq!(package_name(&tokens("")), None);
    }
}
