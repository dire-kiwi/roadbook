//! `roadbook export scxml <file> [--machine Name] [-o out]`
//!
//! One SCXML document per machine. With `--machine`, that machine's document, to standard
//! output or to the file `-o` names. Without it, every machine of the compilation, each to
//! `<Machine>.scxml` in the directory `-o` names; without `-o` too, the one machine of the
//! compilation, and an error if there are several (an XML document has one root).
//!
//! The mapping, in brief (the reference is `docs/design/visualization.md`):
//!
//! - **Ids.** A state's id is its Roadbook id with `.` replaced by `_` (`PlaceOrder_Processing`).
//!   Names are `[A-Z][A-Za-z0-9]*`, so `_` maps back to `.`. Dots would be valid in an XML ID,
//!   but SCXML matches events by prefix of their dot-separated tokens, so `done.state.A` would
//!   also match `done.state.A.B`. The ids the exporter adds have a lower-case segment, which no
//!   state name has: `<state>_invoke` for an invoke, `<state>_after_<group>` for a timer.
//! - **Data model.** `datamodel="roadbook"`: every `cond`, `expr`, `delayexpr`, `targetexpr`
//!   and `location` is the canonical text of a Roadbook expression, which reads `event`,
//!   `result` and `error` as the IR says. Parameters and variables are `<data>`, with their
//!   types, and the machine's result type, errors and `strict`, in attributes of the `rb:`
//!   namespace.
//! - **State parameters and variables** are `<data>` in the `<datamodel>` of their state, with
//!   the id `<state>.<name>` (`Reset_Confirming.tries`), which is also the `location` of a `set`
//!   of one, so that same-named locals of two states stay apart; a parameter has
//!   `rb:param="true"`. A variable's initial value is an `<assign>` at the start of the state's
//!   `<onentry>`, and a transition's arguments are `<assign>`s after its effects, since Roadbook
//!   gives a state its locals each time it is entered: SCXML's data is bound once per session
//!   (early or late), has no parameters, and is not dropped when its state is left or restored
//!   by a history state. Expressions still read a local by its name, as the chart writes it.
//! - **The machine** is the `<scxml>` element, unless its body has `entry`, `exit`, an invoke
//!   or transitions: then a `<state>` with the machine's id inside it holds them, and its
//!   transitions with a target inside it are `type="internal"`, so they do not leave the
//!   machine. Top-level final states stay children of `<scxml>`, so reaching one ends the run.
//! - **Triggers** are event names: `on E` is `E`; `on x.E` is `x.E`, also when `x` names a task
//!   that emits (`invoke upload(…) as up` and `on up.Progress`); `on c`, which takes a call,
//!   is `call.c` (see [`call_event`]); `on *` lists the machine's events the state has no `on`
//!   for, never a call; `after d` is a `<send>` of `roadbook.after.<state>.<group>` on entry,
//!   cancelled on exit; `always` has no event; `done` is `done.state.<state>`; an invoke's `done`
//!   is `done.invoke.<invoke>` and its `fail E` is `error.E.<invoke>`, `fail *` the declared
//!   errors no other branch of that invoke lists, and for a call `Unanswered` too; the id of
//!   an invoke is `<state>_invoke`, or `<state>_invoke_<as>` in a state with several; `on x.ended`, a peer's end,
//!   is `x.ended`.
//! - **Effects** are executable content: `set` is `<assign>`; `send x.E` a `<send>` to the
//!   child's invoke (`#_<invoke>`) or to the machine a `ref` holds (`targetexpr`); `emit E` a
//!   `<send>` of type `urn:roadbook:emit`; `reply x done v` a `<send>` of type
//!   `urn:roadbook:reply` to the pending call (`targetexpr="x"`) with the event `done`, and
//!   `reply x fail e` the same with the event `error.E`, `E` the error's type; `do f(…)` the
//!   extension element `<rb:do>`. Event fields, a record reply's fields and arguments are
//!   `<param>`s; any other value is the `<content>`.
//! - **Raised events**: `raise E` is `<raise>`, or a `<send target="#_internal">` when the event
//!   has fields; a delayed `raise E after d as n` is a `<send>` with no target, which SCXML
//!   queues on the session's own external queue once `delay` has passed, with `id="n"` and,
//!   before it, a `<cancel sendid="n">`, since a raise replaces the pending one of its name;
//!   `cancel n` is `<cancel sendid="n">`. SCXML has no internal events: an `internal event` is
//!   an event like the others there.
//! - **Spawned children**, which SCXML has no word for: a `children` collection is a `<data>`
//!   with `rb:children` (its machine) and `rb:key`; `spawn c[k] = M(…)` the element `<rb:spawn>`
//!   and `stop c[k]` `<rb:stop>`, each with `children` and `key`; `send c[k].E` and
//!   `send c.*.E` a `<send>` with `targetexpr` `c[k]` or `c.*`; `on c.done` is the event
//!   `done.spawn.c`, `on c.fail E` is `error.E.spawn.c`, and `on c.E` is `c.E`. A collection of
//!   tasks is the same, with `rb:children` its task and `rb:kind="task"`, and its `<rb:spawn>`
//!   has the task's name as `src` and the type of a task invoke.
//! - **Invokes** of a task have type `urn:roadbook:task` and the task's name as `src`; of a
//!   machine, SCXML's type and `src="<Machine>.scxml"`; of a call, type `urn:roadbook:call` and
//!   `src="x.c"`, the `ref` and the call as the invoke writes them. A final state's `output` is
//!   its `<donedata>`, its `fail` the attribute `rb:fail`.

