//! `roadbook viz --diff <range>`: pages that draw what changed in the charts between two git
//! revisions, or a revision and the working tree.
//!
//! Each side is compiled on its own: the `.road` files it has under the project root (less
//! `exclude`, and only those under the paths given, if any), each read from that revision with
//! its imports, and its entry charts found as `roadbook viz` finds them. Every file in the
//! repository is read from the revision, inside the project or not; only files under
//! `node_modules` and outside the repository are read from disk (see [`SideFs`]), and the
//! summary lists those outside the repository. A page is drawn for
//! each entry path of either side. A chart that does not compile at a side gets a page showing
//! that side's diagnostics instead of a drawing. Next to the pages and the index go
//! `summary.md` (for a pull request comment) and `summary.json`. The configuration
//! (`roadbook.toml`) is the working tree's for both sides.
//!
//! **Matching across the project.** A declaration (machine, type, function, action, task) is
//! known on a side by its kind, name and the file it is in. Each one after is matched with the
//! one before in the same file with the same kind and name; those left are matched by kind and
//! name alone where exactly one of that kind and name is left on each side, whatever file and
//! entry chart they are in (a moved one: names are unique within an entry and its imports,
//! not across the project). Each page then compares the after version of its chart with what
//! each of its declarations is matched with before, wherever that was, so renaming a file or
//! importing a chart from another one does not remove and re-add its machines: a machine
//! declared in another file than before is moved. A declaration before that is matched with
//! one after on another page is left out of this page's before version, and a page that is
//! only before and is left with no machine is not drawn: what it had lives on elsewhere.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use roadbook_compiler::{FsLoader, Ir};
use serde_json::{Value, json};

use super::diff::{self, ChartDiff, Counts, Status};
use super::git::{Repo, Side, SideFs};
use super::{VIEWER_FILE, VIEWER_JS, compile_entries, page};
use crate::workspace::Workspace;
use crate::{Exit, report};

/// The charts of one side: entry path, and its IR or the text of its diagnostics.
type Charts = Vec<(String, Result<Ir, String>)>;

pub fn run(
    ws: &Workspace,
    paths: &[PathBuf],
    out: &Path,
    range: &str,
    self_contained: bool,
) -> Exit {
    match write(ws, paths, out, range, self_contained) {
        Ok(()) => Exit::Ok,
        Err(e) => report::fatal(e),
    }
}

