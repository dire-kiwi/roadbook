//! Stand-in for `roadbook::include!`, built only to find out how a macro expanding in one
//! crate can locate a `.road` file shipped by a dependency. There is no compiler here: the
//! macro reads the chart, finds `import { X } from "<package>/<path>.road"` lines, resolves
//! each package by one of four mechanisms, and expands to consts that show what it found.
//!
//! ```ignore
//! include_road!("orders.road", via = "metadata");  // or "env", "toml", "callback"
//! ```
//!
//! For every import it generates `pub mod <package> { PATH, SOURCE, COMPILED_SOURCE, MICROS, …,
//! pub type Event = <dependency crate>::Event; }`, so the dependent refers to the dependency's
//! own types instead of generating copies.

use proc_macro::{Delimiter, Group, Ident, Literal, Span, TokenStream, TokenTree};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Instant;

/// The dependent's entry point. With `via = "callback"` it may expand to a call into the
/// dependency's exported macro, which calls `__include_road_continue` back.
#[proc_macro]
pub fn include_road(input: TokenStream) -> TokenStream {
    expand(input).unwrap_or_else(|e| compile_error(&e))
}

/// Re-entry point for the callback mechanism. Same as `include_road`, with `dep { … }` groups
/// appended by the dependencies' macros.
#[doc(hidden)]
#[proc_macro]
pub fn __include_road_continue(input: TokenStream) -> TokenStream {
    expand(input).unwrap_or_else(|e| compile_error(&e))
}

/// The chart owner's side (stand-in for `roadbook::include!` in `inventory`). Embeds the chart
/// and exports a hidden `macro_rules!` that hands the chart text and `$crate` to a callback.
#[proc_macro]
pub fn export_road(input: TokenStream) -> TokenStream {
    let r = (|| {
        let toks: Vec<TokenTree> = input.into_iter().collect();
        let rel = toks.first().map(lit_str).ok_or("expected a path")??;
        let dir = std::env::var("CARGO_MANIFEST_DIR").map_err(|e| e.to_string())?;
        let path = Path::new(&dir).join(&rel);
        let source = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let pkg = std::env::var("CARGO_PKG_NAME").unwrap_or_default();
        let path_s = path.display().to_string();
        let code = format!(
            "pub const ROAD_SOURCE: &str = include_str!({path_lit});\n\
             pub const ROAD_PATH: &str = {path_lit};\n\
             #[doc(hidden)]\n#[macro_export]\n\
             macro_rules! __roadbook_chart_{name} {{\n\
                 ($cb:path {{ $($args:tt)* }}) => {{\n\
                     $cb!{{ $($args)* dep {{ package = {pkg_lit}, krate = $crate, path = {path_raw}, source = {src_raw} }} }}\n\
                 }};\n\
             }}\n",
            path_lit = Literal::string(&path_s),
            name = mangle(&rel),
            pkg_lit = Literal::string(&pkg),
            path_raw = raw(&path_s),
            src_raw = raw(&source),
        );
        code.parse::<TokenStream>().map_err(|e| e.to_string())
    })();
    r.unwrap_or_else(|e| compile_error(&e))
}

struct Dep {
    package: String,
    krate: TokenStream,
    path: String,
    source: String,
}

struct Resolved {
    dir: PathBuf,
    krate: String,
    micros: u128,
    cached: bool,
    note: String,
}

