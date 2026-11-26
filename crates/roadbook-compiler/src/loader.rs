//! Where source files come from.
//!
//! The compiler never touches the file system itself. It asks a [`Loader`] for each file by a
//! [`FileRef`]: a path in the project, or a path in another package. A path is relative to
//! the root of the project or package, with `/` separators, normalised (no `.` segments, and
//! `..` only at the start). The command line uses [`FsLoader`]; tests use [`MemoryLoader`];
//! the Rust macro and the WebAssembly build supply their own.
//!
//! An import path that starts with `./`, `../` or `/` names a file of the importing file's own
//! project or package: relative to the importing file, or to the root for `/`. Any other path
//! names a file in another package: its first segment is the package's name, or its first two
//! for a scoped name (`@shop/inventory`), and the rest is the path inside the package:
//! `"inventory/inventory.road"`, `"@shop/inventory/charts/inventory.road"`.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use roadbook_ir::Ir;

/// A file of a compilation: a path in the project (`package` is `None`) or in a package.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FileRef {
    /// The package's name, as written in imports: `inventory`, `@shop/inventory`.
    pub package: Option<String>,
    /// Relative to the root of the project or the package, normalised.
    pub path: String,
}

impl FileRef {
    /// A file of the project.
    pub fn project(path: &str) -> FileRef {
        FileRef {
            package: None,
            path: normalize(path),
        }
    }

    /// A file of a package.
    pub fn in_package(package: &str, path: &str) -> FileRef {
        FileRef {
            package: Some(package.to_string()),
            path: normalize(path),
        }
    }
}

/// The import path that names the file: `types.road` in the project, `inventory/types.road`
/// in package `inventory`.
impl fmt::Display for FileRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.package {
            Some(p) => write!(f, "{p}/{}", self.path),
            None => f.write_str(&self.path),
        }
    }
}

/// What a loader hands back for a file.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Loaded {
    /// The text of a `.road` file.
    Source(String),
    /// An import satisfied by an already compiled IR, handed over by another crate or package:
    /// the IR whose entry file (`sources[0]`) is the file asked for. Its declarations enter
    /// the importing file's scope as a parsed file's do: those of its entry file can be
    /// imported from it, and every declaration of the IR is part of the compilation, so the
    /// output stays self-contained. They are not checked again, and they are copied into the
    /// output IR unchanged, except that their spans are renumbered and their files are
    /// marked as belonging to the package (see `spec/ir.md`, "Sources"). Each file of the IR
    /// is identified with the file of the same package and path, or of the same
    /// [`Loader::location`], if the compilation has already loaded one, and its copy of the
    /// declarations is then left out.
    Compiled(Box<Ir>),
}

/// Supplies source files to the compiler.
pub trait Loader {
    /// The file that `import … from "spec"` names when written in `importer`. The default
    /// is [`resolve_spec`], which resolves paths without looking at any file; a loader that
    /// finds packages on disk checks there that the package exists.
    fn resolve(&mut self, importer: &FileRef, spec: &str) -> Result<FileRef, String> {
        resolve_spec(importer, spec)
    }

    /// Read a file. The error is a short reason, shown in the diagnostic.
    fn load(&mut self, file: &FileRef) -> Result<Loaded, String>;

    /// Where the file is, to tell files apart: files at the same location are one file, however
    /// each was named (a file of a package in the project can be imported by a relative path
    /// and as a file of the package). `None`, the default, identifies a file by its
    /// [`FileRef`] alone. [`FsLoader`] gives the canonical path on disk.
    fn location(&self, _file: &FileRef) -> Option<PathBuf> {
        None
    }

    /// How diagnostics name the file. The default is the file's import path; [`FsLoader`]
    /// names a package's file by where it is on disk, relative to the project root.
    fn display(&self, file: &FileRef) -> String {
        file.to_string()
    }
}

