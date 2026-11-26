//! `roadbook lsp`: the language server, on standard input and output.
//!
//! Editors start it as a child process. Each file's project is found as for the other
//! commands: the nearest `roadbook.toml` above it, with its packages, options and files; a
//! file in no project is a project of its own directory.

use std::path::Path;

use roadbook_lsp::ProjectConfig;

use crate::Exit;
use crate::workspace::Workspace;

#[derive(clap::Args)]
pub struct Args {
    /// Talk over standard input and output (the only transport; accepted for editors that
    /// pass it).
    #[arg(long)]
    stdio: bool,
}

pub fn run(_args: Args) -> Exit {
    match roadbook_lsp::run_stdio(Box::new(project)) {
        Ok(()) => Exit::Ok,
        Err(e) => {
            eprintln!("roadbook lsp: {e}");
            Exit::Usage
        }
    }
}

/// The project of `file`, an absolute path.
fn project(file: &Path) -> Result<ProjectConfig, String> {
    let dir = file.parent().unwrap_or(file);
    let ws = Workspace::find(Some(file))?;
    // `Workspace::find` falls back to the current directory, which means nothing here.
    if !ws.root.join("roadbook.toml").is_file() || !file.starts_with(&ws.root) {
        return Ok(ProjectConfig::standalone(dir));
    }
    Ok(ProjectConfig {
        root: ws.root.clone(),
        loader: ws.loader(),
        options: ws.options(),
        files: ws.inputs(&[])?,
    })
}
