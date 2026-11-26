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
            Err(e) => {
                eprintln!("error: `{file}`: {e}");
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
