//! An IR document made ready to run: every machine's states indexed in document order.

use std::collections::{HashMap, HashSet};
use std::fmt;

use std::borrow::Cow;

use roadbook_ir::{Ir, Machine, State, Trigger, TypeRef};

use serde_json::Value;

use crate::redact::{Marks, Redactor};

/// An IR document, checked and indexed for the interpreter.
///
/// Build one with [`Program::new`] (or [`Program::from_json`]) and keep it for as long as runs
/// of its machines go on: [`Program::start`] and [`Program::step`] take it by reference.
#[derive(Debug, Clone)]
pub struct Program {
    ir: Ir,
    machines: HashMap<String, MachineIndex>,
    marks: Marks,
}

/// The states of one machine, in document order: a state's index is its position in a
/// pre-order walk of the tree, so "innermost first" is descending index for a chain.
#[derive(Debug, Clone)]
pub(crate) struct MachineIndex {
    /// For each state, the child positions leading to it from the root.
    paths: Vec<Vec<usize>>,
    /// For each state, its parent's index (`None` for the root).
    pub parents: Vec<Option<usize>>,
    /// For each state, its children's indices, in document order. A history state is not a
    /// child: it is never active.
    pub children: Vec<Vec<usize>>,
    pub by_id: HashMap<String, usize>,
    /// For each state, its transitions' targets' indices, in order (`None`: no target).
    pub targets: Vec<Vec<Option<usize>>>,
    /// For each state, its initial child's index, if it names one.
    pub initial: Vec<Option<usize>>,
    /// The emitted events the machine has an `on x.E` for, anywhere in it, by the sender's
    /// machine, and `ended` for a machine it has an `on x.ended` for. A run receives no
    /// others.
    hears: HashMap<String, HashSet<String>>,
}

/// Why an IR cannot be run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgramError {
    /// The IR could not be read (its version, or its shape).
    Read(String),
}

impl fmt::Display for ProgramError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProgramError::Read(e) => f.write_str(e),
        }
    }
}

impl std::error::Error for ProgramError {}

/// The name a peer's end has in [`Program::heard`].
const ENDED_INPUT: &str = "ended";

impl Program {
    /// The name in [`Program::heard`] that says a run hears the end of a run of the sender's
    /// machine: `"ended"`, which is never an event's name.
    pub const ENDED: &'static str = ENDED_INPUT;

    /// Indexes `ir`. An IR whose transition targets or initial states name states their
    /// machine does not have is refused.
    pub fn new(ir: Ir) -> Result<Program, ProgramError> {
        if let Some(alias) = ir.alias_cycle() {
            return Err(ProgramError::Read(format!(
                "type alias `{alias}` refers to itself, so it cannot be expanded"
            )));
        }
        let mut machines = HashMap::new();
        for (name, m) in &ir.machines {
            let mut idx = MachineIndex {
                paths: Vec::new(),
                parents: Vec::new(),
                children: Vec::new(),
                by_id: HashMap::new(),
                targets: Vec::new(),
                initial: Vec::new(),
                hears: HashMap::new(),
            };
            walk(&m.root, None, &mut Vec::new(), &mut idx);
            // A target or initial state the machine does not have: refused here, since the
            // interpreter would have nothing to enter.
            let index = |id: &String, what: &str, of: &str| {
                idx.by_id.get(id).copied().ok_or_else(|| {
                    ProgramError::Read(format!(
                        "{what} `{id}` of state `{of}` is not a state of machine `{name}`"
                    ))
                })
            };
            for i in 0..idx.paths.len() {
                let s = idx.state(m, i);
                let targets = s
                    .transitions
                    .iter()
                    .map(|t| {
                        t.target
                            .as_ref()
                            .map(|id| index(id, "the target", &s.id))
                            .transpose()
                    })
                    .collect::<Result<_, _>>()?;
                let initial = s
                    .initial
                    .as_ref()
                    .map(|id| index(id, "the initial state", &s.id))
                    .transpose()?;
                idx.targets.push(targets);
                idx.initial.push(initial);
            }
            machines.insert(name.clone(), idx);
        }
        let marks = Marks::new(&ir);
        Ok(Program {
            ir,
            machines,
            marks,
        })
    }

    /// Reads an IR document ([`Ir::from_json`]) and indexes it.
    pub fn from_json(json: &str) -> Result<Program, ProgramError> {
        let ir = Ir::from_json(json).map_err(|e| ProgramError::Read(e.to_string()))?;
        Program::new(ir)
    }

    /// The IR.
    pub fn ir(&self) -> &Ir {
        &self.ir
    }

