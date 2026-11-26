//! `roadbook export <format> …`: one module per format.

pub mod ir;

use crate::Exit;

#[derive(clap::Subcommand)]
pub enum Format {
    /// The IR as JSON, in the committed-file form.
    Ir(ir::Args),
}

pub fn run(format: Format) -> Exit {
    match format {
        Format::Ir(args) => ir::run(args),
    }
}