/// Resolves an import path written in `importer`, as described in the [module](self)
/// documentation, without looking at any file.
pub fn resolve_spec(importer: &FileRef, spec: &str) -> Result<FileRef, String> {
    if spec.starts_with("./") || spec.starts_with("../") || spec.starts_with('/') {
        let path = join(&importer.path, spec);
        if path == ".." || path.starts_with("../") {
            if let Some(p) = &importer.package {
                return Err(format!("the path goes above the root of package `{p}`"));
            }
        }
        return Ok(FileRef {
            package: importer.package.clone(),
            path,
        });
    }
    let (package, rest) = split_package(spec).ok_or_else(|| {
        format!(
            "an import path that does not start with `./`, `../` or `/` names a file in another \
             package, as `package/file.road`; for a file of this project write `./{spec}`"
        )
    })?;
    let path = normalize(rest);
    if path.is_empty() || path == ".." || path.starts_with("../") {
        return Err(format!(
            "`{spec}` names package `{package}` but no file in it"
        ));
    }
    Ok(FileRef {
        package: Some(package.to_string()),
        path,
    })
}

/// `inventory/a/b.road` is `("inventory", "a/b.road")`, `@shop/inventory/a.road` is
/// `("@shop/inventory", "a.road")`. `None` if the path has no file part.
fn split_package(spec: &str) -> Option<(&str, &str)> {
    let segments = if spec.starts_with('@') { 2 } else { 1 };
    let mut end = 0;
    for _ in 0..segments {
        let slash = spec[end..].find('/')? + end;
        if slash == end {
            return None;
        }
        end = slash + 1;
    }
    let (package, rest) = spec.split_at(end - 1);
    let package_ok = package
        .split('/')
        .all(|s| !s.is_empty() && s != "." && s != "..");
    package_ok.then_some((package, &rest[1..]))
}

/// The file system [`FsLoader`] reads: the real one ([`StdFs`]), or one a host supplies, as
/// the WebAssembly build's host does, which has no file system of its own.
pub trait FileSystem {
    /// The file's text. The error is a short reason, shown in the diagnostic.
    fn read_to_string(&self, path: &Path) -> Result<String, String>;
    /// Whether the path is a directory (following symbolic links).
    fn is_dir(&self, path: &Path) -> bool;
    /// The canonical path, with symbolic links resolved, if the path exists.
    fn canonicalize(&self, path: &Path) -> Option<PathBuf>;
}

/// The real file system, through `std::fs`.
#[derive(Clone, Copy, Debug, Default)]
pub struct StdFs;

impl FileSystem for StdFs {
    fn read_to_string(&self, path: &Path) -> Result<String, String> {
        std::fs::read_to_string(path).map_err(|e| e.to_string())
    }

    fn is_dir(&self, path: &Path) -> bool {
        path.is_dir()
    }

    fn canonicalize(&self, path: &Path) -> Option<PathBuf> {
        std::fs::canonicalize(path).ok()
    }
}

/// Reads files under a project root on a file system ([`StdFs`] unless given another with
/// [`FsLoader::with_fs`]), and finds packages: first in
/// the explicit map ([`FsLoader::with_package`], `[packages]` in `roadbook.toml`), then, if
/// enabled, as Node does, in `node_modules/<package>` of the importing file's directory and
/// of each directory above it.
///
/// When a package's directory (found either way) has a `package.json` with `exports`, a file
/// of the package is where `exports` maps it, as Node resolves it, with the conditions
/// [`LOAD_CONDITIONS`]; a file the map does not export cannot be imported. See [`exported`]
/// for the subset of Node's rules followed. Without `exports`, a file's path is taken inside
/// the package's directory.
///
/// A file's identity ([`Loader::location`]) is its canonical path on disk, so a file reached
/// both by a relative path and as a file of a package, or through a symbolic link, is loaded
/// once.
#[derive(Clone, Debug)]
pub struct FsLoader<F = StdFs> {
    pub root: PathBuf,
    /// Package name to directory. A relative directory is relative to `root`.
    pub packages: BTreeMap<String, PathBuf>,
    /// Whether packages are looked up in `node_modules` directories.
    pub node_modules: bool,
    /// Where each file of a package was found, by [`Loader::resolve`].
    found: HashMap<FileRef, PathBuf>,
    /// The directory each package was found in, once found.
    dirs: HashMap<String, PathBuf>,
    fs: F,
}

/// Where [`FsLoader`] found a file of a package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageFile {
    /// The file.
    pub disk: PathBuf,
    /// The package's directory.
    pub dir: PathBuf,
    /// Whether the package is one of [`FsLoader::packages`], as opposed to one found in
    /// `node_modules`.
    pub listed: bool,
}

