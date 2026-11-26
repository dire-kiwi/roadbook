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
//! }
//! ```
//!
//! - M is the machine and every machine it invokes, transitively (a machine
//!   that invokes itself is included once).
//! - F, A and K are the functions called (`call`), actions run (`do`) and
//!   tasks invoked by the machines in M.
//! - T is every type named by the machines in M and by F, A and K, closed
//!   under the types their declarations name. A type is named by the base of
//!   any TypeRef (parameters, results, variables, event fields, alias
//!   targets, record fields and every expression's `type`), by an entry of
//!   an `errors` list and by an `invoke.fail` trigger's `error` other than
//!   `"*"`. A name that is only an event of the machine being walked (the
//!   type of a bare `event`) is not a type and is skipped.
//! - `strip` removes every member named `doc`, `span` or `text` from every
//!   object, recursively. The only name-keyed map inside these values is a
//!   machine's `events`, whose keys are upper-case, so no user-chosen name
//!   is ever stripped.
//!
//! Comments, formatting, source positions and unused declarations therefore
//! do not change a hash; any change to behaviour or to a type a machine uses
//! does, including in a child machine.

use std::collections::BTreeSet;
use std::fmt;

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::expr::{Expr, ExprKind};
use crate::jcs;
use crate::model::{Effect, Field, InvokeKind, Ir, Machine, State, Trigger, TypeDef};
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
        /// `"type"`, `"function"`, `"action"`, `"task"` or `"machine"`.
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
}

struct Walker<'a> {
    ir: &'a Ir,
    out: Closure,
    pending_machines: Vec<String>,
    /// The machine being walked and its events.
    machine: String,
    events: BTreeSet<String>,
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
        if self.ir.types.contains_key(name) {
            self.out.types.insert(name.to_owned());
            Ok(())
        } else if self.events.contains(name) {
            Ok(())
        } else {
            Err(self.unresolved("type", name))
        }
    }

    fn type_ref(&mut self, t: &TypeRef) -> Result<(), HashError> {
        match t.named_base() {
            Some(n) => self.type_name(n),
            None => Ok(()),
        }
    }

    fn fields(&mut self, fields: &[Field]) -> Result<(), HashError> {
        fields.iter().try_for_each(|f| self.type_ref(&f.ty))
    }

    fn machine(&mut self, name: &str, m: &Machine) -> Result<(), HashError> {
        self.machine = name.to_owned();
        self.events = m.events.keys().cloned().collect();
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
        for e in m.events.values() {
            self.fields(&e.fields)?;
        }
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
            }
            inv.args.iter().try_for_each(|a| self.expr(a))?;
        }
        for t in &s.transitions {
            match &t.trigger {
                Trigger::After { delay } => self.expr(delay)?,
                Trigger::InvokeFail { error } if error != "*" => self.type_name(error)?,
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
    // Close the types over their own declarations.
    let mut pending: Vec<String> = w.out.types.iter().cloned().collect();
    while let Some(t) = pending.pop() {
        let before = w.out.types.clone();
        match &ir.types[&t].def {
            TypeDef::Record { fields } | TypeDef::Error { fields } => w.fields(fields)?,
            TypeDef::Alias { ty } => w.type_ref(ty)?,
            TypeDef::Enum { .. } | TypeDef::Extern => {}
        }
        pending.extend(w.out.types.difference(&before).cloned());
    }
    Ok(w.out)
}

/// Removes every `doc`, `span` and `text` member, recursively.
fn strip(v: &mut Value) {
    match v {
        Value::Object(m) => {
            for key in ["doc", "span", "text"] {
                m.remove(key);
            }
            m.values_mut().for_each(strip);
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
    view.insert("types".into(), section(&c.types, |n| &ir.types[n]));
    view.insert(
        "functions".into(),
        section(&c.functions, |n| &ir.functions[n]),
    );
    view.insert("actions".into(), section(&c.actions, |n| &ir.actions[n]));
    view.insert("tasks".into(), section(&c.tasks, |n| &ir.tasks[n]));
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
