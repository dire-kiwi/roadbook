//! The machine hash.
//!
//! `hash = "sha256:" + hex(SHA-256(JCS(view)))`, where `view` is the
//! machine's semantic view:
//!
//! ```text
//! {
//!   "machine":   <the machine's name>,
//!   "machines":  { name: strip(machine without "hash") }  for every machine in M,
//!   "types":     { name: strip(declaration) }             for every type in T,
//!   "functions": { name: strip(declaration) }             for every fn in F,
//!   "actions":   { … }, "tasks": { … }                    likewise
//!   "interfaces": { name: strip({events, emits, calls}) } for every machine in R
//! }
//! ```
//!
//! - M is the machine and every machine it invokes, transitively (a machine
//!   that invokes itself is included once).
//! - F, A and K are the functions called (`call`), actions run (`do`) and
//!   tasks invoked or spawned (the task of a `children` collection) by the machines in M.
//! - R is every machine outside M that the machines in M, F, A and K name
//!   without invoking it: by a `ref` type, by the address of a `send`, of
//!   an `emitted` or `ended` trigger or of a call invoke, or by a dotted event type
//!   `Machine.Event`. Only its interface, its `events`, `emits` and `calls`,
//!   is part of the view, so a change to how a peer works inside does not
//!   change the machines that talk to it.
//! - T is every type named by the machines in M, by F, A and K and by the
//!   interfaces of R, closed under the types their declarations name. A type is named by any name
//!   in any TypeRef (its base, type arguments and map key and value) (parameters, results, variables, event fields, alias
//!   targets, record fields and every expression's `type`), by an entry of
//!   an `errors` list and by an `invoke.fail` trigger's `error` other than
//!   `"*"` and the built-in `Unanswered`. A name that is an event of the
//!   machine being walked (the type of a bare `event`, or of an `emit`
//!   value), or a dotted event type (`Machine.Event`, or a task's `task.Event`),
//!   is not a type and is skipped. An address of kind `task` names no machine.
//! - `strip` removes every member named `doc`, `span`, `text` or
//!   `sensitive` from every object, recursively, except the keys of a
//!   machine's `calls` and `children` maps, which are names the chart chose
//!   and are kept as they are. The other name-keyed maps inside these values
//!   are a machine's `events` and `emits`, whose keys are upper-case, so no
//!   user-chosen name is ever stripped. It also removes a machine's
//!   `children` when it is empty and a `send` effect's `key` when it is
//!   `null`, and a type declaration's `params` is removed when it is `[]`, so
//!   a machine that uses neither keyed spawn nor generic types hashes as it
//!   did before they existed. A transition's `internal` is removed when it
//!   is `false`, and its `targets`, when it has at most one, is written as
//!   IR 0.8's `target` (the id, or `null`), so a machine with neither
//!   internal transitions nor several targets keeps its 0.8 hash. A
//!   collection of machines is written in IR 0.9's form, `{"machine", "key"}`
//!   without `kind` and `src`, and a `spawn` effect of one has `machine` in
//!   place of `src`; a task's `emits` is removed when it is `{}`. So a machine
//!   that uses no task emits and spawns no tasks keeps its 0.9 hash. A
//!   machine's `internalEvents` is removed when it is `{}`, so a machine with no
//!   internal event keeps its 0.10 hash. A state with at most one invoke is written
//!   with `invoke` in place of `invokes`, and its outcome triggers without `as`, so a
//!   machine with no state of several invokes keeps its 0.11 hash. A state's `params`
//!   and `vars` and a transition's `args` are removed when they are `[]`, and the
//!   `state` of a `ref` expression or a `set` effect when it is `null`, so a machine
//!   with no state parameters or variables keeps its 0.12 hash. A state's `default` is
//!   removed when it is `[]`, so a machine with no history default keeps its 0.13 hash.
//!   A state's `tags` and `meta` are always removed: they say nothing about what a run
//!   does, so adding a tag keeps the hash and the snapshots saved before it.
//!   `calls` and `strict` are always kept.
//!
//! Comments, formatting, source positions, `sensitive` marks and unused
//! declarations therefore do not change a hash (a mark changes only what
//! traces show, so a snapshot saved before it was added still restores); any change to behaviour or to a type a machine uses
//! does, including in a child machine.

