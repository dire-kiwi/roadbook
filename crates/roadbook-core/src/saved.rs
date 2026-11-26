//! Checking a saved snapshot before it is restored (`spec/execution.md#restoring-a-snapshot`):
//! the schema, the machine, the hash and a migration, the status, `ref` parameters, and
//! whether it fits the machine. The checks are made in the order the spec lists them, and the
//! first that fails gives the reason.

use std::collections::HashSet;
use std::fmt;

use roadbook_ir::{Builtin, Field, StateKind, Trigger, TypeDef, TypeRef};
use serde_json::{Map, Value};

use crate::program::Program;
use crate::{RunResult, Saved};

/// Why a saved snapshot was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Refusal {
    /// It does not match `snapshot.schema.json`, its version is not 1 or 2, or it does not
    /// fit the machine.
    Invalid,
    /// The IR has no machine of its name, or it is not the machine asked for.
    Machine,
    /// Its hash is not the machine's, and no migration was given (or the migration's result
    /// has another hash too).
    Ir,
    /// The migration failed.
    Migration,
    /// Its status is not `running`.
    Ended,
    /// The machine has a `ref` parameter: restoring a run that holds `ref`s is not specified
    /// yet.
    Ref,
}

impl Refusal {
    /// The reason as the spec names it: `invalid`, `machine`, `ir`, `migration`, `ended` or
    /// `ref`.
    pub fn as_str(self) -> &'static str {
        match self {
            Refusal::Invalid => "invalid",
            Refusal::Machine => "machine",
            Refusal::Ir => "ir",
            Refusal::Migration => "migration",
            Refusal::Ended => "ended",
            Refusal::Ref => "ref",
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A saved snapshot that cannot be restored, with the reason and what was wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreError {
    refusal: Refusal,
    message: String,
}

impl RestoreError {
    /// A refusal for `refusal`, saying what was wrong.
    pub fn new(refusal: Refusal, message: impl Into<String>) -> RestoreError {
        RestoreError {
            refusal,
            message: message.into(),
        }
    }

    pub(crate) fn invalid(message: impl Into<String>) -> RestoreError {
        RestoreError::new(Refusal::Invalid, message)
    }

    /// The reason: `invalid`, `machine`, `ir`, `migration`, `ended` or `ref`.
    pub fn reason(&self) -> &'static str {
        self.refusal.as_str()
    }

    /// The reason as an enum.
    pub fn refusal(&self) -> Refusal {
        self.refusal
    }

    /// What was wrong.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for RestoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the saved snapshot was refused ({}): {}",
            self.refusal, self.message
        )
    }
}

impl std::error::Error for RestoreError {}

/// A migration: the old saved snapshot to a new one, both plain JSON. `Err` (or a panic,
/// which the binding layer catches) is a failed migration.
pub type Migration<'a> = dyn FnMut(Value) -> Result<Value, String> + 'a;

