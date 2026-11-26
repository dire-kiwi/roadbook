//! The files the server knows: open documents, the projects they belong to, which file
//! imports which, and the latest compilation of each group of files that import one another.

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

use lsp_types::Uri;
use roadbook_compiler::loader::{FileRef, PackageFile};
use roadbook_compiler::{Compilation, FileId, FsLoader, Loaded, Loader, MemoryLoader, Options};
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
    /// The project's root; [`MEMORY`] for a document that is not a file.
    pub project: PathBuf,
    /// The files of the compilation that belong to another package (reached by a package's
    /// name): only errors are reported in them, and rename does not edit them.
    pub packaged: HashSet<PathBuf>,
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
    /// from. Files of packages the project's files import are included.
    imports: HashMap<PathBuf, (u64, Vec<PathBuf>)>,
    /// The files of packages that the project's files import: the name each is reached by,
    /// and where its package was found, so that its own imports resolve as the compiler
    /// resolves them.
    packaged: HashMap<PathBuf, (FileRef, PackageFile)>,
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

/// The directory the keys of documents that are not files live in (an editor's untitled
/// buffers): a relative path holding a NUL, so never one on disk.
pub const MEMORY: &str = "\0memory";

/// The key of a document that is not a file, by its URI: one path segment, the URI with
/// `%`, `/` and `\\` escaped.
pub fn memory_key(uri: &Uri) -> PathBuf {
    let escaped = uri
        .as_str()
        .replace('%', "%25")
        .replace('/', "%2F")
        .replace('\\', "%5C");
    Path::new(MEMORY).join(escaped)
}

pub fn is_memory(path: &Path) -> bool {
    path.starts_with(MEMORY)
}

/// The URI of a document that is not a file, from its key.
fn memory_uri(path: &Path) -> Option<Uri> {
    let escaped = path.strip_prefix(MEMORY).ok()?.to_str()?;
    let s = escaped
        .replace("%2F", "/")
        .replace("%5C", "\\")
        .replace("%25", "%");
    s.parse().ok()
}

/// The name a document that is not a file is compiled by: its URI's last segment, as a
/// `.road` file.
fn memory_name(uri: &Uri) -> String {
    let s = uri.as_str();
    let last = s.rsplit(['/', ':']).next().unwrap_or("");
    let last: String = last
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let last = if last.is_empty() {
        "untitled".to_string()
    } else {
        last
    };
    if last.ends_with(".road") {
        last
    } else {
        format!("{last}.road")
    }
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

/// The project path of `path` in the project at `root`, if it is one of the project's own
/// files: inside the root, and not in a `node_modules` directory, which holds other packages
/// (`roadbook check` never searches one).
fn project_path(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    (!parts.is_empty() && !parts.iter().any(|p| p == "node_modules")).then(|| parts.join("/"))
}

/// What `roadbook/ir` answers: one file compiled as an entry.
pub struct EntryIr {
    pub out: Compilation,
    /// The project's root, `None` for a document that is not a file.
    pub root: Option<PathBuf>,
    /// The entry's name in the compilation: its project path.
    pub entry: String,
}

/// The file system with the open documents laid over it.
pub struct Overlay<'a> {
    inner: FsLoader,
    docs: &'a HashMap<PathBuf, Doc>,
    /// Texts that replace both, for a trial compilation.
    edits: &'a HashMap<PathBuf, String>,
    /// The files loaded as files of a package.
    packaged: HashSet<PathBuf>,
}

