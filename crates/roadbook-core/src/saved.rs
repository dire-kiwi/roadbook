//! Checking a saved snapshot before it is restored (`spec/execution.md#restoring-a-snapshot`):
//! the schema, the machine, the hash and a migration, the status, `ref` parameters, pending
//! calls, and whether it fits the machine. The checks are made in the order the spec lists
//! them, each for the whole tree (the snapshot and every child run's snapshot nested in it)
//! before the next, and the first that fails gives the reason.

use std::collections::HashSet;
use std::fmt;

use roadbook_ir::{Builtin, ChildrenKind, Field, Machine, StateKind, Trigger, TypeDef, TypeRef};
use serde_json::{Map, Value};

use crate::program::{MachineIndex, Program};
use crate::{RunResult, Saved};

/// Why a saved snapshot was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Refusal {
    /// It does not match `snapshot.schema.json`, its version is not 1, 2, 3 or 4, or it (or a
    /// snapshot nested in it) does not fit its machine.
    Invalid,
    /// The IR has no machine of its name, or it is not the machine asked for.
    Machine,
    /// Its hash is not the machine's, or a nested snapshot's is not its entry's machine's, and
    /// no migration was given (or the migration's result has such a hash too).
    Ir,
    /// The migration failed.
    Migration,
    /// Its status is not `running`.
    Ended,
    /// The machine, or the machine of a nested snapshot, has a `ref` parameter: restoring a
    /// run that holds `ref`s is not specified yet.
    Ref,
    /// It has pending calls, whose callers belong to the process that saved it.
    Calls,
}

impl Refusal {
    /// The reason as the spec names it: `invalid`, `machine`, `ir`, `migration`, `ended`,
    /// `ref` or `calls`.
    pub fn as_str(self) -> &'static str {
        match self {
            Refusal::Invalid => "invalid",
            Refusal::Machine => "machine",
            Refusal::Ir => "ir",
            Refusal::Migration => "migration",
            Refusal::Ended => "ended",
            Refusal::Ref => "ref",
            Refusal::Calls => "calls",
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

    /// The reason: `invalid`, `machine`, `ir`, `migration`, `ended`, `ref` or `calls`.
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
    /// snapshot names. Each reason is checked for the whole tree, the snapshot and every child
    /// run's snapshot nested in it, before the next. `migrate` is called at most once, for the
    /// whole tree, and only when a hash in it differs; its result is checked from the top,
    /// without a second migration, as a snapshot of the same machine: a migration cannot turn
    /// it into a snapshot of another (`machine`).
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
        if let Some(stale) = self.stale(&saved, name, &m.hash) {
            let Some(migrate) = migrate else {
                return Err(RestoreError::new(Refusal::Ir, stale));
            };
            let old = saved["ir"].as_str().unwrap_or_default().to_owned();
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
        if let Some(e) = self.holds_refs(&saved, name) {
            return Err(e);
        }
        if let Some(calls) = saved.get("calls").and_then(Value::as_array)
            && !calls.is_empty()
        {
            return Err(RestoreError::new(
                Refusal::Calls,
                format!(
                    "it has {} pending call(s), whose callers belong to the process that saved it",
                    calls.len()
                ),
            ));
        }
        let saved: Saved =
            serde_json::from_value(saved).map_err(|e| RestoreError::invalid(e.to_string()))?;
        self.fits_tree(&saved).map_err(RestoreError::invalid)?;
        Ok((saved, None))
    }

    /// The child run snapshots nested in `saved`, a saved snapshot as JSON, each with the
    /// machine its entry starts: an `invokes` entry's `src` when that is a machine, a
    /// `spawned` entry's collection's machine when the collection is one of `machine`'s and
    /// holds machines.
    /// Entries whose work is not such a machine are left out: they are refused as `invalid`.
    fn nested<'v>(&'v self, saved: &'v Value, machine: &str) -> Vec<(&'v Value, &'v str)> {
        let mut out = Vec::new();
        let entries = |k: &str| saved.get(k).and_then(Value::as_array).into_iter().flatten();
        for inv in entries("invokes") {
            let src = inv["src"].as_str().unwrap_or_default();
            if let (Some(child), Some((name, _))) =
                (inv.get("child"), self.ir().machines.get_key_value(src))
            {
                out.push((child, name.as_str()));
            }
        }
        let collections = self.ir().machines.get(machine).map(|m| &m.children);
        for ch in entries("spawned") {
            let decl = collections.and_then(|c| c.get(ch["children"].as_str().unwrap_or_default()));
            if let (Some(child), Some(decl)) = (ch.get("child"), decl)
                && decl.kind == ChildrenKind::Machine
            {
                out.push((child, decl.src.as_str()));
            }
        }
        out
    }

