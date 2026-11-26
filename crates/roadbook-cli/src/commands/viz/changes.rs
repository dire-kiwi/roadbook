//! `roadbook viz --diff <range>`: pages that draw what changed in the charts between two git
//! revisions, or a revision and the working tree.
//!
//! Each side is compiled on its own: the `.road` files it has under the project root (less
//! `exclude`, and only those under the paths given, if any), each read from that revision with
//! its imports, and its entry charts found as `roadbook viz` finds them. A chart is matched
//! across the sides by the project path of its entry file, so one can be added, removed or
//! changed; [`super::diff`] matches what is inside. A chart that does not compile at a side gets
//! a page showing that side's diagnostics instead of a drawing. Next to the pages and the index
//! go `summary.md` (for a pull request comment) and `summary.json`. The configuration
//! (`roadbook.toml`) is the working tree's for both sides.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use roadbook_compiler::Ir;
use serde_json::{Value, json};

use super::diff::{self, ChartDiff, Counts, Status};
use super::git::{Repo, RevLoader, Side};
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
    let before = side_charts(ws, &repo, &from, paths, &filter)?;
    let after = side_charts(ws, &repo, &to, paths, &filter)?;

    let entries: BTreeSet<&str> = before
        .iter()
        .chain(&after)
        .map(|(e, _)| e.as_str())
        .collect();
    fn find<'a>(side: &'a Charts, e: &str) -> Option<&'a Result<Ir, String>> {
        side.iter().find(|(x, _)| x == e).map(|(_, r)| r)
    }
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
        let d = diff::diff(b, a, from.label());
        let status = match (b, a) {
            (None, _) => "added",
            (_, None) => "removed",
            _ if d.changed() => "changed",
            _ => "unchanged",
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
                        states: d.map(|d| d.state_counts()).unwrap_or_default(),
                        transitions: d.map(|d| d.transition_counts()).unwrap_or_default(),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let summary = Summary {
        from: from.label().to_string(),
        to: to.label().to_string(),
        charts: &charts,
        names: &names,
        machines: &machines,
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

/// The entry charts of one side, compiled.
fn side_charts(
    ws: &Workspace,
    repo: &Repo,
    side: &Side,
    paths: &[PathBuf],
    filter: &[String],
) -> Result<Charts, String> {
    let options = ws.options();
    let entries = match side {
        Side::WorkTree => {
            let files = ws.inputs(paths)?;
            compile_entries(&mut ws.loader(), &files, &options)?
        }
        Side::Rev { label, commit } => {
            let tracked: HashSet<String> = repo.files(commit)?.into_iter().collect();
            let mut files: Vec<String> = tracked
                .iter()
                .filter(|p| p.ends_with(".road") && !ws.skips(p))
                .filter(|p| {
                    filter.is_empty()
                        || filter.iter().any(|f| {
                            f.is_empty() || f == "." || *p == f || p.starts_with(&format!("{f}/"))
                        })
                })
                .cloned()
                .collect();
            files.sort();
            let mut loader = RevLoader {
                fs: ws.loader(),
                repo,
                label: label.clone(),
                commit: commit.clone(),
                tracked,
            };
            compile_entries(&mut loader, &files, &options)?
        }
    };
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
    states: Counts,
    transitions: Counts,
}

struct Summary<'a> {
    from: String,
    to: String,
    charts: &'a [Chart],
    names: &'a [String],
    machines: &'a [MachineRow],
}

impl Summary<'_> {
    fn totals(&self) -> (Counts, Counts, Counts) {
        let (mut m, mut s, mut t) = (Counts::default(), Counts::default(), Counts::default());
        for r in self.machines {
            match r.status {
                Status::Added => m.added += 1,
                Status::Removed => m.removed += 1,
                Status::Changed => m.changed += 1,
                Status::Unchanged => {}
            }
            s.add(r.states);
            t.add(r.transitions);
        }
        (m, s, t)
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
            "summary": { "machines": m.to_json(), "states": s.to_json(), "transitions": t.to_json() },
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
            "machines": m.to_json(),
            "states": s.to_json(),
            "transitions": t.to_json(),
            "changes": self.changed().map(|(_, r)| json!({
                "machine": r.name,
                "status": r.status.as_str(),
                "chart": self.charts[r.page].entry,
                "page": self.names[r.page],
                "states": r.states.to_json(),
                "transitions": r.transitions.to_json(),
            })).collect::<Vec<_>>(),
            "errors": self.charts.iter().zip(self.names).flat_map(|(c, page)| c.errors.iter().map(move |(side, _)| json!({
                "chart": c.entry,
                "side": side,
                "page": page,
            }))).collect::<Vec<_>>(),
        })
    }

    fn markdown(&self) -> String {
        let (m, s, t) = self.totals();
        let mut out = format!("### Chart changes: `{}` → `{}`\n\n", self.from, self.to);
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
                    format!("- `{}` does not compile at `{label}`\n", c.entry)
                })
            })
            .collect();
        if m.is_empty() && errors.is_empty() {
            out.push_str("No machine changed.\n");
            return out;
        }
        out.push_str(&format!(
            "Machines: {}. States: {}. Transitions: {}.\n\n",
            words(m),
            words(s),
            words(t)
        ));
        if !m.is_empty() {
            out.push_str(
                "| | Machine | Chart | States | Transitions |\n| --- | --- | --- | --- | --- |\n",
            );
            for (_, r) in self.changed() {
                out.push_str(&format!(
                    "| {} {} | `{}` | `{}` | {} | {} |\n",
                    mark(r.status),
                    r.status.as_str(),
                    r.name,
                    self.charts[r.page].entry,
                    signs(r.states),
                    signs(r.transitions)
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
