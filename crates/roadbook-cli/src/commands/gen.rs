//! `roadbook gen [--check] [paths…]`
//!
//! Compiles each `.road` file as an entry and writes the TypeScript module for it next to it,
//! `<file>.road.ts`. Only errors are printed: warnings are for `roadbook check`, and a file
//! that only declares types and tasks would warn about every task it does not use itself.
//! The project's `gen-exclude` globs name charts it generates nothing for.
//!
//! A generated module whose `.road` file no longer exists is an orphan: `--check` lists it
//! and fails, and plain `gen` warns about it. Neither deletes it.

use std::path::PathBuf;

use std::collections::BTreeMap;

use roadbook_compiler::loader::{FileRef, NotExported, export_of, relative_path};
use roadbook_compiler::{FsLoader, Ir};
use roadbook_gen_ts::PackageModule;

use crate::workspace::Workspace;
use crate::{Exit, report};

#[derive(clap::Args)]
pub struct Args {
    /// Write nothing; list the generated files that are stale, missing or orphaned (left
    /// behind by a deleted chart) and fail if there are any.
    #[arg(long)]
    check: bool,
    /// Files or directories to generate for. Default: every `.road` file in the project.
    paths: Vec<PathBuf>,
}

pub fn run(args: Args) -> Exit {
    let ws = match Workspace::find(args.paths.first().map(PathBuf::as_path)) {
        Ok(ws) => ws,
        Err(e) => return report::fatal(e),
    };
    let files = match ws.gen_inputs(&args.paths) {
        Ok(f) => f,
        Err(e) => return report::fatal(e),
    };
    let mut exit = Exit::Ok;
    match orphans(&ws, &args.paths) {
        Ok(orphans) => {
            for (module, chart) in orphans {
                if args.check {
                    println!("{module}");
                    exit = Exit::Errors;
                }
                let what = if args.check { "error" } else { "warning" };
                eprintln!(
                    "{what}: `{module}` was generated from `{chart}`, which no longer exists; delete it"
                );
            }
        }
        Err(e) => return report::fatal(e),
    }
    let compiler = ws.options();
    for file in files {
        let mut loader = ws.loader();
        let out = match roadbook_compiler::compile(&mut loader, &file, &compiler) {
            Ok(out) => out,
            Err(e) => return report::fatal(e),
        };
        let Some(ir) = out.ir.as_ref().filter(|_| !out.has_errors()) else {
            report::diagnostics(&out, false);
            eprintln!("`{file}` has errors; nothing was generated for it");
            exit = Exit::Errors;
            continue;
        };
        let modules = match package_modules(&ws, &loader, ir) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("error: `{file}`: {e}");
                exit = Exit::Errors;
                continue;
            }
        };
        let options = roadbook_gen_ts::Options::default().with_modules(modules);
        let code = match roadbook_gen_ts::generate(ir, &options) {
            Ok(code) => code,
            Err(mut e) => {
                if let roadbook_gen_ts::GenError::ErrorField {
                    file, line, field, ..
                } = &mut e
                {
                    // The IR knows only the declaration's line: find the field's.
                    let text = std::fs::read_to_string(ws.disk_path(file)).unwrap_or_default();
                    if let Some(found) = field_line(&text, *line, field) {
                        *line = found;
                    }
                    eprintln!("error: {e}");
                } else {
                    eprintln!("error: `{file}`: {e}");
                }
                exit = Exit::Errors;
                continue;
            }
        };
        let target = format!("{file}.ts");
        let disk = ws.disk_path(&target);
        if std::fs::read_to_string(&disk).is_ok_and(|old| old == code) {
            continue;
        }
        if args.check {
            println!("{target}");
            exit = Exit::Errors;
        } else if let Err(e) = std::fs::write(&disk, code) {
            return report::fatal(format!("cannot write `{target}`: {e}"));
        }
    }
    exit
}

/// The conditions TypeScript and Node match in an `exports` map when the generated module
/// imports another package's module.
const MODULE_CONDITIONS: &[&str] = &["types", "import", "node", "default"];

/// How the module generated from `ir` imports the modules of the files of other packages.
/// A file of a `[packages]` package is imported by a relative path to where it was found
/// (`exports` applied). A file of a package in `node_modules` is imported by the specifier
/// that the package's `exports` maps to the module next to the file: the file's own path
/// with `.js` added if the package has no `exports`; a package whose `exports` maps no
/// subpath there is an error, since Node and TypeScript could not resolve the import.
fn package_modules(
    ws: &Workspace,
    loader: &FsLoader,
    ir: &Ir,
) -> Result<BTreeMap<(String, String), PackageModule>, String> {
    let mut out = BTreeMap::new();
    for source in &ir.sources {
        let Some(package) = &source.package else {
            continue;
        };
        let Some(found) = loader.package_file(&FileRef::in_package(package, &source.path)) else {
            continue;
        };
        let module = if found.listed {
            PackageModule::Path(relative_path(&ws.root, &found.disk))
        } else {
            let inside = relative_path(&found.dir, &found.disk);
            let json = std::fs::read_to_string(found.dir.join("package.json")).unwrap_or_default();
            let targets = [format!("{inside}.js"), format!("{inside}.ts")];
            match export_of(&json, &targets, MODULE_CONDITIONS) {
                Ok(None) => PackageModule::Specifier(format!("{package}/{inside}.js")),
                Ok(Some(subpath)) => PackageModule::Specifier(format!("{package}/{subpath}")),
                Err(why) => {
                    let why = match why {
                        NotExported::Missing => format!(
                            "its `package.json` `exports` maps no subpath to `./{inside}.js`; \
                             export the generated modules next to the charts, as \
                             `\"./{}.js\": \"./{inside}.js\"` or a pattern such as \
                             `\"./*.road.js\": \"./<dir>/*.road.js\"`",
                            source.path
                        ),
                        NotExported::Invalid(why) => why,
                    };
                    return Err(format!(
                        "cannot import the generated module of `{package}/{}`: {why}",
                        source.path
                    ));
                }
            }
        };
        out.insert((package.clone(), source.path.clone()), module);
    }
    Ok(out)
}

/// The first line from `from` on (1-based) where `field` is declared: the name as a whole
/// word followed by `:`.
fn field_line(text: &str, from: u32, field: &str) -> Option<u32> {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    for (i, line) in text
        .lines()
        .enumerate()
        .skip(from.saturating_sub(1) as usize)
    {
        let code = line.split("//").next().unwrap_or_default();
        for (at, _) in code.match_indices(field) {
            let before = code[..at].chars().next_back();
            let after = code[at + field.len()..].trim_start();
            if !before.is_some_and(word) && after.starts_with(':') {
                return Some(i as u32 + 1);
            }
        }
    }
    None
}

/// The modules `roadbook gen` wrote whose chart is gone, with the chart's path: `*.road.ts`
/// files starting with the generated header whose `.road` file does not exist.
fn orphans(ws: &Workspace, paths: &[PathBuf]) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    for module in ws.gen_outputs(paths)? {
        let chart = module.trim_end_matches(".ts").to_owned();
        if ws.disk_path(&chart).exists() {
            continue;
        }
        let generated = std::fs::read_to_string(ws.disk_path(&module))
            .is_ok_and(|text| text.starts_with("// Generated by roadbook gen from "));
        if generated {
            out.push((module, chart));
        }
    }
    Ok(out)
}