impl FsLoader {
    /// A loader for the project at `root`, with no packages.
    pub fn new(root: impl Into<PathBuf>) -> FsLoader {
        FsLoader::with_fs(root, StdFs)
    }
}

impl<F: FileSystem> FsLoader<F> {
    /// A loader for the project at `root` on the file system `fs`, with no packages.
    pub fn with_fs(root: impl Into<PathBuf>, fs: F) -> FsLoader<F> {
        FsLoader {
            root: root.into(),
            packages: BTreeMap::new(),
            node_modules: false,
            found: HashMap::new(),
            dirs: HashMap::new(),
            fs,
        }
    }

    /// The file system it reads.
    pub fn fs(&self) -> &F {
        &self.fs
    }

    /// Maps a package name to its directory.
    pub fn with_package(mut self, name: &str, dir: impl Into<PathBuf>) -> FsLoader<F> {
        self.packages.insert(name.to_string(), dir.into());
        self
    }

    /// Turns the `node_modules` lookup on or off.
    pub fn with_node_modules(mut self, on: bool) -> FsLoader<F> {
        self.node_modules = on;
        self
    }

    /// The file on disk, for a file [`Loader::resolve`] has returned or a project file.
    pub fn disk_path(&self, file: &FileRef) -> Option<PathBuf> {
        match &file.package {
            None => Some(self.root.join(&file.path)),
            Some(p) => self
                .found
                .get(file)
                .cloned()
                .or_else(|| self.dirs.get(p).map(|d| d.join(&file.path))),
        }
    }

    /// Where a file of a package was found, for a file [`Loader::resolve`] has returned.
    pub fn package_file(&self, file: &FileRef) -> Option<PackageFile> {
        let package = file.package.as_ref()?;
        Some(PackageFile {
            disk: self.disk_path(file)?,
            dir: self.dirs.get(package)?.clone(),
            listed: self.packages.contains_key(package),
        })
    }

    /// Notes where a file of a package is, as another loader's [`package_file`] found it, so
    /// that this loader resolves the file's own imports as that one would.
    ///
    /// [`package_file`]: FsLoader::package_file
    pub fn remember(&mut self, file: &FileRef, found: &PackageFile) {
        let Some(package) = &file.package else {
            return;
        };
        self.found.insert(file.clone(), found.disk.clone());
        self.dirs
            .entry(package.clone())
            .or_insert_with(|| found.dir.clone());
    }

    fn package_dir(&self, package: &str, from: &Path) -> Result<PathBuf, String> {
        if let Some(d) = self.packages.get(package) {
            return Ok(self.root.join(d));
        }
        if !self.node_modules {
            return Err(format!(
                "package `{package}` is not known: map it to its directory under `[packages]` in \
                 `roadbook.toml`"
            ));
        }
        let mut dir = Some(from);
        while let Some(d) = dir {
            let candidate = d.join("node_modules").join(package);
            if self.fs.is_dir(&candidate) {
                return Ok(candidate);
            }
            dir = d.parent();
        }
        Err(format!(
            "package `{package}` is not known: map it to its directory under `[packages]` in \
             `roadbook.toml`, or install it in a `node_modules` directory"
        ))
    }
}

/// The conditions [`FsLoader`] matches in an `exports` map, in addition to the order written:
/// an object of conditions resolves to its first member named one of these.
pub const LOAD_CONDITIONS: &[&str] = &["roadbook", "default"];

/// Why `exports` does not give a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotExported {
    /// The map has no entry for it, or the entry is `null`.
    Missing,
    /// The map is one Node would reject, or uses a form this loader does not follow.
    Invalid(String),
}

/// A package's `exports`, with the members of an object in the order written.
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum Exports {
    Str(String),
    Arr(Vec<Exports>),
    Obj(IndexMap<String, Exports>),
    Other(serde_json::Value),
}

#[derive(serde::Deserialize)]
struct PackageJson {
    exports: Option<Exports>,
}

fn exports_of(package_json: &str) -> Option<Exports> {
    serde_json::from_str::<PackageJson>(package_json)
        .ok()?
        .exports
}

