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

    /// Whether a value of type `t` can hold a sensitive value.
    fn carries(&self, t: &TypeRef) -> bool {
        match t {
            TypeRef::Named(n) => self.marks.carries.get(n).copied().unwrap_or(false),
            TypeRef::List(e) | TypeRef::Optional(e) => self.carries(e),
            TypeRef::Builtin(_) | TypeRef::Ref(_) => false,
        }
    }

    /// Redacts `v`, a value of type `t`. A value of a type that can hold a sensitive value is
    /// redacted whole when it does not have the type's shape (a list that is not an array, a
    /// record that is not an object), so a value host code got wrong shows nothing.
    pub fn value(&self, t: &TypeRef, v: &mut Value) {
        if !self.carries(t) {
            return;
        }
        match t {
            TypeRef::Builtin(_) | TypeRef::Ref(_) => {}
            TypeRef::List(e) => match v {
                Value::Array(items) => items.iter_mut().for_each(|i| self.value(e, i)),
                _ => redact(v),
            },
            TypeRef::Optional(e) => {
                if !v.is_null() {
                    self.value(e, v);
                }
            }
            TypeRef::Named(n) => match self.ir.types.get(n).map(|t| &t.def) {
                Some(TypeDef::Record { fields } | TypeDef::Error { fields }) => {
                    self.fields(fields, v)
                }
                Some(TypeDef::Alias { ty }) => self.value(ty, v),
                _ => {}
            },
        }
    }

    /// Redacts `v`, a record, error or event with these fields that can hold a sensitive
    /// value: redacted whole if it is not an object.
    fn fields(&self, fields: &[Field], v: &mut Value) {
        match v {
            Value::Object(obj) => self.object(fields, obj),
            _ => redact(v),
        }
    }

    /// Redacts `obj`, keyed by these fields or parameters: a sensitive one's member whole, any
    /// other declared one's by its type, and a member none declares (other than `_tag`) whole.
    pub fn object(&self, fields: &[Field], obj: &mut Object) {
        for (name, member) in obj.iter_mut() {
            match fields.iter().find(|f| f.name == *name) {
                Some(f) if f.sensitive => redact(member),
                Some(f) => self.value(&f.ty, member),
                None if name == "_tag" => {}
                None => redact(member),
            }
        }
    }

    /// Redacts `v`, a tagged value (an event or an error) that `decls` may declare: by each
    /// declaration that can hold a sensitive value. One that none declares has every member
    /// but `_tag` redacted, and one that is not a tagged object is redacted whole.
    fn tagged<'d>(&self, decls: impl Iterator<Item = &'d [Field]>, v: &mut Value) {
        let Value::Object(obj) = v else {
            return redact(v);
        };
        if !obj.get("_tag").is_some_and(Value::is_string) {
            return redact(v);
        }
        let mut declared = false;
        for fields in decls {
            declared = true;
            if fields.iter().any(|f| f.sensitive || self.carries(&f.ty)) {
                self.object(fields, obj);
            }
        }
        if !declared {
            self.object(&[], obj);
        }
    }

    /// Redacts `v`, by its tag one of `m`'s `emits` if `emits`, else of its `events`.
    fn event(&self, m: &Machine, emits: bool, v: &mut Value) {
        let t = tag(v).unwrap_or_default();
        let decl = if emits {
            m.emits.get(&t)
        } else {
            m.events.get(&t)
        };
        self.tagged(decl.map(|e| e.fields.as_slice()).into_iter(), v);
    }

    /// Redacts `v`, an emitted event, as each machine of the IR that emits an event of its tag
    /// with exactly its members would: the receiving run does not always know which machine
    /// sent it (a peer, or a child whose invoke has ended), and the sender's own declaration is
    /// always one of those, since a core builds the event with every field it declares.
    fn emitted(&self, v: &mut Value) {
        let t = tag(v).unwrap_or_default();
        let members: Vec<&String> = match v {
            Value::Object(obj) => obj.keys().filter(|k| *k != "_tag").collect(),
            _ => Vec::new(),
        };
        let fits = |fields: &[Field]| {
            fields.len() == members.len() && fields.iter().all(|f| members.contains(&&f.name))
        };
        let decls: Vec<&[Field]> = self
            .ir
            .machines
            .values()
            .filter_map(|m| m.emits.get(&t))
            .map(|e| e.fields.as_slice())
            .filter(|fields| fits(fields))
            .collect();
        self.tagged(decls.into_iter(), v);
    }

    /// Redacts `v`, a tagged error, by its tag.
    fn error(&self, v: &mut Value) {
        let t = tag(v).unwrap_or_default();
        let decl = match self.ir.types.get(&t).map(|d| &d.def) {
            Some(TypeDef::Error { fields } | TypeDef::Record { fields }) => Some(fields.as_slice()),
            _ => None,
        };
        self.tagged(decl.into_iter(), v);
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
            Input::Emitted { value, .. } => self.emitted(value),
            Input::InvokeDone { invoke, value } => {
                match invokes.iter().find(|i| i.id == *invoke) {
                    // A task or machine that returns nothing gives `null`: anything else is
                    // redacted whole.
                    Some(inv) => match self.returns(&inv.src) {
                        Some(t) => self.value(t, value),
                        None if value.is_null() => {}
                        None => redact(value),
                    },
                    // A stale outcome: the invoke has ended, and with it what it ran, so the
                    // type of the value is not known. Redacted whole: the run ignores it.
                    None => redact(value),
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
            Record::Set { var, value } => self.var(m, var, value),
            Record::Action { action, args } => {
                let params = self.ir.actions.get(action.as_str()).map(|a| &a.params);
                self.object(params.map_or(&[][..], |p| p), args);
            }
            Record::InvokeStarted { src, args, .. } => {
                let params = match self.ir.tasks.get(src.as_str()) {
                    Some(t) => Some(&t.params),
                    None => self.ir.machines.get(src.as_str()).map(|m| &m.params),
                };
                self.object(params.map_or(&[][..], |p| p), args);
            }
            Record::Emitted { value, .. } => self.event(m, true, value),
            Record::Done { output, .. } => self.output(m, output),
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

    /// Redacts `value`, the value of variable `name` of `m`: whole if it is sensitive, or if `m`
    /// has no such variable.
    fn var(&self, m: &Machine, name: &str, value: &mut Value) {
        match m.vars.iter().find(|v| v.name == name) {
            Some(v) if !v.sensitive => self.value(&v.ty, value),
            _ => redact(value),
        }
    }

    /// Redacts `output`, the output of a run of `m`: `null` for a machine that returns nothing,
    /// so anything else is redacted whole.
    fn output(&self, m: &Machine, output: &mut Value) {
        match &m.returns {
            Some(t) => self.value(t, output),
            None if output.is_null() => {}
            None => redact(output),
        }
    }

    /// Redacts `value`, an event sent to a run of machine `to`.
    pub fn sent(&self, to: &str, value: &mut Value) {
        if !self.active() {
            return;
        }
        match self.ir.machines.get(to) {
            Some(m) => self.event(m, false, value),
            None => self.tagged(std::iter::empty(), value),
        }
    }

    /// Redacts the copy of a saved snapshot of a run of `m` that a `restored` record holds.
    fn saved(&self, m: &Machine, saved: &mut Saved) {
        self.object(&m.params, &mut saved.input);
        for (name, value) in saved.vars.iter_mut() {
            self.var(m, name, value);
        }
        match &mut saved.result {
            Some(crate::RunResult::Output(output)) => self.output(m, output),
            Some(crate::RunResult::Error(error)) => self.error(error),
            Some(crate::RunResult::Defect(_)) | None => {}
        }
    }
}

fn redact(v: &mut Value) {
    *v = Value::String(REDACTED.to_owned());
}

fn tag(v: &Value) -> Option<String> {
    v.get("_tag").and_then(Value::as_str).map(str::to_owned)
}