fn expand(input: TokenStream) -> Result<TokenStream, String> {
    let original = input.clone();
    let toks: Vec<TokenTree> = input.into_iter().collect();
    let file = toks.first().map(lit_str).ok_or("expected a chart path")??;
    let mut via = "metadata".to_owned();
    let mut deps = Vec::new();
    let mut i = 1;
    while i < toks.len() {
        match &toks[i] {
            TokenTree::Ident(id) if id.to_string() == "via" => {
                via = lit_str(toks.get(i + 2).ok_or("via = \"…\"")?)?;
                i += 3;
            }
            TokenTree::Ident(id) if id.to_string() == "dep" => {
                if let Some(TokenTree::Group(g)) = toks.get(i + 1) {
                    deps.push(parse_dep(g)?);
                }
                i += 2;
            }
            _ => i += 1,
        }
    }

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").map_err(|e| e.to_string())?);
    let chart = manifest_dir.join(&file);
    let source = std::fs::read_to_string(&chart).map_err(|e| format!("{}: {e}", chart.display()))?;

    let mut out = format!(
        "pub const CHART_SOURCE: &str = include_str!({});\n\
         pub const RUSTC_EXTERNS: &str = {};\n",
        Literal::string(&chart.display().to_string()),
        Literal::string(&rustc_externs()),
    )
    .parse::<TokenStream>()
    .map_err(|e| e.to_string())?;

    for (package, rel) in imports(&source) {
        let module = match via.as_str() {
            "callback" => match deps.iter().find(|d| d.package == package) {
                Some(d) => module_from_dep(&package, d)?,
                // Not seen yet: ask the dependency's exported macro to call us back with its chart.
                None => return callback(&package, &rel, original),
            },
            "metadata" => module_from_file(&package, &rel, &via, by_metadata(&package, &manifest_dir)?)?,
            "env" => module_from_file(&package, &rel, &via, by_env(&package)?)?,
            "toml" => {
                let (r, toml) = by_toml(&package, &manifest_dir)?;
                let mut m = module_from_file(&package, &rel, &via, r)?;
                // Track the mapping file too, so editing it re-expands the macro.
                m.extend(
                    format!("const _: &[u8] = include_bytes!({});", Literal::string(&toml.display().to_string()))
                        .parse::<TokenStream>()
                        .map_err(|e| e.to_string())?,
                );
                m
            }
            other => return Err(format!("unknown mechanism `{other}`")),
        };
        out.extend(module);
    }
    Ok(out)
}

/// `import { A, B } from "pkg/dir/file.road"` lines whose path is package-qualified.
fn imports(source: &str) -> Vec<(String, String)> {
    let mut v = Vec::new();
    for line in source.lines() {
        let line = line.trim();
        if !line.starts_with("import") {
            continue;
        }
        let Some(spec) = line.split("from \"").nth(1).and_then(|s| s.split('"').next()) else { continue };
        if spec.starts_with('.') {
            continue;
        }
        if let Some((pkg, rel)) = spec.split_once('/') {
            v.push((pkg.to_owned(), rel.to_owned()));
        }
    }
    v
}

fn callback(package: &str, rel: &str, original: TokenStream) -> Result<TokenStream, String> {
    let krate = package.replace('-', "_");
    let mut ts: TokenStream = format!("::{krate}::__roadbook_chart_{}!", mangle(rel)).parse().map_err(|e: proc_macro::LexError| e.to_string())?;
    let mut inner: TokenStream = "::road_probe_macro::__include_road_continue".parse().unwrap();
    inner.extend([TokenTree::Group(Group::new(Delimiter::Brace, original))]);
    ts.extend([TokenTree::Group(Group::new(Delimiter::Brace, inner))]);
    Ok(ts)
}

// ---- mechanism 1: ask Cargo -------------------------------------------------------------

struct Meta {
    json: Json,
    micros: u128,
    stderr: String,
    wrote_lock: bool,
}

static META: OnceLock<Result<Meta, String>> = OnceLock::new();

