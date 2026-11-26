//! `roadbook export mermaid <file> [--machine Name] [-o out]`
//!
//! Mermaid's `stateDiagram-v2`, which GitHub renders inline. Without `--machine`, every machine
//! of the compilation as Markdown: a heading and a ```` ```mermaid ```` block each. With it, the
//! bare diagram of that machine.
//!
//! What Mermaid can show: states, nesting, parallel regions (`--`), initial (`[*] -->`) and
//! final (`--> [*]`) states, transitions labelled with trigger, guard and effects, and a
//! state's invoke, `entry`, `exit`, internal transitions and `output`/`fail` as its
//! description. Messaging is written as in the chart: `on upload.Progress` as the trigger
//! `upload.Progress`, `send upload.Pause` and `emit Progress { … }` as effects, and
//! `invoke machine Upload(file) as upload`. Mermaid allows no description on a state with states inside, so a compound or
//! parallel state (and the machine, when it is drawn as a state) has them in a note beside it.
//! What it loses: the line styles of the kinds of transition, the details panel, drill-down
//! into invoked machines, and the viewer's layout.
//!
//! Mermaid ids are a state's path below the machine with `.` replaced by `_`, so they read like
//! the chart. Names are `[A-Z][A-Za-z0-9]*`, so ids outside that space cannot clash with one:
//! a name Mermaid reads as a keyword (`State`, `Note`, …) gets a trailing `_`, and the machine
//! drawn as a state is `<Machine>_machine`.

use std::fmt::Write as _;
use std::path::PathBuf;

use roadbook_compiler::Ir;
use roadbook_ir::{
    Effect, InvokeKind, Machine, ReplyOutcome, State, StateKind, Transition, Trigger,
};

use crate::workspace::Workspace;
use crate::{Exit, report};

#[derive(clap::Args)]
pub struct Args {
    /// The entry `.road` file. Every machine it declares or imports is exported.
    file: PathBuf,
    /// Export only this machine, as a bare diagram rather than Markdown.
    #[arg(long, value_name = "NAME")]
    machine: Option<String>,
    /// Write to this file instead of standard output.
    #[arg(short, long, value_name = "FILE")]
    output: Option<PathBuf>,
}

pub fn run(args: Args) -> Exit {
    if !args.file.is_file() {
        return report::fatal(format!("`{}` is not a file", args.file.display()));
    }
    let ws = match Workspace::find(Some(&args.file)) {
        Ok(ws) => ws,
        Err(e) => return report::fatal(e),
    };
    let entry = ws.project_path(&args.file);
    let out = match roadbook_compiler::compile(&mut ws.loader(), &entry, &ws.options()) {
        Ok(out) => out,
        Err(e) => return report::fatal(e),
    };
    let exit = report::diagnostics(&out, false);
    let Some(ir) = out.ir else {
        return exit;
    };
    let text = match &args.machine {
        Some(name) => match ir.machines.get(name) {
            Some(m) => diagram(name, m),
            None => {
                let names: Vec<&str> = ir.machines.keys().map(String::as_str).collect();
                return report::fatal(format!(
                    "no machine `{name}` in `{entry}` (it has: {})",
                    names.join(", ")
                ));
            }
        },
        None => markdown(&ir),
    };
    match &args.output {
        Some(path) => {
            if let Err(e) = std::fs::write(path, text) {
                return report::fatal(format!("cannot write `{}`: {e}", path.display()));
            }
        }
        None => print!("{text}"),
    }
    exit
}

/// Every machine: a heading, its doc comment, and a fenced `mermaid` block.
pub fn markdown(ir: &Ir) -> String {
    let mut out = String::new();
    for (i, (name, m)) in ir.machines.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        // A file of another package as imports name it: `inventory/inventory.road`.
        let source = ir
            .sources
            .get(m.span.file as usize)
            .map_or("?".into(), |s| {
                s.package
                    .as_ref()
                    .map_or(s.path.clone(), |p| format!("{p}/{}", s.path))
            });
        let _ = writeln!(
            out,
            "## {name}\n\n{}\n",
            code_span(&format!("{source}:{}", m.span.line))
        );
        if !m.doc.is_empty() {
            // Fenced as text, so nothing in it is read as Markdown or HTML or can end a fence.
            let fence = "`".repeat(3.max(longest_run(&m.doc, '`') + 1));
            let _ = writeln!(out, "{fence}text\n{}\n{fence}\n", m.doc.trim_end());
        }
        let _ = write!(out, "```mermaid\n{}```\n", diagram(name, m));
    }
    out
}

/// `text` as an inline code span that nothing in it can end.
fn code_span(text: &str) -> String {
    let text = text.replace(['\n', '\r'], " ");
    let ticks = "`".repeat(longest_run(&text, '`') + 1);
    let pad = if text.starts_with('`') || text.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{ticks}{pad}{text}{pad}{ticks}")
}

