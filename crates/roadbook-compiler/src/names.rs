//! Top-level names: the project-wide declaration table, each file's scope (what it declares
//! plus what it imports), and the "unknown name" diagnostics with their suggestions and fixes.

use std::collections::{HashMap, HashSet};

use roadbook_syntax::ast::{Decl, DeclKind, Ident};
use roadbook_syntax::diagnostic::Code;
use roadbook_syntax::{Diagnostic, FileId, Pos, Span};

use crate::codes;
use crate::project::Project;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct DeclId(pub usize);

pub(crate) struct DeclEntry<'p> {
    pub file: usize,
    pub decl: &'p Decl,
    /// A second declaration of a name already declared in the project. Reported once and
    /// otherwise ignored.
    pub duplicate: bool,
}

/// What a name refers to from one file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Found {
    Decl(DeclId),
    /// Imported from a file that could not be loaded: already reported, so not an error here.
    Poisoned,
    Missing,
}

pub(crate) struct Names<'p> {
    pub project: &'p Project,
    pub decls: Vec<DeclEntry<'p>>,
    /// Every name declared in the project, to its first declaration.
    pub global: HashMap<&'p str, DeclId>,
    /// Per file, the names it can use.
    scopes: Vec<HashMap<&'p str, DeclId>>,
    poisoned: Vec<HashSet<&'p str>>,
    /// Names declared more than once. Already reported, and poisoned in every file: which
    /// declaration a use meant is unknown, so checking it against either would mislead.
    duplicated: HashSet<&'p str>,
    /// `(file, name)` for which a fix importing `name` into `file` was offered. Offered once:
    /// applying two identical fixes would import the name twice.
    import_fixes: HashSet<(usize, String)>,
}

/// How a declaration kind is called in messages: "a record type", "an action".
pub(crate) fn kind_phrase(kind: &DeclKind) -> &'static str {
    match kind {
        DeclKind::Record { .. } => "a record type",
        DeclKind::Alias { .. } => "a type alias",
        DeclKind::Enum { .. } => "an enum",
        DeclKind::Error { .. } => "an error",
        DeclKind::Extern => "an extern type",
        DeclKind::Fn { .. } => "a function",
        DeclKind::Action { .. } => "an action",
        DeclKind::Task { .. } => "a task",
        DeclKind::Machine(_) => "a machine",
    }
}

pub(crate) fn is_type(kind: &DeclKind) -> bool {
    matches!(
        kind,
        DeclKind::Record { .. }
            | DeclKind::Alias { .. }
            | DeclKind::Enum { .. }
            | DeclKind::Error { .. }
            | DeclKind::Extern
    )
}