fn by_metadata(package: &str, manifest_dir: &Path) -> Result<Resolved, String> {
    let start = Instant::now();
    let mut cached = true;
    let meta = META.get_or_init(|| {
        cached = false;
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let lock = manifest_dir.join("Cargo.lock");
        let lock_before = lock.exists();
        let mut cmd = Command::new(cargo);
        cmd.args(["metadata", "--format-version", "1", "--manifest-path"]).arg(manifest_dir.join("Cargo.toml"));
        if let Ok(extra) = std::env::var("ROAD_PROBE_METADATA_ARGS") {
            cmd.args(extra.split_whitespace());
        }
        let out = cmd.output().map_err(|e| format!("running cargo metadata: {e}"))?;
        let stderr = String::from_utf8_lossy(&out.stderr).trim().replace('\n', " | ");
        if !out.status.success() {
            return Err(format!("cargo metadata failed: {stderr}"));
        }
        let json = Json::parse(&String::from_utf8_lossy(&out.stdout))?;
        Ok(Meta { json, micros: start.elapsed().as_micros(), stderr, wrote_lock: !lock_before && lock.exists() })
    });
    let meta = meta.as_ref().map_err(|e| e.clone())?;
    let j = &meta.json;
    let me = canonical(&manifest_dir.join("Cargo.toml"));
    let pkgs = j.get("packages").map(Json::arr).unwrap_or_default();
    let root = pkgs
        .iter()
        .find(|p| p.get("manifest_path").and_then(Json::str).map(|m| canonical(Path::new(m))) == Some(me.clone()))
        .ok_or("this package is not in cargo metadata")?;
    let root_id = root.get("id").and_then(Json::str).unwrap_or_default();
    let node = j
        .get("resolve")
        .and_then(|r| r.get("nodes"))
        .map(Json::arr)
        .unwrap_or_default()
        .iter()
        .find(|n| n.get("id").and_then(Json::str) == Some(root_id))
        .ok_or("no resolve node")?;
    for d in node.get("deps").map(Json::arr).unwrap_or_default() {
        let id = d.get("pkg").and_then(Json::str).unwrap_or_default();
        let Some(p) = pkgs.iter().find(|p| p.get("id").and_then(Json::str) == Some(id)) else { continue };
        if p.get("name").and_then(Json::str) != Some(package) {
            continue;
        }
        let manifest = PathBuf::from(p.get("manifest_path").and_then(Json::str).unwrap_or_default());
        let cwd = std::env::current_dir().map(|d| d.display().to_string()).unwrap_or_default();
        let mut note = format!("id={id} cwd={cwd}");
        if meta.wrote_lock {
            note.push_str(" WROTE-Cargo.lock-in-package-dir");
        }
        if !meta.stderr.is_empty() {
            note.push_str(&format!(" stderr={}", meta.stderr));
        }
        return Ok(Resolved {
            dir: manifest.parent().unwrap().to_owned(),
            // `name` here is the extern crate name as the dependent sees it (renames applied).
            krate: d.get("name").and_then(Json::str).unwrap_or(package).to_owned(),
            micros: if cached { start.elapsed().as_micros() } else { meta.micros },
            cached,
            note,
        });
    }
    Err(format!("`{package}` is not a direct dependency according to cargo metadata"))
}

fn canonical(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_owned())
}

// ---- mechanism 2: build-script metadata, forwarded by the dependent's build.rs ----------

fn by_env(package: &str) -> Result<Resolved, String> {
    let start = Instant::now();
    let key = format!("ROADBOOK_DIR_{}", package.replace('-', "_"));
    let dir = std::env::var(&key).map_err(|_| format!("{key} is not set (no build.rs forwarding it?)"))?;
    let dep_visible = std::env::vars().any(|(k, _)| k.starts_with("DEP_"));
    Ok(Resolved {
        dir: dir.into(),
        krate: package.replace('-', "_"),
        micros: start.elapsed().as_micros(),
        cached: false,
        note: format!("DEP_*-visible-to-macro={dep_visible}"),
    })
}

// ---- mechanism 3: explicit mapping in roadbook.toml -------------------------------------

fn by_toml(package: &str, manifest_dir: &Path) -> Result<(Resolved, PathBuf), String> {
    let start = Instant::now();
    let toml = manifest_dir.join("roadbook.toml");
    let text = std::fs::read_to_string(&toml).map_err(|e| format!("{}: {e}", toml.display()))?;
    let mut section = "";
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            section = line;
            continue;
        }
        if section != "[packages]" {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            if k.trim() == package {
                let dir = manifest_dir.join(v.trim().trim_matches('"'));
                let r = Resolved { dir, krate: package.replace('-', "_"), micros: start.elapsed().as_micros(), cached: false, note: String::new() };
                return Ok((r, toml));
            }
        }
    }
    Err(format!("`{package}` is not listed under [packages] in {}", toml.display()))
}

// ---- output -----------------------------------------------------------------------------

