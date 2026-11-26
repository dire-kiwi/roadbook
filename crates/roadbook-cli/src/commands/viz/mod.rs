//! `roadbook viz [paths…] [-o dir] [--self-contained] [--trace run.jsonl | --coverage traces… | --diff range] [--watch [--port N]]`
//! (`--coverage` takes one file or directory and is repeated for more:
//! `--coverage a.jsonl --coverage traces/`)
//!
//! Writes one HTML page per entry chart, plus `index.html`, the project index. An entry chart
//! is a `.road` file given (or found under a directory given, or in the project if none is)
//! that no other of those files imports and that declares a machine; its page draws every
//! machine of its compilation. Each page holds its IR as JSON and loads the viewer script
//! `roadbook-viewer.js`, written once next to the pages, so a directory of pages shares one copy
//! and still opens from `file://`. `--self-contained` inlines the script in every page instead.
//! `--trace` puts a trace (spec/trace.md) in the pages that have its top-level run's machine,
//! which then open in replay mode. `--coverage` puts many traces in the pages that have a
//! machine of theirs, which then open in coverage mode; a line that is not a trace record (a truncated last line, as
//! a production trace often has) is skipped with a warning, as the page does. `--diff` draws what changed between two
//! git revisions instead (`changes.rs`, `diff.rs`, `git.rs`).
//!
//! The viewer script is `packages/viewer` built by its `build.mjs` into `assets/`, and is
//! included here at compile time, so building the CLI needs no Node.

mod changes;
mod diff;
mod git;
mod page;
mod serve;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use roadbook_compiler::{Compilation, Ir};

use crate::workspace::Workspace;
use crate::{Exit, report};

/// The built viewer (`packages/viewer`, `pnpm --filter @roadbook/viewer build`).
pub const VIEWER_JS: &str = include_str!("../../../assets/roadbook-viewer.js");

/// The name of the shared viewer script in the output directory.
pub const VIEWER_FILE: &str = "roadbook-viewer.js";

/// The output directory when `-o` is not given, relative to the current directory.
pub const DEFAULT_OUT: &str = "roadbook-viz";

#[derive(clap::Args)]
pub struct Args {
    /// Files or directories of charts. Default: every `.road` file in the project.
    paths: Vec<PathBuf>,
    /// The directory to write the pages to.
    #[arg(short, long = "out", value_name = "DIR", default_value = DEFAULT_OUT)]
    out: PathBuf,
    /// Put the viewer script inside every page, so each page is one file that works alone,
    /// instead of sharing one `roadbook-viewer.js` next to them.
    #[arg(long)]
    self_contained: bool,
    /// Rebuild when a chart changes, and serve the pages on a local port, reloading them.
    #[arg(long)]
    watch: bool,
    /// The port `--watch` serves on (0: any free port).
    #[arg(long, default_value_t = 7357, requires = "watch")]
    port: u16,
    /// A trace (JSON Lines, as a run writes it) to replay: it goes into the page of every chart
    /// that has the machine of its first top-level run, and those pages open in replay mode.
    #[arg(long, value_name = "FILE", conflicts_with = "coverage")]
    trace: Option<PathBuf>,
    /// Draw what changed between two git revisions: `main..HEAD`, `main...HEAD` (since HEAD
    /// forked from main), or one revision, against the working tree. Writes `summary.md` and
    /// `summary.json` next to the pages.
    #[arg(long, value_name = "RANGE", conflicts_with_all = ["watch", "trace", "coverage"])]
    diff: Option<String>,
    /// A trace (a JSON Lines file, or a directory searched for `*.jsonl`) to shade the charts
    /// with: how often each transition was taken and each state entered. Repeat it for more
    /// (`--coverage a.jsonl --coverage traces/`). The pages open in coverage mode.
    #[arg(long, value_name = "TRACES")]
    coverage: Vec<PathBuf>,
}

/// How `build` writes the pages: with the viewer script inline or shared, and with a trace.
pub struct BuildOptions<'a> {
    pub self_contained: bool,
    pub trace: Option<&'a Path>,
    /// `--coverage`: trace files and directories of them.
    pub coverage: &'a [PathBuf],
}

