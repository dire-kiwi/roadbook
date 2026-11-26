//! What `roadbook::include!` expands to, from a chart's IR.
//!
//! Generated code names everything by absolute path (`::core::…`, `::roadbook::__private::…`)
//! and refers to the chart's types from a machine's module as `super::Name`, so names in the
//! user's scope (or in the chart) never change what it means.

use std::collections::{HashMap, HashSet};

use proc_macro2::{Ident, Literal, TokenStream};
use quote::{format_ident, quote};
use roadbook_ir::{
    Builtin, Effect, Expr, ExprKind, Field, Ir, Machine, State, StateKind, Trigger, TypeDef,
    TypeRef,
};

use crate::names::{camel, ident, plain, snake};

/// What another crate generated: the declarations of a chart's IR that come from another
/// package, which the expansion refers to instead of generating them again.
#[derive(Default)]
pub struct Foreign {
    /// For each such declaration, the crate whose `roadbook::include!` generated (or
    /// re-exported) it, as a path: the `$crate` of that crate's hidden macro.
    pub krate: HashMap<String, TokenStream>,
    /// The names those crates gave the error enums of their fallible tasks (`false`) and
    /// machines (`true`).
    pub error_enums: HashMap<(bool, String), String>,
}

/// The name of the error enum of every fallible task (`false`) and machine (`true`) of `ir`,
/// as its expansion with `foreign` names them: what the crate hands over to importers, which
/// cannot work the names out from `ir` alone, since they depend on the names `foreign` took.
pub fn error_enum_names(
    ir: &Ir,
    foreign: &Foreign,
) -> Result<HashMap<(bool, String), String>, String> {
    let g = Gen::new(ir, foreign)?;
    Ok(g.error_enums
        .into_iter()
        .map(|((m, n), id)| ((m, n.to_owned()), plain(&id)))
        .collect())
}

/// The name of the hidden constant that holds the IR of the chart whose package path,
/// encoded, is `suffix`.
pub fn ir_const(suffix: &str) -> Ident {
    format_ident!("__ROADBOOK_IR_{}", suffix)
}

/// The name of the hidden static that holds the program of the chart whose package path,
/// encoded, is `suffix`.
fn program_static(suffix: &str) -> Ident {
    format_ident!("__ROADBOOK_PROGRAM_{}", suffix)
}