fn module_from_file(package: &str, rel: &str, via: &str, r: Resolved) -> Result<TokenStream, String> {
    let path = r.dir.join(rel);
    if !path.exists() {
        return Err(format!("{} does not exist", path.display()));
    }
    let path_lit = Literal::string(&path.display().to_string());
    let krate: TokenStream = format!("::{}", r.krate).parse().map_err(|e: proc_macro::LexError| e.to_string())?;
    module(
        package,
        &format!(
            "pub const VIA: &str = {via}; pub const PATH: &str = {path_lit}; pub const SOURCE: &str = include_str!({path_lit});\n\
             pub const CRATE: &str = {krate}; pub const MICROS: u64 = {micros}; pub const CACHED: bool = {cached}; pub const NOTE: &str = {note};",
            via = Literal::string(via),
            krate = Literal::string(&r.krate),
            micros = r.micros,
            cached = r.cached,
            note = Literal::string(&r.note),
        ),
        krate,
    )
}

fn module_from_dep(package: &str, d: &Dep) -> Result<TokenStream, String> {
    module(
        package,
        &format!(
            "pub const VIA: &str = \"callback\"; pub const PATH: &str = {}; pub const SOURCE: &str = {};\n\
             pub const CRATE: &str = \"$crate of the dependency's macro\"; pub const MICROS: u64 = 0; pub const CACHED: bool = false; pub const NOTE: &str = \"\";",
            Literal::string(&d.path),
            Literal::string(&d.source),
        ),
        d.krate.clone(),
    )
}

/// `pub mod <package> { <consts> pub type Event = KRATE::Event; … }`, with `krate` spliced in
/// as tokens so that `$crate` from a dependency's macro survives.
fn module(package: &str, consts: &str, krate: TokenStream) -> Result<TokenStream, String> {
    let template = format!(
        "{consts}\n\
         pub type Event = __KRATE__::Event;\n\
         pub const COMPILED_SOURCE: &str = __KRATE__::ROAD_SOURCE;\n\
         pub use __KRATE__::accepts as deliver;\n"
    );
    let mut body = TokenStream::new();
    for (i, piece) in template.split("__KRATE__").enumerate() {
        if i > 0 {
            body.extend(krate.clone());
        }
        body.extend(piece.parse::<TokenStream>().map_err(|e| e.to_string())?);
    }
    let mut ts: TokenStream = "pub mod".parse().unwrap();
    ts.extend([
        TokenTree::Ident(Ident::new(&package.replace('-', "_"), Span::call_site())),
        TokenTree::Group(Group::new(Delimiter::Brace, body)),
    ]);
    Ok(ts)
}

/// The `--extern name=path` flags of the running rustc. A proc macro runs inside rustc, so it
/// can see them; they show the dependent's name for each dependency, renames included.
fn rustc_externs() -> String {
    let args: Vec<String> = std::env::args().collect();
    let mut v = Vec::new();
    for w in args.windows(2) {
        if w[0] == "--extern" {
            let (name, path) = w[1].split_once('=').unwrap_or((&w[1], ""));
            let file = Path::new(path).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
            v.push(format!("{name}={file}"));
        }
    }
    v.join(" ")
}

// ---- small helpers ----------------------------------------------------------------------

fn parse_dep(g: &Group) -> Result<Dep, String> {
    let mut d = Dep { package: String::new(), krate: TokenStream::new(), path: String::new(), source: String::new() };
    let toks: Vec<TokenTree> = g.stream().into_iter().collect();
    for field in toks.split(|t| matches!(t, TokenTree::Punct(p) if p.as_char() == ',')) {
        let (Some(TokenTree::Ident(k)), Some(_eq)) = (field.first(), field.get(1)) else { continue };
        let value = &field[2..];
        match k.to_string().as_str() {
            "package" => d.package = lit_str(&value[0])?,
            "krate" => d.krate = value.iter().cloned().collect(),
            "path" => d.path = lit_str(&value[0])?,
            "source" => d.source = lit_str(&value[0])?,
            _ => {}
        }
    }
    Ok(d)
}

