//! The files the server knows: open documents, the projects they belong to, which file
//! imports which, and the latest compilation of each group of files that import one another.

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

use lsp_types::Uri;
use roadbook_compiler::loader::FileRef;
use roadbook_compiler::{Compilation, FileId, FsLoader, Loaded, Loader};
use roadbook_syntax::parse_file;

use crate::text::path_uri;
use crate::{FindProject, ProjectConfig};

/// An open document.
pub struct Doc {
    pub uri: Uri,
    pub text: String,
    pub version: i32,
}

/// A compilation of a group of files that import one another, and where its files are.
pub struct Analysis {
    pub out: Compilation,
    /// The file on disk of each file of the compilation, by [`FileId`]; `None` for one that
    /// is not a file (a file of a compiled IR).
    pub paths: Vec<Option<PathBuf>>,
    /// The files compiled as entries.
    pub roots: Vec<PathBuf>,
    /// The project's root.
    pub project: PathBuf,
}

impl Analysis {
    pub fn file_id(&self, path: &Path) -> Option<FileId> {
        self.paths
            .iter()
            .position(|p| p.as_deref() == Some(path))
            .map(|i| FileId(i as u32))
    }

    pub fn path(&self, file: FileId) -> Option<&Path> {
        self.paths.get(file.0 as usize)?.as_deref()
    }

    pub fn text(&self, file: FileId) -> &str {
        &self.out.files[file.0 as usize].1
    }
}

struct Project {
    config: ProjectConfig,
    /// The files each file of the project imports, with a hash of the text they were read
    /// from.
    imports: HashMap<PathBuf, (u64, Vec<PathBuf>)>,
    /// Whether every file must be read again (after a save, or when files were added).
    rescan: bool,
}

pub struct World {
    find: FindProject,
    /// Open documents by file.
    pub docs: HashMap<PathBuf, Doc>,
    projects: Vec<Project>,
    /// The project of each file seen, by index in `projects`.
    project_of: HashMap<PathBuf, usize>,
    /// The latest analysis of each file.
    pub analyses: HashMap<PathBuf, Rc<Analysis>>,
    /// Files changed since the last [`World::flush`].
    dirty: BTreeSet<PathBuf>,
}

/// How the server identifies a file: its canonical path, or for one that does not exist
/// (yet) its absolute path with `.` and `..` applied.
pub fn key(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| lexical(path))
}

fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c.as_os_str()),
        }
    }
    out
}

fn hash(text: &str) -> u64 {
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// The project path of `path` in the project at `root`, if it is inside it.
fn project_path(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// The file system with the open documents laid over it.
pub struct Overlay<'a> {
    inner: FsLoader,
    docs: &'a HashMap<PathBuf, Doc>,
    /// Texts that replace both, for a trial compilation.
    edits: &'a HashMap<PathBuf, String>,
}

impl Loader for Overlay<'_> {
    fn resolve(&mut self, importer: &FileRef, spec: &str) -> Result<FileRef, String> {
        self.inner.resolve(importer, spec)
    }

    fn load(&mut self, file: &FileRef) -> Result<Loaded, String> {
        if let Some(at) = self.inner.location(file) {
            if let Some(text) = self.edits.get(&at) {
                return Ok(Loaded::Source(text.clone()));
            }
            if let Some(doc) = self.docs.get(&at) {
                return Ok(Loaded::Source(doc.text.clone()));
            }
        }
        self.inner.load(file)
    }

    fn location(&self, file: &FileRef) -> Option<PathBuf> {
        self.inner.location(file).map(|p| key(&p))
    }

    fn display(&self, file: &FileRef) -> String {
        self.inner.display(file)
    }
}

impl World {
    pub fn new(find: FindProject) -> World {
        World {
            find,
            docs: HashMap::new(),
            projects: Vec::new(),
            project_of: HashMap::new(),
            analyses: HashMap::new(),
            dirty: BTreeSet::new(),
        }
    }

    /// The URI to give the client for a file: the one it opened the file by, if it did.
    pub fn uri(&self, path: &Path) -> Uri {
        match self.docs.get(path) {
            Some(d) => d.uri.clone(),
            None => path_uri(path),
        }
    }

