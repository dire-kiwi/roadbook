//! `roadbook export mermaid <file> [--machine Name] [-o out]`
//!
//! Mermaid's `stateDiagram-v2`, which GitHub renders inline. Without `--machine`, every machine
//! of the compilation as Markdown: a heading and a ```` ```mermaid ```` block each. With it, the
//! bare diagram of that machine.
//!
//! What Mermaid can show: states, nesting, parallel regions (`--`), initial (`[*] -->`) and
//! final (`--> [*]`) states, transitions labelled with trigger, guard and effects, and a
//! state's invoke, `entry`, `exit`, internal transitions and `output`/`fail` as its
//! description. What it loses: the line styles of the kinds of transition, the details
//! panel, drill-down into invoked machines, and the viewer's layout.

use std::fmt::Write as _;
use std::path::PathBuf;

use roadbook_compiler::{Ir, Options};
use roadbook_ir::{Effect, InvokeKind, Machine, State, StateKind, Transition, Trigger};

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
    let out = match roadbook_compiler::compile(&mut ws.loader(), &entry, &Options::default()) {
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
        let source = ir
            .sources
            .get(m.span.file as usize)
            .map_or("?", |s| s.path.as_str());
        let _ = writeln!(out, "## {name}\n\n`{source}:{}`\n", m.span.line);
        if !m.doc.is_empty() {
            let _ = writeln!(out, "{}\n", m.doc);
        }
        let _ = write!(out, "```mermaid\n{}```\n", diagram(name, m));
    }
    out
}

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

    /// A Mermaid id for a state: its path below the machine, `.` replaced by `_`. State names
    /// have no `_`, so ids are unique, and Mermaid ids are global to the diagram.
    fn id(&self, state_id: &str) -> String {
        if state_id == self.machine {
            return state_id.to_string();
        }
        state_id
            .strip_prefix(self.machine)
            .and_then(|s| s.strip_prefix('.'))
            .unwrap_or(state_id)
            .replace('.', "_")
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
        for d in description(s) {
            let line = format!("{id}: {}", escape(&d));
            self.line(depth, &line);
        }
        if !s.states.is_empty() {
            let line = format!("state {id} {{");
            self.line(depth, &line);
            self.body(s, depth + 1);
            self.line(depth, "}");
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
    }
}

fn trigger(t: &Trigger) -> String {
    match t {
        Trigger::Event { event } => event.clone(),
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
        out.push(format!("invoke {kind}{}({})", inv.src, args.join(", ")));
    }
    out.extend(s.entry.iter().map(|e| format!("entry {}", effect(e))));
    out.extend(s.exit.iter().map(|e| format!("exit {}", effect(e))));
    for t in s.transitions.iter().filter(|t| t.target.is_none()) {
        let head = match &t.trigger {
            Trigger::Event { event } => format!("on {event}"),
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

/// Characters Mermaid would read as syntax, as its entity codes.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '#' => out.push_str("#35;"),
            ';' => out.push_str("#59;"),
            '\n' => out.push(' '),
            c => out.push(c),
        }
    }
    out
}
