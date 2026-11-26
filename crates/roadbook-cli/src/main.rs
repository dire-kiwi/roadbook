//! `roadbook`, the command line.
//!
//! Each command is a module of [`commands`] with an `Args` type (clap) and a `run` function
//! returning an [`Exit`]; this file only lists them and dispatches. To add a command, add a
//! module, a variant of [`Command`] and an arm in `main`. What commands share (finding the
//! project root and its `.road` files, printing diagnostics) is in [`workspace`] and
//! [`report`].

use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod commands;
mod report;
mod workspace;

use commands::{check, export, fmt, r#gen, viz};

/// Exit codes shared by every command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    /// No errors. Warnings may have been printed.
    Ok = 0,
    /// The input has errors (or, for `fmt --check`, is not formatted).
    Errors = 1,
    /// Bad usage or an I/O error. Clap also exits with 2 on bad arguments.
    Usage = 2,
}

#[derive(Parser)]
#[command(
    name = "roadbook",
    version,
    about = "Statecharts as code: check, format, draw and export `.road` files"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Compile and report diagnostics.
    Check(check::Args),
    /// Format `.road` files in place, canonically.
    Fmt(fmt::Args),
    /// Write the TypeScript module for each `.road` file next to it: `<file>.road.ts`.
    Gen(r#gen::Args),
    /// Write the HTML diagram viewer: a page per entry chart, plus an index.
    Viz(viz::Args),
    /// Write a chart in another format.
    #[command(subcommand)]
    Export(export::Format),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let exit = match cli.command {
        Command::Check(args) => check::run(args),
        Command::Fmt(args) => fmt::run(args),
        Command::Gen(args) => r#gen::run(args),
        Command::Viz(args) => viz::run(args),
        Command::Export(format) => export::run(format),
    };
    ExitCode::from(exit as u8)
}
