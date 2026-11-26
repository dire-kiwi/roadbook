//! The project a command works on: its root, and the `.road` files it is given.
//!
//! The root is the nearest directory holding a `roadbook.toml`, searched upward from the first
//! path given (or the current directory), else the current directory. Files are named by
//! their path relative to the root, with `/` separators: that is how the compiler, the IR's
//! `sources` and diagnostics name them.
//!
//! `roadbook.toml` may hold `exclude = ["glob", …]`: project paths of files and directories
//! that a directory walk skips. A glob matches a whole path; `*` matches within one segment,
//! `**` any number of segments, `?` one character. A file named on the command line is never
//! excluded. `gen-exclude` lists more of them, which only `roadbook gen` skips. Symbolic
//! links to directories are not followed, so a link loop is harmless.
//!
//! A `[packages]` table maps the names of other packages to their directories, relative to
//! the root: `inventory = "examples/inventory"`. An import of a package not listed there is
//! looked up in `node_modules`, as Node does (see `roadbook_compiler::FsLoader`).

use std::path::{Component, Path, PathBuf};

use roadbook_compiler::FsLoader;

/// Directories never searched for `.road` files.
const SKIPPED: &[&str] = &["target", "node_modules", ".git"];

pub struct Workspace {
    pub root: PathBuf,
    /// The `exclude` globs of `roadbook.toml`.
    exclude: Vec<String>,
    /// Its `gen-exclude` globs.
    gen_exclude: Vec<String>,
    /// Its `[packages]` table: package name to directory, relative to the root.
    packages: Vec<(String, String)>,
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
            let config = d.join("roadbook.toml");
            if config.is_file() {
                let text = std::fs::read_to_string(&config)
                    .map_err(|e| format!("cannot read `{}`: {e}", config.display()))?;
                let list = |key| {
                    string_list(&text, key).map_err(|e| format!("`{}`: {e}", config.display()))
                };
                return Ok(Workspace {
                    root: d.to_path_buf(),
                    exclude: list("exclude")?,
                    gen_exclude: list("gen-exclude")?,
                    packages: string_table(&text, "packages")
                        .map_err(|e| format!("`{}`: {e}", config.display()))?,
                });
            }
            dir = d.parent();
        }
        Ok(Workspace {
            root: cwd,
            exclude: Vec::new(),
            gen_exclude: Vec::new(),
            packages: Vec::new(),
        })
    }

    /// A loader for the project: its files, the packages of `[packages]`, and packages in
    /// `node_modules`.
    pub fn loader(&self) -> FsLoader {
        let mut loader = FsLoader::new(&self.root).with_node_modules(true);
        for (name, dir) in &self.packages {
            loader = loader.with_package(name, dir);
        }
        loader
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
        self.files(paths, &[], ".road")
    }

    /// The files `roadbook gen` works on: [`inputs`](Self::inputs), also skipping `gen-exclude`.
    pub fn gen_inputs(&self, paths: &[PathBuf]) -> Result<Vec<String>, String> {
        self.files(paths, &self.gen_exclude, ".road")
    }

    /// The generated modules (`*.road.ts`) under the directories `roadbook gen` would search
    /// for `paths`; a file named in `paths` stands for none.
    pub fn gen_outputs(&self, paths: &[PathBuf]) -> Result<Vec<String>, String> {
        let dirs: Vec<PathBuf> = paths.iter().filter(|p| p.is_dir()).cloned().collect();
        if !paths.is_empty() && dirs.is_empty() {
            return Ok(Vec::new());
        }
        self.files(&dirs, &self.gen_exclude, ".road.ts")
    }

    /// The files named `*<suffix>` under each directory given (or the root if none is), skipping
    /// `extra` as well as `exclude`, and each file given. Sorted, without duplicates.
    fn files(
        &self,
        paths: &[PathBuf],
        extra: &[String],
        suffix: &str,
    ) -> Result<Vec<String>, String> {
        let mut files = Vec::new();
        if paths.is_empty() {
            self.walk(&self.root, extra, suffix, &mut files);
        }
        let mut named = Vec::new();
        for p in paths {
            if p.is_dir() {
                self.walk(p, extra, suffix, &mut files);
            } else if p.is_file() {
                named.push(p.clone());
            } else {
                return Err(format!("`{}` does not exist", p.display()));
            }
        }
        let mut out: Vec<String> = files
            .iter()
            .map(|f| self.project_path(f))
            .filter(|f| !self.excluded(f, extra))
            .chain(named.iter().map(|f| self.project_path(f)))
            .collect();
        out.sort();
        out.dedup();
        Ok(out)
    }

    /// The path of a project file on disk.
    pub fn disk_path(&self, project_path: &str) -> PathBuf {
        self.root.join(project_path)
    }

    /// Whether a project path, or a directory containing it, matches an `exclude` glob or one
    /// of `extra`.
    fn excluded(&self, project_path: &str, extra: &[String]) -> bool {
        let mut prefix = String::new();
        project_path.split('/').any(|seg| {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(seg);
            self.exclude
                .iter()
                .chain(extra)
                .any(|g| glob(g.trim_end_matches('/'), &prefix))
        })
    }

    /// Every file named `*<suffix>` under `dir`, sorted, skipping [`SKIPPED`] and excluded
    /// directories and not following links to directories.
    fn walk(&self, dir: &Path, extra: &[String], suffix: &str, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<(PathBuf, bool)> = entries
            .filter_map(|e| {
                let e = e.ok()?;
                let link = e.file_type().ok()?.is_symlink();
                Some((e.path(), link))
            })
            .collect();
        entries.sort();
        for (p, link) in entries {
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if p.is_dir() {
                if !link
                    && !SKIPPED.contains(&name.as_str())
                    && !self.excluded(&self.project_path(&p), extra)
                {
                    self.walk(&p, extra, suffix, out);
                }
            } else if name.len() > suffix.len() && name.ends_with(suffix) {
                out.push(p);
            }
        }
    }
}