impl Program {
    /// Checks `saved` against the IR before restoring it, in the spec's order, and gives the
    /// snapshot to restore and, if `migrate` was called, the hash the snapshot had before.
    /// `machine` is the machine restore was asked to restore, or `None` for the one the
    /// snapshot names. `migrate` is called at most once, and only when the hashes differ;
    /// its result is checked from the top, without a second migration, as a snapshot of the
    /// same machine: a migration cannot turn it into a snapshot of another (`machine`).
    /// Numbers are read in any form: an integral one written as a float (`2.0`, `-0.0`) is
    /// the integer, and the snapshot given back has it in integer form.
    pub fn check_saved(
        &self,
        mut saved: Value,
        machine: Option<&str>,
        migrate: Option<&mut Migration<'_>>,
    ) -> Result<(Saved, Option<String>), RestoreError> {
        integral_numbers(&mut saved);
        schema(&saved).map_err(RestoreError::invalid)?;
        let name = saved["machine"].as_str().unwrap_or_default();
        let Some((m, _)) = self.machine(name) else {
            return Err(RestoreError::new(
                Refusal::Machine,
                format!("the IR has no machine `{name}`"),
            ));
        };
        if let Some(want) = machine.filter(|w| *w != name) {
            return Err(RestoreError::new(
                Refusal::Machine,
                format!("it is a snapshot of `{name}`, not of `{want}`"),
            ));
        }
        let hash = saved["ir"].as_str().unwrap_or_default();
        if hash != m.hash {
            let Some(migrate) = migrate else {
                return Err(RestoreError::new(
                    Refusal::Ir,
                    format!(
                        "it was saved with `{name}` at {hash}, and the IR has it at {}",
                        m.hash
                    ),
                ));
            };
            let old = hash.to_owned();
            let want = machine.unwrap_or(name).to_owned();
            let new = migrate(saved).map_err(|e| {
                RestoreError::new(Refusal::Migration, format!("the migration failed: {e}"))
            })?;
            let (saved, _) = self.check_saved(new, Some(&want), None)?;
            return Ok((saved, Some(old)));
        }
        if saved["status"] != "running" {
            return Err(RestoreError::new(
                Refusal::Ended,
                format!("the run has ended: its status is {}", saved["status"]),
            ));
        }
        if let Some(p) = m.params.iter().find(|p| self.holds_ref(&p.ty)) {
            return Err(RestoreError::new(
                Refusal::Ref,
                format!(
                    "`{name}` has the `ref` parameter `{}`; restoring a run that holds `ref`s \
                     is not supported yet",
                    p.name
                ),
            ));
        }
        let saved: Saved =
            serde_json::from_value(saved).map_err(|e| RestoreError::invalid(e.to_string()))?;
        self.fits(&saved).map_err(RestoreError::invalid)?;
        Ok((saved, None))
    }

    /// Checks `event` as an event that host code gives a run of `machine`: one of the events
    /// the machine accepts, with exactly its fields, each of its type. Gives it with its
    /// integers in integer form, or says what is wrong.
    pub fn check_event(&self, machine: &str, mut event: Value) -> Result<Value, String> {
        let m = self
            .ir()
            .machines
            .get(machine)
            .ok_or_else(|| format!("the IR has no machine `{machine}`"))?;
        integral_numbers(&mut event);
        let tag = event
            .get("_tag")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some(decl) = m.events.get(tag) else {
            let tag = event.get("_tag").unwrap_or(&Value::Null);
            return Err(format!("`{machine}` has no event {tag}"));
        };
        if !self.fields_fit(machine, Some(tag), &decl.fields, &event) {
            return Err(format!("{event} is not a `{machine}` event `{tag}`"));
        }
        Ok(event)
    }

    /// Whether `ty` is `ref M`, `ref M?` or `ref M[]`, through aliases.
    fn holds_ref(&self, ty: &TypeRef) -> bool {
        match self.resolve(ty) {
            TypeRef::Ref(_) => true,
            TypeRef::Optional(t) | TypeRef::List(t) => self.holds_ref(t),
            _ => false,
        }
    }