use std::fmt::Write as _;
use std::path::PathBuf;

use roadbook_compiler::Ir;
use roadbook_ir::{
    AddressKind, ChildrenKind, Effect, Expr, ExprKind, Field, InvokeKind, Literal, Machine,
    ReplyOutcome, State, StateKind, Transition, Trigger, TypeRef,
};

use crate::workspace::Workspace;
use crate::{Exit, report};

/// The namespace of the attributes and elements SCXML has no word for.
pub const RB_NS: &str = "urn:roadbook:scxml";
/// The `<invoke type>` of a task.
pub const TASK_TYPE: &str = "urn:roadbook:task";
/// The `<send type>` of `emit`: to the invoker and every holder of a `ref`, those with an `on`
/// for the event.
pub const EMIT_TYPE: &str = "urn:roadbook:emit";
/// The `<invoke type>` of a call of the run a `ref` holds.
pub const CALL_TYPE: &str = "urn:roadbook:call";
/// The `<send type>` of `reply`: to the caller of a pending call alone, whose invoke takes it
/// as `done.invoke.<invoke>` or `error.E.<invoke>`.
pub const REPLY_TYPE: &str = "urn:roadbook:reply";
const SCXML_TYPE: &str = "http://www.w3.org/TR/scxml/";

#[derive(clap::Args)]
pub struct Args {
    /// The entry `.road` file. Every machine it declares or imports is exported.
    file: PathBuf,
    /// Export only this machine.
    #[arg(long, value_name = "NAME")]
    machine: Option<String>,
    /// With `--machine`, the file to write. Without it, the directory to write
    /// `<Machine>.scxml` files to.
    #[arg(short, long, value_name = "PATH")]
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
    let names = || {
        let names: Vec<&str> = ir.machines.keys().map(String::as_str).collect();
        names.join(", ")
    };
    let write = |path: &PathBuf, text: String| {
        std::fs::write(path, text).map_err(|e| format!("cannot write `{}`: {e}", path.display()))
    };
    let result = match (&args.machine, &args.output) {
        (Some(name), output) => match ir.machines.get(name) {
            None => Err(format!(
                "no machine `{name}` in `{entry}` (it has: {})",
                names()
            )),
            Some(m) => match output {
                Some(path) => write(path, document(&ir, name, m)),
                None => {
                    print!("{}", document(&ir, name, m));
                    Ok(())
                }
            },
        },
        (None, Some(dir)) => std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create `{}`: {e}", dir.display()))
            .and_then(|()| {
                ir.machines.iter().try_for_each(|(name, m)| {
                    write(&dir.join(format!("{name}.scxml")), document(&ir, name, m))
                })
            }),
        (None, None) => match ir.machines.first() {
            Some((name, m)) if ir.machines.len() == 1 => {
                print!("{}", document(&ir, name, m));
                Ok(())
            }
            Some(_) => Err(format!(
                "`{entry}` has several machines ({}): name one with --machine, or write them all to a directory with -o",
                names()
            )),
            None => Err(format!("`{entry}` has no machine")),
        },
    };
    match result {
        Ok(()) => exit,
        Err(e) => report::fatal(e),
    }
}

