//! `roadbook viz [paths…] [-o dir] [--self-contained] [--watch [--port N]]`
//!
//! Writes one HTML page per entry chart, plus `index.html`, the project index. An entry chart
//! is a `.road` file given (or found under a directory given, or in the project if none is)
//! that no other of those files imports and that declares a machine; its page draws every
//! machine of its compilation. Each page holds its IR as JSON and loads the viewer script
//! `roadbook-viewer.js`, written once next to the pages, so a directory of pages shares one copy
//! and still opens from `file://`. `--self-contained` inlines the script in every page instead.
//!
//! The viewer script is `packages/viewer` built by its `build.mjs` into `assets/`, and is
//! included here at compile time, so building the CLI needs no Node.

mod page;
mod serve;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use roadbook_compiler::{Compilation, Ir, Options};

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
    let (exit, built) = build(&ws, &args.paths, &args.out, args.self_contained);
    if !args.watch {
        return exit;
    }
    serve::watch(&ws, &args, built)
}

/// Compiles the charts and writes the pages. On errors writes nothing.
pub fn build(ws: &Workspace, paths: &[PathBuf], out: &Path, self_contained: bool) -> (Exit, Built) {
    let mut built = Built {
        pages: Vec::new(),
        inputs: Vec::new(),
    };
    let files = match ws.inputs(paths) {
        Ok(f) => f,
        Err(e) => return (report::fatal(e), built),
    };
    let mut units: Vec<(String, Compilation)> = Vec::new();
    for f in &files {
        match roadbook_compiler::compile(&mut ws.loader(), f, &Options::default()) {
            Ok(c) => units.push((f.clone(), c)),
            Err(e) => return (report::fatal(e), built),
        }
    }
    let mut read: Vec<String> = units
        .iter()
        .flat_map(|(_, c)| c.files.iter().map(|(p, _)| p.clone()))
        .collect();
    read.sort();
    read.dedup();
    built.inputs = read.iter().map(|p| ws.disk_path(p)).collect();

    // Entries: the files no other file given imports.
    let imported: HashSet<&str> = units
        .iter()
        .flat_map(|(f, c)| {
            c.files
                .iter()
                .skip(1)
                .map(|(p, _)| p.as_str())
                .filter(move |p| p != f)
        })
        .collect();
    let entries: Vec<&(String, Compilation)> = units
        .iter()
        .filter(|(f, _)| !imported.contains(f.as_str()))
        .collect();

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
        let html = page::chart_page(entry, ir, Some(&index_href), &script);
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