    /// Whether a running snapshot fits its machine (the last `invalid` row of the spec).
    fn fits(&self, s: &Saved) -> Result<(), String> {
        let (m, idx) = self.machine(&s.machine).ok_or("no such machine")?;
        if s.result.is_some() {
            return Err("`result` is not null, though the run is running".into());
        }
        // `active` is a configuration, in document order.
        let mut active = Vec::new();
        for id in &s.active {
            let i = *idx
                .by_id
                .get(id)
                .ok_or_else(|| format!("`active` names `{id}`, which `{}` has not", s.machine))?;
            if active.last().is_some_and(|&l| l >= i) {
                return Err("`active` is not in document order".into());
            }
            active.push(i);
        }
        if active.first() != Some(&0) {
            return Err("`active` does not start with the machine itself".into());
        }
        let is_active = |i: usize| active.binary_search(&i).is_ok();
        for &i in &active {
            let st = idx.state(m, i);
            if let Some(p) = idx.parents[i] {
                if !is_active(p) {
                    return Err(format!("`{}` is active but its parent is not", st.id));
                }
            }
            if st.kind.is_history() {
                return Err(format!(
                    "`active` names `{}`, which is a history state",
                    st.id
                ));
            }
            let on = idx.children[i].iter().filter(|&&c| is_active(c)).count();
            match st.kind {
                StateKind::Compound if on != 1 => {
                    return Err(format!(
                        "the compound state `{}` has {on} active children",
                        st.id
                    ));
                }
                StateKind::Parallel if on != idx.children[i].len() => {
                    return Err(format!(
                        "not every region of the parallel state `{}` is active",
                        st.id
                    ));
                }
                _ => {}
            }
        }
        // History.
        for (h, record) in s.history.iter().flatten() {
            self.recordable(&s.machine, h, record)?;
        }
        // Invokes.
        let mut ids = HashSet::new();
        let mut states = HashSet::new();
        for inv in &s.invokes {
            let i = idx.by_id.get(&inv.state).copied().filter(|&i| is_active(i));
            let Some(i) = i else {
                return Err(format!(
                    "invoke {} belongs to a state that is not active",
                    inv.id
                ));
            };
            if idx.state(m, i).invoke.as_ref().map(|v| &v.src) != Some(&inv.src) {
                return Err(format!(
                    "the state `{}` has no invoke of `{}`",
                    inv.state, inv.src
                ));
            }
            if !states.insert(&inv.state) {
                return Err(format!("the state `{}` has two invokes", inv.state));
            }
            if !ids.insert(inv.id) || inv.id >= s.next_invoke {
                return Err(format!(
                    "the invoke id {} is repeated or not below nextInvoke",
                    inv.id
                ));
            }
        }
        // Spawned children: their ids are invoke ids.
        let mut keys = HashSet::new();
        for ch in s.spawned.iter().flatten() {
            let decl = m.children.get(&ch.children).ok_or_else(|| {
                format!(
                    "spawned child {} is of `{}`, which is not a collection of `{}`",
                    ch.id, ch.children, s.machine
                )
            })?;
            if !self.value_fits(&s.machine, &decl.key, &ch.key) {
                return Err(format!(
                    "the key of spawned child {} is not a {}",
                    ch.id, decl.key
                ));
            }
            let child = self
                .ir()
                .machines
                .get(&decl.machine)
                .ok_or_else(|| format!("no machine `{}`", decl.machine))?;
            if ch.args.len() != child.params.len() {
                return Err(format!(
                    "the arguments of spawned child {} are not exactly `{}`'s parameters",
                    ch.id, decl.machine
                ));
            }
            for p in &child.params {
                let v = ch.args.get(&p.name).ok_or_else(|| {
                    format!("spawned child {} lacks the argument `{}`", ch.id, p.name)
                })?;
                if !self.value_fits(&decl.machine, &p.ty, v) {
                    return Err(format!(
                        "the argument `{}` of spawned child {} is not a {}",
                        p.name, ch.id, p.ty
                    ));
                }
            }
            if !keys.insert((ch.children.as_str(), ch.key.to_string())) {
                return Err(format!(
                    "two spawned children of `{}` have the key {}",
                    ch.children, ch.key
                ));
            }
            if !ids.insert(ch.id) || ch.id >= s.next_invoke {
                return Err(format!(
                    "the id {} of a spawned child is repeated or not below nextInvoke",
                    ch.id
                ));
            }
        }
        // Timers.
        let mut ids = HashSet::new();
        let mut blocks = HashSet::new();
        for tm in &s.timers {
            let i = idx.by_id.get(&tm.state).copied().filter(|&i| is_active(i));
            let Some(i) = i else {
                return Err(format!(
                    "timer {} belongs to a state that is not active",
                    tm.id
                ));
            };
            let ts = &idx.state(m, i).transitions;
            let first = matches!(ts.get(tm.index), Some(t) if matches!(t.trigger, Trigger::After { .. })
                && !ts[..tm.index].iter().any(|u| u.group == t.group));
            if !first {
                return Err(format!(
                    "timer {}'s index {} is not the first transition of an `after` block of `{}`",
                    tm.id, tm.index, tm.state
                ));
            }
            if !blocks.insert((&tm.state, tm.index)) {
                return Err(format!(
                    "two timers of `{}` have the index {}",
                    tm.state, tm.index
                ));
            }
            if !ids.insert(tm.id) || tm.id >= s.next_timer {
                return Err(format!(
                    "the timer id {} is repeated or not below nextTimer",
                    tm.id
                ));
            }
        }
        // Values.
        if s.input.len() != m.params.len() {
            return Err("`input` does not have exactly the machine's parameters".into());
        }
        for p in &m.params {
            let v = s
                .input
                .get(&p.name)
                .ok_or_else(|| format!("`input` lacks the parameter `{}`", p.name))?;
            if !self.value_fits(&s.machine, &p.ty, v) {
                return Err(format!("the parameter `{}` is not a {}", p.name, p.ty));
            }
        }
        for (name, v) in &s.vars {
            let var = m
                .vars
                .iter()
                .find(|x| x.name == *name)
                .ok_or_else(|| format!("`vars` has `{name}`, which is not a variable"))?;
            if !self.value_fits(&s.machine, &var.ty, v) {
                return Err(format!("the variable `{name}` is not a {}", var.ty));
            }
        }
        for var in &m.vars {
            if matches!(self.resolve(&var.ty), TypeRef::Optional(_))
                && !s.vars.contains_key(&var.name)
            {
                return Err(format!("`vars` lacks the optional variable `{}`", var.name));
            }
        }
        Ok(())
    }

