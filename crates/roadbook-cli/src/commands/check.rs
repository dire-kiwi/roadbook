//! `roadbook check [--json] [--warn invoke-timeout] [paths…]`

use std::path::PathBuf;

use clap::ValueEnum;
use roadbook_compiler::Options;

use crate::workspace::Workspace;
use crate::{Exit, report};

#[derive(clap::Args)]
pub struct Args {
    /// Print the diagnostics as JSON on standard output.
    #[arg(long)]
    json: bool,
    /// Turn on a warning that is off by default.
    #[arg(long = "warn", value_name = "WARNING")]
    warn: Vec<Warning>,
    /// Files or directories to check. Default: every `.road` file in the project.
    paths: Vec<PathBuf>,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Warning {
    /// RB0904: an `invoke` with no `after` on its state or any state containing it.
    InvokeTimeout,
}

pub fn run(args: Args) -> Exit {
    let ws = match Workspace::find(args.paths.first().map(PathBuf::as_path)) {
        Ok(ws) => ws,
        Err(e) => return report::fatal_json(e, args.json),
    };
    let files = match ws.inputs(&args.paths) {
        Ok(f) => f,
        Err(e) => return report::fatal_json(e, args.json),
    };
    let options = Options {
        warn_invoke_timeout: args.warn.contains(&Warning::InvokeTimeout),
    };
    match roadbook_compiler::check(&mut ws.loader(), &files, &options) {
        Ok(out) => report::diagnostics(&out, args.json),
        Err(e) => report::fatal_json(e, args.json),
    }
}
