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
//!   tasks invoked by the machines in M.
//! - R is every machine outside M that the machines in M, F, A and K name
//!   without invoking it: by a `ref` type, by the address of a `send`, of
//!   an `emitted` trigger or of a call invoke, or by a dotted event type
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
//!   value), or a dotted event type, is not a type and is skipped.
//! - `strip` removes every member named `doc`, `span`, `text` or
//!   `sensitive` from every object, recursively, except the keys of a
//!   machine's `children` and `calls` maps, which are kept as they are. The
//!   other name-keyed maps inside these values are a machine's `events` and
//!   `emits`, whose keys are upper-case, so no user-chosen name is ever
//!   stripped. A type declaration's `params` is removed too when it is `[]`,
//!   so a type that is not generic hashes as it did before generic types
//!   existed.
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
    Address, Call, Effect, Field, InvokeKind, Ir, Machine, State, Trigger, TypeDef, UNANSWERED,
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
        self.named_machine(&a.machine)
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
        self.events = m.events.keys().chain(m.emits.keys()).cloned().collect();
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
        for e in m.events.values().chain(m.emits.values()) {
            self.fields(&e.fields)?;
        }
        for c in m.children.values() {
            self.type_ref(&c.key)?;
            if !self.ir.machines.contains_key(&c.machine) {
                return Err(self.unresolved("machine", &c.machine));
            }
            self.pending_machines.push(c.machine.clone());
        }
        self.calls(m.calls.values())?;
        self.state(&m.root)
    }

    fn state(&mut self, s: &State) -> Result<(), HashError> {
        self.effects(&s.entry)?;
        self.effects(&s.exit)?;
        if let Some(inv) = &s.invoke {
            match inv.kind {
                InvokeKind::Task => {
                    if !self.ir.tasks.contains_key(&inv.src) {
                        return Err(self.unresolved("task", &inv.src));
                    }
                    self.out.tasks.insert(inv.src.clone());
                }
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
                    if !self.ir.machines[&to.machine].calls.contains_key(&inv.src) {
                        let name = format!("{}.{}", to.machine, inv.src);
                        return Err(self.unresolved("call", &name));
                    }
                }
            }
            inv.args.iter().try_for_each(|a| self.expr(a))?;
        }
        for t in &s.transitions {
            match &t.trigger {
                Trigger::After { delay } => self.expr(delay)?,
                Trigger::InvokeFail { error } | Trigger::SpawnFail { error, .. }
                    if error != "*" =>
                {
                    self.type_name(error)?
                }
                Trigger::Emitted { from, .. } => self.address(from)?,
                _ => {}
            }
            if let Some(g) = &t.guard {
                self.expr(g)?;
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
                Effect::Spawn { key, args, .. } => {
                    self.expr(key)?;
                    args.iter().try_for_each(|a| self.expr(a))?;
                }
                Effect::Stop { key, .. } => self.expr(key)?,
                Effect::Reply { call, value, .. } => {
                    self.expr(call)?;
                    if let Some(v) = value {
                        self.expr(v)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn expr(&mut self, e: &Expr) -> Result<(), HashError> {
        self.type_ref(&e.ty)?;
        match &e.kind {
            ExprKind::Lit(_) | ExprKind::Ref { .. } | ExprKind::Enum { .. } => Ok(()),
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
/// of a machine's `children` and `calls` maps, which are names the chart chose (a collection
/// or a call may be named `text`). Also removes what IR 0.5 added where it says nothing: a machine's empty
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
            if m.get("kind").and_then(Value::as_str) == Some("send")
                && m.get("key").is_some_and(Value::is_null)
            {
                m.remove("key");
            }
            for (key, value) in m.iter_mut() {
                match value {
                    Value::Object(named) if key == "children" || key == "calls" => {
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
    view.insert("tasks".into(), section(&c.tasks, |n| &ir.tasks[n]));
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
