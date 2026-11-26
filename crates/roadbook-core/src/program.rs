//! An IR document made ready to run: every machine's states indexed in document order.

use std::collections::HashMap;
use std::fmt;

use roadbook_ir::{Ir, Machine, State, TypeDef, TypeRef};

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
    /// For each state, its transitions' targets' indices, in order (`None`: no target).
    pub targets: Vec<Vec<Option<usize>>>,
    /// For each state, its initial child's index, if it names one.
    pub initial: Vec<Option<usize>>,
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

impl Program {
    /// Indexes `ir`. An IR whose transition targets or initial states name states their
    /// machine does not have is refused.
    pub fn new(ir: Ir) -> Result<Program, ProgramError> {
        let mut machines = HashMap::new();
        for (name, m) in &ir.machines {
            let mut idx = MachineIndex {
                paths: Vec::new(),
                parents: Vec::new(),
                children: Vec::new(),
                by_id: HashMap::new(),
                targets: Vec::new(),
                initial: Vec::new(),
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

fn walk(s: &State, parent: Option<usize>, path: &mut Vec<usize>, idx: &mut MachineIndex) -> usize {
    let me = idx.paths.len();
    idx.paths.push(path.clone());
    idx.parents.push(parent);
    idx.children.push(Vec::new());
    idx.by_id.insert(s.id.clone(), me);
    for (i, c) in s.states.iter().enumerate() {
        path.push(i);
        let ci = walk(c, Some(me), path, idx);
        path.pop();
        idx.children[me].push(ci);
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
}