    /// The current text of a file: its open document, else what is on disk.
    pub fn text(&self, path: &Path) -> Option<String> {
        match self.docs.get(path) {
            Some(d) => Some(d.text.clone()),
            None => std::fs::read_to_string(path).ok(),
        }
    }

    /// Notes that a file changed. With `rescan`, its project's file list and every file's
    /// imports are read again (a save, a file created or deleted).
    pub fn touch(&mut self, path: PathBuf, rescan: bool) {
        if rescan && let Some(&p) = self.project_of.get(&path) {
            self.projects[p].rescan = true;
        }
        self.dirty.insert(path);
    }

    pub fn is_dirty(&self) -> bool {
        !self.dirty.is_empty()
    }

    /// The project of a file, found (and its files read) the first time.
    fn project_for(&mut self, path: &Path) -> Result<usize, String> {
        if let Some(&i) = self.project_of.get(path) {
            return Ok(i);
        }
        let mut config = (self.find)(path)?;
        config.root = key(&config.root);
        let i = match self
            .projects
            .iter()
            .position(|p| p.config.root == config.root)
        {
            Some(i) => i,
            None => {
                self.projects.push(Project {
                    config,
                    imports: HashMap::new(),
                    rescan: true,
                });
                self.projects.len() - 1
            }
        };
        self.project_of.insert(path.to_path_buf(), i);
        Ok(i)
    }

    /// Compiles what the changes since the last call can affect. Returns the new analyses,
    /// and the files that are no longer part of any (deleted, or closed and never saved),
    /// whose diagnostics are to be cleared.
    pub fn flush(&mut self) -> (Vec<Rc<Analysis>>, Vec<PathBuf>) {
        let dirty = std::mem::take(&mut self.dirty);
        let mut by_project: HashMap<usize, Vec<PathBuf>> = HashMap::new();
        let mut gone = Vec::new();
        for path in dirty {
            match self.project_for(&path) {
                Ok(p) => by_project.entry(p).or_default().push(path),
                Err(_) => gone.push(path),
            }
        }
        let mut out = Vec::new();
        let mut projects: Vec<(usize, Vec<PathBuf>)> = by_project.into_iter().collect();
        projects.sort();
        for (p, changed) in projects {
            let (analysis, removed) = self.recheck(p, &changed);
            gone.extend(removed);
            if let Some(a) = analysis {
                let a = Rc::new(a);
                for path in a.paths.iter().flatten() {
                    if a.roots.contains(path) {
                        self.analyses.insert(path.clone(), a.clone());
                    } else {
                        self.analyses
                            .entry(path.clone())
                            .or_insert_with(|| a.clone());
                    }
                }
                out.push(a);
            }
        }
        for g in &gone {
            self.analyses.remove(g);
        }
        (out, gone)
    }

    fn recheck(&mut self, p: usize, changed: &[PathBuf]) -> (Option<Analysis>, Vec<PathBuf>) {
        let before = self.component(p, changed);
        if self.projects[p].rescan {
            if let Ok(mut config) = (self.find)(&self.projects[p].config.root.join("x.road")) {
                config.root = key(&config.root);
                if config.root == self.projects[p].config.root {
                    self.projects[p].config = config;
                }
            }
        }
        self.scan(p, changed);
        let after = self.component(p, changed);
        let project = &self.projects[p];
        let root = project.config.root.clone();
        let mut removed = Vec::new();
        let mut roots: Vec<(String, PathBuf)> = Vec::new();
        for path in before.union(&after) {
            match (
                project.imports.contains_key(path),
                project_path(&root, path),
            ) {
                (true, Some(pp)) => roots.push((pp, path.clone())),
                (false, Some(_)) => removed.push(path.clone()),
                // A file of a package outside the project: never an entry.
                (_, None) => {}
            }
        }
        roots.sort();
        if roots.is_empty() {
            return (None, removed);
        }
        let names: Vec<String> = roots.iter().map(|(n, _)| n.clone()).collect();
        let no_edits = HashMap::new();
        let out = self.compile(p, &names, &no_edits);
        let analysis = out.map(|out| Analysis {
            paths: self.paths(&root, &out),
            out,
            roots: roots.into_iter().map(|(_, p)| p).collect(),
            project: root,
        });
        (analysis, removed)
    }