/// The subpath map of `exports`, or `None` if `exports` names only the package's main entry
/// (a string, an array, or an object of conditions).
fn subpath_map(exports: &Exports) -> Result<Option<&IndexMap<String, Exports>>, NotExported> {
    let Exports::Obj(map) = exports else {
        return Ok(None);
    };
    let dotted = map.keys().filter(|k| k.starts_with('.')).count();
    if dotted == 0 {
        Ok(None)
    } else if dotted == map.len() {
        Ok(Some(map))
    } else {
        Err(NotExported::Invalid(
            "its `exports` mixes subpaths (keys starting with `.`) and conditions".into(),
        ))
    }
}

/// Where a package's `exports` maps the file `path` of the package (a path as imported,
/// without `./`), as Node resolves it: the path inside the package's directory, normalised.
/// `Ok(None)` if the package has no `exports`, so that every file is exported as itself.
///
/// The subset of Node's rules followed: an exact key (`"./a.road"`) wins; otherwise the
/// pattern key with one `*` (`"./charts/*.road"`) whose part before the `*` is longest, then
/// whose key is longest, as Node orders them. A target is a string starting with `./` (every
/// `*` in it is replaced by what the key's `*` matched), `null` (not exported), an object of
/// conditions (the first member, in the order written, named one of `conditions`; nested
/// objects too), or an array (its first entry that resolves). `exports` that is a string, an
/// array or an object of conditions names only the main entry (`"."`), so no file is
/// exported. Keys ending in `/` (folder mappings, removed from Node) are not followed.
pub fn exported(
    package_json: &str,
    path: &str,
    conditions: &[&str],
) -> Result<Option<String>, NotExported> {
    let Some(exports) = exports_of(package_json) else {
        return Ok(None);
    };
    let Some(map) = subpath_map(&exports)? else {
        return Err(NotExported::Missing);
    };
    let want = format!("./{path}");
    let (target, middle) = match_subpath(map, &want).ok_or(NotExported::Missing)?;
    match resolve_target(target, middle, conditions)? {
        Some(path) => Ok(Some(path)),
        None => Err(NotExported::Missing),
    }
}

/// The entry of `map` for `want` (`./…`), and what its key's `*` matched.
fn match_subpath<'a>(
    map: &'a IndexMap<String, Exports>,
    want: &'a str,
) -> Option<(&'a Exports, Option<&'a str>)> {
    if !want.contains('*')
        && let Some(v) = map.get(want)
    {
        return Some((v, None));
    }
    let mut best: Option<(&str, &Exports, &str)> = None;
    for (key, v) in map {
        let Some(star) = key.find('*') else {
            continue;
        };
        let (pre, post) = (&key[..star], &key[star + 1..]);
        if post.contains('*')
            || want.len() < key.len()
            || !want.starts_with(pre)
            || !want.ends_with(post)
        {
            continue;
        }
        // Node's PATTERN_KEY_COMPARE: the longer part before the `*`, then the longer key.
        let better = best.is_none_or(|(b, ..)| {
            let b_star = b.find('*').unwrap_or(b.len());
            star > b_star || (star == b_star && key.len() > b.len())
        });
        if better {
            best = Some((key, v, &want[star..want.len() - post.len()]));
        }
    }
    best.map(|(_, v, middle)| (v, Some(middle)))
}

/// The path a target resolves to, `Ok(None)` if no condition of an object matched.
fn resolve_target(
    target: &Exports,
    middle: Option<&str>,
    conditions: &[&str],
) -> Result<Option<String>, NotExported> {
    match target {
        Exports::Str(s) => {
            if !s.starts_with("./") {
                return Err(NotExported::Invalid(format!(
                    "the target `{s}` does not start with `./`"
                )));
            }
            let s = middle.map_or_else(|| s.clone(), |m| s.replace('*', m));
            let path = normalize(&s);
            let outside = path.is_empty()
                || path == ".."
                || path.starts_with("../")
                || path.split('/').any(|seg| seg == "node_modules");
            if outside {
                return Err(NotExported::Invalid(format!(
                    "the target `{s}` is not a file inside the package"
                )));
            }
            Ok(Some(path))
        }
        Exports::Obj(obj) => {
            for (condition, v) in obj {
                if conditions.contains(&condition.as_str())
                    && let Some(path) = resolve_target(v, middle, conditions)?
                {
                    return Ok(Some(path));
                }
            }
            Ok(None)
        }
        Exports::Arr(items) => {
            let mut last = Ok(None);
            for item in items {
                match resolve_target(item, middle, conditions) {
                    Ok(Some(path)) => return Ok(Some(path)),
                    Err(NotExported::Missing) => return Err(NotExported::Missing),
                    other => last = other,
                }
            }
            last
        }
        Exports::Other(serde_json::Value::Null) => Err(NotExported::Missing),
        Exports::Other(v) => Err(NotExported::Invalid(format!(
            "the target `{v}` is not a string, an object or an array"
        ))),
    }
}