impl<'p> Names<'p> {
    /// Builds the declaration table and the scopes, reporting duplicate declarations and bad
    /// imports.
    pub fn build(project: &'p Project, diags: &mut Vec<Diagnostic>) -> Names<'p> {
        let mut names = Names {
            project,
            decls: Vec::new(),
            global: HashMap::new(),
            scopes: vec![HashMap::new(); project.files.len()],
            poisoned: vec![HashSet::new(); project.files.len()],
            duplicated: HashSet::new(),
            import_fixes: HashSet::new(),
        };
        let mut own: Vec<HashMap<&'p str, DeclId>> = vec![HashMap::new(); project.files.len()];
        for (fi, file) in project.files.iter().enumerate() {
            if file.syntax_errors {
                continue;
            }
            for decl in file.ast.decls() {
                let id = DeclId(names.decls.len());
                let name = decl.name.name.as_str();
                let duplicate = match names.global.get(name) {
                    Some(&first) => {
                        let first = &names.decls[first.0];
                        let message = if first.file == fi {
                            format!("`{name}` is declared twice")
                        } else {
                            let other = &project.files[first.file].path;
                            format!("`{name}` is already declared in `{other}`")
                        };
                        diags.push(
                            Diagnostic::error(codes::DUPLICATE_DECLARATION, decl.name.span, message)
                            .with_secondary(first.decl.name.span, "first declared here")
                            .with_help(
                                "declaration names are unique among the files compiled together (an entry file and what it imports), because the IR is keyed by name",
                            ),
                        );
                        names.duplicated.insert(name);
                        true
                    }
                    None => {
                        names.global.insert(name, id);
                        false
                    }
                };
                own[fi].entry(name).or_insert(id);
                names.scopes[fi].entry(name).or_insert(id);
                names.decls.push(DeclEntry {
                    file: fi,
                    decl,
                    duplicate,
                });
            }
        }
        // In the files of a compiled IR, every name of the IR is in scope: the IR was checked
        // when it was compiled, and its names are unique.
        for group in &project.groups {
            for &fi in group {
                if project.files[fi].compiled.is_none() {
                    continue;
                }
                for &other in group {
                    for (&name, &id) in &own[other] {
                        names.scopes[fi].entry(name).or_insert(id);
                    }
                }
            }
        }
        for (fi, file) in project.files.iter().enumerate() {
            if file.syntax_errors {
                continue;
            }
            let mut imported: HashMap<&str, Span> = HashMap::new();
            for n in file.ast.imports().flat_map(|i| &i.names) {
                if let Some(first) = imported.get(n.name.as_str()) {
                    diags.push(
                        Diagnostic::error(
                            codes::DUPLICATE_NAME,
                            n.span,
                            format!("`{}` is imported twice", n.name),
                        )
                        .with_secondary(*first, "first imported here")
                        .with_help("import each name once"),
                    );
                } else {
                    imported.insert(&n.name, n.span);
                }
            }
            for (import, target) in file.ast.imports().zip(&file.imports) {
                let Some(target) = target.filter(|&t| !project.files[t].syntax_errors) else {
                    for n in &import.names {
                        names.poisoned[fi].insert(n.name.as_str());
                    }
                    continue;
                };
                for n in &import.names {
                    match own[target].get(n.name.as_str()) {
                        Some(&id) => {
                            names.scopes[fi].entry(n.name.as_str()).or_insert(id);
                        }
                        None => {
                            let target_path = &project.files[target].path;
                            let mut d = Diagnostic::error(
                                codes::IMPORT_NOT_DECLARED,
                                n.span,
                                format!("`{target_path}` does not declare `{}`", n.name),
                            );
                            let candidates = own[target].keys().copied();
                            if let Some(s) = suggest(&n.name, candidates) {
                                d = d.with_help(format!("did you mean `{s}`?")).with_fix(
                                    format!("import `{s}`"),
                                    n.span,
                                    s,
                                );
                            } else if let Some(&id) = names.global.get(n.name.as_str()) {
                                let elsewhere = &project.files[names.decls[id.0].file].path;
                                d = d.with_help(format!(
                                    "`{}` is declared in `{elsewhere}`; import it from there",
                                    n.name
                                ));
                            }
                            diags.push(d);
                            names.poisoned[fi].insert(n.name.as_str());
                        }
                    }
                }
            }
        }
        names
    }

    pub fn decl(&self, id: DeclId) -> &'p Decl {
        self.decls[id.0].decl
    }

    pub fn lookup(&self, file: usize, name: &str) -> Found {
        if self.duplicated.contains(name) {
            Found::Poisoned
        } else if let Some(&id) = self.scopes[file].get(name) {
            Found::Decl(id)
        } else if self.poisoned[file].contains(name) {
            Found::Poisoned
        } else {
            Found::Missing
        }
    }

    /// "unknown {what} `name`", with a fix that imports it if it is declared elsewhere in the
    /// project, or one that corrects the spelling if a name of the right kind is close.
    pub fn unknown(
        &mut self,
        file: usize,
        ident: &Ident,
        code: Code,
        what: &str,
        accept: impl Fn(&DeclKind) -> bool,
    ) -> Diagnostic {
        let name = ident.name.as_str();
        let mut d = Diagnostic::error(code, ident.span, format!("unknown {what} `{name}`"));
        if let Some(&id) = self.global.get(name) {
            let e = &self.decls[id.0];
            if accept(&e.decl.kind) && e.file != file {
                let target = &self.project.files[e.file].path;
                d = d.with_help(format!("`{name}` is declared in `{target}`; import it"));
                return self.import_fix(d, file, e.file, name);
            }
        }
        let candidates = self.scopes[file]
            .iter()
            .filter(|(_, id)| accept(&self.decls[id.0].decl.kind))
            .map(|(n, _)| *n);
        if let Some(s) = suggest(name, candidates) {
            d = d.with_help(format!("did you mean `{s}`?")).with_fix(
                format!("replace with `{s}`"),
                ident.span,
                s,
            );
        }
        d
    }