    fn compile(
        &self,
        p: usize,
        roots: &[String],
        edits: &HashMap<PathBuf, String>,
    ) -> Option<Compilation> {
        let config = &self.projects[p].config;
        let mut loader = Overlay {
            inner: config.loader.clone(),
            docs: &self.docs,
            edits,
        };
        roadbook_compiler::check(&mut loader, roots, &config.options).ok()
    }

    /// Compiles the entries of `analysis` again with some files' texts replaced, to see what
    /// an edit would do.
    pub fn trial(
        &self,
        analysis: &Analysis,
        edits: &HashMap<PathBuf, String>,
    ) -> Option<Compilation> {
        let p = self
            .projects
            .iter()
            .position(|p| p.config.root == analysis.project)?;
        let roots: Vec<String> = analysis
            .roots
            .iter()
            .filter_map(|r| project_path(&analysis.project, r))
            .collect();
        self.compile(p, &roots, edits)
    }

    /// Where each file of a compilation is on disk.
    fn paths(&self, root: &Path, out: &Compilation) -> Vec<Option<PathBuf>> {
        out.files
            .iter()
            .map(|(name, _)| {
                let path = key(&root.join(name));
                (path.is_file() || self.docs.contains_key(&path)).then_some(path)
            })
            .collect()
    }

    /// Reads the imports of the files that changed, or of every file of the project when it
    /// is to be rescanned.
    fn scan(&mut self, p: usize, changed: &[PathBuf]) {
        let rescan = std::mem::replace(&mut self.projects[p].rescan, false);
        let root = self.projects[p].config.root.clone();
        let mut files: BTreeSet<PathBuf> = BTreeSet::new();
        if rescan {
            files.extend(
                self.projects[p]
                    .config
                    .files
                    .iter()
                    .map(|f| key(&root.join(f))),
            );
            files.extend(self.projects[p].imports.keys().cloned());
        }
        files.extend(
            self.docs
                .keys()
                .filter(|d| d.starts_with(&root))
                .filter(|d| rescan || !self.projects[p].imports.contains_key(*d))
                .cloned(),
        );
        files.extend(changed.iter().filter(|c| c.starts_with(&root)).cloned());
        let listed: HashSet<PathBuf> = self.projects[p]
            .config
            .files
            .iter()
            .map(|f| key(&root.join(f)))
            .collect();
        let mut loader = self.projects[p].config.loader.clone();
        for path in files {
            let text = match self.docs.get(&path) {
                Some(d) => Some(d.text.clone()),
                None if listed.contains(&path) || self.projects[p].imports.contains_key(&path) => {
                    std::fs::read_to_string(&path).ok()
                }
                None => None,
            };
            let (Some(text), Some(name)) = (text, project_path(&root, &path)) else {
                self.projects[p].imports.remove(&path);
                continue;
            };
            let h = hash(&text);
            if self.projects[p]
                .imports
                .get(&path)
                .is_some_and(|(old, _)| *old == h)
            {
                continue;
            }
            let (ast, _) = parse_file(FileId(0), &text);
            let importer = FileRef::project(&name);
            let targets: Vec<PathBuf> = ast
                .imports()
                .filter_map(|i| {
                    let file = loader.resolve(&importer, &i.path.value).ok()?;
                    loader.location(&file).map(|l| key(&l))
                })
                .collect();
            self.projects[p].imports.insert(path, (h, targets));
        }
    }

    /// The files connected to `start` by imports, either way, `start` included.
    fn component(&self, p: usize, start: &[PathBuf]) -> BTreeSet<PathBuf> {
        let imports = &self.projects[p].imports;
        let mut edges: HashMap<&Path, Vec<&Path>> = HashMap::new();
        for (from, (_, targets)) in imports {
            for to in targets {
                edges.entry(from).or_default().push(to);
                edges.entry(to).or_default().push(from);
            }
        }
        let mut seen: BTreeSet<PathBuf> = start.iter().cloned().collect();
        let mut queue: VecDeque<PathBuf> = start.iter().cloned().collect();
        while let Some(f) = queue.pop_front() {
            for &n in edges.get(f.as_path()).into_iter().flatten() {
                if seen.insert(n.to_path_buf()) {
                    queue.push_back(n.to_path_buf());
                }
            }
        }
        seen
    }
}
