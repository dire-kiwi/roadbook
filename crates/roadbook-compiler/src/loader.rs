//! Where source files come from.
//!
//! The compiler never touches the file system itself. It asks a [`Loader`] for each file by
//! its *project path*: relative to the project root, with `/` separators, normalised (no `.`
//! segments, and `..` only at the start). The command line uses [`FsLoader`]; tests use
//! [`MemoryLoader`]; the Rust macro and the WebAssembly build supply their own.

use std::collections::BTreeMap;
use std::path::PathBuf;

/// What a loader hands back for a path.
///
/// Milestone 5 adds a variant for an import satisfied by an already compiled IR handed over
/// by another crate or package (`Compiled(roadbook_ir::Ir)`): its declarations then enter the
/// importing file's scope exactly as a parsed file's do (see `Project::load` in `project.rs`,
/// the one place that would match on it).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Loaded {
    /// The text of a `.road` file.
    Source(String),
}

/// Supplies source files to the compiler.
pub trait Loader {
    /// The project path of the file that `import … from "spec"` names when written in the file
    /// at project path `importer`. The default resolves `spec` relative to the importer's
    /// directory.
    fn resolve(&self, importer: &str, spec: &str) -> Result<String, String> {
        Ok(join(importer, spec))
    }

    /// Read the file at a project path. The error is a short reason, shown in the diagnostic.
    fn load(&mut self, path: &str) -> Result<Loaded, String>;
}

/// Reads files under a project root on the real file system.
#[derive(Clone, Debug)]
pub struct FsLoader {
    pub root: PathBuf,
}

impl FsLoader {
    pub fn new(root: impl Into<PathBuf>) -> FsLoader {
        FsLoader { root: root.into() }
    }
}

impl Loader for FsLoader {
    fn load(&mut self, path: &str) -> Result<Loaded, String> {
        std::fs::read_to_string(self.root.join(path))
            .map(Loaded::Source)
            .map_err(|e| e.to_string())
    }
}

/// Files held in memory, keyed by project path.
#[derive(Clone, Debug, Default)]
pub struct MemoryLoader {
    pub files: BTreeMap<String, String>,
}

impl MemoryLoader {
    pub fn new() -> MemoryLoader {
        MemoryLoader::default()
    }

    /// Add a file. The path is normalised.
    pub fn with(mut self, path: &str, text: impl Into<String>) -> MemoryLoader {
        self.insert(path, text);
        self
    }

    pub fn insert(&mut self, path: &str, text: impl Into<String>) {
        self.files.insert(normalize(path), text.into());
    }
}

impl Loader for MemoryLoader {
    fn load(&mut self, path: &str) -> Result<Loaded, String> {
        self.files
            .get(path)
            .cloned()
            .map(Loaded::Source)
            .ok_or_else(|| "no such file".to_string())
    }
}

/// `spec` resolved against the directory of `importer`, normalised. An absolute `spec`
/// (starting with `/`) is taken as a project path.
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
}