    /// "`name` is a task, not a function".
    pub fn wrong_kind(&self, ident: &Ident, id: DeclId, wanted: &str) -> Diagnostic {
        let kind = kind_phrase(&self.decl(id).kind);
        Diagnostic::error(
            codes::WRONG_KIND,
            ident.span,
            format!("`{}` is {kind}, not {wanted}", ident.name),
        )
        .with_secondary(self.decl(id).name.span, "declared here")
    }

    /// Adds a fix that imports `name` from file `from` into file `file`: added to an existing
    /// import of that file if there is one, else as a new import at the top.
    fn import_fix(&mut self, d: Diagnostic, file: usize, from: usize, name: &str) -> Diagnostic {
        if !self.import_fixes.insert((file, name.to_string())) {
            return d;
        }
        let f = &self.project.files[file];
        for (import, target) in f.ast.imports().zip(&f.imports) {
            if *target == Some(from)
                && let Some(last) = import.names.last()
            {
                return d.with_fix(
                    format!("add `{name}` to the import from `{}`", import.path.value),
                    last.span.shrink_to_end(),
                    format!(", {name}"),
                );
            }
        }
        let target = &self.project.files[from].file;
        let spec = if target.package == f.file.package {
            relative(&f.file.path, &target.path)
        } else if target.package.is_some() {
            target.to_string()
        } else {
            // A package cannot import a file of the project that imports it.
            return d;
        };
        let start = Pos {
            offset: 0,
            line: 1,
            col: 1,
        };
        let at = Span {
            file: FileId(file as u32),
            start,
            end: start,
        };
        d.with_fix(
            format!("import `{name}` from \"{spec}\""),
            at,
            format!("import {{ {name} }} from \"{spec}\"\n"),
        )
    }
}

/// The import path that names project path `to` from the file at project path `from`.
pub(crate) fn relative(from: &str, to: &str) -> String {
    let from_dir: Vec<&str> = from.split('/').collect::<Vec<_>>();
    let from_dir = &from_dir[..from_dir.len() - 1];
    let to_parts: Vec<&str> = to.split('/').collect();
    let common = from_dir
        .iter()
        .zip(&to_parts)
        .take_while(|(a, b)| a == b)
        .count()
        .min(to_parts.len() - 1);
    let mut out: Vec<&str> = vec![".."; from_dir.len() - common];
    out.extend(&to_parts[common..]);
    let s = out.join("/");
    if s.starts_with("..") {
        s
    } else {
        format!("./{s}")
    }
}

/// The candidate closest to `name` by edit distance, if it is close enough to be a likely
/// misspelling: at most one edit for short names, a third of the length for longer ones.
pub(crate) fn suggest<'a>(
    name: &str,
    candidates: impl Iterator<Item = &'a str>,
) -> Option<&'a str> {
    let limit = (name.chars().count() / 3).max(1);
    let mut best: Option<(usize, &'a str)> = None;
    let mut all: Vec<&'a str> = candidates.filter(|c| *c != name).collect();
    all.sort_unstable();
    for c in all {
        let d = distance(&name.to_lowercase(), &c.to_lowercase());
        if d <= limit && best.is_none_or(|(b, _)| d < b) {
            best = Some((d, c));
        }
    }
    best.map(|(_, c)| c)
}

/// Levenshtein distance, counting a swap of two adjacent characters as one edit.
fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut d = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut v = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                v = v.min(d[i - 2][j - 2] + 1);
            }
            d[i][j] = v;
        }
    }
    d[a.len()][b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggestions() {
        let c = ["Charging", "Charged", "Backoff"];
        assert_eq!(suggest("Chargd", c.into_iter()), Some("Charged"));
        assert_eq!(suggest("Bakcoff", c.into_iter()), Some("Backoff"));
        assert_eq!(suggest("Nothing", c.into_iter()), None);
        assert_eq!(suggest("Charged", c.into_iter()), None);
    }

    #[test]
    fn relative_paths() {
        assert_eq!(relative("a/b.road", "a/c.road"), "./c.road");
        assert_eq!(relative("a/b.road", "c.road"), "../c.road");
        assert_eq!(relative("b.road", "a/c.road"), "./a/c.road");
        assert_eq!(relative("x/y/b.road", "x/z/c.road"), "../z/c.road");
    }
}
