//! The Roadbook language server.
//!
//! It speaks the Language Server Protocol over a [`Connection`] (standard input and output
//! for `roadbook lsp`) and offers, for `.road` files:
//!
//! - diagnostics on open, change and save, with secondary labels as related information and
//!   the compiler's fixes as quick fixes;
//! - go to definition, find references, rename (checked: case, reserved words, clashes, and
//!   a recompilation that must resolve every use as before) and hover, for every kind of
//!   name, across files;
//! - document formatting with the canonical formatter, and the outline of a file;
//! - `roadbook/ir`, a request of its own: a document compiled as an entry, as `roadbook
//!   export ir` would, with the unsaved buffers (see the README).
//!
//! Files of other packages are compiled as the project's files reach them, never as
//! entries, and only their errors are reported. A document that is not a file (an untitled
//! buffer) is compiled on its own, with no project.
//!
//! The compiler does the work: each change compiles the files it can affect (every file
//! connected to it by imports, either way) with the unsaved editor buffers laid over the file
//! system, as `roadbook check` would, and the compiler's
//! [symbol index](roadbook_compiler::index) answers the questions about names.
//!
//! Which project a file belongs to, its packages and options, come from the caller as a
//! [`ProjectConfig`]: the command line finds them as its other commands do.

mod outline;
mod rename;
mod server;
pub mod text;
mod world;

use std::path::{Path, PathBuf};

pub use lsp_server::Connection;
use roadbook_compiler::{FsLoader, Options};

pub use server::run;

/// What the server needs to know about the project a file belongs to.
#[derive(Clone, Debug)]
pub struct ProjectConfig {
    /// The project's root: the directory holding `roadbook.toml`.
    pub root: PathBuf,
    /// Reads the project's files and finds its packages.
    pub loader: FsLoader,
    pub options: Options,
    /// The project paths of every `.road` file `roadbook check` would check, so that the
    /// files importing a changed file can be found.
    pub files: Vec<String>,
}

impl ProjectConfig {
    /// A project of the `.road` files directly in `dir`, with no packages: for a file that
    /// is in no project.
    pub fn standalone(dir: &Path) -> ProjectConfig {
        let mut files: Vec<String> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .filter_map(|e| {
                let name = e.ok()?.file_name().to_string_lossy().into_owned();
                (name.ends_with(".road") && name.len() > 5).then_some(name)
            })
            .collect();
        files.sort();
        ProjectConfig {
            root: dir.to_path_buf(),
            loader: FsLoader::new(dir).with_node_modules(true),
            options: Options::default(),
            files,
        }
    }
}

/// Finds the project of a file, given its absolute path. Called again when files are saved
/// or created, to see new files.
pub type FindProject = Box<dyn Fn(&Path) -> Result<ProjectConfig, String>>;

/// Runs the server on standard input and output until the client asks it to exit.
pub fn run_stdio(find: FindProject) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (connection, io) = Connection::stdio();
    run(connection, find)?;
    io.join()?;
    Ok(())
}