    /// `value`, an event a run of `machine` emits, redacted as its `emitted` record shows it:
    /// what a host listener hears, which is not redacted, as the trace shows it. A machine the
    /// IR does not have leaves it as it is.
    pub fn redact_emitted(&self, machine: &str, value: &mut Value) {
        let r = self.redactor();
        let Some(m) = self.ir.machines.get(machine) else {
            return;
        };
        if !r.active() {
            return;
        }
        let mut record = crate::Record::Emitted {
            message: 0,
            value: std::mem::take(value),
        };
        r.record(m, &mut record);
        if let crate::Record::Emitted { value: v, .. } = record {
            *value = v;
        }
    }

    pub(crate) fn redactor(&self) -> Redactor<'_> {
        Redactor {
            ir: &self.ir,
            marks: &self.marks,
        }
    }

    /// `t` with aliases expanded at the top, transitively ([`Ir::expand_top`]).
    pub(crate) fn resolve<'t>(&'t self, t: &'t TypeRef) -> Cow<'t, TypeRef> {
        self.ir.expand_top(t)
    }

    /// `t` with aliases and `?` removed: the type whose operators an operand of type `t`
    /// uses.
    pub(crate) fn scalar<'t>(&'t self, t: &'t TypeRef) -> Cow<'t, TypeRef> {
        let mut t = self.resolve(t);
        for _ in 0..64 {
            t = match t {
                Cow::Borrowed(TypeRef::Optional(inner)) => self.resolve(inner),
                Cow::Owned(TypeRef::Optional(inner)) => {
                    Cow::Owned(self.resolve(&inner).into_owned())
                }
                other => return other,
            };
        }
        t
    }

    pub(crate) fn machine(&self, name: &str) -> Option<(&Machine, &MachineIndex)> {
        Some((self.ir.machines.get(name)?, self.machines.get(name)?))
    }
}

fn walk(s: &State, parent: Option<usize>, path: &mut Vec<usize>, idx: &mut MachineIndex) -> usize {
    let me = idx.paths.len();
    idx.paths.push(path.clone());
    idx.parents.push(parent);
    idx.children.push(Vec::new());
    idx.by_id.insert(s.id.clone(), me);
    for t in &s.transitions {
        if let Trigger::Emitted { from, event } = &t.trigger {
            idx.hears
                .entry(from.machine.clone())
                .or_default()
                .insert(event.clone());
        }
        if let Trigger::Ended { from } = &t.trigger {
            idx.hears
                .entry(from.machine.clone())
                .or_default()
                .insert(ENDED_INPUT.to_owned());
        }
    }
    for (i, c) in s.states.iter().enumerate() {
        path.push(i);
        let ci = walk(c, Some(me), path, idx);
        path.pop();
        if !c.kind.is_history() {
            idx.children[me].push(ci);
        }
    }
    me
}

impl MachineIndex {
    pub fn state<'m>(&self, m: &'m Machine, i: usize) -> &'m State {
        let mut s = &m.root;
        for &c in &self.paths[i] {
            s = &s.states[c];
        }
        s
    }

    /// Whether `a` is a proper ancestor of `b`.
    pub fn is_ancestor(&self, a: usize, b: usize) -> bool {
        let mut p = self.parents[b];
        while let Some(x) = p {
            if x == a {
                return true;
            }
            p = self.parents[x];
        }
        false
    }
}

impl Program {
    /// The state a run of `machine` whose active states are `active` (in document order) is
    /// in, as host code sees it: the outermost active parallel state if there is one, since
    /// a run inside one is in all its regions at once; otherwise the innermost active state.
    pub fn current_state<'s>(&self, machine: &str, active: &'s [String]) -> Option<&'s str> {
        let (m, idx) = self.machine(machine)?;
        let parallel = active.iter().find(|id| {
            idx.by_id
                .get(*id)
                .is_some_and(|&i| idx.state(m, i).kind == roadbook_ir::StateKind::Parallel)
        });
        parallel.or(active.last()).map(String::as_str)
    }

    /// The events of a run of machine `sender` that a run of `machine` receives when the
    /// sender emits them: those `machine` has an `on x.E` for, where `x` is a run of
    /// `sender`, in any state (execution.md, "Messages between runs"). It has
    /// [`Program::ENDED`] too when `machine` has an `on x.ended` for a run of `sender`: a
    /// run of `machine` hears the end of a run of `sender`. Event names are upper case, so
    /// `ended` is never one.
    pub fn heard(&self, machine: &str, sender: &str) -> HashSet<String> {
        self.machines
            .get(machine)
            .and_then(|idx| idx.hears.get(sender))
            .cloned()
            .unwrap_or_default()
    }
}