    /// What is stale in the tree of `saved`, a snapshot of `machine` whose hash should be
    /// `hash`: its own hash, or that of a snapshot nested in it, which should be the hash of
    /// the machine its entry starts. `None` if every hash is current.
    fn stale(&self, saved: &Value, machine: &str, hash: &str) -> Option<String> {
        let had = saved["ir"].as_str().unwrap_or_default();
        if had != hash {
            let run = saved["run"].as_str().unwrap_or_default();
            return Some(format!(
                "{run} was saved with `{machine}` at {had}, and the IR has it at {hash}"
            ));
        }
        self.nested(saved, machine)
            .into_iter()
            .find_map(|(child, of)| {
                let hash = &self.ir().machines[of].hash;
                self.stale(child, of, hash)
            })
    }

    /// The `ref` refusal, if `machine`, the machine of `saved` (a snapshot as JSON), or the
    /// machine an entry with a snapshot nested in it starts, at any depth, has a `ref`
    /// parameter. A nested snapshot of an entry that starts no machine is left to the `invalid`
    /// check.
    fn holds_refs(&self, saved: &Value, machine: &str) -> Option<RestoreError> {
        let m = self.ir().machines.get(machine)?;
        if let Some(p) = m.params.iter().find(|p| self.holds_ref(&p.ty)) {
            return Some(RestoreError::new(
                Refusal::Ref,
                format!(
                    "`{machine}` has the `ref` parameter `{}`; restoring a run that holds \
                     `ref`s is not supported yet",
                    p.name
                ),
            ));
        }
        self.nested(saved, machine)
            .into_iter()
            .find_map(|(child, of)| self.holds_refs(child, of))
    }