/// One chart of the diff: its entry path, how it changed, its merged IR and diff, and the
/// diagnostics of a side where it does not compile.
struct Chart {
    entry: String,
    status: &'static str,
    diff: ChartDiff,
    errors: Vec<(&'static str, String)>,
}

fn write(
    ws: &Workspace,
    paths: &[PathBuf],
    out: &Path,
    range: &str,
    self_contained: bool,
) -> Result<(), String> {
    let repo = Repo::open(&ws.root)?;
    let (from, to) = repo.range(range)?;
    let filter: Vec<String> = paths.iter().map(|p| ws.project_path(p)).collect();
    let mut outside = BTreeSet::new();
    let before = side_charts(ws, &repo, &from, paths, &filter, &mut outside)?;
    let after = side_charts(ws, &repo, &to, paths, &filter, &mut outside)?;
    let outside: Vec<String> = outside.iter().map(|p| repo.display(p)).collect();

    let entries: BTreeSet<&str> = before
        .iter()
        .chain(&after)
        .map(|(e, _)| e.as_str())
        .collect();
    fn find<'a>(side: &'a Charts, e: &str) -> Option<&'a Result<Ir, String>> {
        side.iter().find(|(x, _)| x == e).map(|(_, r)| r)
    }
    let matching = Matching::new(&before, &after);
    let mut charts: Vec<Chart> = Vec::new();
    for e in entries {
        let (b, a) = (find(&before, e), find(&after, e));
        let mut errors = Vec::new();
        if let Some(Err(text)) = b {
            errors.push(("before", text.clone()));
        }
        if let Some(Err(text)) = a {
            errors.push(("after", text.clone()));
        }
        if !errors.is_empty() {
            charts.push(Chart {
                entry: e.to_string(),
                status: "error",
                diff: diff::diff(None, None, from.label()),
                errors,
            });
            continue;
        }
        let (b, a) = (
            b.and_then(|r| r.as_ref().ok()),
            a.and_then(|r| r.as_ref().ok()),
        );
        // What this chart's declarations are matched with before, wherever they were.
        let matched = matching.before_of(b, a, &before);
        let empty = matched.machines.is_empty()
            && matched.types.is_empty()
            && matched.functions.is_empty()
            && matched.actions.is_empty()
            && matched.tasks.is_empty();
        let (d, status) = match (b, a) {
            (None, Some(a)) if empty => (diff::diff(None, Some(a), from.label()), "added"),
            (_, None) if matched.machines.is_empty() => continue, // it all lives on elsewhere
            (_, None) => (diff::diff(Some(&matched), None, from.label()), "removed"),
            (_, a) => {
                let d = diff::diff(Some(&matched), a, from.label());
                let status = if d.changed() { "changed" } else { "unchanged" };
                (d, status)
            }
        };
        charts.push(Chart {
            entry: e.to_string(),
            status,
            diff: d,
            errors: Vec::new(),
        });
    }
    if charts.is_empty() {
        return Err(format!(
            "no machines: neither {} nor {} has a chart that declares one",
            from.label(),
            to.label()
        ));
    }

    // Pages.
    let names = page::file_names(&charts.iter().map(|c| c.entry.as_str()).collect::<Vec<_>>());
    let script = if self_contained {
        page::Script::Inline(VIEWER_JS)
    } else {
        page::Script::Shared(VIEWER_FILE)
    };
    let index_href = page::href(page::INDEX);
    let mut writes: Vec<(PathBuf, String)> = Vec::new();
    for (c, name) in charts.iter().zip(&names) {
        let mut data = c.diff.to_json();
        data.insert("from".into(), from.label().into());
        data.insert("to".into(), to.label().into());
        data.insert("status".into(), c.status.into());
        data.insert(
            "errors".into(),
            c.errors
                .iter()
                .map(|(side, text)| {
                    let label = if *side == "before" {
                        from.label()
                    } else {
                        to.label()
                    };
                    json!({ "side": side, "label": label, "text": text })
                })
                .collect(),
        );
        let html = page::diff_page(
            &c.entry,
            &c.diff.ir,
            &Value::Object(data),
            Some(&index_href),
            &script,
        );
        writes.push((out.join(name), html));
    }

    // The index, with which machine changed how, and the summary.
    let title = ws
        .root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "project".into());
    let pages: Vec<(&str, &Ir)> = charts
        .iter()
        .map(|c| (c.entry.as_str(), &c.diff.ir))
        .collect();
    let mut index = page::project_index(&title, &pages, &names);
    let machines: Vec<MachineRow> = index["machines"]
        .as_array()
        .map(|list| {
            list.iter()
                .map(|m| {
                    let name = m["name"].as_str().unwrap_or_default().to_string();
                    let page = m["page"].as_u64().unwrap_or_default() as usize;
                    let d = charts[page]
                        .diff
                        .machines
                        .iter()
                        .find(|(n, _)| *n == name)
                        .map(|(_, d)| d);
                    MachineRow {
                        name,
                        page,
                        status: d.map_or(Status::Unchanged, |d| d.status),
                        moved: d.and_then(|d| d.moved.clone()),
                        only_moved: d.is_some_and(|d| d.only_moved()),
                        states: d.map(|d| d.state_counts()).unwrap_or_default(),
                        transitions: d.map(|d| d.transition_counts()).unwrap_or_default(),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let decls = decl_rows(&charts);
    let summary = Summary {
        from: from.label().to_string(),
        to: to.label().to_string(),
        charts: &charts,
        names: &names,
        machines: &machines,
        decls: &decls,
        outside: &outside,
    };
    index["diff"] = summary.index_json();
    writes.push((
        out.join(page::INDEX),
        page::index_page(&format!("Changes · {title}"), &index, &script),
    ));
    if !self_contained {
        writes.push((out.join(VIEWER_FILE), VIEWER_JS.to_string()));
    }
    let markdown = summary.markdown();
    writes.push((out.join("summary.md"), markdown.clone()));
    writes.push((out.join("summary.json"), format!("{:#}\n", summary.json())));

    std::fs::create_dir_all(out).map_err(|e| format!("cannot create `{}`: {e}", out.display()))?;
    for (path, text) in &writes {
        std::fs::write(path, text)
            .map_err(|e| format!("cannot write `{}`: {e}", path.display()))?;
    }
    for c in &charts {
        for (side, _) in &c.errors {
            let label = if *side == "before" {
                from.label()
            } else {
                to.label()
            };
            eprintln!(
                "warning: `{}` does not compile at {label}; its page shows the diagnostics",
                c.entry
            );
        }
    }
    if !outside.is_empty() {
        eprintln!(
            "warning: {} outside the repository, so both sides read {} from disk: {}",
            if outside.len() == 1 {
                "a file is"
            } else {
                "files are"
            },
            if outside.len() == 1 { "it" } else { "them" },
            outside
                .iter()
                .map(|f| format!("`{f}`"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    print!("{markdown}");
    eprintln!(
        "wrote {} page{}, {} and the summary to `{}`",
        charts.len(),
        if charts.len() == 1 { "" } else { "s" },
        page::INDEX,
        out.display()
    );
    Ok(())
}

/// A declaration on one side: its kind, name, and the file it is in.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Key {
    kind: &'static str,
    name: String,
    file: String,
}

/// Runs `$body` once per kind of declaration, with `$kind` its name and `$map` the field of
/// [`Ir`] that holds it.
macro_rules! each_kind {
    (|$kind:ident, $map:ident| $body:block) => {{
        macro_rules! one {
            ($k:literal, $m:ident) => {{
                let $kind: &'static str = $k;
                macro_rules! $map {
                    ($ir:expr) => {
                        $ir.$m
                    };
                }
                $body
            }};
        }
        one!("machine", machines);
        one!("type", types);
        one!("function", functions);
        one!("action", actions);
        one!("task", tasks);
    }};
}

/// Which declaration after is which before, across every chart of the project.
struct Matching {
    /// After to before, and before to after.
    a2b: HashMap<Key, Key>,
    b2a: HashMap<Key, Key>,
    /// The chart (index in the before side) each declaration before is first found in.
    where_before: HashMap<Key, usize>,
}

fn key(ir: &Ir, kind: &'static str, name: &str, span: &roadbook_ir::Span) -> Key {
    Key {
        kind,
        name: name.to_string(),
        file: diff::source_text(ir, span.file),
    }
}

/// Every declaration of a side, and the first chart it is found in.
fn declarations(side: &Charts) -> Vec<(Key, usize)> {
    let mut out: Vec<(Key, usize)> = Vec::new();
    let mut seen: HashSet<Key> = HashSet::new();
    for (i, (_, ir)) in side.iter().enumerate() {
        let Ok(ir) = ir else { continue };
        each_kind!(|kind, map| {
            for (name, x) in &map!(ir) {
                let k = key(ir, kind, name, &x.span);
                if seen.insert(k.clone()) {
                    out.push((k, i));
                }
            }
        });
    }
    out
}

impl Matching {
    fn new(before: &Charts, after: &Charts) -> Matching {
        let b = declarations(before);
        let a = declarations(after);
        let mut m = Matching {
            a2b: HashMap::new(),
            b2a: HashMap::new(),
            where_before: b.iter().cloned().collect(),
        };
        // The same file, kind and name.
        for (k, _) in &a {
            if m.where_before.contains_key(k) {
                m.a2b.insert(k.clone(), k.clone());
                m.b2a.insert(k.clone(), k.clone());
            }
        }
        // Then by kind and name, where exactly one of each is left.
        let left = |side: &[(Key, usize)], taken: &HashMap<Key, Key>| {
            let mut by: HashMap<(&'static str, String), Vec<Key>> = HashMap::new();
            for (k, _) in side {
                if !taken.contains_key(k) {
                    by.entry((k.kind, k.name.clone()))
                        .or_default()
                        .push(k.clone());
                }
            }
            by
        };
        let (lb, la) = (left(&b, &m.b2a), left(&a, &m.a2b));
        for (id, ka) in la {
            if let (Some(kb), [ka]) = (lb.get(&id), ka.as_slice())
                && let [kb] = kb.as_slice()
            {
                m.a2b.insert(ka.clone(), kb.clone());
                m.b2a.insert(kb.clone(), ka.clone());
            }
        }
        m
    }

    /// The before version of a chart for its page: its own before version `b` (if any), less
    /// what is matched with a declaration after that this chart (`a`) does not have, plus
    /// what `a`'s declarations are matched with in other charts before. Spans of what comes
    /// from another chart point into copies of that chart's sources, appended.
    fn before_of(&self, b: Option<&Ir>, a: Option<&Ir>, before: &Charts) -> Ir {
        let mut out = b.cloned().unwrap_or_else(diff::empty_ir);
        let mut grafted: HashMap<usize, u32> = HashMap::new();
        each_kind!(|kind, map| {
            let names: Vec<String> = map!(out).keys().cloned().collect();
            for n in names {
                let kb = key(&out, kind, &n, &map!(out)[&n].span);
                let here = a.and_then(|a| map!(a).get(&n).map(|x| key(a, kind, &n, &x.span)));
                let keep = match (self.b2a.get(&kb), &here) {
                    // Matched: kept if this chart has what it is matched with.
                    (Some(ka), here) => here.as_ref() == Some(ka),
                    // Removed, or compared by name with one here that is matched with none.
                    (None, Some(ka)) => !self.a2b.contains_key(ka),
                    (None, None) => true,
                };
                if !keep {
                    map!(out).shift_remove(&n);
                }
            }
            if let Some(a) = a {
                for (n, x) in &map!(a) {
                    let Some(kb) = self.a2b.get(&key(a, kind, n, &x.span)) else {
                        continue;
                    };
                    if map!(out)
                        .get(n)
                        .is_some_and(|y| key(&out, kind, n, &y.span) == *kb)
                    {
                        continue;
                    }
                    let chart = self.where_before[kb];
                    let Ok(from) = &before[chart].1 else {
                        continue;
                    };
                    let offset = *grafted.entry(chart).or_insert_with(|| {
                        let at = out.sources.len() as u32;
                        out.sources.extend(from.sources.iter().cloned());
                        at
                    });
                    let mut y = map!(from)[n].clone();
                    y.shift(offset);
                    map!(out).insert(n.clone(), y);
                }
            }
        });
        out
    }
}

/// Points a declaration's spans at the sources `offset` further on.
trait Shift {
    fn shift(&mut self, offset: u32);
}

impl Shift for roadbook_ir::Machine {
    fn shift(&mut self, offset: u32) {
        diff::shift_machine(self, offset);
    }
}

macro_rules! shift_span {
    ($($t:ty),*) => {$(
        impl Shift for $t {
            fn shift(&mut self, offset: u32) {
                self.span.file += offset;
            }
        }
    )*};
}
shift_span!(
    roadbook_ir::TypeDecl,
    roadbook_ir::FunctionDecl,
    roadbook_ir::ActionDecl,
    roadbook_ir::TaskDecl
);

/// The entry charts of one side, compiled. The files it read from disk outside the repository
/// are added to `outside`.
fn side_charts(
    ws: &Workspace,
    repo: &Repo,
    side: &Side,
    paths: &[PathBuf],
    filter: &[String],
    outside: &mut BTreeSet<PathBuf>,
) -> Result<Charts, String> {
    let options = ws.options();
    let fs = SideFs::new(repo, side)?;
    let files = match fs.project_files() {
        None => ws.inputs(paths)?,
        Some(files) => files
            .into_iter()
            .filter(|p| p.ends_with(".road") && !ws.skips(p))
            .filter(|p| {
                filter.is_empty()
                    || filter.iter().any(|f| {
                        f.is_empty() || f == "." || p == f || p.starts_with(&format!("{f}/"))
                    })
            })
            .collect(),
    };
    // The project's loader, with its packages, reading through the side's file system.
    let project = ws.loader();
    let mut loader = FsLoader::with_fs(&project.root, fs).with_node_modules(project.node_modules);
    loader.packages = project.packages;
    let entries = compile_entries(&mut loader, &files, &options)?;
    outside.extend(loader.fs().outside());
    Ok(entries
        .into_iter()
        .filter_map(|(entry, c)| {
            if c.has_errors() {
                return Some((entry, Err(c.render())));
            }
            let ir = c.ir?;
            (!ir.machines.is_empty()).then_some((entry, Ok(ir)))
        })
        .collect())
}

struct MachineRow {
    name: String,
    page: usize,
    status: Status,
    /// The file it was declared in and is now, if it moved.
    moved: Option<(String, String)>,
    /// Whether moving is all that changed.
    only_moved: bool,
    states: Counts,
    transitions: Counts,
}

impl MachineRow {
    /// `moved` for a machine that only moved, else its status.
    fn status_text(&self) -> &'static str {
        if self.only_moved {
            "moved"
        } else {
            self.status.as_str()
        }
    }
}

/// A type, function, action or task that is not unchanged, and the machines that use it.
struct DeclRow {
    name: String,
    status: Status,
    machines: Vec<String>,
}

/// The declarations that are not unchanged, across the charts, each with the machines that use
/// it (in any chart), in the order of the charts.
fn decl_rows(charts: &[Chart]) -> Vec<DeclRow> {
    let mut out: Vec<DeclRow> = Vec::new();
    for c in charts {
        for (name, e) in &c.diff.decls {
            let users = diff::users(&c.diff.ir, name);
            let row = match out
                .iter_mut()
                .position(|r| r.name == *name && r.status == e.status)
            {
                Some(i) => &mut out[i],
                None => {
                    out.push(DeclRow {
                        name: name.clone(),
                        status: e.status,
                        machines: Vec::new(),
                    });
                    out.last_mut().unwrap()
                }
            };
            for u in users {
                if !row.machines.contains(&u) {
                    row.machines.push(u);
                }
            }
        }
    }
    out
}

/// Machines added, removed, changed and only moved.
#[derive(Clone, Copy, Default)]
struct MachineCounts {
    counts: Counts,
    moved: usize,
}

struct Summary<'a> {
    from: String,
    to: String,
    charts: &'a [Chart],
    names: &'a [String],
    machines: &'a [MachineRow],
    decls: &'a [DeclRow],
    /// The files read from disk for both sides, being outside the repository, relative to the
    /// project root.
    outside: &'a [String],
}

impl Summary<'_> {
    fn totals(&self) -> (MachineCounts, Counts, Counts) {
        let (mut m, mut s, mut t) = (
            MachineCounts::default(),
            Counts::default(),
            Counts::default(),
        );
        for r in self.machines {
            match r.status {
                _ if r.only_moved => m.moved += 1,
                Status::Added => m.counts.added += 1,
                Status::Removed => m.counts.removed += 1,
                Status::Changed => m.counts.changed += 1,
                Status::Unchanged => {}
            }
            s.add(r.states);
            t.add(r.transitions);
        }
        (m, s, t)
    }

    fn decl_counts(&self) -> Counts {
        let mut c = Counts::default();
        for r in self.decls {
            match r.status {
                Status::Added => c.added += 1,
                Status::Removed => c.removed += 1,
                Status::Changed => c.changed += 1,
                Status::Unchanged => {}
            }
        }
        c
    }

    /// The machines that changed, in the order of the index.
    fn changed(&self) -> impl Iterator<Item = (usize, &MachineRow)> {
        self.machines
            .iter()
            .enumerate()
            .filter(|(_, r)| r.status != Status::Unchanged)
    }

    fn index_json(&self) -> Value {
        let (m, s, t) = self.totals();
        json!({
            "from": self.from,
            "to": self.to,
            "summary": { "machines": machine_counts_json(m), "states": s.to_json(), "transitions": t.to_json() },
            "machines": self.machines.iter().map(|r| json!({
                "status": r.status.as_str(),
                "states": r.states.to_json(),
                "transitions": r.transitions.to_json(),
            })).collect::<Vec<_>>(),
            "charts": self.charts.iter().map(|c| json!({
                "entry": c.entry,
                "status": c.status,
                "errors": c.errors.iter().map(|(side, _)| *side).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        })
    }

    fn json(&self) -> Value {
        let (m, s, t) = self.totals();
        json!({
            "from": self.from,
            "to": self.to,
            "changed": self.charts.iter().any(|c| c.status != "unchanged"),
            "machines": machine_counts_json(m),
            "states": s.to_json(),
            "transitions": t.to_json(),
            "declarations": self.decl_counts().to_json(),
            "changes": self.changed().map(|(_, r)| {
                let mut v = json!({
                    "machine": r.name,
                    "status": r.status_text(),
                    "chart": self.charts[r.page].entry,
                    "page": self.names[r.page],
                    "states": r.states.to_json(),
                    "transitions": r.transitions.to_json(),
                });
                if let Some((from, to)) = &r.moved {
                    v["moved"] = json!({ "from": from, "to": to });
                }
                v
            }).collect::<Vec<_>>(),
            "decls": self.decls.iter().map(|r| json!({
                "name": r.name,
                "status": r.status.as_str(),
                "machines": r.machines,
            })).collect::<Vec<_>>(),
            "errors": self.charts.iter().zip(self.names).flat_map(|(c, page)| c.errors.iter().map(move |(side, _)| json!({
                "chart": c.entry,
                "side": side,
                "page": page,
            }))).collect::<Vec<_>>(),
            "outside": self.outside,
        })
    }

    /// The summary for a pull request comment. Everything that comes from the charts or the
    /// command line (paths, names, revisions) goes through [`code`], so a file name cannot
    /// end the table, mention anyone or add a link.
    fn markdown(&self) -> String {
        let mut out = self.changes_markdown();
        if !self.outside.is_empty() {
            let files: Vec<String> = self.outside.iter().map(|f| code(f)).collect();
            out.push_str(&format!(
                "\nRead from disk for both sides, being outside the repository: {}.\n",
                files.join(", ")
            ));
        }
        out
    }

    fn changes_markdown(&self) -> String {
        let (m, s, t) = self.totals();
        let mut out = format!(
            "### Chart changes: {} → {}\n\n",
            code(&self.from),
            code(&self.to)
        );
        let errors: Vec<String> = self
            .charts
            .iter()
            .flat_map(|c| {
                c.errors.iter().map(|(side, _)| {
                    let label = if *side == "before" {
                        &self.from
                    } else {
                        &self.to
                    };
                    format!("- {} does not compile at {}\n", code(&c.entry), code(label))
                })
            })
            .collect();
        let machines_changed = !m.counts.is_empty() || m.moved > 0;
        if !machines_changed && self.decls.is_empty() && errors.is_empty() {
            out.push_str("No machine changed.\n");
            return out;
        }
        let machine_words = match (m.counts.is_empty(), m.moved) {
            (true, 0) => words(m.counts),
            (true, n) => format!("{n} moved"),
            (false, 0) => words(m.counts),
            (false, n) => format!("{}, {n} moved", words(m.counts)),
        };
        out.push_str(&format!(
            "Machines: {machine_words}. States: {}. Transitions: {}.\n\n",
            words(s),
            words(t)
        ));
        if machines_changed {
            out.push_str(
                "| | Machine | Chart | States | Transitions |\n| --- | --- | --- | --- | --- |\n",
            );
            for (_, r) in self.changed() {
                let (mark, status) = if r.only_moved {
                    ("→", "moved")
                } else {
                    (mark(r.status), r.status.as_str())
                };
                let mut chart = code(&self.charts[r.page].entry);
                if let Some((from, to)) = &r.moved {
                    chart.push_str(&format!(" (moved from {} to {})", code(from), code(to)));
                }
                out.push_str(&format!(
                    "| {mark} {status} | {} | {chart} | {} | {} |\n",
                    code(&r.name),
                    signs(r.states),
                    signs(r.transitions)
                ));
            }
        }
        if !self.decls.is_empty() {
            if machines_changed {
                out.push('\n');
            }
            out.push_str(&format!(
                "Declarations: {}.\n\n| | Declaration | Used by |\n| --- | --- | --- |\n",
                words(self.decl_counts())
            ));
            for r in self.decls {
                let users = if r.machines.is_empty() {
                    "no machine".to_string()
                } else {
                    r.machines
                        .iter()
                        .map(|n| code(n))
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                out.push_str(&format!(
                    "| {} {} | {} | {users} |\n",
                    mark(r.status),
                    r.status.as_str(),
                    code(&r.name)
                ));
            }
        }
        if !errors.is_empty() {
            out.push('\n');
            out.extend(errors);
        }
        out
    }
}

fn machine_counts_json(m: MachineCounts) -> Value {
    let mut v = m.counts.to_json();
    v["moved"] = m.moved.into();
    v
}

/// `s` as inline code in GitHub Markdown, safe anywhere in a line or a table cell whatever it
/// holds. Printable ASCII but `` ` ``, `|` and `\` makes a code span, inside which nothing is
/// Markdown, HTML, a mention or a link, and which only those could end (a backslash could
/// escape a following `|` for the table). Anything else makes a `<code>` element with every
/// character but letters, digits and spaces as a numeric character reference, so none of it
/// can end the code or the table cell, start HTML, a link, a mention or a heading, or break
/// the line (a line break shows as `⏎`); GitHub makes no mention or link inside `<code>`.
/// Control and invisible formatting characters (which could reorder the text shown) show as
/// `�`.
fn code(s: &str) -> String {
    let plain = |c: char| (c == ' ' || c.is_ascii_graphic()) && !"`|\\".contains(c);
    if !s.is_empty() && s.chars().all(plain) {
        return format!("`{s}`");
    }
    let mut out = String::from("<code>");
    for c in s.chars() {
        let invisible = matches!(c,
            '\u{200B}'..='\u{200F}' | '\u{2028}'..='\u{202E}' | '\u{2060}'..='\u{206F}'
            | '\u{FEFF}' | '\u{FFF9}'..='\u{FFFB}');
        let shown = match c {
            '\n' => '⏎',
            '\r' => continue,
            _ if c.is_control() || invisible => '\u{FFFD}',
            _ => c,
        };
        if shown.is_ascii_alphanumeric() || shown == ' ' {
            out.push(shown);
        } else {
            out.push_str(&format!("&#{};", shown as u32));
        }
    }
    out.push_str("</code>");
    out
}

/// `+`, `−` or `~`, as the pages mark them.
fn mark(s: Status) -> &'static str {
    match s {
        Status::Added => "+",
        Status::Removed => "−",
        Status::Changed => "~",
        Status::Unchanged => "",
    }
}

/// `+2 −1 ~3`, or `none`.
fn signs(c: Counts) -> String {
    let mut parts = Vec::new();
    if c.added > 0 {
        parts.push(format!("+{}", c.added));
    }
    if c.removed > 0 {
        parts.push(format!("−{}", c.removed));
    }
    if c.changed > 0 {
        parts.push(format!("~{}", c.changed));
    }
    if parts.is_empty() {
        "none".into()
    } else {
        parts.join(" ")
    }
}

/// `2 added, 1 removed, 3 changed`, or `none changed`.
fn words(c: Counts) -> String {
    if c.is_empty() {
        return "none changed".into();
    }
    format!(
        "{} added, {} removed, {} changed",
        c.added, c.removed, c.changed
    )
}

#[cfg(test)]
mod tests {
    use super::code;

    #[test]
    fn code_is_a_span_when_nothing_in_it_can_end_it() {
        assert_eq!(code("shop/place_order.road"), "`shop/place_order.road`");
        assert_eq!(code("origin/main...HEAD~1"), "`origin/main...HEAD~1`");
        assert_eq!(
            code("@octocat [x](https://e.vil)"),
            "`@octocat [x](https://e.vil)`"
        );
        assert_eq!(code("a|b"), "<code>a&#124;b</code>");
        assert_eq!(code("a\\"), "<code>a&#92;</code>");
        assert_eq!(code("`x`"), "<code>&#96;x&#96;</code>");
        assert_eq!(code("a\r\nb"), "<code>a&#9166;b</code>");
        assert_eq!(code("a\u{202E}b"), "<code>a&#65533;b</code>");
        assert_eq!(code("café"), "<code>caf&#233;</code>");
        assert_eq!(code(""), "<code></code>");
    }
}
