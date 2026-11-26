//! `roadbook export <format> …`: one module per format.

pub mod ir;
pub mod mermaid;

use crate::Exit;

#[derive(clap::Subcommand)]
pub enum Format {
    /// The IR as JSON, in the committed-file form.
    Ir(ir::Args),
    /// Mermaid `stateDiagram-v2`, which GitHub renders inline. Loses detail the viewer shows.
    Mermaid(mermaid::Args),
}

pub fn run(format: Format) -> Exit {
    match format {
        Format::Ir(args) => ir::run(args),
        Format::Mermaid(args) => mermaid::run(args),
    }
}