/// The list of strings top-level key `key` of a `roadbook.toml` holds: `exclude = ["a",
/// "b/**"]`, on one line or several; empty if there is no such key. Other keys and tables are
/// left for the tools that read them.
fn string_list(toml: &str, key: &str) -> Result<Vec<String>, String> {
    let mut lines = toml.lines().map(|l| without_comment(l).trim());
    while let Some(line) = lines.next() {
        if line.starts_with('[') {
            break; // a table: `exclude` is a top-level key
        }
        let Some(rest) = line.strip_prefix(key) else {
            continue;
        };
        let Some(rest) = rest.trim_start().strip_prefix('=') else {
            continue;
        };
        let mut text = rest.trim().to_string();
        while !text.contains(']') {
            match lines.next() {
                Some(l) => text.push_str(l),
                None => break,
            }
        }
        let Some(inner) = text.strip_prefix('[').and_then(|t| t.split(']').next()) else {
            return Err(format!(
                "`{key}` must be a list of strings, such as `{key} = [\"tests/**\"]`"
            ));
        };
        let mut out = Vec::new();
        for item in inner.split(',').map(str::trim).filter(|i| !i.is_empty()) {
            match item.strip_prefix('"').and_then(|i| i.strip_suffix('"')) {
                Some(s) => out.push(s.to_string()),
                None => return Err(format!("`{key}`: `{item}` is not a string")),
            }
        }
        return Ok(out);
    }
    Ok(Vec::new())
}

/// The entries `key = "value"` of the table `[name]` of a `roadbook.toml`, in order; empty if
/// there is no such table. A key may be quoted, as a scoped package name must be:
/// `"@shop/inventory" = "packages/inventory"`.
fn string_table(toml: &str, name: &str) -> Result<Vec<(String, String)>, String> {
    let header = format!("[{name}]");
    let mut lines = toml.lines().map(|l| without_comment(l).trim());
    if !lines.any(|l| l == header) {
        return Ok(Vec::new());
    }
    let unquote = |s: &str| {
        let s = s.trim();
        s.strip_prefix('"')
            .and_then(|x| x.strip_suffix('"'))
            .map(String::from)
    };
    let mut out = Vec::new();
    for line in lines {
        if line.starts_with('[') {
            break;
        }
        if line.is_empty() {
            continue;
        }
        let bad = || format!("`[{name}]`: `{line}` is not `name = \"directory\"`");
        let (key, value) = if let Some(quoted) = line.strip_prefix('"') {
            let (k, rest) = quoted.split_once('"').ok_or_else(bad)?;
            (
                k.to_string(),
                rest.trim_start().strip_prefix('=').ok_or_else(bad)?,
            )
        } else {
            let (k, v) = line.split_once('=').ok_or_else(bad)?;
            (k.trim().to_string(), v)
        };
        if key.is_empty() {
            return Err(bad());
        }
        out.push((key, unquote(value).ok_or_else(bad)?));
    }
    Ok(out)
}