fn longest_run(text: &str, c: char) -> usize {
    let (mut best, mut run) = (0, 0);
    for x in text.chars() {
        run = if x == c { run + 1 } else { 0 };
        best = best.max(run);
    }
    best
}

/// Words of Mermaid's state diagram grammar (case-insensitive) that an id equal to one would
/// be read as: each fails to parse, or (`accTitle`, `accDescr`) silently means something else.
const KEYWORDS: &[&str] = &[
    "accdescr",
    "acctitle",
    "as",
    "class",
    "classdef",
    "click",
    "default",
    "direction",
    "href",
    "note",
    "scale",
    "state",
    "statediagram",
    "style",
];

/// One machine as `stateDiagram-v2`.
pub fn diagram(name: &str, m: &Machine) -> String {
    let mut parallel = Vec::new();
    collect_parallel(&m.root, &mut parallel);
    let mut w = Writer {
        machine: name,
        out: String::from("stateDiagram-v2\n"),
        parallel,
    };
    let root = &m.root;
    let wrap = !root.transitions.is_empty()
        || !root.entry.is_empty()
        || !root.exit.is_empty()
        || root.invoke.is_some();
    if wrap {
        // The machine has transitions of its own: draw it as a state around the others.
        w.line(1, &format!("[*] --> {}", w.id(&root.id)));
        w.state(root, 1);
        w.transitions(root, "", 1);
    } else {
        w.body(root, 1);
    }
    w.out
}

struct Writer<'a> {
    machine: &'a str,
    out: String,
    /// Ids of the parallel states. A transition between two of their regions is written in
    /// the block around them: inside, the lines after the last `--` belong to the last region.
    parallel: Vec<String>,
}

impl Writer<'_> {
    fn line(&mut self, depth: usize, text: &str) {
        let _ = writeln!(self.out, "{}{text}", "  ".repeat(depth));
    }

    /// A Mermaid id for a state: its path below the machine, `.` replaced by `_`; a keyword
    /// gets a trailing `_`, the machine itself is `<Machine>_machine`. State names are
    /// `[A-Z][A-Za-z0-9]*`, so ids are unique (Mermaid ids are global to the diagram).
    fn id(&self, state_id: &str) -> String {
        if state_id == self.machine {
            return format!("{state_id}_machine");
        }
        let id = state_id
            .strip_prefix(self.machine)
            .and_then(|s| s.strip_prefix('.'))
            .unwrap_or(state_id)
            .replace('.', "_");
        if KEYWORDS.contains(&id.to_ascii_lowercase().as_str()) {
            format!("{id}_")
        } else {
            id
        }
    }

    /// The states inside `container`, its initial arrow, and every transition whose source
    /// and target meet first in it.
    fn body(&mut self, container: &State, depth: usize) {
        for (i, child) in container.states.iter().enumerate() {
            if container.kind == StateKind::Parallel && i > 0 {
                self.line(depth, "--");
            }
            self.state(child, depth);
        }
        if let Some(initial) = &container.initial {
            let line = format!("[*] --> {}", self.id(initial));
            self.line(depth, &line);
        }
        // Transitions are written in the block of the innermost state containing both ends,
        // after every state of it is declared, so Mermaid never creates a state in the wrong
        // block.
        self.transitions(container, &container.id, depth);
        for c in &container.states {
            if c.kind == StateKind::Final {
                let line = format!("{} --> [*]", self.id(&c.id));
                self.line(depth, &line);
            }
        }
    }

    /// The transitions of `s` and the states inside it that belong in the block `block`.
    fn transitions(&mut self, s: &State, block: &str, depth: usize) {
        for t in &s.transitions {
            let Some(target) = &t.target else { continue };
            let mut meet = meeting(&s.id, target);
            while self.parallel.iter().any(|p| p == meet) {
                meet = meet.rfind('.').map_or("", |at| &meet[..at]);
            }
            if meet == block {
                let line = format!(
                    "{} --> {}: {}",
                    self.id(&s.id),
                    self.id(target),
                    escape(&label(s, t))
                );
                self.line(depth, &line);
            }
        }
        for c in &s.states {
            self.transitions(c, block, depth);
        }
    }

    fn state(&mut self, s: &State, depth: usize) {
        let id = self.id(&s.id);
        if id != s.name {
            let line = format!("state \"{}\" as {id}", s.name);
            self.line(depth, &line);
        }
        let description = description(s);
        if s.states.is_empty() {
            for d in description {
                let line = format!("{id}: {}", escape(&d));
                self.line(depth, &line);
            }
            return;
        }
        let line = format!("state {id} {{");
        self.line(depth, &line);
        self.body(s, depth + 1);
        self.line(depth, "}");
        // Mermaid refuses a description on a state with states inside: a note instead.
        if !description.is_empty() {
            let line = format!("note right of {id}");
            self.line(depth, &line);
            for d in description {
                self.line(depth + 1, &escape(&d));
            }
            self.line(depth, "end note");
        }
    }
}

