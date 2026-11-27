//! Rename: which names are valid, and a check that the rename means the same program.
//!
//! A new name must be a name of the right case that is not reserved, and must not be taken
//! by a symbol it would clash with (a sibling state, another variable of the machine, …).
//! Then the files are compiled again with the edits applied: every use must resolve to the
//! same symbol as before (a renamed state could otherwise capture a target that meant a
//! state further out), and the edit must not add errors (two declarations with one name
//! among the files compiled together, an event named like a type).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use roadbook_compiler::{Span, SymbolId, SymbolIndex, SymbolKind};
use roadbook_syntax::lexer::Keyword;

use crate::world::{Analysis, World};

/// The edits of a rename, by file, each the span of a name.
pub type Edits = BTreeMap<PathBuf, Vec<Span>>;

/// Why a symbol cannot be renamed at all, if it cannot.
pub fn renameable(analysis: &Analysis, id: SymbolId) -> Result<Edits, String> {
    let index = &analysis.out.index;
    let s = index.symbol(id);
    let mut edits = Edits::new();
    for span in std::iter::once(s.span).chain(index.references_to(id)) {
        let path = analysis
            .path(span.file)
            .filter(|_| span.start.offset < span.end.offset)
            .ok_or_else(|| {
                format!(
                    "`{}` is declared or used in a compiled package, which cannot be edited",
                    s.name
                )
            })?;
        if analysis.packaged.contains(path) || !path.starts_with(&analysis.project) {
            let shown = path
                .strip_prefix(&analysis.project)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            return Err(format!(
                "`{}` is declared or used in `{shown}`, a file of another package: rename \
                 edits only the project's own files",
                s.name
            ));
        }
        edits.entry(path.to_path_buf()).or_default().push(span);
    }
    for spans in edits.values_mut() {
        spans.sort_by_key(|s| s.start.offset);
        spans.dedup();
    }
    Ok(edits)
}

/// Checks that `new` can replace the name of symbol `id`, and returns the edits.
pub fn rename(
    world: &World,
    analysis: &Analysis,
    id: SymbolId,
    new: &str,
) -> Result<Edits, String> {
    let index = &analysis.out.index;
    let s = index.symbol(id);
    let edits = renameable(analysis, id)?;
    if new == s.name {
        return Ok(Edits::new());
    }
    valid_name(s.kind, new)?;
    if let Some(why) = clash(index, id, new) {
        return Err(why);
    }
    // Compile the files again with the edits applied.
    let mut texts: HashMap<PathBuf, String> = HashMap::new();
    for (path, spans) in &edits {
        let Some(file) = analysis.file_id(path) else {
            continue;
        };
        let mut text = analysis.text(file).to_string();
        for span in spans.iter().rev() {
            text.replace_range(span.range(), new);
        }
        texts.insert(path.clone(), text);
    }
    let Some(after) = world.trial(analysis, &texts) else {
        return Err("the files could not be compiled again to check the rename".into());
    };
    if after.error_count() > analysis.out.error_count() {
        let before: HashSet<(String, String)> = analysis
            .out
            .diagnostics
            .iter()
            .filter(|d| d.is_error())
            .map(|d| (d.code.to_string(), d.message.clone()))
            .collect();
        let d = after
            .diagnostics
            .iter()
            .filter(|d| d.is_error())
            .find(|d| !before.contains(&(d.code.to_string(), d.message.clone())))
            .or_else(|| after.diagnostics.iter().find(|d| d.is_error()));
        let why = d.map_or_else(String::new, |d| format!(": {}", d.message));
        return Err(format!(
            "renaming `{}` to `{new}` would cause an error{why}",
            s.name
        ));
    }
    let want = targets(
        |f| analysis.path(f),
        index,
        |t| substituted(index, t, id, new),
    );
    let paths: Vec<Option<PathBuf>> = after
        .files
        .iter()
        .map(|(name, _)| {
            analysis
                .out
                .files
                .iter()
                .position(|(n, _)| n == name)
                .and_then(|i| analysis.paths[i].clone())
        })
        .collect();
    let got = targets(
        |f| paths[f.0 as usize].as_deref(),
        &after.index,
        |t| after.index.qualified_name(t),
    );
    for (path, want) in &want {
        let got = got.get(path).map(Vec::as_slice).unwrap_or(&[]);
        if let Some(i) = (0..want.len().max(got.len())).find(|&i| want.get(i) != got.get(i)) {
            let file = path
                .file_name()
                .map_or_else(String::new, |f| f.to_string_lossy().into_owned());
            let why = match (want.get(i), got.get(i)) {
                (Some(w), Some(g)) => {
                    format!("a use of `{w}` in `{file}` would mean `{g}` instead")
                }
                _ => format!("a name in `{file}` would no longer refer to what it does"),
            };
            return Err(format!(
                "renaming `{}` to `{new}` would change what names refer to: {why}",
                s.name
            ));
        }
    }
    Ok(edits)
}

/// For each file, the qualified names of what its uses refer to, in order.
fn targets<'a>(
    path: impl Fn(roadbook_compiler::FileId) -> Option<&'a Path>,
    index: &SymbolIndex,
    name: impl Fn(SymbolId) -> String,
) -> HashMap<PathBuf, Vec<String>> {
    let mut out: HashMap<PathBuf, Vec<String>> = HashMap::new();
    for r in index.references() {
        if let Some(p) = path(r.span.file) {
            out.entry(p.to_path_buf()).or_default().push(name(r.symbol));
        }
    }
    out
}