/// What one build wrote, and the files it read (for `--watch`).
pub struct Built {
    pub pages: Vec<PathBuf>,
    pub inputs: Vec<PathBuf>,
}

pub fn run(args: Args) -> Exit {
    let ws = match Workspace::find(args.paths.first().map(PathBuf::as_path)) {
        Ok(ws) => ws,
        Err(e) => return report::fatal(e),
    };
    if let Some(range) = &args.diff {
        return changes::run(&ws, &args.paths, &args.out, range, args.self_contained);
    }
    let options = BuildOptions {
        self_contained: args.self_contained,
        trace: args.trace.as_deref(),
        coverage: &args.coverage,
    };
    let (exit, built) = build(&ws, &args.paths, &args.out, &options);
    if !args.watch {
        return exit;
    }
    serve::watch(&ws, &args, built)
}

/// Compiles the charts and writes the pages. On errors writes nothing.
pub fn build(
    ws: &Workspace,
    paths: &[PathBuf],
    out: &Path,
    options: &BuildOptions,
) -> (Exit, Built) {
    let self_contained = options.self_contained;
    let mut built = Built {
        pages: Vec::new(),
        inputs: Vec::new(),
    };
    let trace = match options.trace.map(read_trace).transpose() {
        Ok(t) => t,
        Err(e) => return (report::fatal(e), built),
    };
    let coverage = match read_coverage(options.coverage) {
        Ok(c) => c,
        Err(e) => return (report::fatal(e), built),
    };
    let files = match ws.inputs(paths) {
        Ok(f) => f,
        Err(e) => return (report::fatal(e), built),
    };
    let entries = match compile_entries(&mut ws.loader(), &files, &ws.options()) {
        Ok(e) => e,
        Err(e) => return (report::fatal(e), built),
    };
    let mut read: Vec<String> = entries
        .iter()
        .flat_map(|(_, c)| c.files.iter().map(|(p, _)| p.clone()))
        .collect();
    read.sort();
    read.dedup();
    built.inputs = read.iter().map(|p| ws.disk_path(p)).collect();

    let mut exit = Exit::Ok;
    for (_, c) in &entries {
        if report::diagnostics(c, false) == Exit::Errors {
            exit = Exit::Errors;
        }
    }
    if exit == Exit::Errors {
        return (exit, built);
    }
    let charts: Vec<(&str, &Ir)> = entries
        .iter()
        .filter_map(|(f, c)| c.ir.as_ref().map(|ir| (f.as_str(), ir)))
        .filter(|(_, ir)| !ir.machines.is_empty())
        .collect();
    if charts.is_empty() {
        return (
            report::fatal("no machines: none of the charts given declares a machine"),
            built,
        );
    }

    // The trace goes into the pages that have its first top-level run's machine.
    if let Some(t) = &trace {
        let with: Vec<&Ir> = charts
            .iter()
            .map(|(_, ir)| *ir)
            .filter(|ir| ir.machines.contains_key(&t.machine))
            .collect();
        if with.is_empty() {
            return (
                report::fatal(format!(
                    "`{}`: its run {} is of machine {}, which none of the charts has",
                    t.name, t.run, t.machine
                )),
                built,
            );
        }
        if with.iter().any(|ir| ir.machines[&t.machine].hash != t.hash) {
            eprintln!(
                "warning: `{}` was recorded from another version of {} (IR {}); the viewer will say so",
                t.name, t.machine, t.hash
            );
        }
    }

    if !options.coverage.is_empty() && !coverage_warnings(&coverage, &charts) {
        return (
            report::fatal(
                "--coverage: none of the traces has a run of a machine of the charts given",
            ),
            built,
        );
    }

    let names = page::file_names(&charts.iter().map(|(f, _)| *f).collect::<Vec<_>>());
    let title = ws
        .root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "project".into());
    let script = if self_contained {
        page::Script::Inline(VIEWER_JS)
    } else {
        page::Script::Shared(VIEWER_FILE)
    };
    let mut writes: Vec<(PathBuf, String)> = Vec::new();
    let index_href = page::href(page::INDEX);
    for ((entry, ir), name) in charts.iter().zip(&names) {
        let extra = if let Some(t) = trace
            .as_ref()
            .filter(|t| ir.machines.contains_key(&t.machine))
        {
            page::Extra::Trace(&t.name, &t.text)
        } else if !options.coverage.is_empty() {
            // Each page gets the traces with a run of one of its machines.
            page::Extra::Coverage(
                coverage
                    .iter()
                    .filter(|c| c.runs.iter().any(|r| ir.machines.contains_key(&r.machine)))
                    .map(|c| (c.name.as_str(), c.text.as_str()))
                    .collect(),
            )
        } else {
            page::Extra::None
        };
        let html = page::chart_page(entry, ir, Some(&index_href), &extra, &script);
        writes.push((out.join(name), html));
    }
    let index = page::project_index(&title, &charts, &names);
    writes.push((
        out.join(page::INDEX),
        page::index_page(&title, &index, &script),
    ));
    if !self_contained {
        writes.push((out.join(VIEWER_FILE), VIEWER_JS.to_string()));
    }

    if let Err(e) = std::fs::create_dir_all(out) {
        return (
            report::fatal(format!("cannot create `{}`: {e}", out.display())),
            built,
        );
    }
    for (path, text) in &writes {
        // Unchanged files are left alone, so a watching browser or tool sees no change.
        if std::fs::read_to_string(path).is_ok_and(|old| &old == text) {
            continue;
        }
        if let Err(e) = std::fs::write(path, text) {
            return (
                report::fatal(format!("cannot write `{}`: {e}", path.display())),
                built,
            );
        }
    }
    built.pages = writes
        .iter()
        .map(|(p, _)| p.clone())
        .filter(|p| p.extension().is_some_and(|e| e == "html"))
        .collect();
    eprintln!(
        "wrote {} page{} and {} to `{}`",
        charts.len(),
        if charts.len() == 1 { "" } else { "s" },
        page::INDEX,
        out.display()
    );
    (exit, built)
}