fn collect_parallel(s: &State, out: &mut Vec<String>) {
    if s.kind == StateKind::Parallel {
        out.push(s.id.clone());
    }
    for c in &s.states {
        collect_parallel(c, out);
    }
}

/// The id of the innermost state that has both ends of a transition strictly inside it, or
/// `""` if there is none (a transition of the machine itself, drawn as a state).
fn meeting<'a>(source: &'a str, target: &str) -> &'a str {
    let a: Vec<&str> = source.split('.').collect();
    let b: Vec<&str> = target.split('.').collect();
    let mut k = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    if k == a.len() || k == b.len() {
        k -= 1;
    }
    let len: usize = a[..k].iter().map(|s| s.len()).sum::<usize>() + k.saturating_sub(1);
    &source[..len]
}

fn effect(e: &Effect) -> String {
    match e {
        Effect::Set { var, value } => format!("set {var} = {}", value.text),
        Effect::Do { action, args } => {
            let args: Vec<&str> = args.iter().map(|a| a.text.as_str()).collect();
            format!("do {action}({})", args.join(", "))
        }
        Effect::Send { to, value, .. } => format!("send {}.{}", to.name, value.text),
        Effect::Emit { value, .. } => format!("emit {}", value.text),
        Effect::Reply {
            call,
            outcome,
            value,
        } => {
            let outcome = match outcome {
                ReplyOutcome::Done => "done",
                ReplyOutcome::Fail => "fail",
            };
            let value = value
                .as_ref()
                .map_or(String::new(), |v| format!(" {}", v.text));
            format!("reply {} {outcome}{value}", call.text)
        }
    }
}

fn trigger(t: &Trigger) -> String {
    match t {
        Trigger::Event { event } => event.clone(),
        Trigger::Emitted { from, event } => format!("{}.{event}", from.name),
        Trigger::Call { call } => call.clone(),
        Trigger::Any => "on *".into(),
        Trigger::After { delay } => format!("after {}", delay.text),
        Trigger::Always => "always".into(),
        Trigger::Done | Trigger::InvokeDone => "done".into(),
        Trigger::InvokeFail { error } => format!("fail {error}"),
    }
}

/// `trigger [guard] / effects`, with `[else]` for the last unguarded branch of a block.
fn label(s: &State, t: &Transition) -> String {
    let mut text = trigger(&t.trigger);
    match &t.guard {
        Some(g) => text.push_str(&format!(" [{}]", g.text)),
        None => {
            let first = s.transitions.iter().position(|o| o.group == t.group);
            let this = s.transitions.iter().position(|o| std::ptr::eq(o, t));
            if first != this {
                text.push_str(" [else]");
            }
        }
    }
    if !t.effects.is_empty() {
        let effects: Vec<String> = t.effects.iter().map(effect).collect();
        text.push_str(&format!(" / {}", effects.join(", ")));
    }
    text
}

/// The lines Mermaid shows inside a state box.
fn description(s: &State) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(inv) = &s.invoke {
        let args: Vec<&str> = inv.args.iter().map(|a| a.text.as_str()).collect();
        let kind = if inv.kind == InvokeKind::Machine {
            "machine "
        } else {
            ""
        };
        let alias = inv
            .alias
            .as_ref()
            .map_or(String::new(), |a| format!(" as {a}"));
        out.push(format!(
            "invoke {kind}{}({}){alias}",
            inv.callee(),
            args.join(", ")
        ));
    }
    out.extend(s.entry.iter().map(|e| format!("entry {}", effect(e))));
    out.extend(s.exit.iter().map(|e| format!("exit {}", effect(e))));
    for t in s.transitions.iter().filter(|t| t.target.is_none()) {
        let head = match &t.trigger {
            Trigger::Event { .. } | Trigger::Emitted { .. } => {
                format!("on {}", trigger(&t.trigger))
            }
            other => trigger(other),
        };
        let rest = label(s, t);
        let rest = rest.strip_prefix(&trigger(&t.trigger)).unwrap_or(&rest);
        out.push(format!("{head}{rest}"));
    }
    if let Some(e) = &s.output {
        out.push(format!("output {}", e.text));
    }
    if let Some(e) = &s.fail {
        out.push(format!("fail {}", e.text));
    }
    out
}

/// Text Mermaid would read as syntax, as its entity codes: `#`, `;` and `:`, and the `d` of
/// `direction`, which Mermaid reads as a statement anywhere in a line.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for (i, c) in s.char_indices() {
        match c {
            '#' => out.push_str("#35;"),
            ';' => out.push_str("#59;"),
            ':' => out.push_str("#58;"),
            '\n' | '\r' => out.push(' '),
            'd' | 'D'
                if s.get(i..i + 9)
                    .is_some_and(|w| w.eq_ignore_ascii_case("direction")) =>
            {
                out.push_str(if c == 'd' { "#100;" } else { "#68;" })
            }
            c => out.push(c),
        }
    }
    out
}
