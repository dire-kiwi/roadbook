//! `roadbook export ir <file> [-o out]`

use std::path::PathBuf;

use roadbook_compiler::Options;

use crate::workspace::Workspace;
use crate::{Exit, report};

#[derive(clap::Args)]
pub struct Args {
    /// The entry `.road` file. Everything it imports is included.
    file: PathBuf,
    /// Write to this file instead of standard output.
    #[arg(short, long, value_name = "FILE")]
    output: Option<PathBuf>,
}

pub fn run(args: Args) -> Exit {
    if !args.file.is_file() {
        return report::fatal(format!("`{}` is not a file", args.file.display()));
    }
    let ws = match Workspace::find(Some(&args.file)) {
        Ok(ws) => ws,
        Err(e) => return report::fatal(e),
    };
    let entry = ws.project_path(&args.file);
    let out = match roadbook_compiler::compile(&mut ws.loader(), &entry, &Options::default()) {
        Ok(out) => out,
        Err(e) => return report::fatal(e),
    };
    let exit = report::diagnostics(&out, false);
    let Some(ir) = out.ir else {
        return exit;
    };
    let json = ir.to_json();
    match &args.output {
        Some(path) => {
            if let Err(e) = std::fs::write(path, json) {
                return report::fatal(format!("cannot write `{}`: {e}", path.display()));
            }
        }
        None => print!("{json}"),
    }
    exit
}
