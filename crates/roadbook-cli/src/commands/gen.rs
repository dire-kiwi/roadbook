//! `roadbook gen [--check] [paths…]`
//!
//! Compiles each `.road` file as an entry and writes the TypeScript module for it next to it,
//! `<file>.road.ts`. Only errors are printed: warnings are for `roadbook check`, and a file
//! that only declares types and tasks would warn about every task it does not use itself.

use std::path::PathBuf;

use roadbook_compiler::Options;

use crate::workspace::Workspace;
use crate::{Exit, report};

#[derive(clap::Args)]
pub struct Args {
    /// Write nothing; list the generated files that are stale or missing and fail if there
    /// are any.
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
    let files = match ws.inputs(&args.paths) {
        Ok(f) => f,
        Err(e) => return report::fatal(e),
    };
    let mut exit = Exit::Ok;
    for file in files {
        let out = match roadbook_compiler::compile(&mut ws.loader(), &file, &Options::default()) {
            Ok(out) => out,
            Err(e) => return report::fatal(e),
        };
        let Some(ir) = out.ir.as_ref().filter(|_| !out.has_errors()) else {
            report::diagnostics(&out, false);
            eprintln!("`{file}` has errors; nothing was generated for it");
            exit = Exit::Errors;
            continue;
        };
        let code = match roadbook_gen_ts::generate(ir, &roadbook_gen_ts::Options::default()) {
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