/// A TOML line without its `#` comment, if it has one outside a string.
fn without_comment(line: &str) -> &str {
    let mut in_string = false;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_string = !in_string,
            '#' if !in_string => return &line[..i],
            _ => {}
        }
    }
    line
}

/// Whether `path` matches the whole of `pattern`: `*` within a segment, `**` across
/// segments, `?` one character other than `/`.
fn glob(pattern: &str, path: &str) -> bool {
    fn go(p: &[u8], s: &[u8]) -> bool {
        match p {
            [] => s.is_empty(),
            [b'*', b'*'] => true,
            [b'*', b'*', rest @ ..] => {
                let rest = rest.strip_prefix(b"/").unwrap_or(rest);
                (0..=s.len()).any(|i| (i == 0 || s[i - 1] == b'/') && go(rest, &s[i..]))
                    || go(rest, s)
            }
            [b'*', rest @ ..] => (0..=s.len())
                .take_while(|&i| i == 0 || s[i - 1] != b'/')
                .any(|i| go(rest, &s[i..])),
            [b'?', rest @ ..] => s.first().is_some_and(|&c| c != b'/') && go(rest, &s[1..]),
            [c, rest @ ..] => s.first() == Some(c) && go(rest, &s[1..]),
        }
    }
    go(pattern.as_bytes(), path.as_bytes())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob("spike", "spike"));
        assert!(!glob("spike", "spikes"));
        assert!(glob("a/*.road", "a/b.road"));
        assert!(!glob("a/*.road", "a/b/c.road"));
        assert!(glob("a/**/c.road", "a/c.road"));
        assert!(glob("a/**/c.road", "a/b/x/c.road"));
        assert!(glob("**/fail", "crates/x/tests/fail"));
        assert!(glob("**", "anything/at/all"));
        assert!(glob("t?st", "test"));
        assert!(!glob("t?st", "t/st"));
    }

    #[test]
    fn exclude_lists() {
        let excludes = |toml| string_list(toml, "exclude");
        assert_eq!(excludes("").unwrap(), Vec::<String>::new());
        assert_eq!(
            excludes("# c\nexclude = [\"a\", \"b/**\"]\n[ts]\nexclude = [\"x\"]").unwrap(),
            ["a", "b/**"]
        );
        assert_eq!(
            excludes("exclude = [\n  \"a\", # why\n  \"b\",\n]\n").unwrap(),
            ["a", "b"]
        );
        assert!(excludes("exclude = \"a\"").is_err());
        assert!(excludes("exclude = [a]").is_err());
        let both = "gen-exclude = [\"g\"]\nexclude = [\"e\"]\n";
        assert_eq!(excludes(both).unwrap(), ["e"]);
        assert_eq!(string_list(both, "gen-exclude").unwrap(), ["g"]);
        assert!(string_list("gen-exclude = 1", "gen-exclude").is_err());
    }

    #[test]
    fn package_tables() {
        let toml = "exclude = [\"x\"]\n[packages]\ninventory = \"examples/inventory\" # local\n\"@shop/inv\" = \"p/inv\"\n\n[other]\na = \"b\"\n";
        assert_eq!(
            string_table(toml, "packages").unwrap(),
            [
                ("inventory".to_string(), "examples/inventory".to_string()),
                ("@shop/inv".to_string(), "p/inv".to_string())
            ]
        );
        assert!(string_table("", "packages").unwrap().is_empty());
        assert!(string_table("[packages]\ninventory = examples", "packages").is_err());
        assert!(string_table("[packages]\n= \"x\"", "packages").is_err());
    }
}