use std::collections::BTreeSet;
use std::fmt;

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::expr::{Expr, ExprKind};
use crate::jcs;
use crate::model::{
    Address, Call, ChildrenKind, Effect, Field, InvokeKind, Ir, Machine, State, Trigger, TypeDef,
    UNANSWERED,
};
use crate::types::TypeRef;

/// The prefix of every hash.
pub const HASH_PREFIX: &str = "sha256:";

/// Why a hash could not be computed: the IR names something it does not
/// declare.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HashError {
    /// No machine with this name.
    UnknownMachine(String),
    /// A machine (or a declaration it uses) names an undeclared item.
    Unresolved {
        /// The machine being walked when the name was found.
        machine: String,
        /// `"type"`, `"event"`, `"function"`, `"action"`, `"task"`, `"machine"`
        /// or `"call"`.
        kind: &'static str,
        /// The missing name.
        name: String,
    },
}

impl fmt::Display for HashError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HashError::UnknownMachine(m) => write!(f, "the IR has no machine `{m}`"),
            HashError::Unresolved {
                machine,
                kind,
                name,
            } => write!(
                f,
                "machine `{machine}` refers to {kind} `{name}`, which the IR does not declare"
            ),
        }
    }
}

impl std::error::Error for HashError {}

#[derive(Default)]
struct Closure {
    machines: BTreeSet<String>,
    types: BTreeSet<String>,
    functions: BTreeSet<String>,
    actions: BTreeSet<String>,
    tasks: BTreeSet<String>,
    /// Machines named but not necessarily invoked: R before M is taken out.
    interfaces: BTreeSet<String>,
}

struct Walker<'a> {
    ir: &'a Ir,
    out: Closure,
    pending_machines: Vec<String>,
    /// The machine being walked and its events.
    machine: String,
    events: BTreeSet<String>,
    /// The type parameters of the generic declaration being walked: names that are not types.
    params: Vec<String>,
}