/// The SCXML id of a Roadbook state id: `.` replaced by `_`, which no name contains.
pub fn xml_id(state_id: &str) -> String {
    state_id.replace('.', "_")
}

/// The `<data>` id of a state's parameter or variable: `Reset_Confirming.tries`.
fn local_id(state_id: &str, name: &str) -> String {
    format!("{}.{name}", xml_id(state_id))
}

/// The SCXML id of the invoke of `s` named `alias`: `<state>_invoke`, or with several invokes
/// in the state, `<state>_invoke_<as>`.
fn invoke_id(s: &State, alias: Option<&str>) -> String {
    match alias {
        Some(a) if s.has_several_invokes() => format!("{}_invoke_{a}", xml_id(&s.id)),
        _ => format!("{}_invoke", xml_id(&s.id)),
    }
}

/// The state with id `id`, at or below `s`.
fn find_state<'a>(s: &'a State, id: &str) -> Option<&'a State> {
    if s.id == id {
        return Some(s);
    }
    s.states.iter().find_map(|c| find_state(c, id))
}

/// The event a callee takes a call `c` as: `call.c`. It is named like an emitted event, after
/// the name the receiver knows the sender by, which in `on c` is the implicit `call`. `call` is
/// a keyword, so no address is named `call` and `call.c` is no emitted event; an event's name
/// is upper-case, and SCXML's own events start `done.` or `error.`, and timers `roadbook.`.
pub fn call_event(call: &str) -> String {
    format!("call.{call}")
}

/// One machine as an SCXML document.
pub fn document(ir: &Ir, name: &str, m: &Machine) -> String {
    let mut w = Writer {
        ir,
        m,
        out: String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n"),
    };
    w.comment(0, &m.doc);
    let root = &m.root;
    let wrap = !root.transitions.is_empty()
        || !root.entry.is_empty()
        || !root.exit.is_empty()
        || !root.invokes.is_empty();
    let is_final = |id: &str| {
        root.states
            .iter()
            .any(|s| s.id == id && s.kind == StateKind::Final)
    };
    let initial = root.initial.as_deref().unwrap_or_default();
    let mut attrs = vec![
        ("xmlns", "http://www.w3.org/2005/07/scxml".to_string()),
        ("xmlns:rb", RB_NS.to_string()),
        ("version", "1.0".to_string()),
        ("name", name.to_string()),
        ("datamodel", "roadbook".to_string()),
        (
            "initial",
            xml_id(if wrap && !is_final(initial) {
                &root.id
            } else {
                initial
            }),
        ),
    ];
    if let Some(t) = &m.returns {
        attrs.push(("rb:returns", t.to_string()));
    }
    if !m.errors.is_empty() {
        attrs.push(("rb:errors", m.errors.join(" ")));
    }
    if m.strict {
        attrs.push(("rb:strict", "true".into()));
    }
    w.open(0, "scxml", &attrs, false);
    if !m.params.is_empty() || !m.vars.is_empty() || !m.children.is_empty() {
        w.open(1, "datamodel", &[], false);
        for p in &m.params {
            w.comment(2, &p.doc);
            let attrs = [
                ("id", p.name.clone()),
                ("rb:type", p.ty.to_string()),
                ("rb:param", "true".into()),
            ];
            w.open(2, "data", &attrs, true);
        }
        for v in &m.vars {
            w.comment(2, &v.doc);
            let mut attrs = vec![("id", v.name.clone())];
            if let Some(init) = &v.init {
                attrs.push(("expr", init.text.clone()));
            }
            attrs.push(("rb:type", v.ty.to_string()));
            w.open(2, "data", &attrs, true);
        }
        for (name, c) in &m.children {
            w.comment(2, &c.doc);
            let mut attrs = vec![
                ("id", name.clone()),
                ("rb:children", c.src.clone()),
                ("rb:key", c.key.to_string()),
            ];
            if c.kind == ChildrenKind::Task {
                attrs.push(("rb:kind", "task".into()));
            }
            w.open(2, "data", &attrs, true);
        }
        w.close(1, "datamodel");
    }
    if wrap {
        let mut inner = root.clone();
        inner.states.retain(|s| s.kind != StateKind::Final);
        if is_final(initial) {
            inner.initial = None;
        }
        // The machine's doc is already above `<scxml>`.
        inner.doc.clear();
        w.state(&inner, 1);
        for s in root.states.iter().filter(|s| s.kind == StateKind::Final) {
            w.state(s, 1);
        }
    } else {
        for s in &root.states {
            w.state(s, 1);
        }
    }
    w.close(0, "scxml");
    w.out
}