fn lit_str(t: &TokenTree) -> Result<String, String> {
    let s = match t {
        TokenTree::Literal(l) => l.to_string(),
        // Tokens passed through `macro_rules!` may arrive wrapped in an invisible group.
        TokenTree::Group(g) if g.delimiter() == Delimiter::None => {
            return lit_str(&g.stream().into_iter().next().ok_or("empty group")?);
        }
        other => return Err(format!("expected a string literal, found `{other}`")),
    };
    if let Some(rest) = s.strip_prefix('r') {
        let hashes = rest.len() - rest.trim_start_matches('#').len();
        return Ok(rest[hashes + 1..rest.len() - hashes - 1].to_owned());
    }
    let inner = s.strip_prefix('"').and_then(|s| s.strip_suffix('"')).ok_or_else(|| format!("expected a string literal, found {s}"))?;
    Ok(inner.replace("\\\"", "\"").replace("\\\\", "\\"))
}

fn raw(s: &str) -> String {
    let mut hashes = String::from("#");
    while s.contains(&format!("\"{hashes}")) {
        hashes.push('#');
    }
    format!("r{hashes}\"{s}\"{hashes}")
}

fn mangle(rel: &str) -> String {
    rel.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect()
}

fn compile_error(msg: &str) -> TokenStream {
    format!("compile_error!({});", Literal::string(&format!("road-probe: {msg}"))).parse().unwrap()
}

// ---- just enough JSON for `cargo metadata` ----------------------------------------------

enum Json {
    Null,
    Bool(#[allow(dead_code)] bool),
    Num,
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    fn parse(s: &str) -> Result<Json, String> {
        let mut p = Parser { b: s.as_bytes(), i: 0 };
        p.value()
    }
    fn get(&self, k: &str) -> Option<&Json> {
        match self {
            Json::Obj(v) => v.iter().find(|(key, _)| key == k).map(|(_, v)| v),
            _ => None,
        }
    }
    fn str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }
    fn arr(&self) -> &[Json] {
        match self {
            Json::Arr(v) => v,
            _ => &[],
        }
    }
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() && self.b[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }
    fn value(&mut self) -> Result<Json, String> {
        self.ws();
        let c = *self.b.get(self.i).ok_or("unexpected end of JSON")?;
        match c {
            b'{' => {
                self.i += 1;
                let mut v = Vec::new();
                loop {
                    self.ws();
                    if self.b[self.i] == b'}' {
                        self.i += 1;
                        return Ok(Json::Obj(v));
                    }
                    let k = self.string()?;
                    self.ws();
                    self.i += 1; // ':'
                    let val = self.value()?;
                    v.push((k, val));
                    self.ws();
                    if self.b[self.i] == b',' {
                        self.i += 1;
                    }
                }
            }
            b'[' => {
                self.i += 1;
                let mut v = Vec::new();
                loop {
                    self.ws();
                    if self.b[self.i] == b']' {
                        self.i += 1;
                        return Ok(Json::Arr(v));
                    }
                    v.push(self.value()?);
                    self.ws();
                    if self.b[self.i] == b',' {
                        self.i += 1;
                    }
                }
            }
            b'"' => Ok(Json::Str(self.string()?)),
            b't' => {
                self.i += 4;
                Ok(Json::Bool(true))
            }
            b'f' => {
                self.i += 5;
                Ok(Json::Bool(false))
            }
            b'n' => {
                self.i += 4;
                Ok(Json::Null)
            }
            _ => {
                while self.i < self.b.len() && matches!(self.b[self.i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
                    self.i += 1;
                }
                Ok(Json::Num)
            }
        }
    }
    fn string(&mut self) -> Result<String, String> {
        self.i += 1; // opening quote
        let mut out = Vec::new();
        loop {
            let c = *self.b.get(self.i).ok_or("unterminated JSON string")?;
            self.i += 1;
            match c {
                b'"' => return String::from_utf8(out).map_err(|e| e.to_string()),
                b'\\' => {
                    let e = self.b[self.i];
                    self.i += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b't' => out.push(b'\t'),
                        b'r' => out.push(b'\r'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'u' => {
                            let hex = std::str::from_utf8(&self.b[self.i..self.i + 4]).map_err(|e| e.to_string())?;
                            self.i += 4;
                            let ch = char::from_u32(u32::from_str_radix(hex, 16).map_err(|e| e.to_string())?).unwrap_or('\u{fffd}');
                            out.extend_from_slice(ch.to_string().as_bytes());
                        }
                        other => out.push(other),
                    }
                }
                _ => out.push(c),
            }
        }
    }
}
