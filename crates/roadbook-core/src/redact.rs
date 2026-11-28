//! Redaction (`spec/trace.md#redaction`): the values of trace records with every value of a
//! `sensitive` field, parameter or variable replaced by [`REDACTED`]. Only trace records are
//! redacted: snapshots, commands and what host code sees keep the real values.
//!
//! A value is redacted by its declared type, which the IR gives: a record's or error's
//! sensitive fields are replaced, and its other fields, list items, map values and present
//! optional values are redacted by their own types, through aliases, with the arguments of an
//! instance of a generic type substituted. A member a type does not declare is left as it is.

use std::collections::{HashMap, HashSet};

use roadbook_ir::{Call, ChildrenKind, Field, Ir, Machine, TypeDef, TypeRef};
use serde_json::Value;

use crate::{Defect, Input, InvokeEntry, Object, Record, Saved, SpawnedEntry};

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
            TypeDef::Record { fields: fs, .. } | TypeDef::Error { fields: fs } => fields(fs),
            _ => false,
        }) || ir.functions.values().any(|f| fields(&f.params))
            || ir.actions.values().any(|a| fields(&a.params))
            || ir
                .tasks
                .values()
                .any(|t| fields(&t.params) || t.emits.values().any(|e| fields(&e.fields)))
            || ir.machines.values().any(|m| {
                fields(&m.params)
                    || m.vars.iter().any(|v| v.sensitive)
                    || m.calls.values().any(|c| fields(&c.params))
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

/// Whether a value of the declared type `name` can hold a sensitive value whatever its type
/// arguments: a type parameter (not a key of `types`) carries nothing of its own. An instance
/// also carries what its arguments do (see [`Redactor::carries`]).
fn carries_named(ir: &Ir, name: &str, seen: &mut HashSet<String>) -> bool {
    if !seen.insert(name.to_owned()) {
        return false;
    }
    match ir.types.get(name).map(|t| &t.def) {
        Some(TypeDef::Record { fields, .. } | TypeDef::Error { fields }) => fields
            .iter()
            .any(|f| f.sensitive || carries_ref(ir, &f.ty, seen)),
        Some(TypeDef::Alias { ty, .. }) => carries_ref(ir, ty, seen),
        _ => false,
    }
}

fn carries_ref(ir: &Ir, t: &TypeRef, seen: &mut HashSet<String>) -> bool {
    match t {
        TypeRef::Named(n) => carries_named(ir, n, seen),
        TypeRef::Generic(n, args) => {
            carries_named(ir, n, seen) || args.iter().any(|a| carries_ref(ir, a, seen))
        }
        TypeRef::List(e) | TypeRef::Optional(e) | TypeRef::Map(_, e) => carries_ref(ir, e, seen),
        TypeRef::Builtin(_) | TypeRef::Ref(_) | TypeRef::Call(_) => false,
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

    /// Whether a value of type `t` can hold a sensitive value. An instance of a generic type
    /// can if its type can whatever the arguments, or if an argument can: the compiler makes
    /// every type parameter used, so each argument's values are in the instance's.
    fn carries(&self, t: &TypeRef) -> bool {
        match t {
            TypeRef::Named(n) => self.marks.carries.get(n).copied().unwrap_or(false),
            TypeRef::Generic(n, args) => {
                self.marks.carries.get(n).copied().unwrap_or(false)
                    || args.iter().any(|a| self.carries(a))
            }
            TypeRef::List(e) | TypeRef::Optional(e) | TypeRef::Map(_, e) => self.carries(e),
            TypeRef::Builtin(_) | TypeRef::Ref(_) | TypeRef::Call(_) => false,
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
            TypeRef::Builtin(_) | TypeRef::Ref(_) | TypeRef::Call(_) => {}
            TypeRef::List(e) => match v {
                Value::Array(items) => items.iter_mut().for_each(|i| self.value(e, i)),
                _ => redact(v),
            },
            TypeRef::Optional(e) => {
                if !v.is_null() {
                    self.value(e, v);
                }
            }
            TypeRef::Map(_, e) => match v {
                Value::Object(entries) => entries.values_mut().for_each(|x| self.value(e, x)),
                _ => redact(v),
            },
            TypeRef::Named(_) | TypeRef::Generic(..) => {
                if let Some((_, _, fields)) = self.ir.fields_of(t) {
                    return self.fields(&fields, v);
                }
                let expanded = self.ir.expand_top(t);
                if *expanded != *t {
                    self.value(&expanded, v);
                }
            }
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

    /// Redacts `v`, by its tag one of `m`'s `emits` if `emits`, else of its own events (its
    /// `events` and `internalEvents`).
    fn event(&self, m: &Machine, emits: bool, v: &mut Value) {
        let t = tag(v).unwrap_or_default();
        let decl = if emits {
            m.emits.get(&t)
        } else {
            m.own_event(&t)
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

    /// Redacts `v`, an event a task emitted, as task `task`'s emitted event of its tag, or,
    /// when the task is not known (a stale one, whose invoke or spawned task has ended), as
    /// each task of the IR that emits an event of its tag with exactly its members would.
    fn task_emitted(&self, task: Option<&str>, v: &mut Value) {
        let t = tag(v).unwrap_or_default();
        if let Some(task) = task {
            let decl = self.ir.tasks.get(task).and_then(|d| d.emits.get(&t));
            return self.tagged(decl.map(|e| e.fields.as_slice()).into_iter(), v);
        }
        let members: Vec<&String> = match v {
            Value::Object(obj) => obj.keys().filter(|k| *k != "_tag").collect(),
            _ => Vec::new(),
        };
        let fits = |fields: &[Field]| {
            fields.len() == members.len() && fields.iter().all(|f| members.contains(&&f.name))
        };
        let decls: Vec<&[Field]> = self
            .ir
            .tasks
            .values()
            .filter_map(|d| d.emits.get(&t))
            .map(|e| e.fields.as_slice())
            .filter(|fields| fits(fields))
            .collect();
        self.tagged(decls.into_iter(), v);
    }

    /// Redacts `v`, a tagged error, by its tag.
    fn error(&self, v: &mut Value) {
        let t = tag(v).unwrap_or_default();
        let decl = match self.ir.types.get(&t).map(|d| &d.def) {
            Some(TypeDef::Error { fields } | TypeDef::Record { fields, .. }) => {
                Some(fields.as_slice())
            }
            _ => None,
        };
        self.tagged(decl.into_iter(), v);
    }

    /// Redacts the input `input` of a run of `m`, whose running invokes are `invokes` and
    /// running spawned children `spawned`.
    pub fn input(
        &self,
        m: &Machine,
        invokes: &[InvokeEntry],
        spawned: &[SpawnedEntry],
        input: &mut Input,
    ) {
        if !self.active() {
            return;
        }
        match input {
            Input::Event { value } | Input::Message { value, .. } | Input::Raised { value } => {
                self.event(m, false, value)
            }
            // The receiving run does not always know the sender's machine (a peer, or a child
            // whose invoke has ended), so the event is redacted as each machine that emits an
            // event of its name would redact it.
            Input::Emitted { value, .. } => self.emitted(value),
            // The task that the invoke or spawned task runs; a stale one's is not known.
            Input::TaskEmitted { invoke, value } => {
                let task = match invokes.iter().find(|i| i.id == *invoke) {
                    Some(inv) => Some(inv.src.as_str()),
                    None => spawned
                        .iter()
                        .find(|c| c.id == *invoke)
                        .and_then(|c| m.children.get(&c.children))
                        .map(|c| c.src.as_str()),
                };
                self.task_emitted(task, value);
            }
            Input::Call { call, args, .. } => {
                let params = m.calls.get(call.as_str()).map(|c| &c.params);
                self.object(params.map_or(&[][..], |p| p), args);
            }
            Input::InvokeDone { invoke, value } => {
                match invokes.iter().find(|i| i.id == *invoke) {
                    // A task, machine or call that returns nothing gives `null`: anything else
                    // is redacted whole.
                    Some(inv) => match self.returns(m, &inv.src) {
                        Some(t) => self.value(t, value),
                        None if value.is_null() => {}
                        None => redact(value),
                    },
                    // A stale outcome: the invoke has ended, and with it what it ran, so the
                    // type of the value is not known. Redacted whole: the run ignores it.
                    None => redact(value),
                }
            }
            Input::InvokeFail { error, .. } | Input::SpawnFail { error, .. } => self.error(error),
            Input::SpawnDone { spawn, value } => {
                let child = spawned
                    .iter()
                    .find(|c| c.id == *spawn)
                    .and_then(|c| self.collection(m, &c.children));
                match child {
                    Some((_, returns)) => match returns {
                        Some(t) => self.value(t, value),
                        None if value.is_null() => {}
                        None => redact(value),
                    },
                    // A stale outcome: the child is no longer in the snapshot, so the type of
                    // the value is not known.
                    None => redact(value),
                }
            }
            Input::SpawnDefect { .. }
            | Input::InvokeDefect { .. }
            | Input::After { .. }
            // A peer's end holds no value.
            | Input::Ended { .. }
            | Input::Cancel
            | Input::Defect { .. } => {}
        }
    }

    /// The parameters and result type of the machine or task the children of collection
    /// `children` of `m` run.
    fn collection(&self, m: &Machine, children: &str) -> Option<(&[Field], Option<&TypeRef>)> {
        let c = m.children.get(children)?;
        match c.kind {
            ChildrenKind::Machine => self
                .ir
                .machines
                .get(&c.src)
                .map(|m| (m.params.as_slice(), m.returns.as_ref())),
            ChildrenKind::Task => self
                .ir
                .tasks
                .get(&c.src)
                .map(|t| (t.params.as_slice(), t.returns.as_ref())),
        }
    }

    /// The result type of what an invoke of a run of `m` runs: task or machine `src`, or the
    /// call `src` names (`inventory.reserve`).
    fn returns(&self, m: &Machine, src: &str) -> Option<&TypeRef> {
        if src.contains('.') {
            return self.call(m, src).and_then(|c| c.returns.as_ref());
        }
        match self.ir.tasks.get(src) {
            Some(t) => t.returns.as_ref(),
            None => self.ir.machines.get(src).and_then(|m| m.returns.as_ref()),
        }
    }

    /// The call that a call invoke of a run of `m` makes, by its `src` as an invoke entry
    /// gives it: the `ref` parameter or variable, a dot, and the call's name.
    fn call(&self, m: &Machine, src: &str) -> Option<&Call> {
        let (name, call) = src.split_once('.')?;
        let ty = m
            .params
            .iter()
            .find(|p| p.name == name)
            .map(|p| &p.ty)
            .or_else(|| m.vars.iter().find(|v| v.name == name).map(|v| &v.ty))?;
        let peer = match &*self.ir.expand_top(ty) {
            TypeRef::Ref(peer) => peer.clone(),
            TypeRef::Optional(t) => match &*self.ir.expand_top(t) {
                TypeRef::Ref(peer) => peer.clone(),
                _ => return None,
            },
            _ => return None,
        };
        self.ir.machines.get(&peer)?.calls.get(call)
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
                    None if src.contains('.') => self.call(m, src).map(|c| &c.params),
                    None => self.ir.machines.get(src.as_str()).map(|m| &m.params),
                };
                self.object(params.map_or(&[][..], |p| p), args);
            }
            Record::SpawnStarted { src, args, .. } => {
                let params = match self.ir.tasks.get(src.as_str()) {
                    Some(t) => Some(&t.params),
                    None => self.ir.machines.get(src.as_str()).map(|m| &m.params),
                };
                self.object(params.map_or(&[][..], |p| p), args);
            }
            Record::Emitted { value, .. } => self.event(m, true, value),
            Record::Raised { value } | Record::RaiseScheduled { value, .. } => {
                self.event(m, false, value)
            }
            // A `reply`'s result by the call's result type, and its error by its tag.
            Record::Replied {
                call,
                outcome,
                value,
                ..
            } => match outcome {
                roadbook_ir::ReplyOutcome::Fail => self.error(value),
                roadbook_ir::ReplyOutcome::Done => {
                    match m.calls.get(call.as_str()).map(|c| &c.returns) {
                        Some(Some(t)) => self.value(t, value),
                        _ if value.is_null() => {}
                        _ => redact(value),
                    }
                }
            },
            Record::Done { output, .. } => self.output(m, output),
            Record::Failed { error, .. } => self.error(error),
            // `sent` and the core's `undelivered` are redacted where the send's address is
            // known (`Redactor::sent`).
            Record::Sent { .. } | Record::Undelivered { .. } => {}
            Record::Event { .. } | Record::Dropped { .. } | Record::Stale { .. } => {}
            // A strict machine's `unhandled` input as an `event` or `dropped` record shows it:
            // an event, a message or a call, which need no invoke or spawned child.
            Record::Defect {
                defect: Defect::Unhandled { event },
            } => self.input(m, &[], &[], event),
            Record::Entered { .. }
            | Record::Exited { .. }
            | Record::Transition { .. }
            | Record::InvokeCancelled { .. }
            | Record::SpawnStopped { .. }
            | Record::TimerStarted { .. }
            | Record::TimerCancelled { .. }
            | Record::RaiseCancelled { .. }
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

    /// Redacts the copy of a saved snapshot of a run of `m` that a `restored` record holds,
    /// and each child run's snapshot nested in it by the child's own machine (a `restored`
    /// record holds none, but a redacted snapshot shows nothing wherever it is nested).
    fn saved(&self, m: &Machine, saved: &mut Saved) {
        let nested = saved
            .invokes
            .iter_mut()
            .filter_map(|i| i.child.as_deref_mut())
            .chain(
                saved
                    .spawned
                    .iter_mut()
                    .flatten()
                    .filter_map(|c| c.child.as_deref_mut()),
            );
        for child in nested {
            if let Some(cm) = self.ir.machines.get(&child.machine) {
                self.saved(cm, child);
            }
        }
        self.object(&m.params, &mut saved.input);
        for (name, value) in saved.vars.iter_mut() {
            self.var(m, name, value);
        }
        for ch in saved.spawned.iter_mut().flatten() {
            let params = self.collection(m, &ch.children).map(|(p, _)| p);
            self.object(params.unwrap_or_default(), &mut ch.args);
        }
        for sc in &mut saved.scheduled {
            self.event(m, false, &mut sc.value);
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