/// The reverse of [`exported`]: a subpath (without `./`) that the package's `exports` maps to
/// one of `targets` (paths inside the package's directory). `Ok(None)` if the package has no
/// `exports`, so that every file is exported as itself. Each key is tried in the order
/// written, and a candidate counts only if [`exported`] maps it back to a target, so that
/// precedence is respected.
pub fn export_of(
    package_json: &str,
    targets: &[String],
    conditions: &[&str],
) -> Result<Option<String>, NotExported> {
    let Some(exports) = exports_of(package_json) else {
        return Ok(None);
    };
    let Some(map) = subpath_map(&exports)? else {
        return Err(NotExported::Missing);
    };
    let back = |candidate: &str| {
        let sub = candidate.strip_prefix("./")?;
        let to = exported(package_json, sub, conditions).ok()??;
        targets.contains(&to).then(|| sub.to_string())
    };
    for (key, v) in map {
        if key == "." {
            continue;
        }
        if !key.contains('*') {
            if let Some(sub) = back(key) {
                return Ok(Some(sub));
            }
            continue;
        }
        let mut leaves = Vec::new();
        string_leaves(v, &mut leaves);
        for leaf in leaves {
            let Some((pre, post)) = leaf.split_once('*') else {
                continue;
            };
            if post.contains('*') {
                continue;
            }
            for t in targets {
                let t = format!("./{t}");
                if t.len() < pre.len() + post.len() {
                    continue;
                }
                let Some(middle) = t.strip_prefix(pre).and_then(|r| r.strip_suffix(post)) else {
                    continue;
                };
                if let Some(sub) = back(&key.replace('*', middle)) {
                    return Ok(Some(sub));
                }
            }
        }
    }
    Err(NotExported::Missing)
}

fn string_leaves<'a>(e: &'a Exports, out: &mut Vec<&'a str>) {
    match e {
        Exports::Str(s) => out.push(s),
        Exports::Arr(items) => items.iter().for_each(|i| string_leaves(i, out)),
        Exports::Obj(obj) => obj.values().for_each(|v| string_leaves(v, out)),
        Exports::Other(_) => {}
    }
}

