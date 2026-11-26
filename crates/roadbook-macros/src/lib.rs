//! The `roadbook::include!` macro: compiles a chart at build time and expands to its Rust
//! types, a module per machine with a `Handlers` trait, and the functions that run it.
//!
//! The generator is in [`codegen`]; this file finds and compiles the chart, and gets the
//! charts of other crates from those crates' hidden macros.

use std::path::{Component, Path, PathBuf};

use std::collections::HashMap;

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
///   uses directly), `State`, `Event` (what it accepts) and `Emitted` (what it `emits`) enums,
///   `Ref` (`roadbook::Ref<Event>`, a handle to a run of it, the chart's `ref Machine`), and
///   `run`, `run_with`, `spawn` and `spawn_with`. A compound state's variant carries its active child as an enum named after
///   the state; if that name is taken (by `State`, `Event`, `Handlers`, a type of the chart
///   or another such enum), it is prefixed with the names of the states above it.
/// - Types map as `string` to `String`, `int` to `i64`, `float` to `f64`, `bool` to `bool`,
///   `duration` to `std::time::Duration` (whole milliseconds in JSON), `json` to
///   `serde_json::Value`, `T[]` to `Vec<T>` and `T?` to `Option<T>`. An `int` or `duration`
///   reads any integral JSON number (`2.0` as well as `2`), and a non-finite `float` does not
///   encode: from host code it is a host defect, not `null`. `Handlers` methods take
///   strings, lists, records, errors, extern types, `json` and aliases of those by
///   reference (`&str`, `&[T]`, `&T`), and numbers, `bool`, `duration` and enums by value.
///   `ref M` is `m::Ref`, taken by reference too.
///
/// **Charts of other crates.** An import of another package's file,
/// `import { Inventory } from "inventory/inventory.road"`, is answered by the crate of that
/// name (`-` read as `_`): it must be a dependency whose own `roadbook::include!` includes
/// that file (`roadbook::include!("inventory.road")` in package `inventory`), at its crate
/// root. Every `include!` exports a hidden macro that hands its chart's IR and `$crate` to
/// an importing crate's `include!`, so the importer reads no file of the dependency, always
/// gets the version that is compiled, and is rebuilt when it changes. The importer generates
/// nothing for the declarations from the other package: it re-exports the owning crate's
/// (`pub use ::inventory::Item;`, `pub use ::inventory::inventory;`), so there is one Rust
/// type for each. A dependency renamed in `Cargo.toml` is named with
/// `crates(package = name)`.
///
/// Parallel states are not supported until milestone 6.
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
}

/// The macro's arguments.
struct Args {
    path: String,
    /// `crates(package = alias, …)`: the name a package's crate has in this crate.
    crates: Vec<(String, Ident)>,
    deps: Vec<Dep>,
}

const USAGE: &str = "roadbook::include! takes one string literal, the path of a `.road` file relative \
    to the package directory, and optionally `crates(package = name, …)` naming the crates of \
    renamed dependencies: `roadbook::include!(\"orders.road\", crates(inventory = inv))`";

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

fn parse_args(input: Tokens) -> Result<Args, String> {
    let parts = split(input);
    let mut parts = parts.into_iter();
    let path = match parts.next().as_deref() {
        Some([t]) => string(t).ok_or(USAGE)?,
        _ => return Err(USAGE.into()),
    };
    let mut args = Args {
        path,
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
                        [
                            TokenTree::Ident(package),
                            TokenTree::Punct(eq),
                            TokenTree::Ident(name),
                        ] if eq.as_char() == '=' => {
                            args.crates.push((package.to_string(), name.clone()));
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

/// `package = "…", path = "…", krate = $crate, ir = "…"`, as the hidden macros write it.
fn parse_dep(input: Tokens) -> Result<Dep, String> {
    let (mut package, mut path, mut krate, mut ir) = (None, None, None, None);
    for field in split(input) {
        let [TokenTree::Ident(key), TokenTree::Punct(eq), value @ ..] = &field[..] else {
            return Err("roadbook::include!: a malformed hand-over from another crate".into());
        };
        if eq.as_char() != '=' {
            return Err("roadbook::include!: a malformed hand-over from another crate".into());
        }
        let text = || match value {
            [t] => string(t),
            _ => None,
        };
        match key.to_string().as_str() {
            "package" => package = text(),
            "path" => path = text(),
            "ir" => ir = text(),
            "krate" => krate = Some(value.iter().cloned().collect::<Tokens>()),
            _ => {}
        }
    }
    let (Some(package), Some(path), Some(krate), Some(ir)) = (package, path, krate, ir) else {
        return Err("roadbook::include!: a malformed hand-over from another crate".into());
    };
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
    })
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

/// The name of the hidden macro through which a crate hands over the chart at `path`,
/// relative to its package directory: `__roadbook_chart_` and the path with every character
/// that is not an ASCII letter or digit written as `_` and its hex code.
fn chart_macro(path: &str) -> Ident {
    let mut name = String::from("__roadbook_chart_");
    for c in path.chars() {
        if c.is_ascii_alphanumeric() {
            name.push(c);
        } else {
            name.push_str(&format!("_{:x}", c as u32));
        }
    }
    Ident::new(&name, Span::call_site())
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
        let krate = match args.crates.iter().find(|(p, _)| *p == package) {
            Some((_, name)) => name.clone(),
            None => {
                let name = package.replace('-', "_");
                if syn::parse_str::<Ident>(&name).is_err() {
                    return error(&format!(
                        "roadbook::include!: package `{package}` is not a crate name; name its \
                         crate with `crates({package} = name)`"
                    ));
                }
                Ident::new(&name, Span::call_site())
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
    match codegen::generate(&ir, &foreign) {
        Ok(items) => {
            let export = export_macro(&ir, &relative(&manifest_dir, &entry));
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
    let mut names: HashMap<usize, HashMap<(bool, String), String>> = HashMap::new();
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
        let owned = match names.entry(i) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(codegen::error_enum_names(&deps[i].ir)?)
            }
        };
        for machine in [false, true] {
            let key = (machine, name.clone());
            if let Some(e) = owned.get(&key) {
                out.error_enums.insert(key, e.clone());
            }
        }
    }
    Ok(out)
}

/// The hidden `#[macro_export] macro_rules!` through which this chart is handed to the
/// `include!` of a crate whose chart imports it: called with the importer's continuation and
/// what it asked for, it calls the continuation back with this chart's IR and `$crate`.
fn export_macro(ir: &roadbook_ir::Ir, package_path: &str) -> Tokens {
    let name = chart_macro(package_path);
    let json = ir.to_json();
    quote! {
        #[doc(hidden)]
        #[macro_export]
        macro_rules! #name {
            ($cb:path { $($args:tt)* } { $($dep:tt)* }) => {
                $cb! { $($args)* , __dep { $($dep)* krate = $crate, ir = #json } }
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
