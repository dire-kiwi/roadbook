//! Redaction (`spec/trace.md#redaction`): the values of trace records with every value of a
//! `sensitive` field, parameter or variable replaced by [`REDACTED`]. Only trace records are
//! redacted: snapshots, commands and what host code sees keep the real values.
//!
//! A value is redacted by its declared type, which the IR gives: a record's or error's
//! sensitive fields are replaced, and its other fields, list items and present optional
//! values are redacted by their own types, through aliases. A member a type does not declare
//! is left as it is.

use std::collections::{HashMap, HashSet};

use roadbook_ir::{Field, Ir, Machine, TypeDef, TypeRef};
use serde_json::Value;

use crate::{Input, InvokeEntry, Object, Record, Saved};

/// What a redacted value is written as.
pub const REDACTED: &str = "<redacted>";

/// What the IR marks sensitive, precomputed for [`crate::Program`].
#[derive(Debug, Clone, Default)]
pub(crate) struct Marks {
    /// Whether anything in the IR is marked: if not, nothing is ever redacted.
    any: bool,
    /// For each declared type, whether one of its values can hold a sensitive value.
    carries: HashMap<String, bool>,
}

impl Marks {
    pub fn new(ir: &Ir) -> Marks {
        let fields = |fs: &[Field]| fs.iter().any(|f| f.sensitive);
        let any = ir.types.values().any(|t| match &t.def {
            TypeDef::Record { fields: fs } | TypeDef::Error { fields: fs } => fields(fs),
            _ => false,
        }) || ir.functions.values().any(|f| fields(&f.params))
            || ir.actions.values().any(|a| fields(&a.params))
            || ir.tasks.values().any(|t| fields(&t.params))
            || ir.machines.values().any(|m| {
                fields(&m.params)
                    || m.vars.iter().any(|v| v.sensitive)
                    || m.events
                        .values()
                        .chain(m.emits.values())
                        .any(|e| fields(&e.fields))
            });
        let mut marks = Marks {
            any,
            carries: HashMap::new(),
        };
        if any {
            for name in ir.types.keys() {
                let c = carries_named(ir, name, &mut HashSet::new());
                marks.carries.insert(name.clone(), c);
            }
        }
        marks
    }
}

fn carries_named(ir: &Ir, name: &str, seen: &mut HashSet<String>) -> bool {
    if !seen.insert(name.to_owned()) {
        return false;
    }
    match ir.types.get(name).map(|t| &t.def) {
        Some(TypeDef::Record { fields } | TypeDef::Error { fields }) => fields
            .iter()
            .any(|f| f.sensitive || carries_ref(ir, &f.ty, seen)),
        Some(TypeDef::Alias { ty }) => carries_ref(ir, ty, seen),
        _ => false,
    }
}

fn carries_ref(ir: &Ir, t: &TypeRef, seen: &mut HashSet<String>) -> bool {
    match t {
        TypeRef::Named(n) => carries_named(ir, n, seen),
        TypeRef::List(e) | TypeRef::Optional(e) => carries_ref(ir, e, seen),
        TypeRef::Builtin(_) | TypeRef::Ref(_) => false,
    }
}

/// Redacts values of one IR.
pub(crate) struct Redactor<'p> {
    pub ir: &'p Ir,
    pub marks: &'p Marks,
}