/// The items for a chart, or why it cannot be expanded. Declarations in `foreign` are
/// re-exported from the crate that generated them. `suffix` is the chart's package path,
/// encoded, which the hidden items are named with, so that two charts can sit side by side.
pub fn generate(ir: &Ir, foreign: &Foreign, suffix: &str) -> Result<TokenStream, String> {
    let mut g = Gen::new(ir, foreign)?;
    g.program = program_static(suffix);
    let mut items = Vec::new();
    for (name, decl) in &ir.types {
        match foreign.krate.get(name) {
            Some(krate) => {
                let id = ident(name);
                items.push(quote!(pub use #krate::#id;));
            }
            None => items.push(g.type_decl(name, &decl.def, &decl.doc)?),
        }
    }
    items.extend(g.error_enums());
    let mut modules: HashMap<String, String> = HashMap::new();
    for (name, m) in &ir.machines {
        let module = plain(&ident(&snake(name)));
        if let Some(other) = modules.insert(module.clone(), name.clone()) {
            return Err(format!(
                "machines `{other}` and `{name}` would both be the Rust module `{module}`"
            ));
        }
        match foreign.krate.get(name) {
            Some(krate) => {
                let id = ident(&snake(name));
                items.push(quote!(pub use #krate::#id;));
            }
            None => items.push(g.machine(name, m)?),
        }
    }
    let json = ir.to_json();
    let ir_const = ir_const(suffix);
    let program = &g.program;
    Ok(quote! {
        /// The chart's IR, as the compiler wrote it.
        #[doc(hidden)]
        #[allow(dead_code, non_upper_case_globals)]
        pub const #ir_const: &str = #json;

        /// The chart's IR, read once.
        #[doc(hidden)]
        #[allow(dead_code, non_upper_case_globals)]
        pub static #program: ::roadbook::__private::LazyLock<::roadbook::__private::Program> =
            ::roadbook::__private::LazyLock::new(|| ::roadbook::__private::program(#ir_const));

        #(#items)*
    })
}

/// `#[doc = "…"]`, or nothing for an empty doc.
fn doc(text: &str) -> TokenStream {
    if text.is_empty() {
        TokenStream::new()
    } else {
        quote!(#[doc = #text])
    }
}

/// `#[serde(rename = "name")]` if the Rust name is not the chart's.
fn rename(name: &str, id: &Ident) -> TokenStream {
    if plain(id) == name {
        TokenStream::new()
    } else {
        quote!(#[serde(rename = #name)])
    }
}

/// The derives every generated data type has.
fn serde_derives() -> TokenStream {
    quote! {
        ::roadbook::__private::serde::Serialize,
        ::roadbook::__private::serde::Deserialize
    }
}

/// How a `Handlers` parameter is passed, given the decoded owned value.
#[derive(Clone, Copy)]
enum Pass {
    /// By value.
    Value,
    /// `&value`.
    Ref,
    /// `value.as_ref()`.
    AsRef,
    /// `value.as_deref()`.
    AsDeref,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Function,
    Action,
    Task,
    Machine,
}

/// One `Handlers` method.
struct Method<'a> {
    /// The declaration's name in the chart.
    name: &'a str,
    kind: Kind,
    ident: Ident,
    params: &'a [Field],
    returns: Option<&'a TypeRef>,
    /// The error enum, for a fallible task or machine.
    error: Option<Ident>,
    doc: &'a str,
}

struct Gen<'a> {
    ir: &'a Ir,
    /// The error enum of each fallible task (`false`) and machine (`true`).
    error_enums: HashMap<(bool, &'a str), Ident>,
    /// The crate each declaration from another package is re-exported from.
    foreign: &'a HashMap<String, TokenStream>,
    /// The hidden static the machines' runners read the program from.
    program: Ident,
    /// For each type, the types its Rust form contains in place (not behind a `Vec`): those
    /// of its fields or, for an alias, its target, through `?`, and theirs in turn.
    contains: HashMap<&'a str, HashSet<&'a str>>,
}

/// The types `t` names in place, not inside a list, each with whether it is behind a `?`.
fn in_place(t: &TypeRef, optional: bool, out: &mut Vec<(String, bool)>) {
    match t {
        TypeRef::Named(n) => out.push((n.clone(), optional)),
        TypeRef::Optional(e) => in_place(e, true, out),
        TypeRef::Builtin(_) | TypeRef::List(_) | TypeRef::Ref(_) => {}
    }
}

/// The types a declaration's Rust form holds in place, each with whether it is behind a `?`.
fn holds(def: &TypeDef) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    match def {
        TypeDef::Record { fields } | TypeDef::Error { fields } => {
            for f in fields {
                in_place(&f.ty, false, &mut out);
            }
        }
        TypeDef::Alias { ty } => in_place(ty, false, &mut out),
        TypeDef::Enum { .. } | TypeDef::Extern => {}
    }
    out
}

/// Every type reachable from `from` along `edges`, not counting `from` unless on a cycle.
fn reachable<'a>(from: &'a str, edges: &HashMap<&'a str, Vec<&'a str>>) -> HashSet<&'a str> {
    let mut seen = HashSet::new();
    let mut stack: Vec<&str> = edges.get(from).cloned().unwrap_or_default();
    while let Some(n) = stack.pop() {
        if seen.insert(n) {
            stack.extend(edges.get(n).into_iter().flatten().copied());
        }
    }
    seen
}

/// For each type of `ir`, the types it contains in place; or the error for a type that
/// contains itself with no `?` on the way, which has no finite value.
fn containment(ir: &Ir) -> Result<HashMap<&str, HashSet<&str>>, String> {
    let mut all: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut plain: HashMap<&str, Vec<&str>> = HashMap::new();
    for (name, decl) in &ir.types {
        for (n, optional) in holds(&decl.def) {
            let Some((n, _)) = ir.types.get_key_value(&n) else {
                continue;
            };
            all.entry(name.as_str()).or_default().push(n.as_str());
            if !optional {
                plain.entry(name.as_str()).or_default().push(n.as_str());
            }
        }
    }
    let mut out = HashMap::new();
    for name in ir.types.keys() {
        if reachable(name, &plain).contains(name.as_str()) {
            return Err(format!(
                "type `{name}` contains itself with no `?` or `[]` on the way, so it has no \
                 finite value"
            ));
        }
        out.insert(name.as_str(), reachable(name, &all));
    }
    Ok(out)
}

impl<'a> Gen<'a> {
    fn new(ir: &'a Ir, foreign: &'a Foreign) -> Result<Gen<'a>, String> {
        let fallible: Vec<(bool, &str)> = ir
            .tasks
            .iter()
            .filter(|(_, t)| !t.errors.is_empty())
            .map(|(n, _)| (false, n.as_str()))
            .chain(
                ir.machines
                    .iter()
                    .filter(|(_, m)| !m.errors.is_empty())
                    .map(|(n, _)| (true, n.as_str())),
            )
            .collect();
        let short = |n: &str| format!("{}Error", camel(n));
        let mut count: HashMap<String, usize> = HashMap::new();
        for (_, n) in &fallible {
            *count.entry(short(n)).or_default() += 1;
        }
        let mut taken: HashSet<String> = ir.types.keys().cloned().collect();
        let mut error_enums = HashMap::new();
        // Another crate's names first: they are fixed.
        for &(machine, n) in &fallible {
            if let Some(name) = foreign.error_enums.get(&(machine, n.to_owned())) {
                taken.insert(name.clone());
                error_enums.insert((machine, n), ident(name));
            }
        }
        for (machine, n) in fallible {
            if error_enums.contains_key(&(machine, n)) {
                continue;
            }
            let mut name = short(n);
            if count[&name] > 1 || taken.contains(&name) {
                let kind = if machine { "Machine" } else { "Task" };
                name = format!("{}{kind}Error", camel(n));
            }
            if !taken.insert(name.clone()) {
                return Err(format!(
                    "the error enum of `{n}` would be named `{name}`, which is already taken"
                ));
            }
            error_enums.insert((machine, n), ident(&name));
        }
        Ok(Gen {
            ir,
            error_enums,
            foreign: &foreign.krate,
            program: program_static(""),
            contains: containment(ir)?,
        })
    }

    // ---- types --------------------------------------------------------------------------

    fn decl(&self, name: &str) -> Option<&'a TypeDef> {
        self.ir.types.get(name).map(|d| &d.def)
    }

    /// The owned Rust type. Inside a machine's module (`sup`), chart types are `super::Name`.
    fn ty(&self, t: &TypeRef, sup: bool) -> TokenStream {
        self.ty_in(t, sup, None)
    }

    /// The owned Rust type of a field of type `owner`, or of alias `owner`'s target: a `T?`
    /// where `T` contains `owner` in place is `Option<Box<T>>`, which closes the cycle with
    /// a pointer, as Rust requires. A list needs no `Box`: a `Vec` is one.
    fn ty_in(&self, t: &TypeRef, sup: bool, owner: Option<&str>) -> TokenStream {
        match t {
            TypeRef::Builtin(b) => match b {
                Builtin::String => quote!(::std::string::String),
                Builtin::Int => quote!(i64),
                Builtin::Float => quote!(f64),
                Builtin::Bool => quote!(bool),
                Builtin::Duration => quote!(::core::time::Duration),
                Builtin::Json => quote!(::roadbook::__private::serde_json::Value),
            },
            TypeRef::Named(n) => {
                let id = ident(n);
                if sup { quote!(super::#id) } else { quote!(#id) }
            }
            TypeRef::List(e) => {
                let e = self.ty(e, sup);
                quote!(::std::vec::Vec<#e>)
            }
            TypeRef::Optional(e) => {
                let cycle = match (&**e, owner) {
                    (TypeRef::Named(n), Some(owner)) => {
                        n == owner
                            || self
                                .contains
                                .get(n.as_str())
                                .is_some_and(|c| c.contains(owner))
                    }
                    _ => false,
                };
                let e = self.ty_in(e, sup, owner);
                if cycle {
                    quote!(::core::option::Option<::std::boxed::Box<#e>>)
                } else {
                    quote!(::core::option::Option<#e>)
                }
            }
            TypeRef::Ref(m) => {
                let module = ident(&snake(m));
                if sup {
                    quote!(super::#module::Ref)
                } else {
                    quote!(#module::Ref)
                }
            }
        }
    }

    /// Whether the type is built from `int`, `float` or `duration` with `[]` and `?` (through
    /// aliases), so its JSON form goes through `roadbook::__private::wire`: integral numbers
    /// in any form for an `int`, milliseconds for a `duration`, no non-finite `float`.
    fn is_wire(&self, t: &TypeRef) -> bool {
        self.is_wire_at(t, 0)
    }

    fn is_wire_at(&self, t: &TypeRef, depth: usize) -> bool {
        match t {
            TypeRef::Builtin(b) => matches!(b, Builtin::Int | Builtin::Float | Builtin::Duration),
            TypeRef::List(e) | TypeRef::Optional(e) => self.is_wire_at(e, depth),
            TypeRef::Named(n) => match self.decl(n) {
                Some(TypeDef::Alias { ty }) if depth < 64 => self.is_wire_at(ty, depth + 1),
                _ => false,
            },
            TypeRef::Ref(_) => false,
        }
    }

    /// Whether the type is optional at the top, through aliases.
    fn is_optional(&self, t: &TypeRef, depth: usize) -> bool {
        match t {
            TypeRef::Optional(_) => true,
            TypeRef::Named(n) => match self.decl(n) {
                Some(TypeDef::Alias { ty }) if depth < 64 => self.is_optional(ty, depth + 1),
                _ => false,
            },
            _ => false,
        }
    }

    /// Whether a parameter of this type is passed by value: numbers, `bool`, `duration`,
    /// enums, and aliases and optionals of those.
    fn by_value(&self, t: &TypeRef, depth: usize) -> bool {
        match t {
            TypeRef::Builtin(b) => !matches!(b, Builtin::String | Builtin::Json),
            TypeRef::List(_) => false,
            TypeRef::Optional(e) => self.by_value(e, depth),
            TypeRef::Named(n) => match self.decl(n) {
                Some(TypeDef::Enum { .. }) => true,
                Some(TypeDef::Alias { ty }) if depth < 64 => self.by_value(ty, depth + 1),
                _ => false,
            },
            TypeRef::Ref(_) => false,
        }
    }

    /// The parameter type of a `Handlers` method, and how the owned value is passed to it.
    fn param(&self, t: &TypeRef) -> (TokenStream, Pass) {
        if self.by_value(t, 0) {
            return (self.ty(t, true), Pass::Value);
        }
        match t {
            TypeRef::Builtin(Builtin::String) => (quote!(&str), Pass::Ref),
            TypeRef::List(e) => {
                let e = self.ty(e, true);
                (quote!(&[#e]), Pass::Ref)
            }
            TypeRef::Optional(e) => {
                let (p, _) = self.param(e);
                let pass = match **e {
                    TypeRef::Builtin(Builtin::String) | TypeRef::List(_) => Pass::AsDeref,
                    _ => Pass::AsRef,
                };
                (quote!(::core::option::Option<#p>), pass)
            }
            _ => {
                let ty = self.ty(t, true);
                (quote!(&#ty), Pass::Ref)
            }
        }
    }

    /// A `Result<T, Defect>` decoding the JSON value `v` as `t`.
    fn decode(&self, t: &TypeRef, v: TokenStream, what: &str) -> TokenStream {
        let ty = self.ty(t, true);
        if self.is_wire(t) {
            quote!(::roadbook::__private::decode_wire::<#ty>(#v, #what))
        } else {
            quote!(::roadbook::__private::decode::<#ty>(#v, #what))
        }
    }

    /// A `Result<T, Defect>` taking argument `name` out of the object `__args`.
    fn decode_arg(&self, t: &TypeRef, name: &str) -> TokenStream {
        let ty = self.ty(t, true);
        if self.is_wire(t) {
            quote!(::roadbook::__private::arg_wire::<#ty>(&mut __args, #name))
        } else {
            quote!(::roadbook::__private::arg::<#ty>(&mut __args, #name))
        }
    }

    /// A `Result<Value, Defect>` encoding `x`, an owned `t`.
    fn encode(&self, t: &TypeRef, x: TokenStream) -> TokenStream {
        if self.is_wire(t) {
            quote!(::roadbook::__private::encode_wire(&#x))
        } else {
            quote!(::roadbook::__private::encode(&#x))
        }
    }

    /// A `t` decoded from `v`, a value the core produced. One that does not decode raises
    /// a host defect.
    fn expect(&self, t: &TypeRef, v: TokenStream) -> TokenStream {
        let ty = self.ty(t, true);
        if self.is_wire(t) {
            quote!(::roadbook::__private::expect_wire::<#ty>(#v))
        } else {
            quote!(::roadbook::__private::expect::<#ty>(#v))
        }
    }

    /// Struct fields (record, error) or struct-variant fields (event, with no `pub`).
    fn fields(&self, owner: &str, fields: &[Field], public: bool) -> Result<TokenStream, String> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for f in fields {
            let id = ident(&snake(&f.name));
            if !seen.insert(plain(&id)) {
                return Err(format!(
                    "two fields of `{owner}` would both be the Rust field `{id}`"
                ));
            }
            let doc = doc(&f.doc);
            let rename = rename(&f.name, &id);
            let ms = if self.is_wire(&f.ty) {
                // `with` loses serde's "a missing `Option` is `None`".
                let default = if self.is_optional(&f.ty, 0) {
                    quote!(#[serde(default)])
                } else {
                    TokenStream::new()
                };
                quote!(#[serde(with = "::roadbook::__private::wire")] #default)
            } else {
                TokenStream::new()
            };
            // Only a type's fields can close a cycle: an event is no type.
            let ty = self.ty_in(&f.ty, !public, public.then_some(owner));
            let vis = if public {
                quote!(pub)
            } else {
                TokenStream::new()
            };
            out.push(quote!(#doc #rename #ms #vis #id: #ty));
        }
        Ok(quote!(#(#out),*))
    }

    /// The item for one entry of `types`.
    fn type_decl(&self, name: &str, def: &TypeDef, text: &str) -> Result<TokenStream, String> {
        let id = ident(name);
        let doc = doc(text);
        let derives = serde_derives();
        let tokens = match def {
            TypeDef::Record { fields } => {
                let fields = self.fields(name, fields, true)?;
                let rename = rename(name, &id);
                quote! {
                    #doc
                    #[derive(::core::fmt::Debug, ::core::clone::Clone, ::core::cmp::PartialEq, #derives)]
                    #[serde(crate = "::roadbook::__private::serde")]
                    #rename
                    #[allow(dead_code, missing_docs)]
                    pub struct #id { #fields }
                }
            }
            TypeDef::Enum { variants } => {
                let variants = variants.iter().map(|v| {
                    let vid = ident(v);
                    let rename = rename(v, &vid);
                    quote!(#rename #vid)
                });
                let rename = rename(name, &id);
                quote! {
                    #doc
                    #[derive(
                        ::core::fmt::Debug, ::core::clone::Clone, ::core::marker::Copy,
                        ::core::cmp::PartialEq, ::core::cmp::Eq, ::core::hash::Hash, #derives
                    )]
                    #[serde(crate = "::roadbook::__private::serde")]
                    #rename
                    #[allow(dead_code, missing_docs)]
                    pub enum #id { #(#variants),* }
                }
            }
            TypeDef::Error { fields } if fields.is_empty() => quote! {
                #doc
                #[derive(
                    ::core::fmt::Debug, ::core::clone::Clone, ::core::marker::Copy,
                    ::core::cmp::PartialEq, ::core::cmp::Eq, ::core::hash::Hash,
                    ::core::default::Default
                )]
                #[allow(dead_code, missing_docs)]
                pub struct #id;

                // `{"_tag": "Name"}`: serde has no tagged unit struct, but it has tagged
                // unit variants.
                impl ::roadbook::__private::serde::Serialize for #id {
                    fn serialize<S: ::roadbook::__private::serde::Serializer>(
                        &self,
                        serializer: S,
                    ) -> ::core::result::Result<S::Ok, S::Error> {
                        #[derive(::roadbook::__private::serde::Serialize)]
                        #[serde(crate = "::roadbook::__private::serde", tag = "_tag")]
                        enum Tagged { #[serde(rename = #name)] Tag }
                        ::roadbook::__private::serde::Serialize::serialize(&Tagged::Tag, serializer)
                    }
                }

                impl<'de> ::roadbook::__private::serde::Deserialize<'de> for #id {
                    fn deserialize<D: ::roadbook::__private::serde::Deserializer<'de>>(
                        deserializer: D,
                    ) -> ::core::result::Result<Self, D::Error> {
                        #[derive(::roadbook::__private::serde::Deserialize)]
                        #[serde(crate = "::roadbook::__private::serde", tag = "_tag")]
                        enum Tagged { #[serde(rename = #name)] Tag }
                        let Tagged::Tag = <Tagged as ::roadbook::__private::serde::Deserialize>::deserialize(deserializer)?;
                        ::core::result::Result::Ok(#id)
                    }
                }

                impl ::core::fmt::Display for #id {
                    fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                        f.write_str(#name)
                    }
                }

                impl ::std::error::Error for #id {}
            },
            TypeDef::Error { fields: decl } => {
                let fields = self.fields(name, decl, true)?;
                let ids: Vec<Ident> = decl.iter().map(|f| ident(&snake(&f.name))).collect();
                let expected = format!("the error `{name}`");
                quote! {
                    #doc
                    #[derive(
                        ::core::fmt::Debug, ::core::clone::Clone, ::core::cmp::PartialEq,
                        ::roadbook::__private::serde::Serialize
                    )]
                    #[serde(crate = "::roadbook::__private::serde", tag = "_tag", rename = #name)]
                    #[allow(dead_code, missing_docs)]
                    pub struct #id { #fields }

                    // serde ignores the tag of a tagged struct when it deserializes one, so
                    // check `"_tag"` first, as for a bare error, then read the fields.
                    impl<'de> ::roadbook::__private::serde::Deserialize<'de> for #id {
                        fn deserialize<__D: ::roadbook::__private::serde::Deserializer<'de>>(
                            deserializer: __D,
                        ) -> ::core::result::Result<Self, __D::Error> {
                            use ::roadbook::__private::serde::de::Error as _;
                            #[derive(::roadbook::__private::serde::Deserialize)]
                            #[serde(crate = "::roadbook::__private::serde")]
                            #[allow(dead_code, missing_docs)]
                            struct __Fields { #fields }
                            let value = <::roadbook::__private::serde_json::Value as ::roadbook::__private::serde::Deserialize>::deserialize(deserializer)?;
                            if value.get("_tag").and_then(::roadbook::__private::serde_json::Value::as_str) != ::core::option::Option::Some(#name) {
                                return ::core::result::Result::Err(__D::Error::custom(::std::format!(
                                    "{} is not {}", value, #expected
                                )));
                            }
                            let f: __Fields = ::roadbook::__private::serde_json::from_value(value).map_err(__D::Error::custom)?;
                            ::core::result::Result::Ok(#id { #( #ids: f.#ids ),* })
                        }
                    }

                    impl ::core::fmt::Display for #id {
                        fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                            ::core::fmt::Debug::fmt(self, f)
                        }
                    }

                    impl ::std::error::Error for #id {}
                }
            }
            TypeDef::Alias { ty } => {
                let ty = self.ty_in(ty, false, Some(name));
                quote! {
                    #doc
                    #[allow(dead_code, missing_docs)]
                    pub type #id = #ty;
                }
            }
            // The user defines it next to the call.
            TypeDef::Extern => TokenStream::new(),
        };
        Ok(tokens)
    }

    /// The error enum of every fallible task and machine.
    fn error_enums(&self) -> Vec<TokenStream> {
        let tasks = self
            .ir
            .tasks
            .iter()
            .map(|(n, t)| (false, n, &t.errors, "task"));
        let machines = self
            .ir
            .machines
            .iter()
            .map(|(n, m)| (true, n, &m.errors, "machine"));
        tasks
            .chain(machines)
            .filter_map(|(machine, n, errors, what)| {
                let id = self.error_enums.get(&(machine, n.as_str()))?;
                Some(match self.foreign.get(n) {
                    Some(krate) => quote!(pub use #krate::#id;),
                    None => self.error_enum(id, &format!("{what} `{n}`"), errors),
                })
            })
            .collect()
    }

    fn error_enum(&self, id: &Ident, what: &str, errors: &[String]) -> TokenStream {
        let text =
            format!("The errors {what} declares. Its JSON form is the error's, with its `_tag`.");
        let variants: Vec<Ident> = errors.iter().map(|e| ident(e)).collect();
        let tags = errors.iter().map(|e| Literal::string(e));
        let froms = variants.iter().map(|v| {
            quote! {
                impl ::core::convert::From<#v> for #id {
                    fn from(e: #v) -> Self {
                        Self::#v(e)
                    }
                }
            }
        });
        let expected = format!("an error of {what}");
        quote! {
            #[doc = #text]
            #[derive(::core::fmt::Debug, ::core::clone::Clone, ::core::cmp::PartialEq, ::roadbook::__private::serde::Serialize)]
            #[serde(crate = "::roadbook::__private::serde", untagged)]
            #[allow(dead_code, missing_docs)]
            pub enum #id { #( #variants(#variants) ),* }

            impl<'de> ::roadbook::__private::serde::Deserialize<'de> for #id {
                fn deserialize<D: ::roadbook::__private::serde::Deserializer<'de>>(
                    deserializer: D,
                ) -> ::core::result::Result<Self, D::Error> {
                    use ::roadbook::__private::serde::de::Error as _;
                    let value = <::roadbook::__private::serde_json::Value as ::roadbook::__private::serde::Deserialize>::deserialize(deserializer)?;
                    let tag = value
                        .get("_tag")
                        .and_then(::roadbook::__private::serde_json::Value::as_str)
                        .map(::std::borrow::ToOwned::to_owned);
                    match tag.as_deref() {
                        #(
                            ::core::option::Option::Some(#tags) => ::roadbook::__private::serde_json::from_value(value)
                                .map(Self::#variants)
                                .map_err(D::Error::custom),
                        )*
                        _ => ::core::result::Result::Err(D::Error::custom(::std::format!(
                            "{} is not {}", value, #expected
                        ))),
                    }
                }
            }

            impl ::core::fmt::Display for #id {
                fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                    match self {
                        #( Self::#variants(e) => ::core::fmt::Display::fmt(e, f), )*
                    }
                }
            }

            impl ::std::error::Error for #id {}

            #(#froms)*
        }
    }

    // ---- machines -----------------------------------------------------------------------

    fn machine(&self, name: &'a str, m: &'a Machine) -> Result<TokenStream, String> {
        let module = ident(&snake(name));
        let methods = self.methods(name, m)?;
        let handlers = self.handlers(name, &methods);
        let bind = self.bind(&methods);
        let (states, from_state) = self.states(name, m)?;
        let events = self.events(name, m)?;
        let runners = self.runners(name, m);
        let module_doc = doc(&m.doc);
        let hash = &m.hash;
        let hash_doc = format!(
            "The hash of machine `{name}` in the chart's IR: the `ir` a saved snapshot of it \
             records. A snapshot with another hash is restored only through a migration \
             (`roadbook::Config::migrate`), which sets this one."
        );
        Ok(quote! {
            #module_doc
            #[allow(dead_code, unused_imports, missing_docs, non_camel_case_types, non_snake_case)]
            pub mod #module {
                use super::*;

                #[doc = #hash_doc]
                pub const HASH: &str = #hash;

                #handlers
                #states
                #from_state
                #events
                #bind
                #runners
            }
        })
    }

    /// The `Handlers` methods of machine `name`: what it uses directly, in declaration order.
    fn methods(&self, name: &'a str, m: &'a Machine) -> Result<Vec<Method<'a>>, String> {
        let mut uses = Uses::default();
        for v in &m.vars {
            if let Some(e) = &v.init {
                uses.expr(e);
            }
        }
        uses.state(&m.root);
        let ir = self.ir;
        let mut methods = Vec::new();
        for (n, f) in ir
            .functions
            .iter()
            .filter(|(n, _)| uses.functions.contains(*n))
        {
            methods.push(Method {
                name: n,
                kind: Kind::Function,
                ident: ident(&snake(n)),
                params: &f.params,
                returns: Some(&f.returns),
                error: None,
                doc: &f.doc,
            });
        }
        for (n, a) in ir.actions.iter().filter(|(n, _)| uses.actions.contains(*n)) {
            methods.push(Method {
                name: n,
                kind: Kind::Action,
                ident: ident(&snake(n)),
                params: &a.params,
                returns: None,
                error: None,
                doc: &a.doc,
            });
        }
        for (n, t) in ir.tasks.iter().filter(|(n, _)| uses.invokes.contains(*n)) {
            methods.push(Method {
                name: n,
                kind: Kind::Task,
                ident: ident(&snake(n)),
                params: &t.params,
                returns: t.returns.as_ref(),
                error: self.error_enums.get(&(false, n.as_str())).cloned(),
                doc: &t.doc,
            });
        }
        for (n, c) in ir
            .machines
            .iter()
            .filter(|(n, _)| uses.invokes.contains(*n))
        {
            methods.push(Method {
                name: n,
                kind: Kind::Machine,
                ident: ident(&snake(n)),
                params: &c.params,
                returns: c.returns.as_ref(),
                error: self.error_enums.get(&(true, n.as_str())).cloned(),
                doc: &c.doc,
            });
        }
        let mut seen: HashMap<String, &str> = HashMap::new();
        for method in &methods {
            if let Some(other) = seen.insert(plain(&method.ident), method.name) {
                return Err(format!(
                    "`{other}` and `{}`, both used by machine `{name}`, would both be the \
                     `Handlers` method `{}`",
                    method.name, method.ident
                ));
            }
            let mut params = HashSet::new();
            for p in method.params {
                if !params.insert(plain(&ident(&snake(&p.name)))) {
                    return Err(format!(
                        "two parameters of `{}` would both be the Rust parameter `{}`",
                        method.name,
                        snake(&p.name)
                    ));
                }
            }
        }
        Ok(methods)
    }

    /// What a task or machine method's future gives: `T`, `()`, or `Result<T, E>`.
    fn invoke_output(&self, returns: Option<&TypeRef>, error: Option<&Ident>) -> TokenStream {
        let t = returns.map_or_else(|| quote!(()), |t| self.ty(t, true));
        match error {
            Some(e) => quote!(::core::result::Result<#t, super::#e>),
            None => t,
        }
    }

    fn handlers(&self, name: &str, methods: &[Method]) -> TokenStream {
        let sigs = methods.iter().map(|m| {
            let id = &m.ident;
            let params = m.params.iter().map(|p| {
                let pid = ident(&snake(&p.name));
                let (ty, _) = self.param(&p.ty);
                quote!(#pid: #ty)
            });
            let kind = match m.kind {
                Kind::Function => "Function",
                Kind::Action => "Action",
                Kind::Task => "Task",
                Kind::Machine => "Machine",
            };
            let mut text = format!("{kind} `{}`.", m.name);
            if m.kind == Kind::Machine {
                text.push_str(&format!(
                    " Usually `{}::run(self, …)`, which then runs as this run's child.",
                    snake(m.name)
                ));
            }
            if !m.doc.is_empty() {
                text = format!("{}\n\n{text}", m.doc);
            }
            let ret = match m.kind {
                Kind::Function => {
                    let t = self.ty(m.returns.expect("a function returns a value"), true);
                    quote!(-> #t)
                }
                Kind::Action => quote! {
                    -> impl ::core::future::Future<Output = ()> + ::core::marker::Send
                },
                Kind::Task | Kind::Machine => {
                    let out = self.invoke_output(m.returns, m.error.as_ref());
                    quote! {
                        -> impl ::core::future::Future<Output = #out> + ::core::marker::Send
                    }
                }
            };
            quote! {
                #[doc = #text]
                fn #id(&self, #(#params),*) #ret;
            }
        });
        let text = format!(
            "What machine `{name}` needs from host code: one method per function, action, task \
             and child machine it uses. Functions are plain `fn`s; actions, tasks and child \
             machines can be implemented as `async fn`s, whose futures must be `Send`."
        );
        quote! {
            #[doc = #text]
            pub trait Handlers: ::core::marker::Send + ::core::marker::Sync {
                #(#sigs)*
            }
        }
    }

    /// The locals decoding a method's arguments, and the expressions passing them.
    fn args(
        &self,
        m: &Method,
        decode: impl Fn(&TypeRef, &str) -> TokenStream,
        bail: &TokenStream,
    ) -> (Vec<TokenStream>, Vec<TokenStream>) {
        let mut lets = Vec::new();
        let mut pass = Vec::new();
        for (i, p) in m.params.iter().enumerate() {
            let a = format_ident!("__a{}", i);
            let ty = self.ty(&p.ty, true);
            let value = decode(&p.ty, &p.name);
            lets.push(quote! {
                let #a: #ty = match #value {
                    ::core::result::Result::Ok(v) => v,
                    ::core::result::Result::Err(e) => #bail,
                };
            });
            pass.push(match self.param(&p.ty).1 {
                Pass::Value => quote!(#a),
                Pass::Ref => quote!(&#a),
                Pass::AsRef => quote!(#a.as_ref()),
                Pass::AsDeref => quote!(#a.as_deref()),
            });
        }
        (lets, pass)
    }

    /// The adapter from `Handlers` to the driver's `Host`.
    fn bind(&self, methods: &[Method]) -> TokenStream {
        let functions = methods
            .iter()
            .filter(|m| m.kind == Kind::Function)
            .map(|m| {
                let name = m.name;
                let id = &m.ident;
                let (lets, pass) = self.args(
                    m,
                    |t, p| {
                        let v = quote! {
                            __args.next().unwrap_or(::roadbook::__private::serde_json::Value::Null)
                        };
                        self.decode(t, v, &format!("argument `{p}`"))
                    },
                    &quote!(return ::core::result::Result::Err(e)),
                );
                let encode =
                    self.encode(m.returns.expect("a function returns a value"), quote!(__r));
                quote! {
                    #name => {
                        #(#lets)*
                        let __r = <H as Handlers>::#id(&*self.0, #(#pass),*);
                        #encode
                    }
                }
            });
        let actions = methods.iter().filter(|m| m.kind == Kind::Action).map(|m| {
            let name = m.name;
            let id = &m.ident;
            let (lets, pass) = self.args(
                m,
                |t, p| self.decode_arg(t, p),
                &quote!(return ::roadbook::__private::ready(::core::result::Result::Err(e))),
            );
            quote! {
                #name => {
                    #(#lets)*
                    ::std::boxed::Box::pin(async move {
                        <H as Handlers>::#id(&*self.0, #(#pass),*).await;
                        ::core::result::Result::Ok(())
                    })
                }
            }
        });
        let invokes = methods
            .iter()
            .filter(|m| matches!(m.kind, Kind::Task | Kind::Machine))
            .map(|m| {
                let name = m.name;
                let id = &m.ident;
                let (lets, pass) = self.args(
                    m,
                    |t, p| self.decode_arg(t, p),
                    &quote! {
                        return ::roadbook::__private::ready(::roadbook::__private::Outcome::Defect(e))
                    },
                );
                let call = quote!(<H as Handlers>::#id(&*self.0, #(#pass),*).await);
                let null = quote!(::roadbook::__private::serde_json::Value::Null);
                let ok = quote!(::core::result::Result::Ok);
                let err = quote!(::core::result::Result::Err);
                let body = match (m.returns, &m.error) {
                    (None, None) => quote! {
                        #call;
                        ::roadbook::__private::outcome(#ok(#ok(#null)))
                    },
                    (Some(t), None) => {
                        let enc = self.encode(t, quote!(__r));
                        quote! {
                            let __r = #call;
                            ::roadbook::__private::outcome((#enc).map(#ok))
                        }
                    }
                    (None, Some(_)) => quote! {
                        let __r = #call;
                        ::roadbook::__private::outcome(match __r {
                            #ok(()) => #ok(#ok(#null)),
                            #err(__e) => ::roadbook::__private::encode(&__e).map(#err),
                        })
                    },
                    (Some(t), Some(_)) => {
                        let enc = self.encode(t, quote!(__v));
                        quote! {
                            let __r = #call;
                            ::roadbook::__private::outcome(match __r {
                                #ok(__v) => (#enc).map(#ok),
                                #err(__e) => ::roadbook::__private::encode(&__e).map(#err),
                            })
                        }
                    }
                };
                quote! {
                    #name => {
                        #(#lets)*
                        ::std::boxed::Box::pin(async move { #body })
                    }
                }
            });
        quote! {
            /// Adapts `Handlers` to the driver's erased `Host`. Every call is fully qualified,
            /// so one type can implement the `Handlers` of several charts.
            struct Bind<P>(P);

            #[allow(clippy::all, unused_mut, unused_variables, unreachable_code)]
            impl<H, P> ::roadbook::__private::Host for Bind<P>
            where
                H: Handlers + ?::core::marker::Sized,
                P: ::core::ops::Deref<Target = H> + ::core::marker::Send + ::core::marker::Sync,
            {
                fn function(
                    &self,
                    name: &str,
                    args: ::std::vec::Vec<::roadbook::__private::serde_json::Value>,
                ) -> ::core::result::Result<::roadbook::__private::serde_json::Value, ::roadbook::__private::Defect> {
                    let mut __args = args.into_iter();
                    match name {
                        #(#functions)*
                        _ => ::core::result::Result::Err(::roadbook::__private::unknown("function", name)),
                    }
                }

                fn action<'a>(
                    &'a self,
                    name: &str,
                    args: ::roadbook::__private::Object,
                ) -> ::roadbook::__private::BoxFuture<'a, ::core::result::Result<(), ::roadbook::__private::Defect>> {
                    let mut __args = args;
                    match name {
                        #(#actions)*
                        _ => ::roadbook::__private::ready(::core::result::Result::Err(
                            ::roadbook::__private::unknown("action", name),
                        )),
                    }
                }

                fn invoke<'a>(
                    &'a self,
                    call: ::roadbook::__private::Invocation,
                ) -> ::roadbook::__private::BoxFuture<'a, ::roadbook::__private::Outcome> {
                    let mut __args = call.args;
                    match call.src.as_str() {
                        #(#invokes)*
                        _ => ::roadbook::__private::ready(::roadbook::__private::Outcome::Defect(
                            ::roadbook::__private::unknown("task or machine", &call.src),
                        )),
                    }
                }
            }
        }
    }

    /// `State` with its nested enums, and `__from_state_id`.
    fn states(&self, machine: &str, m: &Machine) -> Result<(TokenStream, TokenStream), String> {
        let mut taken: HashSet<String> = ["State", "Event", "Emitted", "Ref", "Handlers", "Bind"]
            .into_iter()
            .map(str::to_owned)
            .chain(self.ir.types.keys().cloned())
            .chain(self.error_enums.values().map(plain))
            .collect();
        let mut enums = Vec::new();
        let mut leaves = Vec::new();
        let state = format_ident!("State");
        let text = format!(
            "The state a `{machine}` run is in: its innermost active state, or the final state \
             it ended in. A compound state's variant carries its active child. A parallel \
             state's carries nothing: a run inside one is in all its regions at once, and is \
             in the outermost such state."
        );
        self.state_enum(
            &m.root,
            &state,
            &text,
            &|inner| inner,
            &mut taken,
            &mut enums,
            &mut leaves,
        )?;
        let arms = leaves
            .iter()
            .map(|(id, ctor)| quote!(#id => ::core::option::Option::Some(#ctor),));
        let from = quote! {
            /// The `State` of a leaf state's id.
            fn __from_state_id(id: &str) -> ::core::option::Option<State> {
                match id {
                    #(#arms)*
                    _ => ::core::option::Option::None,
                }
            }
        };
        Ok((quote!(#(#enums)*), from))
    }

    #[allow(clippy::too_many_arguments)]
    fn state_enum(
        &self,
        s: &State,
        name: &Ident,
        text: &str,
        wrap: &dyn Fn(TokenStream) -> TokenStream,
        taken: &mut HashSet<String>,
        enums: &mut Vec<TokenStream>,
        leaves: &mut Vec<(String, TokenStream)>,
    ) -> Result<(), String> {
        let mut variants = Vec::new();
        let mut nested = Vec::new();
        // A history state is never active, so it is never the state a run is in.
        for c in s.states.iter().filter(|c| !c.kind.is_history()) {
            let vid = ident(&c.name);
            let vdoc = doc(&c.doc);
            if c.states.is_empty() || c.kind == StateKind::Parallel {
                let ctor = wrap(quote!(#name::#vid));
                leaves.push((c.id.clone(), ctor));
                variants.push(quote!(#vdoc #vid));
            } else {
                let segments: Vec<&str> = c.id.split('.').collect();
                let child = (1..=segments.len())
                    .map(|n| segments[segments.len() - n..].concat())
                    .find(|cand| !taken.contains(cand))
                    .ok_or_else(|| {
                        format!("no name is left for the Rust enum of state `{}`", c.id)
                    })?;
                taken.insert(child.clone());
                let child = ident(&child);
                variants.push(quote!(#vdoc #vid(#child)));
                nested.push((c, vid, child));
            }
        }
        enums.push(quote! {
            #[doc = #text]
            #[derive(
                ::core::fmt::Debug, ::core::clone::Clone, ::core::marker::Copy,
                ::core::cmp::PartialEq, ::core::cmp::Eq, ::core::hash::Hash
            )]
            pub enum #name { #(#variants),* }
        });
        for (c, vid, child) in nested {
            let text = if c.doc.is_empty() {
                format!("The active child of state `{}`.", c.name)
            } else {
                format!("The active child of state `{}`.\n\n{}", c.name, c.doc)
            };
            let wrap_child = |inner: TokenStream| wrap(quote!(#name::#vid(#inner)));
            self.state_enum(c, &child, &text, &wrap_child, taken, enums, leaves)?;
        }
        Ok(())
    }

    /// A tagged enum of `events`, `Event` or `Emitted`.
    fn event_enum(
        &self,
        id: &Ident,
        text: &str,
        events: &indexmap::IndexMap<String, roadbook_ir::EventDecl>,
    ) -> Result<TokenStream, String> {
        let mut variants = Vec::new();
        for (name, e) in events {
            let vid = ident(name);
            let rename = rename(name, &vid);
            let doc = doc(&e.doc);
            if e.fields.is_empty() {
                variants.push(quote!(#doc #rename #vid));
            } else {
                let fields = self.fields(name, &e.fields, false)?;
                variants.push(quote!(#doc #rename #vid { #fields }));
            }
        }
        let derives = serde_derives();
        Ok(quote! {
            #[doc = #text]
            #[derive(::core::fmt::Debug, ::core::clone::Clone, ::core::cmp::PartialEq, #derives)]
            #[serde(crate = "::roadbook::__private::serde", tag = "_tag")]
            pub enum #id { #(#variants),* }
        })
    }

    /// `Event`, `Emitted` and `Ref`.
    fn events(&self, machine: &str, m: &Machine) -> Result<TokenStream, String> {
        let text = if m.events.is_empty() {
            format!("Machine `{machine}` accepts no events.")
        } else {
            format!(
                "The events machine `{machine}` accepts, from host code (`Actor::send`, \
                 `Ref::send`) and from other machines (`send`). In JSON an event is an object \
                 with `\"_tag\": \"Name\"` and its fields."
            )
        };
        let event = self.event_enum(&format_ident!("Event"), &text, &m.events)?;
        let text = if m.emits.is_empty() {
            format!("Machine `{machine}` emits no events.")
        } else {
            format!(
                "The events machine `{machine}` emits (`emits`), which its invoker (if it \
                 named it with `as`) and every machine holding a `ref` to it receive. In JSON \
                 an event is an object with `\"_tag\": \"Name\"` and its fields."
            )
        };
        let emitted = self.event_enum(&format_ident!("Emitted"), &text, &m.emits)?;
        let text = format!(
            "A handle to a running `{machine}`: the chart's `ref {machine}`. Get one from a \
             spawned run's `Actor::handle`, pass it as a machine argument, or send it an \
             `Event`."
        );
        Ok(quote! {
            #event
            #emitted
            #[doc = #text]
            pub type Ref = ::roadbook::Ref<Event>;
        })
    }

    /// `run`, `run_with`, `spawn`, `spawn_with` and their helpers.
    fn runners(&self, name: &'a str, m: &Machine) -> TokenStream {
        let params: Vec<(Ident, TokenStream, &Field)> = m
            .params
            .iter()
            .map(|p| {
                let mut s = snake(&p.name);
                if s == "handlers" || s == "config" {
                    s.push('_');
                }
                (ident(&s), self.ty(&p.ty, true), p)
            })
            .collect();
        let decl: Vec<TokenStream> = params.iter().map(|(id, ty, _)| quote!(#id: #ty)).collect();
        let names: Vec<&Ident> = params.iter().map(|(id, _, _)| id).collect();
        let input = if params.is_empty() {
            quote!(::core::result::Result::Ok(
                ::roadbook::__private::Object::new()
            ))
        } else {
            let inserts = params.iter().map(|(id, _, p)| {
                let key = &p.name;
                let enc = self.encode(&p.ty, quote!(#id));
                quote! {
                    __input.insert(::std::string::String::from(#key), (#enc)?);
                }
            });
            quote! {
                let mut __input = ::roadbook::__private::Object::new();
                #(#inserts)*
                ::core::result::Result::Ok(__input)
            }
        };

        let output = m
            .returns
            .as_ref()
            .map_or_else(|| quote!(()), |t| self.ty(t, true));
        let output_fn = match &m.returns {
            Some(t) => {
                let decode = self.expect(t, quote!(v));
                quote! {
                    fn __output(v: ::roadbook::__private::serde_json::Value) -> #output {
                        #decode
                    }
                }
            }
            None => quote!(
                fn __output(_: ::roadbook::__private::serde_json::Value) {}
            ),
        };
        let error_enum = self.error_enums.get(&(true, name));
        let (error, decode_error) = match error_enum {
            Some(e) => (
                quote!(super::#e),
                quote!(::roadbook::__private::expect::<super::#e>(v)),
            ),
            None => (
                quote!(::core::convert::Infallible),
                quote! {
                    ::core::unreachable!("machine `{}` declares no errors, but failed with {}", #name, v)
                },
            ),
        };
        let (ret, finish) = match error_enum {
            Some(_) => (
                quote!(-> ::core::result::Result<#output, #error>),
                quote! {
                    match __end {
                        ::core::result::Result::Ok(v) => ::core::result::Result::Ok(__output(v)),
                        ::core::result::Result::Err(e) => ::core::result::Result::Err(__error(e)),
                    }
                },
            ),
            None => (
                if m.returns.is_some() {
                    quote!(-> #output)
                } else {
                    TokenStream::new()
                },
                quote! {
                    match __end {
                        ::core::result::Result::Ok(v) => __output(v),
                        ::core::result::Result::Err(e) => match __error(e) {},
                    }
                },
            ),
        };

        let returns_text = match (&m.returns, error_enum) {
            (Some(t), Some(e)) => format!(
                "Returns its output (`{t}`), or `Err` with the error it failed with \
                 (`{}`).",
                plain(e)
            ),
            (None, Some(e)) => format!(
                "Returns `Ok(())` when it ends, or `Err` with the error it failed with (`{}`).",
                plain(e)
            ),
            (Some(t), None) => {
                format!("`{name}` declares no errors, so this returns its output (`{t}`) directly.")
            }
            (None, None) => format!("`{name}` declares no errors and returns nothing."),
        };
        let machine_doc = if m.doc.is_empty() {
            String::new()
        } else {
            format!("\n\n{}", m.doc)
        };
        let run_doc = format!(
            "Runs machine `{name}` to its end and gives how it ended.{machine_doc}\n\n\
             {returns_text}\n\n\
             Called from a parent chart's `Handlers` method for this machine, it runs as the \
             parent's child run; else it is a root run with `roadbook::Config::default()`. \
             The future borrows `handlers` and needs no particular executor, except that \
             `after` timers use Tokio's by default. A run that ends with a defect resumes \
             unwinding with a `roadbook::Defect` payload. Dropping the future stops the run at \
             once."
        );
        let run_with_doc = format!(
            "Like `run`, but always a root run, set up by `config` (trace sink, timer, run id).\n\n\
             {returns_text}"
        );
        let spawn_doc = format!(
            "Starts machine `{name}` on a new Tokio task with `roadbook::Config::default()`, \
             and gives its actor: send it events, watch its state, wait for its result or \
             cancel it."
        );
        let spawn_with_doc = "Like `spawn`, set up by `config`.";
        let restore_doc = format!(
            "Restores a saved snapshot of `{name}` as a new run on a new Tokio task, with \
             `roadbook::Config::default()`, and gives its actor, as `spawn` does. The run \
             starts from the snapshot: the invokes that were running start again, and the \
             timers keep their deadlines (one already due expires first). A snapshot that \
             does not fit is refused before anything starts: `RestoreError::reason()` is \
             `invalid`, `machine`, `ir` (another hash and no migration, see \
             `roadbook::Config::migrate`), `migration`, `ended` or `ref`."
        );
        let restore_with_doc =
            "Like `restore`, set up by `config` (trace sink, timer, run id, migration).";
        let resume_doc = format!(
            "The request handler's operation on a saved `{name}`: restores `saved` as a new \
             run, gives it `event` if there is one, waits until it is idle (its queue empty and \
             no task or child machine running), stops it without exit effects and gives the \
             new saved snapshot. Timers that fell due while it was saved expire before the \
             event. A run that ends gives its final snapshot (`done`, `failed` or \
             `cancelled`); one that ends with a defect resumes unwinding with the \
             `roadbook::Defect`. A refused snapshot is `Err` before anything starts. The \
             future borrows `handlers`; dropping it stops the run at once."
        );
        let resume_with_doc =
            "Like `resume`, set up by `config` (trace sink, timer, run id, migration).";
        let program = &self.program;
        let program = quote!(&super::#program);
        quote! {
            /// The arguments in their JSON form, or the host defect encoding one met (a
            /// non-finite `float`).
            fn __input(#(#decl),*) -> ::core::result::Result<::roadbook::__private::Object, ::roadbook::__private::Defect> {
                #input
            }

            #output_fn

            fn __error(v: ::roadbook::__private::serde_json::Value) -> #error {
                #decode_error
            }

            fn __event(e: &Event) -> ::core::result::Result<::roadbook::__private::serde_json::Value, ::roadbook::__private::Defect> {
                ::roadbook::__private::encode(e)
            }

            #[doc = #run_doc]
            #[allow(clippy::unit_arg)]
            pub async fn run<H: Handlers + ?::core::marker::Sized>(handlers: &H, #(#decl),*) #ret {
                let __host = Bind(handlers);
                let (__in, __refs) = ::roadbook::__private::collect(|| __input(#(#names),*));
                let __end = ::roadbook::__private::run(#program, #name, ::roadbook::__private::input(__in), &__refs, &__host).await;
                #finish
            }

            #[doc = #run_with_doc]
            #[allow(clippy::unit_arg)]
            pub async fn run_with<H: Handlers + ?::core::marker::Sized>(
                config: &::roadbook::Config,
                handlers: &H,
                #(#decl),*
            ) #ret {
                let __host = Bind(handlers);
                let (__in, __refs) = ::roadbook::__private::collect(|| __input(#(#names),*));
                let __end = ::roadbook::__private::run_with(config, #program, #name, ::roadbook::__private::input(__in), &__refs, &__host).await;
                #finish
            }

            #[doc = #spawn_doc]
            pub fn spawn<H: Handlers + 'static>(
                handlers: ::std::sync::Arc<H>,
                #(#decl),*
            ) -> ::roadbook::Actor<Event, State, #output, #error> {
                spawn_with(::roadbook::Config::default(), handlers, #(#names),*)
            }

            #[doc = #spawn_with_doc]
            pub fn spawn_with<H: Handlers + 'static>(
                config: ::roadbook::Config,
                handlers: ::std::sync::Arc<H>,
                #(#decl),*
            ) -> ::roadbook::Actor<Event, State, #output, #error> {
                let (__in, __refs) = ::roadbook::__private::collect(|| __input(#(#names),*));
                ::roadbook::__private::spawn(
                    config,
                    #program,
                    #name,
                    __in,
                    __refs,
                    ::std::boxed::Box::new(Bind(handlers)),
                    ::roadbook::__private::Codec {
                        event: __event,
                        state: __from_state_id,
                        output: __output,
                        error: __error,
                    },
                )
            }

            #[doc = #restore_doc]
            pub fn restore<H: Handlers + 'static>(
                handlers: ::std::sync::Arc<H>,
                saved: ::roadbook::Saved,
            ) -> ::core::result::Result<::roadbook::Actor<Event, State, #output, #error>, ::roadbook::RestoreError> {
                restore_with(::roadbook::Config::default(), handlers, saved)
            }

            #[doc = #restore_with_doc]
            pub fn restore_with<H: Handlers + 'static>(
                config: ::roadbook::Config,
                handlers: ::std::sync::Arc<H>,
                saved: ::roadbook::Saved,
            ) -> ::core::result::Result<::roadbook::Actor<Event, State, #output, #error>, ::roadbook::RestoreError> {
                ::roadbook::__private::spawn_restored(
                    config,
                    #program,
                    #name,
                    &saved,
                    ::std::boxed::Box::new(Bind(handlers)),
                    ::roadbook::__private::Codec {
                        event: __event,
                        state: __from_state_id,
                        output: __output,
                        error: __error,
                    },
                )
            }

            #[doc = #resume_doc]
            pub async fn resume<H: Handlers + ?::core::marker::Sized>(
                handlers: &H,
                saved: ::roadbook::Saved,
                event: ::core::option::Option<Event>,
            ) -> ::core::result::Result<::roadbook::Saved, ::roadbook::RestoreError> {
                resume_with(&::roadbook::Config::default(), handlers, saved, event).await
            }

            #[doc = #resume_with_doc]
            pub async fn resume_with<H: Handlers + ?::core::marker::Sized>(
                config: &::roadbook::Config,
                handlers: &H,
                saved: ::roadbook::Saved,
                event: ::core::option::Option<Event>,
            ) -> ::core::result::Result<::roadbook::Saved, ::roadbook::RestoreError> {
                let __host = Bind(handlers);
                let __ev = ::roadbook::__private::event(event.as_ref());
                ::roadbook::__private::resume(config, #program, ::core::option::Option::Some(#name), &saved, __ev, &__host).await
            }
        }
    }
}

/// The functions, actions, tasks and machines a machine uses directly.
#[derive(Default)]
struct Uses {
    functions: HashSet<String>,
    actions: HashSet<String>,
    /// Tasks and machines it invokes.
    invokes: HashSet<String>,
}

impl Uses {
    fn state(&mut self, s: &State) {
        for e in s.entry.iter().chain(&s.exit) {
            self.effect(e);
        }
        if let Some(i) = &s.invoke {
            self.invokes.insert(i.src.clone());
            i.args.iter().for_each(|a| self.expr(a));
        }
        for t in &s.transitions {
            if let Trigger::After { delay } = &t.trigger {
                self.expr(delay);
            }
            if let Some(g) = &t.guard {
                self.expr(g);
            }
            t.effects.iter().for_each(|e| self.effect(e));
        }
        for e in s.output.iter().chain(&s.fail) {
            self.expr(e);
        }
        s.states.iter().for_each(|c| self.state(c));
    }

    fn effect(&mut self, e: &Effect) {
        match e {
            Effect::Set { value, .. } => self.expr(value),
            Effect::Do { action, args } => {
                self.actions.insert(action.clone());
                args.iter().for_each(|a| self.expr(a));
            }
            Effect::Send { value, .. } | Effect::Emit { value, .. } => self.expr(value),
        }
    }

    fn expr(&mut self, e: &Expr) {
        match &e.kind {
            ExprKind::Call { func, args } => {
                self.functions.insert(func.clone());
                args.iter().for_each(|a| self.expr(a));
            }
            ExprKind::Op { args, .. } => args.iter().for_each(|a| self.expr(a)),
            ExprKind::Field { of, .. } => self.expr(of),
            ExprKind::Record { fields } => fields.iter().for_each(|f| self.expr(&f.value)),
            ExprKind::Lit(_) | ExprKind::Ref { .. } | ExprKind::Enum { .. } => {}
        }
    }
}
