//! Printing diagnostics: rendered to standard error, or as JSON to standard output.

use roadbook_compiler::Compilation;

use crate::Exit;

/// Prints the diagnostics of a compilation and returns the exit code they call for.
pub fn diagnostics(out: &Compilation, json: bool) -> Exit {
    if json {
        println!("{}", out.to_json());
    } else if !out.diagnostics.is_empty() {
        eprint!("{}", out.render());
        eprintln!();
        eprintln!("{}", summary(out.error_count(), out.warning_count()));
    }
    if out.has_errors() {
        Exit::Errors
    } else {
        Exit::Ok
    }
}

/// "2 errors, 1 warning".
pub fn summary(errors: usize, warnings: usize) -> String {
    let n = |n: usize, what: &str| format!("{n} {what}{}", if n == 1 { "" } else { "s" });
    match (errors, warnings) {
        (0, w) => n(w, "warning"),
        (e, 0) => n(e, "error"),
        (e, w) => format!("{}, {}", n(e, "error"), n(w, "warning")),
    }
}

/// An I/O or usage error.
pub fn fatal(message: impl std::fmt::Display) -> Exit {
    eprintln!("error: {message}");
    Exit::Usage
}