impl Redactor<'_> {
    pub fn active(&self) -> bool {
        self.marks.any
    }

    /// Redacts `v`, a value of type `t`.
    pub fn value(&self, t: &TypeRef, v: &mut Value) {
        match t {
            TypeRef::Builtin(_) | TypeRef::Ref(_) => {}
            TypeRef::List(e) => {
                if let Value::Array(items) = v {
                    items.iter_mut().for_each(|i| self.value(e, i));
                }
            }
            TypeRef::Optional(e) => {
                if !v.is_null() {
                    self.value(e, v);
                }
            }
            TypeRef::Named(n) => {
                if !self.marks.carries.get(n).copied().unwrap_or(false) {
                    return;
                }
                match self.ir.types.get(n).map(|t| &t.def) {
                    Some(TypeDef::Record { fields } | TypeDef::Error { fields }) => {
                        self.fields(fields, v)
                    }
                    Some(TypeDef::Alias { ty }) => self.value(ty, v),
                    _ => {}
                }
            }
        }
    }

    /// Redacts `v`, an object with these fields (a record, an error or an event).
    pub fn fields(&self, fields: &[Field], v: &mut Value) {
        if let Value::Object(obj) = v {
            self.object(fields, obj);
        }
    }

    /// Redacts `obj`, keyed by these fields or parameters.
    pub fn object(&self, fields: &[Field], obj: &mut Object) {
        for f in fields {
            if let Some(member) = obj.get_mut(&f.name) {
                if f.sensitive {
                    *member = Value::String(REDACTED.to_owned());
                } else {
                    self.value(&f.ty, member);
                }
            }
        }
    }

    /// Redacts `v`, by its tag one of `m`'s `emits` if `emits`, else of its `events`.
    fn event(&self, m: &Machine, emits: bool, v: &mut Value) {
        let Some(t) = tag(v) else {
            return;
        };
        let decl = if emits {
            m.emits.get(&t)
        } else {
            m.events.get(&t)
        };
        if let Some(e) = decl {
            self.fields(&e.fields, v);
        }
    }

    /// Redacts `v`, a tagged error, by its tag.
    fn error(&self, v: &mut Value) {
        if let Some(t) = tag(v) {
            self.value(&TypeRef::Named(t), v);
        }
    }

    /// Redacts the input `input` of a run of `m`, whose running invokes are `invokes`.
    pub fn input(&self, m: &Machine, invokes: &[InvokeEntry], input: &mut Input) {
        if !self.active() {
            return;
        }
        match input {
            Input::Event { value } | Input::Message { value, .. } => self.event(m, false, value),
            // The receiving run does not always know the sender's machine (a peer, or a child
            // whose invoke has ended), so the event is redacted as each machine that emits an
            // event of its name would redact it.
            Input::Emitted { value, .. } => {
                for other in self.ir.machines.values() {
                    self.event(other, true, value);
                }
            }
            Input::InvokeDone { invoke, value } => {
                match invokes.iter().find(|i| i.id == *invoke) {
                    Some(inv) => {
                        if let Some(t) = self.returns(&inv.src) {
                            self.value(t, value);
                        }
                    }
                    // A stale outcome: the invoke has ended, and with it what it ran. Redacted
                    // as every task and machine of the IR would redact its result.
                    None => {
                        let tasks = self.ir.tasks.values().filter_map(|t| t.returns.as_ref());
                        let machines = self.ir.machines.values().filter_map(|m| m.returns.as_ref());
                        for t in tasks.chain(machines) {
                            self.value(t, value);
                        }
                    }
                }
            }
            Input::InvokeFail { error, .. } => self.error(error),
            Input::InvokeDefect { .. }
            | Input::After { .. }
            | Input::Cancel
            | Input::Defect { .. } => {}
        }
    }

    fn returns(&self, src: &str) -> Option<&TypeRef> {
        match self.ir.tasks.get(src) {
            Some(t) => t.returns.as_ref(),
            None => self.ir.machines.get(src).and_then(|m| m.returns.as_ref()),
        }
    }

    /// Redacts a record a run of `m` emits, other than an `event`, `dropped` or `stale` record,
    /// whose input [`Redactor::input`] redacts when it is taken.
    pub fn record(&self, m: &Machine, record: &mut Record) {
        if !self.active() {
            return;
        }
        match record {
            Record::Started { input, .. } => self.object(&m.params, input),
            Record::Restored { snapshot, .. } => self.saved(m, snapshot),
            Record::Set { var, value } => {
                if let Some(v) = m.vars.iter().find(|v| v.name == *var) {
                    if v.sensitive {
                        *value = Value::String(REDACTED.to_owned());
                    } else {
                        self.value(&v.ty, value);
                    }
                }
            }
            Record::Action { action, args } => {
                if let Some(a) = self.ir.actions.get(action.as_str()) {
                    self.object(&a.params, args);
                }
            }
            Record::InvokeStarted { src, args, .. } => {
                let params = match self.ir.tasks.get(src.as_str()) {
                    Some(t) => Some(&t.params),
                    None => self.ir.machines.get(src.as_str()).map(|m| &m.params),
                };
                if let Some(params) = params {
                    self.object(params, args);
                }
            }
            Record::Emitted { value, .. } => self.event(m, true, value),
            Record::Done { output, .. } => {
                if let Some(t) = &m.returns {
                    self.value(t, output);
                }
            }
            Record::Failed { error, .. } => self.error(error),
            // `sent` and the core's `undelivered` are redacted where the send's address is
            // known (`Redactor::sent`).
            Record::Sent { .. } | Record::Undelivered { .. } => {}
            Record::Event { .. } | Record::Dropped { .. } | Record::Stale { .. } => {}
            Record::Entered { .. }
            | Record::Exited { .. }
            | Record::Transition { .. }
            | Record::InvokeCancelled { .. }
            | Record::TimerStarted { .. }
            | Record::TimerCancelled { .. }
            | Record::Cancelled
            | Record::Defect { .. }
            | Record::Suspended => {}
        }
    }

    /// Redacts `value`, an event sent to a run of machine `to`.
    pub fn sent(&self, to: &str, value: &mut Value) {
        if !self.active() {
            return;
        }
        if let Some(m) = self.ir.machines.get(to) {
            self.event(m, false, value);
        }
    }

    /// Redacts the copy of a saved snapshot of a run of `m` that a `restored` record holds.
    fn saved(&self, m: &Machine, saved: &mut Saved) {
        self.object(&m.params, &mut saved.input);
        for v in &m.vars {
            if let Some(value) = saved.vars.get_mut(&v.name) {
                if v.sensitive {
                    *value = Value::String(REDACTED.to_owned());
                } else {
                    self.value(&v.ty, value);
                }
            }
        }
        match &mut saved.result {
            Some(crate::RunResult::Output(output)) => {
                if let Some(t) = &m.returns {
                    self.value(t, output);
                }
            }
            Some(crate::RunResult::Error(error)) => self.error(error),
            Some(crate::RunResult::Defect(_)) | None => {}
        }
    }
}

fn tag(v: &Value) -> Option<String> {
    v.get("_tag").and_then(Value::as_str).map(str::to_owned)
}
