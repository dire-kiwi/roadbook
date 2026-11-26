//! The project a command works on: its root, and the `.road` files it is given.
//!
//! The root is the nearest directory holding a `roadbook.toml`, searched upward from the first
//! path given (or the current directory), else the current directory. Files are named by
//! their path relative to the root, with `/` separators: that is how the compiler, the IR's
//! `sources` and diagnostics name them.

use std::path::{Component, Path, PathBuf};

use roadbook_compiler::FsLoader;

/// Directories never searched for `.road` files.
const SKIPPED: &[&str] = &["target", "node_modules", ".git"];

pub struct Workspace {
    pub root: PathBuf,
}

impl Workspace {
    /// The workspace containing `start` (a file or directory), or the current directory's.
    pub fn find(start: Option<&Path>) -> Result<Workspace, String> {
        let cwd = std::env::current_dir()
            .map_err(|e| format!("cannot read the current directory: {e}"))?;
        let from = match start {
            Some(p) => {
                let abs = absolute(&cwd, p);
                if abs.is_dir() {
                    abs
                } else {
                    abs.parent().map(Path::to_path_buf).unwrap_or(cwd.clone())
                }
            }
            None => cwd.clone(),
        };
        let mut dir = Some(from.as_path());
        while let Some(d) = dir {
            if d.join("roadbook.toml").is_file() {
                return Ok(Workspace {
                    root: d.to_path_buf(),
                });
            }
            dir = d.parent();
        }
        Ok(Workspace { root: cwd })
    }

    pub fn loader(&self) -> FsLoader {
        FsLoader::new(&self.root)
    }

    /// The project path of a file: relative to the root, `/`-separated.
    pub fn project_path(&self, path: &Path) -> String {
        let cwd = std::env::current_dir().unwrap_or_default();
        let abs = normalize(&absolute(&cwd, path));
        let root = normalize(&self.root);
        let rel = relative(&root, &abs);
        rel.components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/")
    }

    /// The files to work on: each path given (a directory stands for every `.road` file under
    /// it), or every `.road` file under the root if none is given. Sorted, without duplicates.
    pub fn inputs(&self, paths: &[PathBuf]) -> Result<Vec<String>, String> {
        let mut files = Vec::new();
        if paths.is_empty() {
            road_files(&self.root, &mut files);
        }
        for p in paths {
            if p.is_dir() {
                road_files(p, &mut files);
            } else if p.is_file() {
                files.push(p.clone());
            } else {
                return Err(format!("`{}` does not exist", p.display()));
            }
        }
        let mut out: Vec<String> = files.iter().map(|f| self.project_path(f)).collect();
        out.sort();
        out.dedup();
        Ok(out)
    }

    /// The path of a project file on disk.
    pub fn disk_path(&self, project_path: &str) -> PathBuf {
        self.root.join(project_path)
    }
}

fn road_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entries.sort();
    for p in entries {
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if p.is_dir() {
            if !SKIPPED.contains(&name.as_str()) {
                road_files(&p, out);
            }
        } else if p.extension().is_some_and(|e| e == "road") {
            out.push(p);
        }
    }
}

fn absolute(cwd: &Path, p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    }
}

/// Removes `.` and applies `..` lexically.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            c => out.push(c.as_os_str()),
        }
    }
    out
}

/// `to` relative to the directory `from`, with `..` where it is outside.
fn relative(from: &Path, to: &Path) -> PathBuf {
    let from: Vec<Component> = from.components().collect();
    let to: Vec<Component> = to.components().collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut out = PathBuf::new();
    for _ in common..from.len() {
        out.push("..");
    }
    for c in &to[common..] {
        out.push(c.as_os_str());
    }
    out
}