impl Walker<'_> {
    fn unresolved(&self, kind: &'static str, name: &str) -> HashError {
        HashError::Unresolved {
            machine: self.machine.clone(),
            kind,
            name: name.to_owned(),
        }
    }

    fn type_name(&mut self, name: &str) -> Result<(), HashError> {
        if name == UNANSWERED {
            // Built in: the type of `error` in a call's `fail Unanswered`.
            Ok(())
        } else if self.ir.types.contains_key(name) {
            self.out.types.insert(name.to_owned());
            Ok(())
        } else if self.events.contains(name) {
            Ok(())
        } else if let Some((task, event)) = name
            .split_once('.')
            .filter(|(t, _)| t.starts_with(|c: char| c.is_ascii_lowercase()))
        {
            // `task.Event`: an event a task emits. Like `Machine.Event` it names an event, not
            // a type; the task is in K through the invoke or collection that receives it.
            let found = self
                .ir
                .tasks
                .get(task)
                .is_some_and(|t| t.emits.contains_key(event));
            if !found {
                return Err(self.unresolved("event", name));
            }
            self.out.tasks.insert(task.to_owned());
            Ok(())
        } else if let Some((machine, event)) = name.split_once('.') {
            let found = self
                .ir
                .machines
                .get(machine)
                .is_some_and(|m| m.events.contains_key(event) || m.emits.contains_key(event));
            if !found {
                return Err(self.unresolved("event", name));
            }
            self.out.interfaces.insert(machine.to_owned());
            Ok(())
        } else {
            Err(self.unresolved("type", name))
        }
    }

    fn named_machine(&mut self, name: &str) -> Result<(), HashError> {
        if !self.ir.machines.contains_key(name) {
            return Err(self.unresolved("machine", name));
        }
        self.out.interfaces.insert(name.to_owned());
        Ok(())
    }

    fn type_ref(&mut self, t: &TypeRef) -> Result<(), HashError> {
        match t {
            TypeRef::Named(n) if self.params.contains(n) => Ok(()),
            TypeRef::Named(n) => self.type_name(n),
            TypeRef::Generic(n, args) => {
                self.type_name(n)?;
                args.iter().try_for_each(|a| self.type_ref(a))
            }
            TypeRef::Map(k, v) => {
                self.type_ref(k)?;
                self.type_ref(v)
            }
            TypeRef::List(t) | TypeRef::Optional(t) => self.type_ref(t),
            TypeRef::Ref(m) => self.named_machine(m),
            TypeRef::Builtin(_) | TypeRef::Call(_) => Ok(()),
        }
    }

    fn address(&mut self, a: &Address) -> Result<(), HashError> {
        match (&a.machine, &a.task) {
            (Some(m), _) => self.named_machine(m),
            // A task's address names no machine for R. Its task is in K through the invoke
            // or the collection.
            (None, Some(t)) => self.task(t),
            (None, None) => Err(self.unresolved("machine", "")),
        }
    }

    fn task(&mut self, name: &str) -> Result<(), HashError> {
        if !self.ir.tasks.contains_key(name) {
            return Err(self.unresolved("task", name));
        }
        self.out.tasks.insert(name.to_owned());
        Ok(())
    }

    fn fields(&mut self, fields: &[Field]) -> Result<(), HashError> {
        fields.iter().try_for_each(|f| self.type_ref(&f.ty))
    }

    fn calls<'c>(&mut self, calls: impl Iterator<Item = &'c Call>) -> Result<(), HashError> {
        for c in calls {
            self.fields(&c.params)?;
            if let Some(t) = &c.returns {
                self.type_ref(t)?;
            }
            for e in &c.errors {
                self.type_name(e)?;
            }
        }
        Ok(())
    }

    fn machine(&mut self, name: &str, m: &Machine) -> Result<(), HashError> {
        self.machine = name.to_owned();
        self.events = m
            .events
            .keys()
            .chain(m.internal_events.keys())
            .chain(m.emits.keys())
            .cloned()
            .collect();
        self.fields(&m.params)?;
        if let Some(t) = &m.returns {
            self.type_ref(t)?;
        }
        for e in &m.errors {
            self.type_name(e)?;
        }
        for v in &m.vars {
            self.type_ref(&v.ty)?;
            if let Some(e) = &v.init {
                self.expr(e)?;
            }
        }
        for e in m
            .events
            .values()
            .chain(m.internal_events.values())
            .chain(m.emits.values())
        {
            self.fields(&e.fields)?;
        }
        self.calls(m.calls.values())?;
        for c in m.children.values() {
            self.type_ref(&c.key)?;
            match c.kind {
                // A collection of tasks adds its task to K; M is unchanged.
                ChildrenKind::Task => self.task(&c.src)?,
                ChildrenKind::Machine => {
                    if !self.ir.machines.contains_key(&c.src) {
                        return Err(self.unresolved("machine", &c.src));
                    }
                    self.pending_machines.push(c.src.clone());
                }
            }
        }
        self.state(&m.root)
    }

    fn state(&mut self, s: &State) -> Result<(), HashError> {
        self.fields(&s.params)?;
        for v in &s.vars {
            self.type_ref(&v.ty)?;
            if let Some(e) = &v.init {
                self.expr(e)?;
            }
        }
        self.effects(&s.entry)?;
        self.effects(&s.exit)?;
        for inv in &s.invokes {
            match inv.kind {
                InvokeKind::Task => self.task(&inv.src)?,
                InvokeKind::Machine => {
                    if !self.ir.machines.contains_key(&inv.src) {
                        return Err(self.unresolved("machine", &inv.src));
                    }
                    self.pending_machines.push(inv.src.clone());
                }
                InvokeKind::Call => {
                    let Some(to) = &inv.to else {
                        return Err(self.unresolved("machine", ""));
                    };
                    self.address(to)?;
                    let has_call = to
                        .machine
                        .as_ref()
                        .is_some_and(|m| self.ir.machines[m].calls.contains_key(&inv.src));
                    if !has_call {
                        let name = format!("{}.{}", to.peer(), inv.src);
                        return Err(self.unresolved("call", &name));
                    }
                }
            }
            inv.args.iter().try_for_each(|a| self.expr(a))?;
        }
        for t in &s.transitions {
            match &t.trigger {
                Trigger::After { delay } => self.expr(delay)?,
                Trigger::InvokeFail { error, .. } | Trigger::SpawnFail { error, .. }
                    if error != "*" =>
                {
                    self.type_name(error)?
                }
                Trigger::Emitted { from, .. } | Trigger::Ended { from } => self.address(from)?,
                _ => {}
            }
            if let Some(g) = &t.guard {
                self.expr(g)?;
            }
            for a in &t.args {
                a.values.iter().try_for_each(|v| self.expr(v))?;
            }
            self.effects(&t.effects)?;
        }
        for e in s.output.iter().chain(&s.fail) {
            self.expr(e)?;
        }
        s.states.iter().try_for_each(|c| self.state(c))
    }

    fn effects(&mut self, effects: &[Effect]) -> Result<(), HashError> {
        for e in effects {
            match e {
                Effect::Set { value, .. } => self.expr(value)?,
                Effect::Do { action, args } => {
                    if !self.ir.actions.contains_key(action) {
                        return Err(self.unresolved("action", action));
                    }
                    self.out.actions.insert(action.clone());
                    args.iter().try_for_each(|a| self.expr(a))?;
                }
                Effect::Send { to, key, value, .. } => {
                    self.address(to)?;
                    if let Some(k) = key {
                        self.expr(k)?;
                    }
                    self.expr(value)?;
                }
                Effect::Emit { value, .. } => self.expr(value)?,
                Effect::Reply { call, value, .. } => {
                    self.expr(call)?;
                    if let Some(v) = value {
                        self.expr(v)?;
                    }
                }
                Effect::Spawn { key, args, .. } => {
                    self.expr(key)?;
                    args.iter().try_for_each(|a| self.expr(a))?;
                }
                Effect::Stop { key, .. } => self.expr(key)?,
                Effect::Raise { value, delay, .. } => {
                    self.expr(value)?;
                    if let Some(d) = delay {
                        self.expr(d)?;
                    }
                }
                Effect::Cancel { .. } => {}
            }
        }
        Ok(())
    }

    fn expr(&mut self, e: &Expr) -> Result<(), HashError> {
        self.type_ref(&e.ty)?;
        match &e.kind {
            ExprKind::Lit(_)
            | ExprKind::Ref { .. }
            | ExprKind::Enum { .. }
            | ExprKind::In { .. } => Ok(()),
            ExprKind::Field { of, .. } => self.expr(of),
            ExprKind::Call { func, args } => {
                if !self.ir.functions.contains_key(func) {
                    return Err(self.unresolved("function", func));
                }
                self.out.functions.insert(func.clone());
                args.iter().try_for_each(|a| self.expr(a))
            }
            ExprKind::Op { args, .. } => args.iter().try_for_each(|a| self.expr(a)),
            ExprKind::Record { fields } => fields.iter().try_for_each(|f| self.expr(&f.value)),
        }
    }
}