impl<F: FileSystem> Loader for FsLoader<F> {
    fn resolve(&mut self, importer: &FileRef, spec: &str) -> Result<FileRef, String> {
        let file = resolve_spec(importer, spec)?;
        let Some(package) = file.package.clone() else {
            return Ok(file);
        };
        if self.found.contains_key(&file) {
            return Ok(file);
        }
        if importer.package.as_ref() == Some(&package) {
            // A file of the importer's own package: relative to the importer on disk, as Node
            // resolves a relative import, or to the package's directory for `/`.
            let disk = if spec.starts_with('/') {
                self.dirs.get(&package).map(|d| d.join(&file.path))
            } else {
                self.disk_path(importer)
                    .and_then(|p| p.parent().map(|d| d.join(spec)))
            };
            if let Some(disk) = disk {
                self.found.insert(file.clone(), disk);
            }
            return Ok(file);
        }
        let from = self
            .disk_path(importer)
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| self.root.clone());
        let dir = match self.dirs.get(&package) {
            Some(d) => d.clone(),
            None => {
                let dir = self.package_dir(&package, &from)?;
                self.dirs.insert(package.clone(), dir.clone());
                dir
            }
        };
        let mut disk = dir.join(&file.path);
        if let Ok(text) = self.fs.read_to_string(&dir.join("package.json")) {
            match exported(&text, &file.path, LOAD_CONDITIONS) {
                Ok(Some(target)) => disk = dir.join(target),
                Ok(None) => {}
                Err(NotExported::Missing) => {
                    return Err(format!(
                        "package `{package}` does not export `./{}`: its `package.json` \
                         `exports` has no entry for it",
                        file.path
                    ));
                }
                Err(NotExported::Invalid(why)) => {
                    return Err(format!(
                        "cannot follow the `exports` of package `{package}` for `./{}`: {why}",
                        file.path
                    ));
                }
            }
        }
        self.found.insert(file.clone(), disk);
        Ok(file)
    }

    fn load(&mut self, file: &FileRef) -> Result<Loaded, String> {
        let path = self.disk_path(file).ok_or_else(|| {
            format!(
                "package `{}` was not resolved",
                file.package.as_deref().unwrap_or("")
            )
        })?;
        self.fs.read_to_string(&path).map(Loaded::Source)
    }

    fn location(&self, file: &FileRef) -> Option<PathBuf> {
        let disk = self.disk_path(file)?;
        Some(self.fs.canonicalize(&disk).unwrap_or(disk))
    }

    fn display(&self, file: &FileRef) -> String {
        match (&file.package, self.disk_path(file)) {
            (Some(_), Some(disk)) => relative_path(&self.root, &disk),
            _ => file.to_string(),
        }
    }
}

/// `to` relative to the directory `from`, lexically, with `/` separators and `..` where it is
/// outside.
pub fn relative_path(from: &Path, to: &Path) -> String {
    let norm = |p: &Path| -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for c in p.components() {
            match c {
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => {
                    if out.pop().is_none() {
                        out.push("..".into());
                    }
                }
                c => out.push(c.as_os_str().to_string_lossy().into_owned()),
            }
        }
        out
    };
    let (from, to) = (norm(from), norm(to));
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut out: Vec<String> = vec!["..".into(); from.len() - common];
    out.extend(to[common..].iter().cloned());
    out.join("/")
}

/// Files held in memory: project files keyed by path, package files by package and path.
#[derive(Clone, Debug, Default)]
pub struct MemoryLoader {
    pub files: BTreeMap<String, String>,
    pub packages: BTreeMap<FileRef, Loaded>,
}

impl MemoryLoader {
    pub fn new() -> MemoryLoader {
        MemoryLoader::default()
    }

    /// Add a project file. The path is normalised.
    pub fn with(mut self, path: &str, text: impl Into<String>) -> MemoryLoader {
        self.insert(path, text);
        self
    }

    pub fn insert(&mut self, path: &str, text: impl Into<String>) {
        self.files.insert(normalize(path), text.into());
    }

    /// Add a file of a package, as source text or as a compiled IR.
    pub fn with_package(mut self, package: &str, path: &str, file: Loaded) -> MemoryLoader {
        self.packages
            .insert(FileRef::in_package(package, path), file);
        self
    }
}

impl Loader for MemoryLoader {
    fn load(&mut self, file: &FileRef) -> Result<Loaded, String> {
        match &file.package {
            None => self
                .files
                .get(&file.path)
                .cloned()
                .map(Loaded::Source)
                .ok_or_else(|| "no such file".to_string()),
            Some(_) => self
                .packages
                .get(file)
                .cloned()
                .ok_or_else(|| "no such file".to_string()),
        }
    }
}

/// `spec` resolved against the directory of `importer`, normalised. An absolute `spec`
/// (starting with `/`) is taken as a path from the root.
pub fn join(importer: &str, spec: &str) -> String {
    if let Some(abs) = spec.strip_prefix('/') {
        return normalize(abs);
    }
    let dir = importer.rsplit_once('/').map_or("", |(d, _)| d);
    if dir.is_empty() {
        normalize(spec)
    } else {
        normalize(&format!("{dir}/{spec}"))
    }
}