/// The qualified name of `id` once `renamed` is called `new`.
fn substituted(index: &SymbolIndex, id: SymbolId, renamed: SymbolId, new: &str) -> String {
    let s = index.symbol(id);
    let name = if id == renamed { new } else { s.name.as_str() };
    match s.parent {
        Some(p) => format!("{}.{name}", substituted(index, p, renamed, new)),
        None => name.to_string(),
    }
}

/// Whether `name` is a valid name for a symbol of kind `kind`.
pub fn valid_name(kind: SymbolKind, name: &str) -> Result<(), String> {
    let what = kind.describe();
    let Some(first) = name.chars().next() else {
        return Err("the new name is empty".into());
    };
    if name.contains('_') {
        return Err(format!(
            "`{name}`: names cannot contain `_`; write it in camel case"
        ));
    }
    if !first.is_ascii_alphabetic() || !name.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(format!(
            "`{name}` is not a name: a name is a letter followed by letters and digits"
        ));
    }
    if kind.is_upper_case() && !first.is_ascii_uppercase() {
        return Err(format!(
            "`{name}`: the name of {} must start with an upper-case letter",
            article(what)
        ));
    }
    if !kind.is_upper_case() && !first.is_ascii_lowercase() {
        return Err(format!(
            "`{name}`: the name of {} must start with a lower-case letter",
            article(what)
        ));
    }
    if Keyword::lookup(name).is_some() || name == "result" {
        return Err(format!("`{name}` is reserved and cannot be a name"));
    }
    // `key` is the implicit name of a spawned child's key: it can name a field, but nothing
    // in the machine's namespace.
    let in_machine_namespace = matches!(
        kind,
        SymbolKind::Parameter
            | SymbolKind::Variable
            | SymbolKind::InvokeName
            | SymbolKind::Children
    );
    if name == "key" && in_machine_namespace {
        return Err(format!(
            "`key` is reserved and cannot be the name of {}",
            article(what)
        ));
    }
    Ok(())
}

fn article(what: &str) -> String {
    let an = what.starts_with(['a', 'e', 'i', 'o', 'u']);
    format!("{} {what}", if an { "an" } else { "a" })
}

/// A symbol that `id` renamed to `new` would clash with, as a message. Declarations are
/// left to the trial compilation: their names need only be unique among the files of each
/// entry's compilation.
fn clash(index: &SymbolIndex, id: SymbolId, new: &str) -> Option<String> {
    use SymbolKind as K;
    let s = index.symbol(id);
    let typeish = |k: K| {
        matches!(
            k,
            K::Record | K::Alias | K::Enum | K::Error | K::Extern | K::Machine
        )
    };
    // Whether state `outer` is state `inner` or contains it.
    let contains = |outer: Option<SymbolId>, inner: Option<SymbolId>| {
        let mut at = inner;
        while let Some(i) = at {
            if Some(i) == outer {
                return true;
            }
            at = index.symbol(i).parent;
        }
        false
    };
    let machine_of = |mut i: SymbolId| loop {
        let x = index.symbol(i);
        match x.parent {
            _ if x.kind == K::Machine => return Some(i),
            Some(p) => i = p,
            None => return None,
        }
    };
    // A call's parameters are named apart from the machine's parameters and variables.
    let of_call =
        |x: &roadbook_compiler::Symbol| x.parent.is_some_and(|p| index.symbol(p).kind == K::Call);
    let conflicts = |o: &roadbook_compiler::Symbol, oid: SymbolId| match s.kind {
        K::Call => o.kind == K::Call && o.parent == s.parent,
        K::Parameter if of_call(s) => o.kind == K::Parameter && o.parent == s.parent,
        K::Event | K::EmittedEvent => {
            (matches!(o.kind, K::Event | K::EmittedEvent) && o.parent == s.parent)
                || typeish(o.kind)
        }
        K::State | K::Field | K::Variant => o.kind == s.kind && o.parent == s.parent,
        // Two `as` names clash only when one invoking state contains the other: states
        // side by side may use the same one.
        K::InvokeName if o.kind == K::InvokeName => {
            machine_of(oid) == machine_of(id)
                && (contains(s.parent, o.parent) || contains(o.parent, s.parent))
        }
        K::Parameter | K::Variable | K::InvokeName | K::Children => {
            matches!(
                o.kind,
                K::Parameter | K::Variable | K::InvokeName | K::Children
            ) && !of_call(o)
                && machine_of(oid) == machine_of(id)
        }
        _ => false,
    };
    index
        .iter()
        .find(|&(oid, o)| oid != id && o.name == new && conflicts(o, oid))
        .map(|(oid, o)| {
            format!(
                "`{new}` is already the name of {} `{}`",
                article(o.kind.describe()),
                index.qualified_name(oid)
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert!(valid_name(SymbolKind::State, "Waiting").is_ok());
        assert!(valid_name(SymbolKind::State, "waiting").is_err());
        assert!(valid_name(SymbolKind::Variable, "Count").is_err());
        assert!(valid_name(SymbolKind::Variable, "on").is_err());
        assert!(valid_name(SymbolKind::Variable, "result").is_err());
        assert!(valid_name(SymbolKind::Variable, "key").is_err());
        assert!(valid_name(SymbolKind::Children, "key").is_err());
        assert!(valid_name(SymbolKind::Children, "Sessions").is_err());
        assert!(valid_name(SymbolKind::Children, "stop").is_ok());
        assert!(valid_name(SymbolKind::Field, "key").is_ok());
        assert!(valid_name(SymbolKind::Field, "my_field").is_err());
        assert!(valid_name(SymbolKind::Task, "fetch2").is_ok());
        assert!(valid_name(SymbolKind::Task, "2fetch").is_err());
        assert!(valid_name(SymbolKind::Task, "fétch").is_err());
        assert!(valid_name(SymbolKind::Task, "").is_err());
    }
}