fn closure(ir: &Ir, name: &str) -> Result<Closure, HashError> {
    if !ir.machines.contains_key(name) {
        return Err(HashError::UnknownMachine(name.to_owned()));
    }
    let mut w = Walker {
        ir,
        out: Closure::default(),
        pending_machines: vec![name.to_owned()],
        machine: name.to_owned(),
        events: BTreeSet::new(),
        params: Vec::new(),
    };
    while let Some(m) = w.pending_machines.pop() {
        if w.out.machines.insert(m.clone()) {
            w.machine(&m, &ir.machines[&m])?;
        }
    }
    // The signatures of the host declarations used. Their names were checked
    // where they were referenced; their types are reported against the root.
    w.machine = name.to_owned();
    w.events.clear();
    for f in w.out.functions.clone() {
        let d = &ir.functions[&f];
        w.fields(&d.params)?;
        w.type_ref(&d.returns)?;
    }
    for a in w.out.actions.clone() {
        w.fields(&ir.actions[&a].params)?;
    }
    for t in w.out.tasks.clone() {
        let d = &ir.tasks[&t];
        w.fields(&d.params)?;
        if let Some(r) = &d.returns {
            w.type_ref(r)?;
        }
        for e in &d.errors {
            w.type_name(e)?;
        }
        for e in d.emits.values() {
            w.fields(&e.fields)?;
        }
    }
    // Close the types over their own declarations, and add the interfaces of
    // the machines named but not walked whole, with the types their events
    // name, until nothing new is found. (A `ref` alias among the types names a
    // machine; event fields cannot hold a `ref`, so this ends at once.)
    let mut walked: BTreeSet<String> = BTreeSet::new();
    let mut pending: Vec<String> = w.out.types.iter().cloned().collect();
    loop {
        while let Some(t) = pending.pop() {
            let before = w.out.types.clone();
            match &ir.types[&t].def {
                TypeDef::Record { params, fields } => {
                    w.params = params.clone();
                    w.fields(fields)?;
                }
                TypeDef::Error { fields } => w.fields(fields)?,
                TypeDef::Alias { params, ty } => {
                    w.params = params.clone();
                    w.type_ref(ty)?;
                }
                TypeDef::Enum { .. } | TypeDef::Extern => {}
            }
            w.params.clear();
            pending.extend(w.out.types.difference(&before).cloned());
        }
        let peers: Vec<String> = w
            .out
            .interfaces
            .iter()
            .filter(|p| !w.out.machines.contains(*p) && !walked.contains(*p))
            .cloned()
            .collect();
        if peers.is_empty() {
            break;
        }
        let before = w.out.types.clone();
        for p in peers {
            let m = &ir.machines[&p];
            w.machine = p.clone();
            for e in m.events.values().chain(m.emits.values()) {
                w.fields(&e.fields)?;
            }
            w.calls(m.calls.values())?;
            walked.insert(p);
        }
        w.machine = name.to_owned();
        pending.extend(w.out.types.difference(&before).cloned());
    }
    w.out.interfaces = walked;
    Ok(w.out)
}