/// A path with `\` turned into `/`, empty and `.` segments removed and `..` applied where it
/// can be. `..` segments that would go above the start are kept.
pub fn normalize(path: &str) -> String {
    let path = path.replace('\\', "/");
    let mut out: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." if out.last().is_some_and(|s| *s != "..") => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    out.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_and_normalises() {
        assert_eq!(join("a/b.road", "./c.road"), "a/c.road");
        assert_eq!(join("a/b.road", "../c.road"), "c.road");
        assert_eq!(join("b.road", "../c.road"), "../c.road");
        assert_eq!(join("a/b/x.road", "../../y/./z.road"), "y/z.road");
        assert_eq!(join("a/x.road", "/top.road"), "top.road");
        assert_eq!(normalize("./a//b\\c.road"), "a/b/c.road");
    }

    #[test]
    fn resolves_import_paths() {
        let project = FileRef::project("orders/orders.road");
        let r = |importer: &FileRef, spec: &str| resolve_spec(importer, spec);
        assert_eq!(
            r(&project, "./types.road"),
            Ok(FileRef::project("orders/types.road"))
        );
        assert_eq!(
            r(&project, "inventory/inventory.road"),
            Ok(FileRef::in_package("inventory", "inventory.road"))
        );
        assert_eq!(
            r(&project, "@shop/inventory/charts/inventory.road"),
            Ok(FileRef::in_package(
                "@shop/inventory",
                "charts/inventory.road"
            ))
        );
        let in_pkg = FileRef::in_package("inventory", "charts/inventory.road");
        assert_eq!(
            r(&in_pkg, "../types.road"),
            Ok(FileRef::in_package("inventory", "types.road"))
        );
        assert_eq!(
            r(&in_pkg, "/types.road"),
            Ok(FileRef::in_package("inventory", "types.road"))
        );
        assert!(
            r(&in_pkg, "../../x.road")
                .unwrap_err()
                .contains("above the root")
        );
        assert!(
            r(&project, "types.road")
                .unwrap_err()
                .contains("./types.road")
        );
        assert!(r(&project, "inventory/").unwrap_err().contains("no file"));
        assert!(r(&project, "@shop/inventory").is_err());
        assert!(r(&project, "@shop//x.road").is_err());
        assert_eq!(
            FileRef::in_package("@a/b", "c.road").to_string(),
            "@a/b/c.road"
        );
    }

    #[test]
    fn follows_exports_as_node_does() {
        let pkg = |exports: &str| format!("{{\"name\": \"x\", \"exports\": {exports}}}");
        let e = |exports: &str, path: &str| exported(&pkg(exports), path, LOAD_CONDITIONS);
        let found = |s: &str| Ok(Some(s.to_string()));
        assert_eq!(
            e(r#"{"./*.road": "./charts/*.road"}"#, "a/b.road"),
            found("charts/a/b.road")
        );
        assert_eq!(
            e(r#"{"./a.road": "./src/a.road"}"#, "a.road"),
            found("src/a.road")
        );
        // A main entry does not hide the subpaths.
        let with_main = r#"{".": "./index.js", "./*.road": "./charts/*.road"}"#;
        assert_eq!(e(with_main, "y.road"), found("charts/y.road"));
        assert_eq!(e(with_main, "x.js"), Err(NotExported::Missing));
        // The most specific pattern wins, whatever the order; an exact key wins over all.
        let specific = r#"{"./*": "./all/*", "./legacy/*.road": "./old/*.road", "./legacy/*": "./mid/*", "./legacy/x.road": "./exact.road"}"#;
        assert_eq!(e(specific, "legacy/y.road"), found("old/y.road"));
        assert_eq!(e(specific, "legacy/y.txt"), found("mid/y.txt"));
        assert_eq!(e(specific, "legacy/x.road"), found("exact.road"));
        assert_eq!(e(specific, "z.road"), found("all/z.road"));
        // Equal prefixes: the longer key.
        assert_eq!(
            e(
                r#"{"./a/*": "./1/*", "./a/*.road": "./2/*.road"}"#,
                "a/b.road"
            ),
            found("2/b.road")
        );
        // Conditions: the first member, in the order written, that is `roadbook` or `default`.
        assert_eq!(
            e(
                r#"{"./*": {"types": "./t/*", "default": "./d/*"}}"#,
                "a.road"
            ),
            found("d/a.road")
        );
        assert_eq!(
            e(
                r#"{"./*": {"roadbook": "./r/*", "default": "./d/*"}}"#,
                "a.road"
            ),
            found("r/a.road")
        );
        assert_eq!(
            e(
                r#"{"./*": {"default": "./d/*", "roadbook": "./r/*"}}"#,
                "a.road"
            ),
            found("d/a.road")
        );
        assert_eq!(
            e(
                r#"{"./*": {"node": {"default": "./n/*"}}, "./*.road": {"import": "./i/*"}}"#,
                "a.road"
            ),
            Err(NotExported::Missing)
        );
        assert_eq!(
            e(r#"{"./*": [{"import": "./i/*"}, "./f/*"]}"#, "a.road"),
            found("f/a.road")
        );
        // Every `*` of the target is replaced.
        assert_eq!(
            e(r#"{"./*.road": "./*/*.road"}"#, "a.road"),
            found("a/a.road")
        );
        // Excluded, or not listed.
        assert_eq!(
            e(r#"{"./*": "./*", "./private/*": null}"#, "private/a.road"),
            Err(NotExported::Missing)
        );
        assert_eq!(
            e(r#"{"./*.js": "./dist/*.js"}"#, "a.road"),
            Err(NotExported::Missing)
        );
        // `exports` naming only the main entry exports no file.
        assert_eq!(e(r#""./index.js""#, "a.road"), Err(NotExported::Missing));
        assert_eq!(
            e(r#"{"import": "./index.js"}"#, "a.road"),
            Err(NotExported::Missing)
        );
        // Forms Node rejects.
        for bad in [
            r#"{".": "./index.js", "import": "./index.js"}"#,
            r#"{"./*": "*"}"#,
            r#"{"./*": "./../*"}"#,
            r#"{"./*": 1}"#,
        ] {
            assert!(
                matches!(e(bad, "a.road"), Err(NotExported::Invalid(_))),
                "{bad}"
            );
        }
        // No `exports`: every file is exported as itself.
        assert_eq!(
            exported(r#"{"name": "x"}"#, "a.road", LOAD_CONDITIONS),
            Ok(None)
        );
        assert_eq!(e("null", "a.road"), Ok(None));
    }

    #[test]
    fn finds_the_subpath_exports_maps_to_a_file() {
        let pkg = |exports: &str| format!("{{\"name\": \"x\", \"exports\": {exports}}}");
        let conditions = &["types", "import", "node", "default"];
        let r = |exports: &str, target: &str| {
            export_of(&pkg(exports), &[target.to_string()], conditions)
        };
        assert_eq!(
            r(
                r#"{".": "./i.js", "./*.road": "./charts/*.road", "./*.road.js": "./charts/*.road.js"}"#,
                "charts/a/b.road.js"
            ),
            Ok(Some("a/b.road.js".into()))
        );
        assert_eq!(
            r(r#"{"./*": "./charts/*"}"#, "charts/b.road.js"),
            Ok(Some("b.road.js".into()))
        );
        assert_eq!(
            r(
                r#"{"./x.js": {"import": "./charts/x.road.js"}}"#,
                "charts/x.road.js"
            ),
            Ok(Some("x.js".into()))
        );
        // Only `./*.road` is exported: nothing maps to the module.
        assert_eq!(
            r(r#"{"./*.road": "./charts/*.road"}"#, "charts/b.road.js"),
            Err(NotExported::Missing)
        );
        // A candidate that a more specific key takes elsewhere does not count.
        assert_eq!(
            r(
                r#"{"./*": "./charts/*", "./b.road.js": "./other.js"}"#,
                "charts/b.road.js"
            ),
            Err(NotExported::Missing)
        );
        assert_eq!(
            r(r#"{"./a.js": "./a.js"}"#, "charts/b.road.js"),
            Err(NotExported::Missing)
        );
        assert_eq!(export_of(r#"{"name": "x"}"#, &[], conditions), Ok(None));
    }

    #[test]
    fn relative_paths_on_disk() {
        assert_eq!(
            relative_path(Path::new("/r"), Path::new("/r/a/b.road")),
            "a/b.road"
        );
        assert_eq!(
            relative_path(Path::new("/r/x"), Path::new("/r/a.road")),
            "../a.road"
        );
    }
}
