//! Printing diagnostics and the verdicts of checks: rendered to standard error, or as JSON
//! to standard output.

use roadbook_compiler::Compilation;

use crate::Exit;

/// Prints the diagnostics of a compilation and returns the exit code they call for.
pub fn diagnostics(out: &Compilation, json: bool) -> Exit {
    report(out, json, None)
}

/// Prints the diagnostics of a compilation as `roadbook check` does: then, if it evaluated
/// checks, a line counting their verdicts (`checks: 6 proved, 1 failed`), with every check
/// listed before it if `list`. Returns the exit code the diagnostics call for.
pub fn checked(out: &Compilation, json: bool, list: bool) -> Exit {
    report(out, json, Some(list))
}

fn report(out: &Compilation, json: bool, checks: Option<bool>) -> Exit {
    if json {
        println!("{}", out.to_json());
    } else {
        if !out.diagnostics.is_empty() {
            eprint!("{}", out.render());
            eprintln!();
            eprintln!("{}", summary(out.error_count(), out.warning_count()));
        }
        if let (Some(list), Some(line)) = (checks, out.checks_summary()) {
            if list {
                eprint!("{}", out.render_checks());
            }
            eprintln!("{line}");
        }
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

/// An I/O or usage error, also printed as the JSON object if `json`.
pub fn fatal_json(message: impl std::fmt::Display, json: bool) -> Exit {
    if json {
        println!(
            "{}",
            roadbook_compiler::json::fatal_string(&message.to_string())
        );
    }
    fatal(message)
}