    /// Whether `record` is what history state `h` of `machine` could have recorded: one child
    /// of a compound parent or every child of a parallel one for a shallow history state, the
    /// leaves of a configuration below the parent for a deep one; in document order.
    fn recordable(&self, machine: &str, h: &str, record: &[String]) -> Result<(), String> {
        let (m, idx) = self.machine(machine).ok_or("no such machine")?;
        let hi = idx
            .by_id
            .get(h)
            .copied()
            .filter(|&i| idx.state(m, i).kind.is_history())
            .ok_or_else(|| format!("`history` has `{h}`, which is not a history state"))?;
        let p = idx.parents[hi].ok_or("a history state has no parent")?;
        let mut states = Vec::new();
        for id in record {
            let i = *idx
                .by_id
                .get(id)
                .ok_or_else(|| format!("`history.{h}` names `{id}`, which `{machine}` has not"))?;
            if states.last().is_some_and(|&l| l >= i) {
                return Err(format!("`history.{h}` is not in document order"));
            }
            states.push(i);
        }
        let bad = || format!("`history.{h}` is not what `{h}` could have recorded");
        let parent = idx.state(m, p);
        if idx.state(m, hi).kind == StateKind::History {
            let fits = match parent.kind {
                StateKind::Compound => states.len() == 1 && idx.children[p].contains(&states[0]),
                _ => states == idx.children[p],
            };
            return if fits { Ok(()) } else { Err(bad()) };
        }
        // Deep: atomic or final states below the parent, which with their ancestors up to it
        // have one child for each compound state and every child for each parallel one.
        let mut config = vec![p];
        for &l in &states {
            let kind = idx.state(m, l).kind;
            if !matches!(kind, StateKind::Atomic | StateKind::Final) || !idx.is_ancestor(p, l) {
                return Err(bad());
            }
            let mut cur = Some(l);
            while let Some(c) = cur.filter(|&c| c != p) {
                config.push(c);
                cur = idx.parents[c];
            }
        }
        config.sort_unstable();
        config.dedup();
        for &c in &config {
            let on = idx.children[c]
                .iter()
                .filter(|x| config.binary_search(x).is_ok())
                .count();
            let fits = match idx.state(m, c).kind {
                StateKind::Compound => on == 1,
                StateKind::Parallel => on == idx.children[c].len(),
                _ => true,
            };
            if !fits {
                return Err(bad());
            }
        }
        Ok(())
    }

    /// Whether `v` is a value of type `ty` in its JSON form, in machine `machine` (whose own
    /// events a bare event name means).
    fn value_fits(&self, machine: &str, ty: &TypeRef, v: &Value) -> bool {
        match self.resolve(ty) {
            TypeRef::Builtin(b) => match b {
                Builtin::String => v.is_string(),
                Builtin::Int => integer(v).is_some(),
                Builtin::Float => v.is_number(),
                Builtin::Bool => v.is_boolean(),
                Builtin::Duration => integer(v).is_some_and(|i| i >= 0),
                Builtin::Json => true,
            },
            TypeRef::Ref(_) => v.is_string(),
            TypeRef::Optional(t) => v.is_null() || self.value_fits(machine, t, v),
            TypeRef::List(t) => v
                .as_array()
                .is_some_and(|a| a.iter().all(|x| self.value_fits(machine, t, x))),
            TypeRef::Named(n) => match self.ir().types.get(n).map(|d| &d.def) {
                Some(TypeDef::Record { fields }) => self.fields_fit(machine, None, fields, v),
                Some(TypeDef::Error { fields }) => self.fields_fit(machine, Some(n), fields, v),
                Some(TypeDef::Enum { variants }) => {
                    v.as_str().is_some_and(|s| variants.iter().any(|x| x == s))
                }
                Some(TypeDef::Extern) => true,
                Some(TypeDef::Alias { .. }) => false,
                None => {
                    let (owner, ev) = n.split_once('.').unwrap_or((machine, n));
                    let decl = self
                        .ir()
                        .machines
                        .get(owner)
                        .and_then(|m| m.events.get(ev).or_else(|| m.emits.get(ev)));
                    decl.is_some_and(|d| self.fields_fit(machine, Some(ev), &d.fields, v))
                }
            },
        }
    }

