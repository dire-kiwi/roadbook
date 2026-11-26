//! Reading the project at a git revision, for `roadbook viz --diff`: the `.road` files the
//! revision has, and a loader that reads them (and what they import) from it.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use roadbook_compiler::loader::{FileRef, normalize, relative_path};
use roadbook_compiler::{FsLoader, Loaded, Loader};

/// The git repository the project is in.
pub struct Repo {
    /// The project root.
    root: PathBuf,
    /// The project root relative to the top of the repository: `""` or `sub/dir/`.
    prefix: String,
}

/// One side of a diff.
#[derive(Clone, Debug)]
pub enum Side {
    /// A commit: how it was named, and its id.
    Rev { label: String, commit: String },
    /// The files on disk.
    WorkTree,
}

impl Side {
    pub fn label(&self) -> &str {
        match self {
            Side::Rev { label, .. } => label,
            Side::WorkTree => "working tree",
        }
    }
}

fn git(dir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("`git {}` failed: {}", args.join(" "), err.trim()));
    }
    Ok(out.stdout)
}

fn git_text(dir: &Path, args: &[&str]) -> Result<String, String> {
    git(dir, args).map(|b| String::from_utf8_lossy(&b).trim().to_string())
}

impl Repo {
    /// The repository containing the project root.
    pub fn open(root: &Path) -> Result<Repo, String> {
        let prefix = git_text(root, &["rev-parse", "--show-prefix"])
            .map_err(|e| format!("--diff needs the project to be in a git repository: {e}"))?;
        Ok(Repo {
            root: root.to_path_buf(),
            prefix,
        })
    }

    /// The commit a revision names.
    pub fn commit(&self, rev: &str) -> Result<String, String> {
        git_text(
            &self.root,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{rev}^{{commit}}"),
            ],
        )
        .map_err(|_| format!("`{rev}` is not a revision of this repository"))
    }

    /// The two sides of a range: `A..B` (A against B), `A...B` (where B forked from A, against B),
    /// `A` alone (A against the working tree). An empty side of `..` is `HEAD`, as in git.
    pub fn range(&self, range: &str) -> Result<(Side, Side), String> {
        let rev = |label: &str| -> Result<(String, String), String> {
            let label = if label.is_empty() { "HEAD" } else { label };
            Ok((label.to_string(), self.commit(label)?))
        };
        let side = |(label, commit): (String, String)| Side::Rev { label, commit };
        if let Some((a, b)) = range.split_once("...") {
            let ((la, ca), b) = (rev(a)?, rev(b)?);
            let base = git_text(&self.root, &["merge-base", &ca, &b.1])
                .map_err(|_| format!("`{range}`: the two revisions have no common ancestor"))?;
            let short: String = base.chars().take(10).collect();
            let label = format!("{short} (merge base of {la} and {})", b.0);
            return Ok((side((label, base)), side(b)));
        }
        if let Some((a, b)) = range.split_once("..") {
            return Ok((side(rev(a)?), side(rev(b)?)));
        }
        if range.is_empty() {
            return Err("--diff needs a revision or a range, such as `main..HEAD`".into());
        }
        Ok((side(rev(range)?), Side::WorkTree))
    }

    /// The project paths of every file the commit has under the project root.
    pub fn files(&self, commit: &str) -> Result<Vec<String>, String> {
        let out = git(
            &self.root,
            &[
                "ls-tree",
                "-r",
                "-z",
                "--name-only",
                "--full-name",
                commit,
                "--",
                ".",
            ],
        )?;
        Ok(out
            .split(|&b| b == 0)
            .filter(|p| !p.is_empty())
            .filter_map(|p| {
                String::from_utf8_lossy(p)
                    .strip_prefix(&self.prefix)
                    .map(str::to_string)
            })
            .collect())
    }

    /// A file of the commit, by project path.
    pub fn read(&self, commit: &str, path: &str) -> Result<String, String> {
        let bytes = git(
            &self.root,
            &[
                "cat-file",
                "blob",
                &format!("{commit}:{}{path}", self.prefix),
            ],
        )?;
        String::from_utf8(bytes).map_err(|_| format!("`{path}` is not UTF-8 at {commit}"))
    }
}

/// Reads the files of the project from a commit, finding packages as the project's loader does.
/// A package file outside the project or under a `node_modules` directory (which git does not
/// hold) is read from disk.
pub struct RevLoader<'a> {
    pub fs: FsLoader,
    pub repo: &'a Repo,
    pub label: String,
    pub commit: String,
    /// The project paths of the commit's files.
    pub tracked: HashSet<String>,
}

impl RevLoader<'_> {
    fn project_path(&self, file: &FileRef) -> Option<String> {
        let disk = self.fs.disk_path(file)?;
        let rel = normalize(&relative_path(&self.fs.root, &disk));
        (rel != ".." && !rel.starts_with("../")).then_some(rel)
    }
}

impl Loader for RevLoader<'_> {
    fn resolve(&mut self, importer: &FileRef, spec: &str) -> Result<FileRef, String> {
        self.fs.resolve(importer, spec)
    }

    fn load(&mut self, file: &FileRef) -> Result<Loaded, String> {
        match self.project_path(file) {
            Some(p) if self.tracked.contains(&p) => {
                self.repo.read(&self.commit, &p).map(Loaded::Source)
            }
            Some(p) if !p.split('/').any(|s| s == "node_modules") => {
                Err(format!("no such file at {}", self.label))
            }
            _ => self.fs.load(file),
        }
    }

    fn location(&self, file: &FileRef) -> Option<PathBuf> {
        self.fs.location(file)
    }

    fn display(&self, file: &FileRef) -> String {
        self.fs.display(file)
    }
}