/// Removes every `doc`, `span`, `text` and `sensitive` member, recursively, except the keys
/// of a machine's `calls` and `children` maps, which are names the chart chose (a call or a
/// collection may be named `text`). Also removes what keyed spawn added where it says nothing: a machine's empty
/// `children` and a `send` effect's `null` `key`, so a machine that spawns nothing keeps the
/// hash it had before keyed spawn.
fn strip(v: &mut Value) {
    match v {
        Value::Object(m) => {
            for key in ["doc", "span", "text", "sensitive"] {
                m.remove(key);
            }
            if m.contains_key("root")
                && m.get("children")
                    .is_some_and(|c| c.as_object().is_some_and(Map::is_empty))
            {
                m.remove("children");
            }
            if m.contains_key("root")
                && m.get("internalEvents")
                    .is_some_and(|c| c.as_object().is_some_and(Map::is_empty))
            {
                m.remove("internalEvents");
            }
            if m.get("kind").and_then(Value::as_str) == Some("send")
                && m.get("key").is_some_and(Value::is_null)
            {
                m.remove("key");
            }
            locals_as_before(m);
            if m.contains_key("trigger") && m.contains_key("group") {
                transition_as_before(m);
            }
            if m.contains_key("root") {
                machine_as_before(m);
            }
            if m.contains_key("invokes") && m.contains_key("transitions") {
                state_as_before(m);
                default_as_before(m);
                // What tools and host code read about a state, never what a run does. `meta`
                // goes before the recursion, which would strip its keys.
                m.remove("tags");
                m.remove("meta");
            }
            for (key, value) in m.iter_mut() {
                match value {
                    Value::Object(named) if key == "calls" || key == "children" => {
                        named.values_mut().for_each(strip)
                    }
                    value => strip(value),
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(strip),
        _ => {}
    }
}

/// Writes what IR 0.13 added as 0.12 did where it says nothing new: a state's `params` and
/// `vars` that are `[]`, and the `state` of a `ref` expression or a `set` effect that is
/// `null`, are removed (a transition's empty `args` goes in [`transition_as_before`]). A
/// machine with no state parameters or variables keeps its 0.12 hash. An address's `state`
/// is kept: an address is told from a `ref` expression by having no `path`.
fn locals_as_before(m: &mut Map<String, Value>) {
    let empty = |m: &Map<String, Value>, key: &str| {
        m.get(key)
            .is_some_and(|v| v.as_array().is_some_and(Vec::is_empty))
    };
    if m.contains_key("states") && m.contains_key("transitions") {
        for key in ["params", "vars"] {
            if empty(m, key) {
                m.remove(key);
            }
        }
    }
    let kind = m.get("kind").and_then(Value::as_str);
    let scoped = (kind == Some("ref") && m.contains_key("path")) || kind == Some("set");
    if scoped && m.get("state").is_some_and(Value::is_null) {
        m.remove("state");
    }
}

/// Writes a transition as IR 0.8 did where 0.9 says nothing new: an `internal` that is
/// `false` is removed, and `targets` with at most one id becomes `target`, the id or `null`.
/// A machine with neither internal transitions nor several targets keeps its 0.8 hash.
fn transition_as_before(m: &mut Map<String, Value>) {
    if m.get("internal") == Some(&Value::Bool(false)) {
        m.remove("internal");
    }
    if m.get("args")
        .is_some_and(|v| v.as_array().is_some_and(Vec::is_empty))
    {
        m.remove("args");
    }
    if let Some(Value::Array(targets)) = m.get("targets") {
        if targets.len() <= 1 {
            let target = targets.first().cloned().unwrap_or(Value::Null);
            m.remove("targets");
            m.insert("target".into(), target);
        }
    }
}

/// Writes a state with at most one invoke as IR 0.11 did: `invoke`, the one invoke or `null`,
/// in place of `invokes`, and its `invoke.done` and `invoke.fail` triggers without `as`. A
/// machine with no state of several invokes keeps its 0.11 hash.
fn state_as_before(m: &mut Map<String, Value>) {
    let Some(Value::Array(invokes)) = m.get("invokes") else {
        return;
    };
    if invokes.len() > 1 {
        return;
    }
    let invoke = invokes.first().cloned().unwrap_or(Value::Null);
    m.remove("invokes");
    m.insert("invoke".into(), invoke);
    if let Some(Value::Array(transitions)) = m.get_mut("transitions") {
        for t in transitions {
            if let Some(Value::Object(trigger)) = t.get_mut("trigger")
                && matches!(
                    trigger.get("kind").and_then(Value::as_str),
                    Some("invoke.done" | "invoke.fail")
                )
            {
                trigger.remove("as");
            }
        }
    }
}

/// Writes a state as IR 0.13 did where 0.14's `default` says nothing new: a `default` that is
/// `[]` is removed. A machine with no history state that has a default keeps its 0.13 hash.
fn default_as_before(m: &mut Map<String, Value>) {
    if m.get("default")
        .is_some_and(|v| v.as_array().is_some_and(Vec::is_empty))
    {
        m.remove("default");
    }
}

/// Writes what IR 0.10 changed in a machine as 0.9 did where it says nothing new: a
/// collection of machines is `{"machine": src, "key"}` (no `kind`, no `src`), and a `spawn`
/// effect of one has `machine` in place of `src`. A machine that spawns no tasks keeps its
/// 0.9 hash.
fn machine_as_before(m: &mut Map<String, Value>) {
    let mut machine_collections = BTreeSet::new();
    if let Some(Value::Object(children)) = m.get_mut("children") {
        for (name, c) in children.iter_mut() {
            let Value::Object(c) = c else { continue };
            if c.get("kind").and_then(Value::as_str) == Some("machine") {
                c.remove("kind");
                if let Some(src) = c.remove("src") {
                    c.insert("machine".into(), src);
                }
                machine_collections.insert(name.clone());
            }
        }
    }
    if let Some(root) = m.get_mut("root") {
        spawns_as_before(root, &machine_collections);
    }
}

/// Renames `src` to `machine` in every `spawn` effect of a collection in `machines`, anywhere
/// below `v`.
fn spawns_as_before(v: &mut Value, machines: &BTreeSet<String>) {
    match v {
        Value::Object(m) => {
            if m.get("kind").and_then(Value::as_str) == Some("spawn")
                && m.get("children")
                    .and_then(Value::as_str)
                    .is_some_and(|c| machines.contains(c))
                && let Some(src) = m.remove("src")
            {
                m.insert("machine".into(), src);
            }
            m.values_mut().for_each(|v| spawns_as_before(v, machines));
        }
        Value::Array(a) => a.iter_mut().for_each(|v| spawns_as_before(v, machines)),
        _ => {}
    }
}

fn section<'a, T: serde::Serialize + 'a>(
    names: &BTreeSet<String>,
    get: impl Fn(&str) -> &'a T,
) -> Value {
    let mut map = Map::new();
    for n in names {
        // Declarations are plain data; serialising them cannot fail.
        let mut v = serde_json::to_value(get(n)).unwrap_or(Value::Null);
        strip(&mut v);
        map.insert(n.clone(), v);
    }
    Value::Object(map)
}

/// The semantic view of machine `name`, the value whose canonical form is
/// hashed. Exposed so other implementations can compare against it.
pub fn semantic_view(ir: &Ir, name: &str) -> Result<Value, HashError> {
    let c = closure(ir, name)?;
    let mut machines = section(&c.machines, |n| &ir.machines[n]);
    if let Value::Object(map) = &mut machines {
        for m in map.values_mut() {
            if let Value::Object(m) = m {
                m.remove("hash");
            }
        }
    }
    let mut view = Map::new();
    view.insert("machine".into(), Value::String(name.to_owned()));
    view.insert("machines".into(), machines);
    let mut types = section(&c.types, |n| &ir.types[n]);
    // A type that is not generic has the hash it had before `params` existed.
    if let Value::Object(map) = &mut types {
        for t in map.values_mut() {
            if let Value::Object(t) = t
                && t.get("params")
                    .is_some_and(|p| p.as_array().is_some_and(Vec::is_empty))
            {
                t.remove("params");
            }
        }
    }
    view.insert("types".into(), types);
    view.insert(
        "functions".into(),
        section(&c.functions, |n| &ir.functions[n]),
    );
    view.insert("actions".into(), section(&c.actions, |n| &ir.actions[n]));
    let mut tasks = section(&c.tasks, |n| &ir.tasks[n]);
    // A task that emits nothing has the hash it had before tasks could emit.
    if let Value::Object(map) = &mut tasks {
        for t in map.values_mut() {
            if let Value::Object(t) = t
                && t.get("emits")
                    .is_some_and(|e| e.as_object().is_some_and(Map::is_empty))
            {
                t.remove("emits");
            }
        }
    }
    view.insert("tasks".into(), tasks);
    let mut interfaces = Map::new();
    for n in &c.interfaces {
        let m = &ir.machines[n];
        let mut v = serde_json::json!({ "events": m.events, "emits": m.emits, "calls": m.calls });
        strip(&mut v);
        interfaces.insert(n.clone(), v);
    }
    view.insert("interfaces".into(), Value::Object(interfaces));
    Ok(Value::Object(view))
}

/// The hash of machine `name`: `"sha256:"` and 64 lowercase hex digits.
///
/// The machine's current `hash` field is ignored, so this can be called on
/// an IR whose hashes are not yet filled in.
pub fn machine_hash(ir: &Ir, name: &str) -> Result<String, HashError> {
    let view = semantic_view(ir, name)?;
    let digest = Sha256::digest(jcs::to_canonical_vec(&view));
    let mut out = String::with_capacity(HASH_PREFIX.len() + 64);
    out.push_str(HASH_PREFIX);
    for b in digest {
        out.push(char::from_digit(u32::from(b >> 4), 16).unwrap_or('0'));
        out.push(char::from_digit(u32::from(b & 0xf), 16).unwrap_or('0'));
    }
    Ok(out)
}

/// Computes every machine's hash and stores it in its `hash` field. The
/// compiler calls this last, after type checking.
pub fn fill_hashes(ir: &mut Ir) -> Result<(), HashError> {
    let hashes = ir
        .machines
        .keys()
        .map(|n| machine_hash(ir, n))
        .collect::<Result<Vec<_>, _>>()?;
    for (m, h) in ir.machines.values_mut().zip(hashes) {
        m.hash = h;
    }
    Ok(())
}
