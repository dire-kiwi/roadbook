//! `roadbook fmt [--check] [paths…]`

use std::path::PathBuf;

use roadbook_syntax::diagnostic::SingleFile;

use crate::workspace::Workspace;
use crate::{Exit, report};

#[derive(clap::Args)]
pub struct Args {
    /// Change nothing; list the files that are not formatted and fail if there are any.
    #[arg(long)]
    check: bool,
    /// Files or directories to format. Default: every `.road` file in the project.
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
        let disk = ws.disk_path(&file);
        let src = match std::fs::read_to_string(&disk) {
            Ok(s) => s,
            Err(e) => return report::fatal(format!("cannot read `{file}`: {e}")),
        };
        match roadbook_syntax::format(&src) {
            Ok(formatted) if formatted == src => {}
            Ok(formatted) => {
                if args.check {
                    println!("{file}");
                    exit = Exit::Errors;
                } else if let Err(e) = std::fs::write(&disk, formatted) {
                    return report::fatal(format!("cannot write `{file}`: {e}"));
                }
            }
            Err(diags) => {
                let files = SingleFile {
                    name: &file,
                    source: &src,
                };
                for d in &diags {
                    eprintln!("{}", d.render(&files));
                }
                eprintln!("`{file}` has syntax errors and was not formatted");
                exit = Exit::Errors;
            }
        }
    }
    exit
}
