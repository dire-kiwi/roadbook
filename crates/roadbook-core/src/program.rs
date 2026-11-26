//! An IR document made ready to run: every machine's states indexed in document order.

use std::collections::HashMap;
use std::fmt;

use roadbook_ir::{Ir, Machine, State, StateKind, TypeDef, TypeRef};

/// An IR document, checked and indexed for the interpreter.
///
/// Build one with [`Program::new`] (or [`Program::from_json`]) and keep it for as long as runs
/// of its machines go on: [`Program::start`] and [`Program::step`] take it by reference.
#[derive(Debug, Clone)]
pub struct Program {
    ir: Ir,
    machines: HashMap<String, MachineIndex>,
}

/// The states of one machine, in document order: a state's index is its position in a
/// pre-order walk of the tree, so "innermost first" is descending index for a chain.
#[derive(Debug, Clone)]
pub(crate) struct MachineIndex {
    /// For each state, the child positions leading to it from the root.
    paths: Vec<Vec<usize>>,
    /// For each state, its parent's index (`None` for the root).
    pub parents: Vec<Option<usize>>,
    /// For each state, its children's indices.
    pub children: Vec<Vec<usize>>,
    pub by_id: HashMap<String, usize>,
}

/// Why an IR cannot be run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgramError {
    /// The IR could not be read (its version, or its shape).
    Read(String),
    /// A machine has a parallel state, which the runtime does not support yet (milestone 6).
    Parallel {
        /// The machine.
        machine: String,
        /// The parallel state's id.
        state: String,
    },
}

impl fmt::Display for ProgramError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProgramError::Read(e) => f.write_str(e),
            ProgramError::Parallel { machine, state } => write!(
                f,
                "machine `{machine}` has the parallel state `{state}`: parallel states are not \
                 supported by this runtime yet (milestone 6)"
            ),
        }
    }
}

impl std::error::Error for ProgramError {}

impl Program {
    /// Indexes `ir`. Refuses a machine with a parallel state.
    pub fn new(ir: Ir) -> Result<Program, ProgramError> {
        let mut machines = HashMap::new();
        for (name, m) in &ir.machines {
            let mut idx = MachineIndex {
                paths: Vec::new(),
                parents: Vec::new(),
                children: Vec::new(),
                by_id: HashMap::new(),
            };
            walk(&m.root, None, &mut Vec::new(), &mut idx, name)?;
            machines.insert(name.clone(), idx);
        }
        Ok(Program { ir, machines })
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

    /// `t` with aliases expanded at the top, transitively: `Money` for `type Money = int` is
    /// `int`, and `S` for `type S = string?` is `string?`. The compiler rejects alias
    /// cycles; the bound only keeps a hand-written IR from looping.
    pub(crate) fn resolve<'t>(&'t self, mut t: &'t TypeRef) -> &'t TypeRef {
        for _ in 0..64 {
            match t {
                TypeRef::Named(n) => match self.ir.types.get(n).map(|d| &d.def) {
                    Some(TypeDef::Alias { ty }) => t = ty,
                    _ => break,
                },
                _ => break,
            }
        }
        t
    }

    /// `t` with aliases and `?` removed: the type whose operators an operand of type `t`
    /// uses.
    pub(crate) fn scalar<'t>(&'t self, mut t: &'t TypeRef) -> &'t TypeRef {
        for _ in 0..64 {
            match self.resolve(t) {
                TypeRef::Optional(inner) => t = inner,
                other => return other,
            }
        }
        t
    }

    pub(crate) fn machine(&self, name: &str) -> Option<(&Machine, &MachineIndex)> {
        Some((self.ir.machines.get(name)?, self.machines.get(name)?))
    }
}

fn walk(
    s: &State,
    parent: Option<usize>,
    path: &mut Vec<usize>,
    idx: &mut MachineIndex,
    machine: &str,
) -> Result<usize, ProgramError> {
    if s.kind == StateKind::Parallel {
        return Err(ProgramError::Parallel {
            machine: machine.to_owned(),
            state: s.id.clone(),
        });
    }
    let me = idx.paths.len();
    idx.paths.push(path.clone());
    idx.parents.push(parent);
    idx.children.push(Vec::new());
    idx.by_id.insert(s.id.clone(), me);
    for (i, c) in s.states.iter().enumerate() {
        path.push(i);
        let ci = walk(c, Some(me), path, idx, machine)?;
        path.pop();
        idx.children[me].push(ci);
    }
    Ok(me)
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