    /// Whether `v` is an object with exactly `fields` (and `"_tag": tag`, if given).
    fn fields_fit(&self, machine: &str, tag: Option<&str>, fields: &[Field], v: &Value) -> bool {
        let Some(o) = v.as_object() else { return false };
        let extra = usize::from(tag.is_some());
        if o.len() != fields.len() + extra {
            return false;
        }
        if let Some(tag) = tag {
            if o.get("_tag").and_then(Value::as_str) != Some(tag) {
                return false;
            }
        }
        fields.iter().all(|f| {
            o.get(&f.name)
                .is_some_and(|x| self.value_fits(machine, &f.ty, x))
        })
    }
}

/// Rewrites every integral number in `v` that a float holds (`2.0`, `-0.0`) as the integer
/// (`2`, `0`), at any depth: a JSON number's form does not matter, and a saved snapshot always
/// has its integers in integer form. Numbers outside `i64`, and fractions, stay as they are.
pub(crate) fn integral_numbers(v: &mut Value) {
    match v {
        Value::Number(n) if n.is_f64() => {
            if let Some(i) = n.as_f64().and_then(|f| integer(&Value::from(f))) {
                *v = Value::from(i);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(integral_numbers),
        Value::Object(o) => o.values_mut().for_each(integral_numbers),
        _ => {}
    }
}

/// [`integral_numbers`] in the values of a saved snapshot: its input, variables and result.
pub(crate) fn integral_numbers_in(saved: &mut Saved) {
    saved
        .input
        .values_mut()
        .chain(saved.vars.values_mut())
        .chain(
            saved
                .spawned
                .iter_mut()
                .flatten()
                .flat_map(|c| std::iter::once(&mut c.key).chain(c.args.values_mut())),
        )
        .for_each(integral_numbers);
    if let Some(RunResult::Output(v) | RunResult::Error(v)) = &mut saved.result {
        integral_numbers(v);
    }
}

/// An integer from a JSON number in any form (`2`, `2.0`), or `None`.
fn integer(v: &Value) -> Option<i64> {
    if let Some(i) = v.as_i64() {
        return Some(i);
    }
    let f = v.as_f64()?;
    (f.fract() == 0.0 && (i64::MIN as f64..-(i64::MIN as f64)).contains(&f)).then_some(f as i64)
}

// ---- the schema ----------------------------------------------------------------------------

const MEMBERS: [&str; 14] = [
    "snapshotVersion",
    "machine",
    "run",
    "ir",
    "status",
    "active",
    "input",
    "vars",
    "invokes",
    "timers",
    "nextInvoke",
    "nextTimer",
    "nextMessage",
    "result",
];

/// The largest integer `spec/snapshot.schema.json` allows, 2^53 − 1.
const MAX_SAFE: u64 = 9_007_199_254_740_991;

/// Checks `v` against `spec/snapshot.schema.json`, including `snapshotVersion` being 1 (with
/// no `history` and no `spawned`), 2 (with `history`) or 3 (with both).
pub(crate) fn schema(v: &Value) -> Result<(), String> {
    let o = v.as_object().ok_or("a saved snapshot is an object")?;
    let version = o.get("snapshotVersion").and_then(Value::as_u64);
    let mut members = MEMBERS.to_vec();
    if matches!(version, Some(2 | 3)) {
        members.push("history");
    }
    if version == Some(3) {
        members.push("spawned");
    }
    let what = match version {
        Some(v @ (1..=3)) => format!("a version {v} saved snapshot"),
        _ => "a saved snapshot".to_owned(),
    };
    exactly(o, &members, &what)?;
    if !matches!(version, Some(1..=3)) {
        return Err(format!(
            "its snapshotVersion is {}, not 1, 2 or 3",
            o["snapshotVersion"]
        ));
    }
    if let Some(h) = o.get("history") {
        let h = h.as_object().ok_or("`history` is not an object")?;
        for (k, record) in h {
            if !state_id(k) {
                return Err(format!(
                    "`history` has the member `{k}`, which is not a state id"
                ));
            }
            let record = record
                .as_array()
                .filter(|r| !r.is_empty())
                .ok_or_else(|| format!("`history.{k}` is not a list of states"))?;
            let mut seen = HashSet::new();
            for s in record {
                string(s, "a recorded state", state_id)?;
                if !seen.insert(s.as_str()) {
                    return Err(format!("`history.{k}` names {s} twice"));
                }
            }
        }
    }
    string(&o["machine"], "machine", upper_name)?;
    string(&o["run"], "run", is_run_id)?;
    string(&o["ir"], "ir", hash)?;
    let status = o["status"].as_str().unwrap_or_default();
    if !["running", "done", "failed", "cancelled", "defect"].contains(&status) {
        return Err(format!("`status` is {}", o["status"]));
    }
    let active = o["active"].as_array().ok_or("`active` is not an array")?;
    let mut seen = HashSet::new();
    for a in active {
        string(a, "an active state", state_id)?;
        if !seen.insert(a.as_str()) {
            return Err(format!("`active` names {a} twice"));
        }
    }
    args(&o["input"], "input")?;
    args(&o["vars"], "vars")?;
    for inv in o["invokes"].as_array().ok_or("`invokes` is not an array")? {
        let inv = inv.as_object().ok_or("an invoke is not an object")?;
        exactly(inv, &["id", "state", "src"], "an invoke")?;
        natural(&inv["id"], "an invoke's id", 1)?;
        string(&inv["state"], "an invoke's state", state_id)?;
        string(&inv["src"], "an invoke's src", any_name)?;
    }
    let spawned = match o.get("spawned") {
        Some(s) => s.as_array().ok_or("`spawned` is not an array")?.as_slice(),
        None => &[],
    };
    for ch in spawned {
        let ch = ch.as_object().ok_or("a spawned child is not an object")?;
        exactly(ch, &["id", "children", "key", "args"], "a spawned child")?;
        natural(&ch["id"], "a spawned child's id", 1)?;
        string(&ch["children"], "a spawned child's collection", lower_name)?;
        if !(ch["key"].is_string() || ch["key"].is_i64() || ch["key"].is_u64()) {
            return Err(format!("a spawned child's key is {}", ch["key"]));
        }
        args(&ch["args"], "a spawned child's args")?;
    }
    for tm in o["timers"].as_array().ok_or("`timers` is not an array")? {
        let tm = tm.as_object().ok_or("a timer is not an object")?;
        exactly(tm, &["id", "state", "index", "delay", "due"], "a timer")?;
        natural(&tm["id"], "a timer's id", 1)?;
        string(&tm["state"], "a timer's state", state_id)?;
        natural(&tm["index"], "a timer's index", 0)?;
        natural(&tm["delay"], "a timer's delay", 0)?;
        natural(&tm["due"], "a timer's due", 0)?;
    }
    for k in ["nextInvoke", "nextTimer", "nextMessage"] {
        natural(&o[k], k, 1)?;
    }
    match &o["result"] {
        Value::Null => {}
        Value::Object(r) if r.len() == 1 && r.contains_key("output") => {}
        Value::Object(r) if r.len() == 1 && r.contains_key("error") => {
            let tag = r["error"].as_object().and_then(|e| e.get("_tag"));
            if !tag.is_some_and(|t| t.as_str().is_some_and(upper_name)) {
                return Err("`result.error` is not a tagged record".into());
            }
        }
        Value::Object(r) if r.len() == 1 && r.contains_key("defect") => {
            if !r["defect"]
                .as_object()
                .is_some_and(|d| d.contains_key("kind"))
            {
                return Err("`result.defect` is not a defect".into());
            }
        }
        r => return Err(format!("`result` is {r}")),
    }
    Ok(())
}

fn exactly(o: &Map<String, Value>, members: &[&str], what: &str) -> Result<(), String> {
    if let Some(k) = o.keys().find(|k| !members.contains(&k.as_str())) {
        return Err(format!(
            "{what} has the member `{k}`, which the schema does not allow"
        ));
    }
    if let Some(k) = members.iter().find(|k| !o.contains_key(**k)) {
        return Err(format!("{what} lacks the member `{k}`"));
    }
    Ok(())
}

fn string(v: &Value, what: &str, ok: fn(&str) -> bool) -> Result<(), String> {
    match v.as_str() {
        Some(s) if ok(s) => Ok(()),
        _ => Err(format!("{what} is {v}, which the schema does not allow")),
    }
}

fn natural(v: &Value, what: &str, min: u64) -> Result<(), String> {
    match v.as_u64() {
        Some(n) if n >= min && n <= MAX_SAFE => Ok(()),
        _ => Err(format!("{what} is {v}, which the schema does not allow")),
    }
}

fn args(v: &Value, what: &str) -> Result<(), String> {
    let o = v
        .as_object()
        .ok_or_else(|| format!("`{what}` is not an object"))?;
    match o.keys().find(|k| !lower_name(k)) {
        Some(k) => Err(format!(
            "`{what}` has the member `{k}`, which is not a name"
        )),
        None => Ok(()),
    }
}

/// `[A-Za-z0-9]*`
fn alnum(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// `^[A-Z][A-Za-z0-9]*$`
fn upper_name(s: &str) -> bool {
    s.as_bytes().first().is_some_and(u8::is_ascii_uppercase) && alnum(s)
}

/// `^[a-z][A-Za-z0-9]*$`
fn lower_name(s: &str) -> bool {
    s.as_bytes().first().is_some_and(u8::is_ascii_lowercase) && alnum(s)
}

/// `^[A-Za-z][A-Za-z0-9]*$`
fn any_name(s: &str) -> bool {
    s.as_bytes().first().is_some_and(u8::is_ascii_alphabetic) && alnum(s)
}

/// `^[A-Z][A-Za-z0-9]*(\.[A-Z][A-Za-z0-9]*)*$`
fn state_id(s: &str) -> bool {
    s.split('.').all(upper_name)
}

/// The snapshot schema's pattern for a run id: the id of a root run, then the invoke ids of
/// child runs. [`is_run_id`] checks it.
pub const RUN_ID_PATTERN: &str = r"^[A-Za-z0-9_-]+(\.[1-9][0-9]*)*$";

/// Whether `s` is a run id ([`RUN_ID_PATTERN`]). A root run named anything else would save a
/// snapshot that `restore` refuses as `invalid`.
pub fn is_run_id(s: &str) -> bool {
    let mut parts = s.split('.');
    let first = parts.next().unwrap_or_default();
    !first.is_empty()
        && first
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        && parts.all(|p| {
            p.as_bytes()
                .first()
                .is_some_and(|b| (b'1'..=b'9').contains(b))
                && p.bytes().all(|b| b.is_ascii_digit())
        })
}

/// `^sha256:[0-9a-f]{64}$`
fn hash(s: &str) -> bool {
    s.strip_prefix("sha256:").is_some_and(|h| {
        h.len() == 64
            && h.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns() {
        assert!(is_run_id("r1") && is_run_id("r1.4.10") && is_run_id("order-7_x"));
        assert!(!is_run_id("") && !is_run_id("r1.0") && !is_run_id("r1.") && !is_run_id("a b"));
        assert!(state_id("A.B") && !state_id("A.b") && !state_id("A..B") && !state_id(""));
        assert!(hash(&format!("sha256:{}", "0a".repeat(32))));
        assert!(!hash(&format!("sha256:{}", "0A".repeat(32))));
        assert!(lower_name("n2") && !lower_name("N") && !lower_name("a_b"));
        assert!(!is_run_id("order:42"));
    }

    #[test]
    fn the_run_id_pattern_is_the_schemas() {
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../../../spec/snapshot.schema.json")).unwrap();
        assert_eq!(schema["$defs"]["runId"]["pattern"], RUN_ID_PATTERN);
    }
}