impl Loader for Overlay<'_> {
    fn resolve(&mut self, importer: &FileRef, spec: &str) -> Result<FileRef, String> {
        self.inner.resolve(importer, spec)
    }

    fn load(&mut self, file: &FileRef) -> Result<Loaded, String> {
        if let Some(at) = self.inner.location(file).map(|p| key(&p)) {
            if file.package.is_some() {
                self.packaged.insert(at.clone());
            }
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
            None => memory_uri(path).unwrap_or_else(|| path_uri(path)),
        }
    }

    /// The current text of a file: its open document, else what is on disk.
    pub fn text(&self, path: &Path) -> Option<String> {
        match self.docs.get(path) {
            Some(d) => Some(d.text.clone()),
            None if is_memory(path) => None,
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
                    packaged: HashMap::new(),
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
        let mut out = Vec::new();
        for path in dirty {
            if is_memory(&path) {
                match self.memory_analysis(&path) {
                    Some(a) => out.push(a),
                    None => gone.push(path),
                }
                continue;
            }
            match self.project_for(&path) {
                Ok(p) => by_project.entry(p).or_default().push(path),
                Err(_) => gone.push(path),
            }
        }
        let mut projects: Vec<(usize, Vec<PathBuf>)> = by_project.into_iter().collect();
        projects.sort();
        for (p, changed) in projects {
            let (analysis, removed) = self.recheck(p, &changed);
            gone.extend(removed);
            if let Some(a) = analysis {
                out.push(a);
            }
        }
        let out: Vec<Rc<Analysis>> = out.into_iter().map(Rc::new).collect();
        for a in &out {
            for path in a.paths.iter().flatten() {
                if a.roots.contains(path) {
                    self.analyses.insert(path.clone(), a.clone());
                } else {
                    self.analyses
                        .entry(path.clone())
                        .or_insert_with(|| a.clone());
                }
            }
        }
        for g in &gone {
            self.analyses.remove(g);
        }
        (out, gone)
    }

    /// A document that is not a file, compiled on its own: it has no project and can import
    /// nothing. `None` once it is closed.
    fn memory_analysis(&self, path: &Path) -> Option<Analysis> {
        let doc = self.docs.get(path)?;
        let name = memory_name(&doc.uri);
        let mut loader = MemoryLoader::new().with(&name, doc.text.clone());
        let out = roadbook_compiler::check(&mut loader, &[name], &Options::default()).ok()?;
        Some(Analysis {
            out,
            paths: vec![Some(path.to_path_buf())],
            roots: vec![path.to_path_buf()],
            project: PathBuf::from(MEMORY),
            packaged: HashSet::new(),
        })
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
                // A file of another package: never an entry. Its diagnostics (errors only)
                // come from compiling the project's files that import it, as with
                // `roadbook check`.
                (_, None) => {}
            }
        }
        roots.sort();
        if roots.is_empty() {
            return (None, removed);
        }
        let names: Vec<String> = roots.iter().map(|(n, _)| n.clone()).collect();
        let no_edits = HashMap::new();
        let analysis = self
            .compile(p, &names, &no_edits)
            .map(|(out, packaged)| Analysis {
                paths: self.paths(&root, &out),
                out,
                roots: roots.into_iter().map(|(_, p)| p).collect(),
                project: root,
                packaged,
            });
        (analysis, removed)
    }

    fn overlay<'a>(&'a self, p: usize, edits: &'a HashMap<PathBuf, String>) -> Overlay<'a> {
        Overlay {
            inner: self.projects[p].config.loader.clone(),
            docs: &self.docs,
            edits,
            packaged: HashSet::new(),
        }
    }

    /// Checks the entries `roots` of project `p`; with the files loaded as files of a
    /// package.
    fn compile(
        &self,
        p: usize,
        roots: &[String],
        edits: &HashMap<PathBuf, String>,
    ) -> Option<(Compilation, HashSet<PathBuf>)> {
        let mut loader = self.overlay(p, edits);
        let out =
            roadbook_compiler::check(&mut loader, roots, &self.projects[p].config.options).ok()?;
        Some((out, loader.packaged))
    }

    /// Compiles the entries of `analysis` again with some files' texts replaced, to see what
    /// an edit would do.
    pub fn trial(
        &self,
        analysis: &Analysis,
        edits: &HashMap<PathBuf, String>,
    ) -> Option<Compilation> {
        if is_memory(&analysis.project) {
            let path = analysis.roots.first()?;
            let doc = self.docs.get(path)?;
            let name = memory_name(&doc.uri);
            let text = edits.get(path).unwrap_or(&doc.text).clone();
            let mut loader = MemoryLoader::new().with(&name, text);
            return roadbook_compiler::check(&mut loader, &[name], &Options::default()).ok();
        }
        let p = self
            .projects
            .iter()
            .position(|p| p.config.root == analysis.project)?;
        let roots: Vec<String> = analysis
            .roots
            .iter()
            .filter_map(|r| project_path(&analysis.project, r))
            .collect();
        self.compile(p, &roots, edits).map(|(out, _)| out)
    }

    /// Compiles one file as an entry, as `roadbook export ir` does, with the open documents
    /// laid over the file system: for `roadbook/ir`.
    pub fn entry_ir(&mut self, path: &Path) -> Result<EntryIr, String> {
        if is_memory(path) {
            let doc = self
                .docs
                .get(path)
                .ok_or("the document is not open in the language server")?;
            let name = memory_name(&doc.uri);
            let out = roadbook_compiler::compile_source(&name, &doc.text, &Options::default());
            return Ok(EntryIr {
                out,
                root: None,
                entry: name,
            });
        }
        let p = self.project_for(path)?;
        let root = self.projects[p].config.root.clone();
        let Some(entry) = project_path(&root, path) else {
            return Err(format!(
                "`{}` is a file of another package, not of the project at `{}`",
                path.display(),
                root.display()
            ));
        };
        let no_edits = HashMap::new();
        let mut loader = self.overlay(p, &no_edits);
        let out = roadbook_compiler::compile(&mut loader, &entry, &self.projects[p].config.options)
            .map_err(|e| e.to_string())?;
        Ok(EntryIr {
            out,
            root: Some(root),
            entry,
        })
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
    /// is to be rescanned, and of the files of packages they import.
    fn scan(&mut self, p: usize, changed: &[PathBuf]) {
        let rescan = std::mem::replace(&mut self.projects[p].rescan, false);
        let root = self.projects[p].config.root.clone();
        let listed: HashSet<PathBuf> = self.projects[p]
            .config
            .files
            .iter()
            .map(|f| key(&root.join(f)))
            .collect();
        let mut files: BTreeSet<PathBuf> = BTreeSet::new();
        if rescan {
            files.extend(listed.iter().cloned());
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
        files.extend(
            changed
                .iter()
                .filter(|c| self.projects[p].packaged.contains_key(*c))
                .cloned(),
        );
        // The project's own files first: they name the files of packages, whose imports are
        // then resolved from those names.
        let (own, other): (Vec<PathBuf>, Vec<PathBuf>) = files
            .into_iter()
            .partition(|f| project_path(&root, f).is_some());
        let mut queue: VecDeque<PathBuf> = own.into_iter().chain(other).collect();
        let mut seen: HashSet<PathBuf> = HashSet::new();
        let mut loader = self.projects[p].config.loader.clone();
        while let Some(path) = queue.pop_front() {
            if !seen.insert(path.clone()) {
                continue;
            }
            let project = &mut self.projects[p];
            let importer = match project_path(&root, &path) {
                Some(name) => FileRef::project(&name),
                None => match project.packaged.get(&path) {
                    Some((file, found)) => {
                        loader.remember(file, found);
                        file.clone()
                    }
                    // A file of a package that no file of the project imports.
                    None => {
                        project.imports.remove(&path);
                        continue;
                    }
                },
            };
            let package = importer.package.is_some();
            let text = match self.docs.get(&path) {
                Some(d) => Some(d.text.clone()),
                None if package
                    || listed.contains(&path)
                    || project.imports.contains_key(&path) =>
                {
                    std::fs::read_to_string(&path).ok()
                }
                None => None,
            };
            let Some(text) = text else {
                project.imports.remove(&path);
                continue;
            };
            let h = hash(&text);
            if project.imports.get(&path).is_some_and(|(old, _)| *old == h) {
                continue;
            }
            let (ast, _) = parse_file(FileId(0), &text);
            let mut targets = Vec::new();
            for i in ast.imports() {
                let Ok(file) = loader.resolve(&importer, &i.path.value) else {
                    continue;
                };
                let Some(at) = loader.location(&file).map(|l| key(&l)) else {
                    continue;
                };
                if file.package.is_some()
                    && project_path(&root, &at).is_none()
                    && let Some(found) = loader.package_file(&file)
                {
                    let renamed = project
                        .packaged
                        .get(&at)
                        .is_none_or(|(old, _)| *old != file);
                    project.packaged.insert(at.clone(), (file, found));
                    if renamed {
                        // Its imports are to be resolved again, from its new name.
                        project.imports.remove(&at);
                    }
                    if renamed || !project.imports.contains_key(&at) {
                        queue.push_back(at.clone());
                    }
                }
                targets.push(at);
            }
            project.imports.insert(path, (h, targets));
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
