//! Reading the project at a git revision, for `roadbook viz --diff`: the files the revision
//! has, and the file system a side of the diff reads, by which the project's loader reads the
//! charts (and what they import, and the packages' `package.json`) from it.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use roadbook_compiler::loader::{normalize, relative_path};
use roadbook_compiler::{FileSystem, StdFs};

/// The git repository the project is in.
pub struct Repo {
    /// The project root.
    root: PathBuf,
    /// The project root relative to the top of the repository: `""` or `sub/dir/`.
    prefix: String,
    /// The top of the repository, as git gives it (with symbolic links resolved).
    top: PathBuf,
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
        let top = git_text(root, &["rev-parse", "--show-toplevel"])?;
        Ok(Repo {
            root: root.to_path_buf(),
            prefix,
            top: PathBuf::from(top),
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

    /// Every file the commit has, by its path from the top of the repository.
    pub fn files(&self, commit: &str) -> Result<HashSet<String>, String> {
        let out = git(
            &self.root,
            &["ls-tree", "-r", "-z", "--name-only", "--full-tree", commit],
        )?;
        Ok(out
            .split(|&b| b == 0)
            .filter(|p| !p.is_empty())
            .map(|p| String::from_utf8_lossy(p).into_owned())
            .collect())
    }

    /// The project path of a file, by its path from the top of the repository, if it is under
    /// the project root.
    pub fn project_path<'p>(&self, repo_path: &'p str) -> Option<&'p str> {
        repo_path.strip_prefix(&self.prefix)
    }

    /// A file of the commit, by its path from the top of the repository.
    pub fn read(&self, commit: &str, repo_path: &str) -> Result<String, String> {
        let bytes = git(
            &self.root,
            &["cat-file", "blob", &format!("{commit}:{repo_path}")],
        )?;
        String::from_utf8(bytes).map_err(|_| format!("`{repo_path}` is not UTF-8 at {commit}"))
    }

    /// Where a path on disk is in the repository: its path from the top, `""` for the top
    /// itself, or `None` if it is outside. Worked out from the project root, lexically, so a
    /// file that is not on disk has a place too; a path that does not lead into the repository
    /// that way (an absolute path through another symbolic link) is resolved on disk.
    fn repo_path(&self, disk: &Path) -> Option<String> {
        let rel = normalize(&format!(
            "{}{}",
            self.prefix,
            relative_path(&self.root, disk)
        ));
        if rel != ".." && !rel.starts_with("../") {
            return Some(rel);
        }
        let real = std::fs::canonicalize(disk).ok()?;
        let inside = real.strip_prefix(&self.top).ok()?;
        Some(
            inside
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/"),
        )
    }

    /// The path of a file on disk relative to the project root, as the summary names it.
    pub fn display(&self, disk: &Path) -> String {
        relative_path(&self.root, disk)
    }
}

/// A revision's files, as a side of the diff reads them.
struct Tree {
    label: String,
    commit: String,
    /// The paths of its files, from the top of the repository.
    files: HashSet<String>,
    /// The paths of the directories holding them, the top being `""`.
    dirs: HashSet<String>,
}

/// The file system one side of a diff reads, under the project's loader: a file in the
/// repository is the side's (read with git at a revision, from disk for the working tree),
/// and a file outside the repository or under a `node_modules` directory, which git does not
/// hold, is read from disk. It notes the files it reads from disk outside the repository,
/// which are the same for both sides.
pub struct SideFs<'a> {
    repo: &'a Repo,
    /// The revision; `None` for the working tree.
    tree: Option<Tree>,
    outside: RefCell<BTreeSet<PathBuf>>,
}

/// Where [`SideFs`] reads a path from.
enum Place {
    /// The side's, by the path from the top of the repository.
    Repo(String),
    /// Disk: under `node_modules`, or outside the repository.
    Disk { outside: bool },
}

impl<'a> SideFs<'a> {
    pub fn new(repo: &'a Repo, side: &Side) -> Result<SideFs<'a>, String> {
        let tree = match side {
            Side::WorkTree => None,
            Side::Rev { label, commit } => {
                let files = repo.files(commit)?;
                let mut dirs: HashSet<String> = HashSet::from([String::new()]);
                for f in &files {
                    let mut d = f.as_str();
                    while let Some((parent, _)) = d.rsplit_once('/') {
                        if !dirs.insert(parent.to_string()) {
                            break;
                        }
                        d = parent;
                    }
                }
                Some(Tree {
                    label: label.clone(),
                    commit: commit.clone(),
                    files,
                    dirs,
                })
            }
        };
        Ok(SideFs {
            repo,
            tree,
            outside: RefCell::new(BTreeSet::new()),
        })
    }

    /// The project paths of the side's files under the project root, for a revision; `None`
    /// for the working tree, whose files are found on disk.
    pub fn project_files(&self) -> Option<Vec<String>> {
        let tree = self.tree.as_ref()?;
        let mut files: Vec<String> = tree
            .files
            .iter()
            .filter_map(|f| self.repo.project_path(f))
            .map(str::to_string)
            .collect();
        files.sort();
        Some(files)
    }

    /// The files read from disk outside the repository.
    pub fn outside(&self) -> BTreeSet<PathBuf> {
        self.outside.borrow().clone()
    }

    fn place(&self, path: &Path) -> Place {
        let node_modules = |p: &str| p.split('/').any(|s| s == "node_modules");
        match self.repo.repo_path(path) {
            Some(p) if !node_modules(&p) => Place::Repo(p),
            Some(_) => Place::Disk { outside: false },
            None => Place::Disk {
                outside: !path.components().any(|c| c.as_os_str() == "node_modules"),
            },
        }
    }
}

impl FileSystem for SideFs<'_> {
    fn read_to_string(&self, path: &Path) -> Result<String, String> {
        match (self.place(path), &self.tree) {
            (Place::Repo(p), Some(tree)) if tree.files.contains(&p) => {
                self.repo.read(&tree.commit, &p)
            }
            (Place::Repo(_), Some(tree)) => Err(format!("no such file at {}", tree.label)),
            (Place::Repo(_), None) => StdFs.read_to_string(path),
            (Place::Disk { outside }, _) => {
                let text = StdFs.read_to_string(path)?;
                if outside {
                    self.outside.borrow_mut().insert(path.to_path_buf());
                }
                Ok(text)
            }
        }
    }

    fn is_dir(&self, path: &Path) -> bool {
        match (self.place(path), &self.tree) {
            (Place::Repo(p), Some(tree)) => tree.dirs.contains(&p),
            _ => StdFs.is_dir(path),
        }
    }

    fn canonicalize(&self, path: &Path) -> Option<PathBuf> {
        match (self.place(path), &self.tree) {
            (Place::Repo(p), Some(tree)) => {
                (tree.files.contains(&p) || tree.dirs.contains(&p)).then(|| self.repo.top.join(p))
            }
            _ => StdFs.canonicalize(path),
        }
    }
}