/// Compiles each file and keeps the entries: the files no other of them imports.
pub fn compile_entries(
    loader: &mut dyn roadbook_compiler::Loader,
    files: &[String],
    options: &roadbook_compiler::Options,
) -> Result<Vec<(String, Compilation)>, String> {
    let mut units: Vec<(String, Compilation)> = Vec::new();
    for f in files {
        let c = roadbook_compiler::compile(loader, f, options).map_err(|e| e.to_string())?;
        units.push((f.clone(), c));
    }
    let imported: HashSet<String> = units
        .iter()
        .flat_map(|(f, c)| {
            c.files
                .iter()
                .skip(1)
                .map(|(p, _)| p.clone())
                .filter(move |p| p != f)
        })
        .collect();
    units.retain(|(f, _)| !imported.contains(f));
    Ok(units)
}

/// A trace file given with `--coverage`: its name (the path as given), text and runs.
struct CoverageTrace {
    name: String,
    text: String,
    runs: Vec<RunHead>,
}

/// A run, from its `started` or `restored` record.
struct RunHead {
    machine: String,
    hash: String,
    records: usize,
}

/// The files of `--coverage`: each file given, and every `*.jsonl` under each directory given,
/// sorted. Each is read as a trace: JSON Lines of records with a `kind` and a `run`. A line
/// that is not one (a truncated last line is usual in a production trace) is skipped, and the
/// skipped lines of each file are counted in a warning; the page lists them too.
fn read_coverage(paths: &[PathBuf]) -> Result<Vec<CoverageTrace>, String> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
        let entries = std::fs::read_dir(dir)
            .map_err(|e| format!("cannot read the directory `{}`: {e}", dir.display()))?;
        let mut entries: Vec<PathBuf> = entries.filter_map(|e| Some(e.ok()?.path())).collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                walk(&p, out)?;
            } else if p.extension().is_some_and(|e| e == "jsonl") {
                out.push(p);
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    for p in paths {
        if p.is_dir() {
            walk(p, &mut files)?;
        } else {
            files.push(p.clone());
        }
    }
    if !paths.is_empty() && files.is_empty() {
        return Err("--coverage: no `.jsonl` files in the directories given".into());
    }
    let mut out = Vec::new();
    for path in files {
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read the trace `{}`: {e}", path.display()))?;
        let mut runs: Vec<(String, RunHead)> = Vec::new();
        let mut skipped: Vec<usize> = Vec::new();
        for (i, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let record = serde_json::from_str::<serde_json::Value>(line).ok();
            let member = |m: &str| record.as_ref()?.get(m)?.as_str();
            let (Some(kind), Some(run)) = (member("kind"), member("run")) else {
                skipped.push(i + 1);
                continue;
            };
            if let Some((_, r)) = runs.iter_mut().find(|(id, _)| id == run) {
                r.records += 1;
            } else if kind == "started" || kind == "restored" {
                runs.push((
                    run.to_string(),
                    RunHead {
                        machine: member("machine").unwrap_or_default().to_string(),
                        hash: member("ir").unwrap_or_default().to_string(),
                        records: 1,
                    },
                ));
            }
        }
        if let Some(first) = skipped.first() {
            let n = skipped.len();
            eprintln!(
                "warning: `{}`: {n} line{} {} not a trace record (line {first}{}); coverage skips {}",
                path.display(),
                if n == 1 { "" } else { "s" },
                if n == 1 { "is" } else { "are" },
                if n == 1 { "" } else { " and others" },
                if n == 1 { "it" } else { "them" },
            );
        }
        out.push(CoverageTrace {
            name: path.display().to_string(),
            text,
            runs: runs.into_iter().map(|(_, r)| r).collect(),
        });
    }
    Ok(out)
}