struct Writer<'a> {
    ir: &'a Ir,
    m: &'a Machine,
    out: String,
}

impl Writer<'_> {
    fn indent(&mut self, depth: usize) {
        self.out.push_str(&"  ".repeat(depth));
    }

    fn open(&mut self, depth: usize, tag: &str, attrs: &[(&str, String)], empty: bool) {
        self.indent(depth);
        let _ = write!(self.out, "<{tag}");
        for (k, v) in attrs {
            let _ = write!(self.out, " {k}=\"{}\"", attr(v));
        }
        self.out.push_str(if empty { "/>\n" } else { ">\n" });
    }

    fn close(&mut self, depth: usize, tag: &str) {
        self.indent(depth);
        let _ = writeln!(self.out, "</{tag}>");
    }

    /// A doc comment as an XML comment, one line per line of the doc.
    fn comment(&mut self, depth: usize, doc: &str) {
        if doc.is_empty() {
            return;
        }
        let lines: Vec<String> = doc.lines().map(comment_text).collect();
        self.indent(depth);
        if let [line] = lines.as_slice() {
            let _ = writeln!(self.out, "<!-- {line} -->");
            return;
        }
        self.out.push_str("<!--\n");
        for line in lines {
            if !line.is_empty() {
                self.indent(depth + 1);
            }
            let _ = writeln!(self.out, "{line}");
        }
        self.indent(depth);
        self.out.push_str("-->\n");
    }

    fn state(&mut self, s: &State, depth: usize) {
        self.comment(depth, &s.doc);
        let tag = match s.kind {
            StateKind::Atomic | StateKind::Compound => "state",
            StateKind::Parallel => "parallel",
            StateKind::Final => "final",
            StateKind::History | StateKind::DeepHistory => {
                unreachable!("history states are written by `history`")
            }
        };
        let mut attrs = vec![("id", xml_id(&s.id))];
        if let Some(initial) = &s.initial {
            attrs.push(("initial", xml_id(initial)));
        }
        if let Some(fail) = &s.fail {
            attrs.push(("rb:fail", fail.text.clone()));
        }
        // One timer per `after` block: its branches share the group.
        let mut timers: Vec<(u32, &Expr)> = Vec::new();
        for t in &s.transitions {
            if let Trigger::After { delay } = &t.trigger
                && !timers.iter().any(|(g, _)| *g == t.group)
            {
                timers.push((t.group, delay));
            }
        }
        let inits: Vec<&roadbook_ir::Var> = s.vars.iter().filter(|v| v.init.is_some()).collect();
        let empty = s.entry.is_empty()
            && s.exit.is_empty()
            && timers.is_empty()
            && s.params.is_empty()
            && s.vars.is_empty()
            && s.invokes.is_empty()
            && s.transitions.is_empty()
            && s.states.is_empty()
            && s.output.is_none();
        self.open(depth, tag, &attrs, empty);
        if empty {
            return;
        }
        let d = depth + 1;
        if !s.params.is_empty() || !s.vars.is_empty() {
            self.open(d, "datamodel", &[], false);
            self.comment(
                d + 1,
                "Roadbook gives these their values each time the state is entered and drops them when it is left",
            );
            for p in &s.params {
                self.comment(d + 1, &p.doc);
                let attrs = [
                    ("id", local_id(&s.id, &p.name)),
                    ("rb:type", p.ty.to_string()),
                    ("rb:param", "true".into()),
                ];
                self.open(d + 1, "data", &attrs, true);
            }
            for v in &s.vars {
                self.comment(d + 1, &v.doc);
                let attrs = [
                    ("id", local_id(&s.id, &v.name)),
                    ("rb:type", v.ty.to_string()),
                ];
                self.open(d + 1, "data", &attrs, true);
            }
            self.close(d, "datamodel");
        }
        if !s.entry.is_empty() || !timers.is_empty() || !inits.is_empty() {
            self.open(d, "onentry", &[], false);
            for v in &inits {
                let init = v.init.as_ref().map_or(String::new(), |e| e.text.clone());
                let attrs = [("location", local_id(&s.id, &v.name)), ("expr", init)];
                self.open(d + 1, "assign", &attrs, true);
            }
            self.effects(&s.entry, d + 1);
            for (group, delay) in &timers {
                let mut attrs = vec![
                    ("id", format!("{}_after_{group}", xml_id(&s.id))),
                    ("event", after_event(&s.id, *group)),
                ];
                match &delay.kind {
                    ExprKind::Lit(Literal::Duration(ms)) => {
                        attrs.push(("delay", format!("{ms}ms")))
                    }
                    _ => attrs.push(("delayexpr", delay.text.clone())),
                }
                self.open(d + 1, "send", &attrs, true);
            }
            self.close(d, "onentry");
        }
        if !s.exit.is_empty() || !timers.is_empty() {
            self.open(d, "onexit", &[], false);
            for (group, _) in &timers {
                let attrs = [("sendid", format!("{}_after_{group}", xml_id(&s.id)))];
                self.open(d + 1, "cancel", &attrs, true);
            }
            self.effects(&s.exit, d + 1);
            self.close(d, "onexit");
        }
        for inv in &s.invokes {
            let (ty, src, params) = match inv.kind {
                InvokeKind::Task => (
                    TASK_TYPE,
                    inv.src.clone(),
                    self.ir.tasks.get(&inv.src).map(|t| &t.params),
                ),
                InvokeKind::Machine => (
                    SCXML_TYPE,
                    format!("{}.scxml", inv.src),
                    self.ir.machines.get(&inv.src).map(|m| &m.params),
                ),
                InvokeKind::Call => (
                    CALL_TYPE,
                    inv.callee(),
                    inv.to
                        .as_ref()
                        .and_then(|to| self.ir.machines.get(to.peer()))
                        .and_then(|m| m.calls.get(&inv.src))
                        .map(|c| &c.params),
                ),
            };
            let mut attrs = vec![
                ("id", invoke_id(s, inv.alias.as_deref())),
                ("type", ty.to_string()),
                ("src", src),
            ];
            if let Some(alias) = &inv.alias {
                attrs.push(("rb:as", alias.clone()));
            }
            let empty = inv.args.is_empty();
            self.open(d, "invoke", &attrs, empty);
            if !empty {
                self.params(params, &inv.args, d + 1);
                self.close(d, "invoke");
            }
        }
        for t in &s.transitions {
            self.transition(s, t, d);
        }
        for c in &s.states {
            if c.kind.is_history() {
                self.history(s, c, d);
            } else {
                self.state(c, d);
            }
        }
        if let Some(output) = &s.output {
            self.open(d, "donedata", &[], false);
            self.open(d + 1, "content", &[("expr", output.text.clone())], true);
            self.close(d, "donedata");
        }
        self.close(depth, tag);
    }

    /// `<history>`, with the default transition SCXML requires: the parent's default entry,
    /// which is what Roadbook enters when the history state has recorded nothing.
    fn history(&mut self, parent: &State, h: &State, depth: usize) {
        self.comment(depth, &h.doc);
        let ty = if h.kind == StateKind::DeepHistory {
            "deep"
        } else {
            "shallow"
        };
        self.open(
            depth,
            "history",
            &[("id", xml_id(&h.id)), ("type", ty.to_string())],
            false,
        );
        let targets: Vec<String> = match &parent.initial {
            Some(initial) => vec![xml_id(initial)],
            None => parent
                .states
                .iter()
                .filter(|c| !c.kind.is_history())
                .map(|c| xml_id(&c.id))
                .collect(),
        };
        self.open(
            depth + 1,
            "transition",
            &[("target", targets.join(" "))],
            true,
        );
        self.close(depth, "history");
    }

    fn transition(&mut self, s: &State, t: &Transition, depth: usize) {
        let event = match &t.trigger {
            Trigger::Event { event } => Some(event.clone()),
            Trigger::Emitted { from, event } => Some(format!("{}.{event}", from.name)),
            Trigger::Call { call } => Some(call_event(call)),
            Trigger::Any => {
                let handled: Vec<&str> = s
                    .transitions
                    .iter()
                    .filter_map(|o| match &o.trigger {
                        Trigger::Event { event } => Some(event.as_str()),
                        _ => None,
                    })
                    .collect();
                // `on *` takes the machine's own events, internal ones included.
                let rest: Vec<&str> = self
                    .m
                    .events
                    .keys()
                    .chain(self.m.internal_events.keys())
                    .map(String::as_str)
                    .filter(|e| !handled.contains(e))
                    .collect();
                if rest.is_empty() {
                    self.comment(
                        depth,
                        "on * matches no event here: the state has an `on` for each",
                    );
                    return;
                }
                Some(rest.join(" "))
            }
            Trigger::After { .. } => Some(after_event(&s.id, t.group)),
            Trigger::Always => None,
            Trigger::Done => Some(format!("done.state.{}", xml_id(&s.id))),
            Trigger::InvokeDone { alias } => {
                Some(format!("done.invoke.{}", invoke_id(s, alias.as_deref())))
            }
            Trigger::SpawnDone { children } => Some(format!("done.spawn.{children}")),
            Trigger::Ended { from } => Some(format!("{}.ended", from.name)),
            Trigger::SpawnFail { children, error } => {
                let errors: Vec<&str> = if error == "*" {
                    let listed: Vec<&str> = s
                        .transitions
                        .iter()
                        .filter_map(|o| match &o.trigger {
                            Trigger::SpawnFail { children: c, error }
                                if c == children && error != "*" =>
                            {
                                Some(error.as_str())
                            }
                            _ => None,
                        })
                        .collect();
                    self.m
                        .children
                        .get(children)
                        .and_then(|c| match c.kind {
                            ChildrenKind::Machine => {
                                self.ir.machines.get(&c.src).map(|m| m.errors.as_slice())
                            }
                            ChildrenKind::Task => {
                                self.ir.tasks.get(&c.src).map(|t| t.errors.as_slice())
                            }
                        })
                        .unwrap_or_default()
                        .iter()
                        .map(String::as_str)
                        .filter(|e| !listed.contains(e))
                        .collect()
                } else {
                    vec![error.as_str()]
                };
                if errors.is_empty() {
                    self.comment(
                        depth,
                        "fail * matches no error here: another branch lists each",
                    );
                    return;
                }
                let names: Vec<String> = errors
                    .iter()
                    .map(|e| format!("error.{e}.spawn.{children}"))
                    .collect();
                Some(names.join(" "))
            }
            Trigger::InvokeFail { alias, error } => {
                let invoke = s.invoke_named(alias.as_deref());
                let errors: Vec<&str> = if error == "*" {
                    let listed: Vec<&str> = s
                        .transitions
                        .iter()
                        .filter_map(|o| match &o.trigger {
                            Trigger::InvokeFail { alias: a, error }
                                if a == alias && error != "*" =>
                            {
                                Some(error.as_str())
                            }
                            _ => None,
                        })
                        .collect();
                    let declared = invoke.and_then(|inv| match inv.kind {
                        InvokeKind::Task => self.ir.tasks.get(&inv.src).map(|t| &t.errors),
                        InvokeKind::Machine => self.ir.machines.get(&inv.src).map(|m| &m.errors),
                        InvokeKind::Call => inv
                            .to
                            .as_ref()
                            .and_then(|to| self.ir.machines.get(to.peer()))
                            .and_then(|m| m.calls.get(&inv.src))
                            .map(|c| &c.errors),
                    });
                    // A call also fails with the built-in `Unanswered`, which `*` covers.
                    let unanswered = invoke
                        .filter(|inv| inv.kind == InvokeKind::Call)
                        .map(|_| roadbook_ir::UNANSWERED);
                    declared
                        .into_iter()
                        .flatten()
                        .map(String::as_str)
                        .chain(unanswered)
                        .filter(|e| !listed.contains(e))
                        .collect()
                } else {
                    vec![error.as_str()]
                };
                if errors.is_empty() {
                    self.comment(
                        depth,
                        "fail * matches no error here: another branch lists each",
                    );
                    return;
                }
                let id = invoke_id(s, alias.as_deref());
                let names: Vec<String> = errors.iter().map(|e| format!("error.{e}.{id}")).collect();
                Some(names.join(" "))
            }
        };
        let mut attrs = Vec::new();
        if let Some(event) = event {
            attrs.push(("event", event));
        }
        if let Some(guard) = &t.guard {
            attrs.push(("cond", guard.text.clone()));
        }
        if t.targets.is_empty() {
            attrs.push(("type", "internal".into()));
        } else {
            let targets: Vec<String> = t.targets.iter().map(|x| xml_id(x)).collect();
            attrs.push(("target", targets.join(" ")));
            // A transition of the machine body does not leave the machine, unless it goes to a
            // top-level final state, which is outside the machine's `<state>`; an internal one
            // (`-> .X`) does not leave its state.
            let root = &self.m.root;
            let body = s.id == root.id
                && !root
                    .states
                    .iter()
                    .any(|c| c.kind == StateKind::Final && t.targets.contains(&c.id));
            if body || t.internal {
                attrs.push(("type", "internal".into()));
            }
            if t.internal && s.kind == StateKind::Parallel {
                self.comment(
                    depth,
                    "SCXML takes an internal transition of a parallel state as external, leaving every region; Roadbook leaves only the regions its targets are in",
                );
            }
        }
        let empty = t.effects.is_empty() && t.args.is_empty();
        self.open(depth, "transition", &attrs, empty);
        if !empty {
            self.effects(&t.effects, depth + 1);
            // The arguments, after the effects, as Roadbook evaluates them: each into the
            // parameter of the state it enters.
            for a in &t.args {
                let params = find_state(&self.m.root, &a.state).map(|s| s.params.as_slice());
                for (i, v) in a.values.iter().enumerate() {
                    let name = params
                        .and_then(|ps| ps.get(i))
                        .map_or_else(|| i.to_string(), |p| p.name.clone());
                    let attrs = [
                        ("location", local_id(&a.state, &name)),
                        ("expr", v.text.clone()),
                    ];
                    self.open(depth + 1, "assign", &attrs, true);
                }
            }
            self.close(depth, "transition");
        }
    }

    fn effects(&mut self, effects: &[Effect], depth: usize) {
        for e in effects {
            match e {
                Effect::Set { var, state, value } => {
                    let location = match state {
                        Some(state) => local_id(state, var),
                        None => var.clone(),
                    };
                    let attrs = [("location", location), ("expr", value.text.clone())];
                    self.open(depth, "assign", &attrs, true);
                }
                Effect::Do { action, args } => {
                    let params = self.ir.actions.get(action).map(|a| &a.params);
                    let attrs = [("action", action.clone())];
                    let empty = args.is_empty();
                    self.open(depth, "rb:do", &attrs, empty);
                    if !empty {
                        self.params(params, args, depth + 1);
                        self.close(depth, "rb:do");
                    }
                }
                Effect::Send {
                    to,
                    key,
                    event,
                    value,
                } => {
                    let target = match (to.kind, &to.state) {
                        (AddressKind::Child, Some(state)) => {
                            let id = match find_state(&self.m.root, state) {
                                Some(st) => invoke_id(st, Some(&to.name)),
                                None => format!("{}_invoke", xml_id(state)),
                            };
                            ("target", format!("#_{id}"))
                        }
                        (AddressKind::Children, _) => match key {
                            Some(k) => ("targetexpr", format!("{}[{}]", to.name, k.text)),
                            None => ("targetexpr", format!("{}.*", to.name)),
                        },
                        _ => ("targetexpr", to.name.clone()),
                    };
                    self.send(depth, vec![("event", event.clone()), target], value);
                }
                Effect::Emit { event, value } => {
                    let attrs = vec![("event", event.clone()), ("type", EMIT_TYPE.into())];
                    self.send(depth, attrs, value);
                }
                Effect::Reply {
                    call,
                    outcome,
                    value,
                } => {
                    let event = match (outcome, value.as_ref().map(|v| &v.ty)) {
                        (ReplyOutcome::Done, _) => "done".to_string(),
                        (ReplyOutcome::Fail, Some(TypeRef::Named(error))) => {
                            format!("error.{error}")
                        }
                        (ReplyOutcome::Fail, _) => "error".to_string(),
                    };
                    let attrs = vec![
                        ("event", event),
                        ("type", REPLY_TYPE.into()),
                        ("targetexpr", call.text.clone()),
                    ];
                    match value {
                        Some(value) => self.send(depth, attrs, value),
                        None => self.open(depth, "send", &attrs, true),
                    }
                }
                Effect::Spawn {
                    children,
                    src,
                    key,
                    args,
                } => {
                    let task = self
                        .m
                        .children
                        .get(children)
                        .is_some_and(|c| c.kind == ChildrenKind::Task);
                    let mut attrs = vec![("children", children.clone()), ("key", key.text.clone())];
                    let params = if task {
                        attrs.push(("type", TASK_TYPE.into()));
                        attrs.push(("src", src.clone()));
                        self.ir.tasks.get(src).map(|t| &t.params)
                    } else {
                        attrs.push(("src", format!("{src}.scxml")));
                        self.ir.machines.get(src).map(|m| &m.params)
                    };
                    let empty = args.is_empty();
                    self.open(depth, "rb:spawn", &attrs, empty);
                    if !empty {
                        self.params(params, args, depth + 1);
                        self.close(depth, "rb:spawn");
                    }
                }
                Effect::Stop { children, key } => {
                    let attrs = [("children", children.clone()), ("key", key.text.clone())];
                    self.open(depth, "rb:stop", &attrs, true);
                }
                // An immediate raise is SCXML's `<raise>`, or a `<send>` to the internal queue
                // when the event has fields. A delayed one is a `<send>` with no target, which
                // SCXML puts on the session's own external queue once its delay has passed;
                // a named one cancels the pending one of its name first, as Roadbook does.
                Effect::Raise {
                    event,
                    value,
                    delay: None,
                    ..
                } => {
                    let fields =
                        matches!(&value.kind, ExprKind::Record { fields } if !fields.is_empty());
                    if fields {
                        let attrs = vec![("event", event.clone()), ("target", "#_internal".into())];
                        self.send(depth, attrs, value);
                    } else {
                        self.open(depth, "raise", &[("event", event.clone())], true);
                    }
                }
                Effect::Raise {
                    event,
                    value,
                    delay: Some(delay),
                    name,
                } => {
                    let mut attrs = vec![("event", event.clone())];
                    match delay.kind {
                        ExprKind::Lit(Literal::Duration(ms)) => {
                            attrs.push(("delay", format!("{ms}ms")))
                        }
                        _ => attrs.push(("delayexpr", delay.text.clone())),
                    }
                    if let Some(n) = name {
                        self.open(depth, "cancel", &[("sendid", n.clone())], true);
                        attrs.push(("id", n.clone()));
                    }
                    self.send(depth, attrs, value);
                }
                Effect::Cancel { name } => {
                    self.open(depth, "cancel", &[("sendid", name.clone())], true);
                }
            }
        }
    }

    /// A `<send>` whose event's fields are its `<param>`s.
    fn send(&mut self, depth: usize, attrs: Vec<(&str, String)>, value: &Expr) {
        let fields = match &value.kind {
            ExprKind::Record { fields } => fields
                .iter()
                .map(|f| (f.name.clone(), f.value.text.clone()))
                .collect(),
            _ => Vec::new(),
        };
        let content = !matches!(value.kind, ExprKind::Record { .. });
        let empty = fields.is_empty() && !content;
        self.open(depth, "send", &attrs, empty);
        if empty {
            return;
        }
        if content {
            self.open(depth + 1, "content", &[("expr", value.text.clone())], true);
        }
        for (name, expr) in fields {
            self.open(depth + 1, "param", &[("name", name), ("expr", expr)], true);
        }
        self.close(depth, "send");
    }

    /// Arguments as `<param>`s named after the parameters they are passed to.
    fn params(&mut self, params: Option<&Vec<Field>>, args: &[Expr], depth: usize) {
        for (i, a) in args.iter().enumerate() {
            let name = params
                .and_then(|p| p.get(i))
                .map_or_else(|| format!("arg{i}"), |p| p.name.clone());
            self.open(
                depth,
                "param",
                &[("name", name), ("expr", a.text.clone())],
                true,
            );
        }
    }
}

fn after_event(state_id: &str, group: u32) -> String {
    format!("roadbook.after.{}.{group}", xml_id(state_id))
}

/// A character XML 1.0 allows, or U+FFFD in place of one it does not.
fn xml_char(c: char) -> char {
    match c {
        '\t' | '\n' | '\r' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'.. => c,
        _ => '\u{FFFD}',
    }
}

/// `text` as the value of a double-quoted attribute. Line breaks and tabs are character
/// references, so an XML parser's attribute normalisation keeps them.
fn attr(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars().map(xml_char) {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\n' => out.push_str("&#10;"),
            '\r' => out.push_str("&#13;"),
            '\t' => out.push_str("&#9;"),
            c => out.push(c),
        }
    }
    out
}

/// `text` inside a comment, which cannot hold `--` or end with `-`.
fn comment_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars().map(xml_char) {
        if c == '-' && out.ends_with('-') {
            out.push(' ');
        }
        out.push(c);
    }
    if out.ends_with('-') {
        out.push(' ');
    }
    out
}
