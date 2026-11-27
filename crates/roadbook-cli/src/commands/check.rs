//! `roadbook check [--json] [--checks] [--warn invoke-timeout] [paths…]`
//!
//! Compiles each file as an entry and reports the diagnostics, then evaluates the `check`
//! lines of the files it checked: a failing one is an error (`RB0501`–`RB0504`). After the
//! diagnostics, one line on standard error counts the verdicts, if any check was evaluated;
//! `--checks` lists every check above it. `--json` gives them in `checks`.

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
    /// List every `check` line with its verdict, and the guards and path of each that holds
    /// only if guards allow.
    #[arg(long)]
    checks: bool,
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
        ..ws.options()
    };
    match roadbook_compiler::check(&mut ws.loader(), &files, &options) {
        Ok(out) => report::checked(&out, args.json, args.checks),
        Err(e) => report::fatal_json(e, args.json),
    }
}
