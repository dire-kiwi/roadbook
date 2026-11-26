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
    /// is identified with the file of the same package and path if the compilation has
    /// already loaded one, and its copy of the declarations is then left out.
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

/// Reads files under a project root on the real file system, and finds packages: first in
/// the explicit map ([`FsLoader::with_package`], `[packages]` in `roadbook.toml`), then, if
/// enabled, as Node does, in `node_modules/<package>` of the importing file's directory and
/// of each directory above it.
///
/// In a package found in `node_modules`, a `package.json` with an `exports` map whose keys
/// are subpaths (`"./*.road": "./charts/*.road"`) is honoured when the entry for the file is
/// a plain string, or an object with a string `default`, with at most one `*`; a file that
/// such a map does not list is not exported. Any other form of `exports` is ignored and the
/// path is taken inside the package's directory, so a package's charts must be shipped in it.
#[derive(Clone, Debug)]
pub struct FsLoader {
    pub root: PathBuf,
    /// Package name to directory. A relative directory is relative to `root`.
    pub packages: BTreeMap<String, PathBuf>,
    /// Whether packages are looked up in `node_modules` directories.
    pub node_modules: bool,
    /// Where each file of a package was found, by [`Loader::resolve`].
    found: HashMap<FileRef, PathBuf>,
    /// The directory each package was found in, once found.
    dirs: HashMap<String, PathBuf>,
}

impl FsLoader {
    /// A loader for the project at `root`, with no packages.
    pub fn new(root: impl Into<PathBuf>) -> FsLoader {
        FsLoader {
            root: root.into(),
            packages: BTreeMap::new(),
            node_modules: false,
            found: HashMap::new(),
            dirs: HashMap::new(),
        }
    }

    /// Maps a package name to its directory.
    pub fn with_package(mut self, name: &str, dir: impl Into<PathBuf>) -> FsLoader {
        self.packages.insert(name.to_string(), dir.into());
        self
    }

    /// Turns the `node_modules` lookup on or off.
    pub fn with_node_modules(mut self, on: bool) -> FsLoader {
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
            if candidate.is_dir() {
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

/// The target of `exports` for `./path`, if the map is one this loader honours. `Ok(None)`
/// means the map is not honoured and the path is taken as it is.
fn exported(package_json: &str, path: &str) -> Result<Option<String>, ()> {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(package_json) else {
        return Ok(None);
    };
    let Some(serde_json::Value::Object(exports)) = json.get("exports") else {
        return Ok(None);
    };
    if exports.is_empty() || !exports.keys().all(|k| k.starts_with("./")) {
        return Ok(None);
    }
    let target = |v: &serde_json::Value| match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Object(o) => o.get("default").and_then(|d| d.as_str()).map(String::from),
        _ => None,
    };
    let want = format!("./{path}");
    if let Some(v) = exports.get(&want) {
        return target(v).map(Some).ok_or(());
    }
    for (key, v) in exports {
        let Some((pre, post)) = key.split_once('*') else {
            continue;
        };
        if post.contains('*') || want.len() < pre.len() + post.len() {
            continue;
        }
        if let Some(middle) = want
            .strip_prefix(pre)
            .and_then(|rest| rest.strip_suffix(post))
        {
            let t = target(v).ok_or(())?;
            if t.matches('*').count() > 1 {
                return Ok(None);
            }
            return Ok(Some(t.replace('*', middle)));
        }
    }
    Err(())
}

impl Loader for FsLoader {
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
        if let Ok(text) = std::fs::read_to_string(dir.join("package.json")) {
            match exported(&text, &file.path) {
                Ok(Some(target)) => disk = dir.join(normalize(&target)),
                Ok(None) => {}
                Err(()) => {
                    return Err(format!(
                        "package `{package}` does not export `./{}`: its `package.json` \
                         `exports` has no entry for it",
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
        std::fs::read_to_string(path)
            .map(Loaded::Source)
            .map_err(|e| e.to_string())
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
fn relative_path(from: &Path, to: &Path) -> String {
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
    fn honours_simple_exports() {
        let pkg = |exports: &str| format!("{{\"name\": \"x\", \"exports\": {exports}}}");
        let e = |exports: &str, path: &str| exported(&pkg(exports), path);
        assert_eq!(
            e("{\"./*.road\": \"./charts/*.road\"}", "a.road"),
            Ok(Some("./charts/a.road".into()))
        );
        assert_eq!(
            e("{\"./a.road\": \"./src/a.road\"}", "a.road"),
            Ok(Some("./src/a.road".into()))
        );
        assert_eq!(
            e(
                "{\"./*\": {\"types\": \"./t/*\", \"default\": \"./d/*\"}}",
                "a.road"
            ),
            Ok(Some("./d/a.road".into()))
        );
        assert_eq!(e("{\"./*.js\": \"./dist/*.js\"}", "a.road"), Err(()));
        assert_eq!(e("\"./index.js\"", "a.road"), Ok(None));
        assert_eq!(e("{\"import\": \"./index.js\"}", "a.road"), Ok(None));
        assert_eq!(exported("{\"name\": \"x\"}", "a.road"), Ok(None));
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