/// Warns about the runs of `--coverage` recorded from another version of their machine, which
/// the viewer leaves out. False if no run is of a machine of the charts.
fn coverage_warnings(traces: &[CoverageTrace], charts: &[(&str, &Ir)]) -> bool {
    let (mut matched, mut stale, mut stale_records) = (0, 0, 0);
    for r in traces.iter().flat_map(|t| &t.runs) {
        let machines: Vec<&roadbook_ir::Machine> = charts
            .iter()
            .filter_map(|(_, ir)| ir.machines.get(&r.machine))
            .collect();
        if machines.is_empty() {
            continue;
        }
        matched += 1;
        if machines.iter().all(|m| m.hash != r.hash) {
            stale += 1;
            stale_records += r.records;
        }
    }
    if stale > 0 {
        eprintln!(
            "warning: {stale} run{} ({stale_records} records) of the traces were recorded from another version of their machine; coverage leaves them out",
            if stale == 1 { "" } else { "s" }
        );
    }
    let runs: usize = traces.iter().map(|t| t.runs.len()).sum();
    eprintln!(
        "coverage: {} trace file{}, {matched} of {runs} runs of machines these charts have",
        traces.len(),
        if traces.len() == 1 { "" } else { "s" },
    );
    matched > 0
}

/// A trace given with `--trace`: its text, and its first top-level run.
struct Trace {
    /// The file name the page shows.
    name: String,
    text: String,
    run: String,
    machine: String,
    hash: String,
}

/// Reads a trace and checks that it is one: JSON Lines of records, with a `started` record of a
/// top-level run. The viewer checks the records against the chart.
fn read_trace(path: &Path) -> Result<Trace, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read the trace `{}`: {e}", path.display()))?;
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let mut first = None;
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let record: serde_json::Value = serde_json::from_str(line).map_err(|e| {
            format!(
                "`{}`, line {}: not a JSON record: {e}",
                path.display(),
                i + 1
            )
        })?;
        let member = |m: &str| record.get(m).and_then(|v| v.as_str());
        if member("kind").is_none() || member("run").is_none() {
            return Err(format!(
                "`{}`, line {}: not a trace record (it needs `kind` and `run`)",
                path.display(),
                i + 1
            ));
        }
        if first.is_none() && member("kind") == Some("started") && record.get("parent").is_none() {
            first = Some((
                member("run").unwrap_or_default().to_string(),
                member("machine").unwrap_or_default().to_string(),
                member("ir").unwrap_or_default().to_string(),
            ));
        }
    }
    let (run, machine, hash) = first.ok_or_else(|| {
        format!(
            "`{}` has no `started` record of a top-level run: it is not a trace",
            path.display()
        )
    })?;
    Ok(Trace {
        name,
        text,
        run,
        machine,
        hash,
    })
}