    /// Whether `s` fits its machine, and each snapshot nested in it is one of the machine its
    /// entry starts, of the child run `<run>.<id>`, running, with no calls, and fits too.
    fn fits_tree(&self, s: &Saved) -> Result<(), String> {
        self.fits(s)?;
        let m = &self.ir().machines[&s.machine];
        let nested =
            s.invokes
                .iter()
                .filter_map(|i| Some((i.id, &i.src, i.child.as_deref()?)))
                .chain(s.spawned.iter().flatten().filter_map(|c| {
                    Some((c.id, &m.children[&c.children].src, c.child.as_deref()?))
                }));
        for (id, of, child) in nested {
            if !self.ir().machines.contains_key(of) {
                return Err(format!(
                    "invoke {id} runs `{of}`, which is not a machine, but has a child run's \
                     snapshot"
                ));
            }
            if child.machine != *of {
                return Err(format!(
                    "the snapshot nested in entry {id} is of `{}`, not of `{of}`",
                    child.machine
                ));
            }
            let run = format!("{}.{id}", s.run);
            if child.run != run {
                return Err(format!(
                    "the snapshot nested in entry {id} is of the run {}, not {run}",
                    child.run
                ));
            }
            if child.status != crate::Status::Running {
                return Err(format!(
                    "the snapshot nested in entry {id} is not running: its status is {}",
                    serde_json::json!(child.status)
                ));
            }
            if !child.calls.is_empty() {
                return Err(format!(
                    "the snapshot nested in entry {id} has calls, which a child run never takes"
                ));
            }
            self.fits_tree(child)
                .map_err(|e| format!("in the snapshot nested in entry {id}: {e}"))?;
        }
        Ok(())
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
        match &*self.resolve(ty) {
            TypeRef::Ref(_) => true,
            TypeRef::Optional(t) | TypeRef::List(t) | TypeRef::Map(_, t) => self.holds_ref(t),
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
        let mut invokes = HashSet::new();
        for inv in &s.invokes {
            let i = idx.by_id.get(&inv.state).copied().filter(|&i| is_active(i));
            let Some(i) = i else {
                return Err(format!(
                    "invoke {} belongs to a state that is not active",
                    inv.id
                ));
            };
            let state = idx.state(m, i);
            let invoke = match (&inv.alias, state.has_several_invokes()) {
                (Some(a), true) => state.invoke_named(Some(a)),
                (None, false) => state.invokes.first(),
                (Some(a), false) => {
                    return Err(format!(
                        "invoke {} has `as` `{a}`, but the state `{}` has at most one invoke",
                        inv.id, inv.state
                    ));
                }
                (None, true) => {
                    return Err(format!(
                        "invoke {} has no `as`, but the state `{}` has several invokes",
                        inv.id, inv.state
                    ));
                }
            };
            if invoke.map(|v| v.callee()) != Some(inv.src.clone()) {
                let named = inv
                    .alias
                    .as_ref()
                    .map_or(String::new(), |a| format!(" as `{a}`"));
                return Err(format!(
                    "the state `{}` has no invoke of `{}`{named}",
                    inv.state, inv.src
                ));
            }
            if !invokes.insert((&inv.state, &inv.alias)) {
                return Err(format!("the state `{}` runs one invoke twice", inv.state));
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
            let params = match decl.kind {
                ChildrenKind::Machine => self.ir().machines.get(&decl.src).map(|m| &m.params),
                ChildrenKind::Task => self.ir().tasks.get(&decl.src).map(|t| &t.params),
            }
            .ok_or_else(|| format!("no machine or task `{}`", decl.src))?;
            if ch.args.len() != params.len() {
                return Err(format!(
                    "the arguments of spawned child {} are not exactly `{}`'s parameters",
                    ch.id, decl.src
                ));
            }
            // A task's parameters name no events of their own, so the parent stands in for it.
            let owner = match decl.kind {
                ChildrenKind::Machine => decl.src.as_str(),
                ChildrenKind::Task => s.machine.as_str(),
            };
            for p in params {
                let v = ch.args.get(&p.name).ok_or_else(|| {
                    format!("spawned child {} lacks the argument `{}`", ch.id, p.name)
                })?;
                if !self.value_fits(owner, &p.ty, v) {
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
        // Delayed raises: their ids are timer ids.
        let names = m.raise_names();
        let mut taken = HashSet::new();
        for sc in &s.scheduled {
            let tag = sc
                .value
                .get("_tag")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let fits = m
                .own_event(tag)
                .is_some_and(|d| self.fields_fit(&s.machine, Some(tag), &d.fields, &sc.value));
            if !fits {
                return Err(format!(
                    "the value of delayed raise {} is not one of `{}`'s events",
                    sc.id, s.machine
                ));
            }
            if let Some(n) = &sc.name {
                if !names.contains(&n.as_str()) {
                    return Err(format!(
                        "delayed raise {} is named `{n}`, which no `raise` of `{}` uses",
                        sc.id, s.machine
                    ));
                }
                if !taken.insert(n.as_str()) {
                    return Err(format!("two delayed raises are named `{n}`"));
                }
            }
            if !ids.insert(sc.id) || sc.id >= s.next_timer {
                return Err(format!(
                    "the timer id {} of a delayed raise is repeated or not below nextTimer",
                    sc.id
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
            if matches!(*self.resolve(&var.ty), TypeRef::Optional(_))
                && !s.vars.contains_key(&var.name)
            {
                return Err(format!("`vars` lacks the optional variable `{}`", var.name));
            }
        }
        // Locals: one member for each active state that declares any.
        let with_locals = |i: usize| {
            let st = idx.state(m, i);
            !st.params.is_empty() || !st.vars.is_empty()
        };
        let want: Vec<usize> = active.iter().copied().filter(|&i| with_locals(i)).collect();
        self.locals_fit(m, idx, &s.machine, &want, &s.locals, "locals")?;
        // History locals: for a history state with a record, those of the states with locals
        // the record re-enters. A record that re-enters such a state needs its member, or
        // entering the history state would leave their parameters unbound.
        for h in s.history_locals.keys() {
            if !s.history.as_ref().is_some_and(|r| r.contains_key(h)) {
                return Err(format!(
                    "`historyLocals` has `{h}`, which has no member in `history`"
                ));
            }
        }
        let empty = crate::Locals::new();
        for (h, record) in s.history.iter().flatten() {
            let hi = idx.by_id[h];
            let p = idx.parents[hi].ok_or("a history state has no parent")?;
            let mut kept: Vec<usize> = Vec::new();
            for id in record {
                let r = idx.by_id[id];
                if idx.state(m, hi).kind == StateKind::History {
                    kept.push(r);
                    continue;
                }
                let mut cur = Some(r);
                while let Some(c) = cur.filter(|&c| c != p) {
                    kept.push(c);
                    cur = idx.parents[c];
                }
            }
            kept.sort_unstable();
            kept.dedup();
            kept.retain(|&i| with_locals(i));
            let locals = match s.history_locals.get(h) {
                Some(l) => l,
                None if kept.is_empty() => &empty,
                None => {
                    return Err(format!(
                        "`historyLocals` lacks `{h}`, whose record re-enters `{}`, which has parameters or variables",
                        idx.state(m, kept[0]).id
                    ));
                }
            };
            self.locals_fit(
                m,
                idx,
                &s.machine,
                &kept,
                locals,
                &format!("historyLocals.{h}"),
            )?;
        }
        Ok(())
    }

    /// Whether `locals` has exactly one member for each of the states `want`, each holding
    /// only that state's locals, every parameter and every optional variable, each a value of
    /// its declared type.
    fn locals_fit(
        &self,
        m: &Machine,
        idx: &MachineIndex,
        machine: &str,
        want: &[usize],
        locals: &crate::Locals,
        what: &str,
    ) -> Result<(), String> {
        for id in locals.keys() {
            if !want.iter().any(|&i| idx.state(m, i).id == *id) {
                return Err(format!(
                    "`{what}` has `{id}`, which is not an active state with parameters or variables"
                ));
            }
        }
        for &i in want {
            let st = idx.state(m, i);
            let l = locals
                .get(&st.id)
                .ok_or_else(|| format!("`{what}` lacks `{}`", st.id))?;
            for (name, v) in l {
                let ty = st
                    .params
                    .iter()
                    .find(|p| p.name == *name)
                    .map(|p| &p.ty)
                    .or_else(|| st.vars.iter().find(|x| x.name == *name).map(|x| &x.ty))
                    .ok_or_else(|| {
                        format!(
                            "`{what}.{}` has `{name}`, which is not a local of it",
                            st.id
                        )
                    })?;
                if !self.value_fits(machine, ty, v) {
                    return Err(format!("the local `{}.{name}` is not a {ty}", st.id));
                }
            }
            for p in &st.params {
                if !l.contains_key(&p.name) {
                    return Err(format!(
                        "`{what}.{}` lacks the parameter `{}`",
                        st.id, p.name
                    ));
                }
            }
            for var in &st.vars {
                if matches!(*self.resolve(&var.ty), TypeRef::Optional(_))
                    && !l.contains_key(&var.name)
                {
                    return Err(format!(
                        "`{what}.{}` lacks the optional variable `{}`",
                        st.id, var.name
                    ));
                }
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
        match &*self.resolve(ty) {
            TypeRef::Builtin(b) => match b {
                Builtin::String => v.is_string(),
                Builtin::Int => integer(v).is_some(),
                Builtin::Float => v.is_number(),
                Builtin::Bool => v.is_boolean(),
                Builtin::Duration => integer(v).is_some_and(|i| i >= 0),
                Builtin::Json => true,
            },
            TypeRef::Ref(_) => v.is_string(),
            // A pending-call value: the caller's run id and the id of its invoke.
            TypeRef::Call(_) => v.as_object().is_some_and(|o| {
                o.len() == 2
                    && o.get("from").and_then(Value::as_str).is_some_and(is_run_id)
                    && o.get("invoke")
                        .and_then(Value::as_u64)
                        .is_some_and(|i| i >= 1)
            }),
            TypeRef::Optional(t) => v.is_null() || self.value_fits(machine, t, v),
            TypeRef::List(t) => v
                .as_array()
                .is_some_and(|a| a.iter().all(|x| self.value_fits(machine, t, x))),
            TypeRef::Map(_, t) => v
                .as_object()
                .is_some_and(|o| o.values().all(|x| self.value_fits(machine, t, x))),
            t @ TypeRef::Generic(..) => match self.ir().fields_of(t) {
                Some((_, false, fields)) => self.fields_fit(machine, None, &fields, v),
                _ => false,
            },
            TypeRef::Named(n) => match self.ir().types.get(n).map(|d| &d.def) {
                Some(TypeDef::Record { fields, .. }) => self.fields_fit(machine, None, fields, v),
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
                        .and_then(|m| m.own_event(ev).or_else(|| m.emits.get(ev)))
                        .or_else(|| self.ir().tasks.get(owner)?.emits.get(ev));
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
        .chain(saved.locals.values_mut().flat_map(|l| l.values_mut()))
        .chain(
            saved
                .history_locals
                .values_mut()
                .flat_map(|h| h.values_mut())
                .flat_map(|l| l.values_mut()),
        )
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

/// The members every saved snapshot has. `history` (version 2 to 4) and `spawned` (version 3
/// and 4) are added by version; `calls` and `scheduled` are optional in any version.
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
/// no `history` and no `spawned`), 2 (with `history`), 3 (with both) or 4 (with both, and
/// child runs' snapshots nested in its entries, each of version 4 and checked the same way).
pub(crate) fn schema(v: &Value) -> Result<(), String> {
    schema_at(v, false)
}

fn schema_at(v: &Value, nested: bool) -> Result<(), String> {
    let o = v.as_object().ok_or("a saved snapshot is an object")?;
    let version = o.get("snapshotVersion").and_then(Value::as_u64);
    let mut members = MEMBERS.to_vec();
    if matches!(version, Some(2..=4)) {
        members.push("history");
    }
    if matches!(version, Some(3 | 4)) {
        members.push("spawned");
    }
    let what = match version {
        Some(v @ (1..=4)) => format!("a version {v} saved snapshot"),
        _ => "a saved snapshot".to_owned(),
    };
    // `calls` and `scheduled` are optional in any version.
    let mut required = o.clone();
    let calls = required.remove("calls");
    let scheduled = required.remove("scheduled");
    let locals = required.remove("locals");
    let history_locals = required.remove("historyLocals");
    exactly(&required, &members, &what)?;
    if !matches!(version, Some(1..=4)) {
        return Err(format!(
            "its snapshotVersion is {}, not 1, 2, 3 or 4",
            o["snapshotVersion"]
        ));
    }
    if nested && version != Some(4) {
        return Err(format!(
            "a child run's snapshot nested in a saved snapshot is of version 4, not {}",
            o["snapshotVersion"]
        ));
    }
    // A child run's snapshot, nested in an entry: only in version 4.
    let child = |e: &mut Map<String, Value>, what: &str| -> Result<(), String> {
        match e.remove("child") {
            Some(_) if version != Some(4) => Err(format!(
                "{what} has a child run's snapshot, which only version 4 has"
            )),
            Some(c) => schema_at(&c, true).map_err(|err| format!("in {what}'s child: {err}")),
            None => Ok(()),
        }
    };
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
    if let Some(l) = &locals {
        states_locals(l, "locals")?;
    }
    if let Some(h) = &history_locals {
        let h = h.as_object().ok_or("`historyLocals` is not an object")?;
        for (k, l) in h {
            if !state_id(k) {
                return Err(format!(
                    "`historyLocals` has the member `{k}`, which is not a state id"
                ));
            }
            let what = format!("historyLocals.{k}");
            if l.as_object().is_some_and(Map::is_empty) {
                return Err(format!("`{what}` is empty"));
            }
            states_locals(l, &what)?;
        }
    }
    for inv in o["invokes"].as_array().ok_or("`invokes` is not an array")? {
        let mut inv = inv.as_object().ok_or("an invoke is not an object")?.clone();
        child(&mut inv, "an invoke")?;
        if let Some(a) = inv.remove("as") {
            string(&a, "an invoke's as", lower_name)?;
        }
        exactly(&inv, &["id", "state", "src"], "an invoke")?;
        natural(&inv["id"], "an invoke's id", 1)?;
        string(&inv["state"], "an invoke's state", state_id)?;
        string(&inv["src"], "an invoke's src", |s| {
            any_name(s)
                || s.split_once('.')
                    .is_some_and(|(a, c)| lower_name(a) && lower_name(c))
        })?;
    }
    if let Some(calls) = calls {
        let calls = calls.as_array().ok_or("`calls` is not an array")?;
        if calls.is_empty() {
            return Err("`calls` is empty, which the schema does not allow".into());
        }
        for c in calls {
            let c = c.as_object().ok_or("a call is not an object")?;
            exactly(c, &["from", "invoke", "call"], "a call")?;
            string(&c["from"], "a call's from", is_run_id)?;
            natural(&c["invoke"], "a call's invoke", 1)?;
            string(&c["call"], "a call's name", lower_name)?;
        }
    }
    let spawned = match o.get("spawned") {
        Some(s) => s.as_array().ok_or("`spawned` is not an array")?.as_slice(),
        None => &[],
    };
    for ch in spawned {
        let mut ch = ch
            .as_object()
            .ok_or("a spawned child is not an object")?
            .clone();
        child(&mut ch, "a spawned child")?;
        exactly(&ch, &["id", "children", "key", "args"], "a spawned child")?;
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
    if let Some(scheduled) = scheduled {
        let scheduled = scheduled.as_array().ok_or("`scheduled` is not an array")?;
        if scheduled.is_empty() {
            return Err("`scheduled` is empty, which the schema does not allow".into());
        }
        for sc in scheduled {
            let sc = sc.as_object().ok_or("a delayed raise is not an object")?;
            exactly(
                sc,
                &["id", "name", "delay", "value", "due"],
                "a delayed raise",
            )?;
            natural(&sc["id"], "a delayed raise's id", 1)?;
            if !sc["name"].is_null() {
                string(&sc["name"], "a delayed raise's name", lower_name)?;
            }
            natural(&sc["delay"], "a delayed raise's delay", 0)?;
            natural(&sc["due"], "a delayed raise's due", 0)?;
            let tag = sc["value"].as_object().and_then(|e| e.get("_tag"));
            if !tag.is_some_and(|t| t.as_str().is_some_and(upper_name)) {
                return Err("a delayed raise's value is not a tagged record".into());
            }
        }
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

/// `locals`, or a member of `historyLocals`: objects of locals keyed by state id.
fn states_locals(v: &Value, what: &str) -> Result<(), String> {
    let o = v
        .as_object()
        .ok_or_else(|| format!("`{what}` is not an object"))?;
    for (k, l) in o {
        if !state_id(k) {
            return Err(format!(
                "`{what}` has the member `{k}`, which is not a state id"
            ));
        }
        args(l, &format!("{what}.{k}"))?;
    }
    Ok(())
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
