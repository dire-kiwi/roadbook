//! The state space of a machine: the graph of every configuration it can be in, for property
//! checks on charts ("never `S`", "`S` only after `T`", "can always finish").
//!
//! [`explore`] runs a machine *abstractly*. It follows `spec/execution.md` microstep by
//! microstep, except that it evaluates no data: every guard may hold or may not, and every
//! input that could arrive may arrive. So every configuration a real run passes through is a
//! node of the graph, and every microstep it takes is an edge. The converse does not hold:
//! something other than the chart may rule out a path the graph has, which is why each edge
//! says what it assumed of it ([`Edge::assumptions`], [`Assumption`]):
//! - the outcome of each guard it relies on;
//! - for a timer's expiry while another timer of the run is running, that it expires first,
//!   since its delay might not be the shortest;
//! - for an outcome of a child or spawned child machine, or an event that a child, a spawned
//!   child or a peer emits, that the other machine does that;
//! - for any step from a stable node where another machine can act (some edge from it is
//!   that machine's move), that the step comes first: that machine does not end or emit
//!   before it ([`Assumption::Unpreempted`]). A machine's own moves race only the others.
//!
//! The machine's own events and the outcomes of tasks are the host's to give, and a timer
//! that is the only one running expires whenever the run waits long enough, so where no
//! other machine can act those assume nothing. A path whose edges assume nothing is one a
//! real run can take, whatever the data and whatever the other machines do.
//!
//! The graph covers runs started from the chart. A snapshot restored after a migration can be
//! in a configuration the chart cannot reach, which is not in the graph, so a property of the
//! graph says nothing about such a run.
//!
//! A **node** is the machine between two microsteps: its active states, what each history
//! state has recorded, which invokes and timers of the active states have already been
//! consumed (an outcome or an expiry consumes its invoke or timer even when it takes no
//! transition), which `children` collections may have a running child, whether the run has
//! finished, and, while it settles, the completion queue. Nodes are hash-consed: two ways to
//! the same configuration reach the same node.
//!
//! An **edge** is one of:
//! - an input's microstep, from a stable node (one that has settled);
//! - an invoke outcome or a timer expiry that takes no transition, which only consumes it;
//! - a microstep of settling (`always`, or a `done`), from a settling node;
//! - the end of settling ([`Input::Settled`]), from a settling node to the stable node with the
//!   same active states, when every `always` guard and every queued `done` guard may fail.
//!
//! Settling microsteps are edges of their own, so the transient configurations a run passes
//! through while it settles are nodes too ([`Graph::is_settling`]). A settling node takes no
//! input from outside.
//!
//! Where the abstraction loses information it keeps every possibility, so the graph has more
//! than the runs can do, never less:
//! - Guards are independent of one another and of the data, except that one guard has one
//!   outcome within one selection (two walks that reach the same state see the same guards).
//! - A `children` collection is either empty or may have children, of any keys; once a `spawn`
//!   effect has run it may have children for the rest of the run (a `stop` never empties it).
//!   So a spawned child's outcomes and emitted events may arrive at any stable node after it.
//! - An emitted event from a peer may arrive at any time, from any run that any nonempty set
//!   of the `ref` parameters and variables of its machine hold.
//! - A defect, a cancel and the `limit` on settling are not modelled: they end a run, and a
//!   node's run only ever ends at a top-level final state.

use std::cell::{Cell, RefCell};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, VecDeque};
use std::fmt;
use std::hash::{BuildHasher, RandomState};

use indexmap::IndexMap;
use roadbook_ir::{self as ir, AddressKind, Effect, InvokeKind, StateKind, Trigger};

/// A state of the machine, by its position in document order (a pre-order walk of the state
/// tree, the root first), history states included. So the root is `StateIndex(0)`, a state
/// comes before its descendants, and sorting by index is sorting in document order.
/// [`Graph::state_id`] gives its IR id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StateIndex(pub u32);

impl StateIndex {
    /// The position in document order.
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// A node of a [`Graph`], by its position in the order [`explore`] found the nodes (breadth
/// first from the start, which is `NodeId(0)`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub u32);

impl NodeId {
    /// The position among the nodes, from 0 to [`Graph::node_count`].
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// An edge of a [`Graph`], by its position among the edges.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EdgeId(pub u32);

impl EdgeId {
    /// The position among the edges, from 0 to [`Graph::edge_count`].
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// A transition: the state it is written in and its index in that state's `transitions`, as a
/// `transition` trace record names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TransitionRef {
    /// The state the transition belongs to (its source).
    pub state: StateIndex,
    /// Its index in the state's `transitions`.
    pub index: usize,
}

/// Something an edge relies on that the chart does not decide.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Assumption {
    /// The outcome of a guard.
    Guard {
        /// The guarded transition.
        transition: TransitionRef,
        /// Whether its guard was assumed to hold (it was taken) or to fail (it was skipped, so
        /// the search went on past it, or its completion was discarded).
        holds: bool,
    },
    /// The timer of the `after` block of `state` whose first transition is `index` expires
    /// while another timer of the run is running: before it, though its delay might not be the
    /// shortest.
    TimerFirst {
        /// The state of the `after` block.
        state: StateIndex,
        /// The index of the block's first transition.
        index: usize,
    },
    /// Another machine does what the edge's input says: a child or spawned child machine ends
    /// `done` or fails, or a child, a spawned child or a peer emits an event. Always the
    /// edge's own input: [`Input::InvokeDone`] or [`Input::InvokeFail`] of a machine's invoke,
    /// [`Input::Emitted`], [`Input::SpawnDone`] or [`Input::SpawnFail`].
    Machine(Input),
    /// None of these other machines acts first: at a node where they can (an edge from it
    /// assumes [`Assumption::Machine`] of them), the step happens before they end, fail or
    /// emit. Given to every edge from such a node but those of the sender's own moves, which
    /// name only the other senders.
    Unpreempted {
        /// The machines that could act first, sorted.
        senders: Vec<Sender>,
    },
}

impl Assumption {
    /// The machine whose move a [`Assumption::Machine`] input is.
    fn sender(input: &Input) -> Option<Sender> {
        match input {
            Input::InvokeDone { state } | Input::InvokeFail { state, .. } => {
                Some(Sender::Child(*state))
            }
            Input::Emitted { from, .. } => Some(from.clone()),
            Input::SpawnDone { children } | Input::SpawnFail { children, .. } => {
                Some(Sender::Children(children.clone()))
            }
            _ => None,
        }
    }
}

/// Whether two senders may be the same machine: the same child or collection, or peers that
/// some name may hold for both.
fn same_sender(a: &Sender, b: &Sender) -> bool {
    match (a, b) {
        (Sender::Refs(x), Sender::Refs(y)) => x.iter().any(|n| y.contains(n)),
        _ => a == b,
    }
}

/// Gives each successor of a stable node the race it relies on: that the other machines
/// that can act at the node (those some successor's input is a move of) do not act first.
fn add_races(successors: &mut [Successor]) {
    let mut live: Vec<Sender> = Vec::new();
    for s in successors.iter() {
        for a in &s.assumptions {
            if let Assumption::Machine(input) = a
                && let Some(sender) = Assumption::sender(input)
                && !live.contains(&sender)
            {
                live.push(sender);
            }
        }
    }
    if live.is_empty() {
        return;
    }
    live.sort();
    for s in successors.iter_mut() {
        let own = s.assumptions.iter().find_map(|a| match a {
            Assumption::Machine(input) => Assumption::sender(input),
            _ => None,
        });
        let senders: Vec<Sender> = live
            .iter()
            .filter(|x| own.as_ref().is_none_or(|o| !same_sender(o, x)))
            .cloned()
            .collect();
        if !senders.is_empty() {
            s.assumptions.push(Assumption::Unpreempted { senders });
            s.assumptions.sort_unstable();
        }
    }
}

/// Who sent an emitted event.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Sender {
    /// The child machine invoked (with `as`) by this state.
    Child(StateIndex),
    /// A peer that these `ref` parameters and variables hold, and none of the others that a
    /// trigger expects the event through. Several names can hold the same run (a variable
    /// set from a parameter, or two arguments given the same handle).
    Refs(Vec<String>),
    /// A spawned child of this `children` collection.
    Children(String),
}

/// What an edge's microstep was for: its input, or the kind of eventless transition settling
/// took.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Input {
    /// One of the machine's events, from host code or as a message (`on Name`, `on *`).
    Event(String),
    /// The invoke of `state` succeeded.
    InvokeDone {
        /// The state that owns the invoke.
        state: StateIndex,
    },
    /// The invoke of `state` failed with `error`, one its task or machine declares.
    InvokeFail {
        /// The state that owns the invoke.
        state: StateIndex,
        /// The error's name.
        error: String,
    },
    /// The timer of the `after` block of `state` whose first transition is `index` expired.
    After {
        /// The state of the `after` block.
        state: StateIndex,
        /// The index of the block's first transition, as the trace names the timer.
        index: usize,
    },
    /// An event `event` emitted by another run (`on name.Event`).
    Emitted {
        /// The run that emitted it.
        from: Sender,
        /// The event's name.
        event: String,
    },
    /// A spawned child of the collection ended `done` (`on name.done`).
    SpawnDone {
        /// The collection.
        children: String,
    },
    /// A spawned child of the collection failed with `error` (`on name.fail E`).
    SpawnFail {
        /// The collection.
        children: String,
        /// The error's name, one the collection's machine declares.
        error: String,
    },
    /// Settling took the enabled `always` transitions.
    Always,
    /// Settling took a `done` transition of `state`, which had completed.
    Done {
        /// The completed state.
        state: StateIndex,
    },
    /// Settling ended: no `always` transition and no queued `done` transition was taken. No
    /// transition is taken; the edge goes to the stable node with the same active states.
    Settled,
}

/// One step of the graph: a microstep, or a step that takes no transition (an outcome or
/// expiry that only consumes its invoke or timer, or the end of settling).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edge {
    /// The node it leaves.
    pub from: NodeId,
    /// The node it reaches.
    pub to: NodeId,
    /// What it was for.
    pub input: Input,
    /// The transitions the microstep took, in the order of their `transition` records. Empty
    /// for a step that takes none.
    pub transitions: Vec<TransitionRef>,
    /// What the step relies on, sorted: the outcomes of the guards of the transitions taken,
    /// of the guarded ones the search skipped on the way to them, and of the `done`
    /// transitions whose completion was discarded; and the order of timers or what another
    /// machine does, for its input. Empty when the step can happen whatever the data and the
    /// other machines.
    pub assumptions: Vec<Assumption>,
    /// Whether it is a step of settling, from a settling node.
    pub settling: bool,
}

/// The graph was not explored to the end: it is larger than [`explore`] goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TooLarge {
    /// It has more than the budget of nodes.
    Nodes {
        /// How many nodes had been found when exploring stopped: one more than the budget.
        reached: usize,
    },
    /// A peer's event `event` is expected through more `ref` names than
    /// [`MAX_SENDER_NAMES`]: every set of them that may hold the sender would be an input.
    Senders {
        /// The event.
        event: String,
        /// How many names it is expected through.
        names: usize,
    },
    /// The regions' outcomes for the inputs combine in more ways than the budget allows
    /// ([`COMBINATIONS_PER_NODE`] for each node of the budget).
    Combinations {
        /// How many combinations were allowed.
        budget: usize,
    },
}

impl fmt::Display for TooLarge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TooLarge::Nodes { reached } => write!(
                f,
                "the machine has more than {} configurations",
                reached.saturating_sub(1)
            ),
            TooLarge::Senders { event, names } => write!(
                f,
                "`{event}` is expected from a peer through {names} `ref` names, more than the {MAX_SENDER_NAMES} whose every set is explored"
            ),
            TooLarge::Combinations { budget } => write!(
                f,
                "the outcomes of its regions combine in more than {budget} ways"
            ),
        }
    }
}

/// The most `ref` names one peer's event may be expected through: each nonempty set of them
/// that may hold the sender is an input of its own.
pub const MAX_SENDER_NAMES: usize = 16;

/// How many combinations of the walks' outcomes (one per region, roughly) [`explore`] tries
/// beyond the first of each input, over the whole exploration, for each node of its budget.
/// Only guards make more than one, so a chart with few guarded transitions on the same input
/// in different regions spends next to none.
pub const COMBINATIONS_PER_NODE: usize = 16;

impl std::error::Error for TooLarge {}

/// The default budget for [`explore`].
pub const DEFAULT_MAX_NODES: usize = 100_000;

/// Every configuration `machine` (a machine of `ir`) can be in, and the steps between them.
/// Stops with [`TooLarge`] once it has found more than `max_nodes` nodes, or tried more than
/// [`COMBINATIONS_PER_NODE`] times `max_nodes` combinations of outcomes, and fails at once
/// for a peer's event expected through more than [`MAX_SENDER_NAMES`] names.
pub fn explore(ir: &ir::Ir, machine: &ir::Machine, max_nodes: usize) -> Result<Graph, TooLarge> {
    let budget = max_nodes.saturating_mul(COMBINATIONS_PER_NODE);
    let chart = Chart::new(ir, machine, budget);
    if let Some((event, names)) = chart
        .ref_events
        .iter()
        .find(|(_, names)| names.len() > MAX_SENDER_NAMES)
    {
        return Err(TooLarge::Senders {
            event: event.to_string(),
            names: names.len(),
        });
    }
    let mut b = Builder {
        ids: HashMap::new(),
        hasher: RandomState::new(),
        configs: Vec::new(),
        edges: Vec::new(),
        max_nodes,
    };
    let mut start = Config::new(chart.collections.len());
    let mut entries = vec![0];
    chart.default_entry(0, &mut entries);
    chart.enter_states(&mut start, &entries);
    let start = chart.normalize(start);
    b.intern(start)?;
    let mut idle: Vec<Vec<Input>> = Vec::new();
    let mut next = 0;
    while next < b.configs.len() {
        let cfg = b.configs[next].clone();
        let mut successors = chart.successors(&cfg);
        if !cfg.settling {
            add_races(&mut successors);
        }
        if chart.exhausted() {
            return Err(TooLarge::Combinations { budget });
        }
        idle.push(std::mem::take(&mut *chart.idle.borrow_mut()));
        for s in successors {
            let to = b.intern(s.config)?;
            b.edges.push(Edge {
                from: NodeId(next as u32),
                to,
                input: s.input,
                transitions: s.transitions,
                assumptions: s.assumptions,
                settling: cfg.settling,
            });
        }
        next += 1;
    }
    let edges = b.edges;
    let configs = b.configs;
    let mut out = vec![Vec::new(); configs.len()];
    let mut inc = vec![Vec::new(); configs.len()];
    for (i, e) in edges.iter().enumerate() {
        out[e.from.index()].push(EdgeId(i as u32));
        inc[e.to.index()].push(EdgeId(i as u32));
    }
    let nodes = configs
        .into_iter()
        .map(|c| NodeData {
            active: c.active.iter().map(|&s| StateIndex(s)).collect(),
            end: c.end.map(StateIndex),
            settling: c.settling,
            history: c
                .history
                .iter()
                .map(|(h, r)| (StateIndex(*h), r.iter().map(|&s| StateIndex(s)).collect()))
                .collect(),
        })
        .collect();
    let states: Vec<String> = chart.states.iter().map(|s| s.id.clone()).collect();
    let index = states
        .iter()
        .enumerate()
        .map(|(i, id)| (id.clone(), StateIndex(i as u32)))
        .collect();
    Ok(Graph {
        states,
        index,
        nodes,
        edges,
        out,
        inc,
        idle,
    })
}

/// The state space of a machine, from [`explore`].
#[derive(Clone, Debug)]
pub struct Graph {
    states: Vec<String>,
    index: HashMap<String, StateIndex>,
    nodes: Vec<NodeData>,
    edges: Vec<Edge>,
    out: Vec<Vec<EdgeId>>,
    inc: Vec<Vec<EdgeId>>,
    /// By node, the inputs that may take nothing there.
    idle: Vec<Vec<Input>>,
}

#[derive(Clone, Debug)]
struct NodeData {
    active: Box<[StateIndex]>,
    end: Option<StateIndex>,
    settling: bool,
    history: Vec<(StateIndex, Vec<StateIndex>)>,
}

impl Graph {
    /// The node the run starts in: the configuration right after its initial entry, before it
    /// settles (so a settling node if it has anything to settle).
    pub fn start(&self) -> NodeId {
        NodeId(0)
    }

    /// How many nodes there are. Their ids are `NodeId(0)` to `NodeId(node_count() - 1)`.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// How many edges there are.
    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    /// Every node, in the order they were found.
    pub fn nodes(&self) -> impl Iterator<Item = NodeId> + '_ {
        (0..self.nodes.len() as u32).map(NodeId)
    }

    /// The node's active states, in document order. For a node where the run has finished,
    /// the states that were active when it entered its top-level final state, that state
    /// among them (the run then exits them all, but a check wants to see where it ended).
    pub fn active(&self, n: NodeId) -> &[StateIndex] {
        &self.nodes[n.index()].active
    }

    /// Whether `s` is one of the node's active states.
    pub fn is_active(&self, n: NodeId, s: StateIndex) -> bool {
        self.nodes[n.index()].active.binary_search(&s).is_ok()
    }

    /// Whether the run has finished at this node, by entering a top-level final state. Such a
    /// node has no edges.
    pub fn is_final(&self, n: NodeId) -> bool {
        self.nodes[n.index()].end.is_some()
    }

    /// The top-level final state the run finished in, if it has finished at this node.
    pub fn final_state(&self, n: NodeId) -> Option<StateIndex> {
        self.nodes[n.index()].end
    }

    /// Whether the node is a transient configuration of settling: the run has more `always`
    /// or `done` transitions to try before it takes another input. Its edges are settling
    /// steps; it takes no input from outside.
    pub fn is_settling(&self, n: NodeId) -> bool {
        self.nodes[n.index()].settling
    }

    /// What each history state has recorded at this node, by history state, in document
    /// order: the states it would restore, in document order. A history state whose parent
    /// has not been exited yet has no entry.
    pub fn history(&self, n: NodeId) -> &[(StateIndex, Vec<StateIndex>)] {
        &self.nodes[n.index()].history
    }

    /// The edges leaving the node.
    pub fn outgoing(&self, n: NodeId) -> &[EdgeId] {
        &self.out[n.index()]
    }

    /// Whether `input` may take no transition at the node, leaving it as it is: an event, an
    /// emitted event or a spawned child's outcome that some active state has a candidate for,
    /// when every walk's candidates are guarded or absent, so every guard may fail. Such an
    /// outcome has no edge. (An invoke outcome or a timer expiry that takes nothing consumes
    /// its work, so it has an edge of its own.)
    pub fn takes_nothing(&self, n: NodeId, input: &Input) -> bool {
        self.idle[n.index()].contains(input)
    }

    /// The edges reaching the node.
    pub fn incoming(&self, n: NodeId) -> &[EdgeId] {
        &self.inc[n.index()]
    }

    /// An edge.
    pub fn edge(&self, e: EdgeId) -> &Edge {
        &self.edges[e.index()]
    }

    /// How many states the machine has, history states included.
    pub fn state_count(&self) -> usize {
        self.states.len()
    }

    /// The IR id of a state: `Checkout.Paying`.
    pub fn state_id(&self, s: StateIndex) -> &str {
        &self.states[s.index()]
    }

    /// The state with this IR id, if the machine has one.
    pub fn state_index(&self, id: &str) -> Option<StateIndex> {
        self.index.get(id).copied()
    }

    /// The input as a short text for a message: `Go`, `done of the invoke in
    /// Checkout.Paying`, `the after at #0 of Checkout.Backoff` (`#0` is the index of the
    /// block's first transition). State ids are given whole.
    pub fn describe(&self, input: &Input) -> String {
        let id = |s: &StateIndex| self.state_id(*s);
        match input {
            Input::Event(e) => e.clone(),
            Input::InvokeDone { state } => format!("done of the invoke in {}", id(state)),
            Input::InvokeFail { state, error } => {
                format!("fail {error} of the invoke in {}", id(state))
            }
            Input::After { state, index } => format!("the after at #{index} of {}", id(state)),
            Input::Emitted { from, event } => match from {
                Sender::Child(s) => format!("{event} from the child of {}", id(s)),
                Sender::Refs(names) => format!("{}.{event}", names.join(" = ")),
                Sender::Children(c) => format!("{c}.{event}"),
            },
            Input::SpawnDone { children } => format!("{children}.done"),
            Input::SpawnFail { children, error } => format!("{children}.fail {error}"),
            Input::Always => "always".into(),
            Input::Done { state } => format!("done of {}", id(state)),
            Input::Settled => "settled".into(),
        }
    }

    /// A shortest path (fewest edges) from `from` to a node for which `to` holds, breadth
    /// first: the edges in order, empty if `to` holds at `from`. `None` if no such node is
    /// reachable.
    pub fn shortest_path(&self, from: NodeId, to: impl Fn(NodeId) -> bool) -> Option<Vec<EdgeId>> {
        let mut prev: Vec<Option<EdgeId>> = vec![None; self.nodes.len()];
        let mut seen = vec![false; self.nodes.len()];
        let mut queue = VecDeque::from([from]);
        seen[from.index()] = true;
        while let Some(n) = queue.pop_front() {
            if to(n) {
                return Some(self.path_to(n, &prev));
            }
            for &e in self.outgoing(n) {
                let m = self.edges[e.index()].to;
                if !seen[m.index()] {
                    seen[m.index()] = true;
                    prev[m.index()] = Some(e);
                    queue.push_back(m);
                }
            }
        }
        None
    }

    /// A path from `from` to a node for which `to` holds that relies on as few assumptions as
    /// it can: the fewest [`Edge::assumptions`] in all, then the fewest edges. A path with no
    /// assumption happens whatever the data and the other machines, so a counterexample found
    /// this way is real whenever one is. `None` if no such node is reachable.
    pub fn fewest_assumptions_path(
        &self,
        from: NodeId,
        to: impl Fn(NodeId) -> bool,
    ) -> Option<Vec<EdgeId>> {
        // Dijkstra on (assumptions, edges), which compares lexicographically.
        let mut best: Vec<Option<(u32, u32)>> = vec![None; self.nodes.len()];
        let mut prev: Vec<Option<EdgeId>> = vec![None; self.nodes.len()];
        let mut heap = BinaryHeap::from([Reverse(((0u32, 0u32), from))]);
        best[from.index()] = Some((0, 0));
        while let Some(Reverse((cost, n))) = heap.pop() {
            if best[n.index()] != Some(cost) {
                continue;
            }
            if to(n) {
                return Some(self.path_to(n, &prev));
            }
            for &e in self.outgoing(n) {
                let edge = &self.edges[e.index()];
                let step = edge.assumptions.len() as u32;
                let next = (cost.0 + step, cost.1 + 1);
                let m = edge.to;
                if best[m.index()].is_none_or(|b| next < b) {
                    best[m.index()] = Some(next);
                    prev[m.index()] = Some(e);
                    heap.push(Reverse((next, m)));
                }
            }
        }
        None
    }

    fn path_to(&self, mut n: NodeId, prev: &[Option<EdgeId>]) -> Vec<EdgeId> {
        let mut path = Vec::new();
        while let Some(e) = prev[n.index()] {
            path.push(e);
            n = self.edges[e.index()].from;
        }
        path.reverse();
        path
    }
}

/// A transition's trigger as the viewer labels it (`triggerText` in
/// `packages/viewer/src/model.ts`): `Go`, `after 5s`, `fail Declined`, `on *`. `roadbook
/// paths` and the counterexamples of checks write triggers this way.
pub fn trigger_text(t: &Trigger) -> String {
    match t {
        Trigger::Event { event } => event.clone(),
        Trigger::Emitted { from, event } => format!("{}.{event}", from.name),
        Trigger::Any => "on *".into(),
        Trigger::After { delay } => format!("after {}", delay.text),
        Trigger::Always => "always".into(),
        Trigger::Done | Trigger::InvokeDone => "done".into(),
        Trigger::InvokeFail { error } => format!("fail {error}"),
        Trigger::SpawnDone { children } => format!("{children}.done"),
        Trigger::SpawnFail { children, error } => format!("{children}.fail {error}"),
    }
}

/// The guard's text of transition `index` of state `s`; `else` for the unguarded last branch
/// of a guarded block, `None` for an unguarded transition (`guardText` in the viewer, without
/// the brackets).
pub fn guard_text(s: &ir::State, index: usize) -> Option<String> {
    let t = &s.transitions[index];
    if let Some(g) = &t.guard {
        return Some(g.text.clone());
    }
    let first = s.transitions.iter().position(|o| o.group == t.group);
    (first != Some(index)).then(|| "else".to_string())
}

// ---------------------------------------------------------------------------------------------
// The machine's tables.

/// A transition, by state and index, while exploring.
type T = (u32, usize);

/// A selection's outcome: the transitions taken, and the guard outcomes it relies on.
type Selected = (Vec<T>, Vec<(T, bool)>);

/// The slot of an invoke in [`Config::consumed`]; a timer's is its `after` block's group.
const INVOKE: u32 = u32::MAX;

/// A machine's states in document order, with what the exploration looks up.
struct Chart<'a> {
    ir: &'a ir::Ir,
    machine: &'a ir::Machine,
    states: Vec<&'a ir::State>,
    parent: Vec<Option<u32>>,
    /// The states below `s` are `s + 1 .. end[s]`.
    end: Vec<u32>,
    /// The children that are not history states, in order.
    children: Vec<Vec<u32>>,
    /// The history states in each state's `states`.
    histories: Vec<Vec<u32>>,
    initial: Vec<Option<u32>>,
    /// Each transition's target.
    targets: Vec<Vec<Option<u32>>>,
    /// The `children` collections, in declaration order.
    collections: Vec<&'a str>,
    /// The collections a state's `entry` and `exit` spawn into.
    spawn_entry: Vec<Vec<usize>>,
    spawn_exit: Vec<Vec<usize>>,
    /// The collections each transition's effects spawn into.
    spawn_effects: Vec<Vec<Vec<usize>>>,
    /// The `after` blocks of each state: (group, index of its first transition).
    after_blocks: Vec<Vec<(u32, usize)>>,
    /// The events a trigger expects from the child invoked by each state.
    child_events: Vec<Vec<&'a str>>,
    /// For each collection, the events a trigger expects from its children.
    children_events: Vec<Vec<&'a str>>,
    /// The peers' events: for each machine and event that an `on name.Event` with a `ref`
    /// address names, the event and the names it is expected through.
    ref_events: Vec<(&'a str, Vec<&'a str>)>,
    /// Errors of each collection's machine.
    collection_errors: Vec<&'a [String]>,
    /// Whether each state has an `always` transition.
    has_always: Vec<bool>,
    /// The events each state has an `on` for, and whether it has an `on *`.
    on_events: Vec<Vec<&'a str>>,
    on_any: Vec<bool>,
    /// How many more combinations of the walks' outcomes may be tried.
    combinations: Cell<usize>,
    /// The inputs of the node being expanded that may take nothing.
    idle: RefCell<Vec<Input>>,
}

impl<'a> Chart<'a> {
    fn new(ir: &'a ir::Ir, machine: &'a ir::Machine, combinations: usize) -> Chart<'a> {
        let mut c = Chart {
            ir,
            machine,
            states: Vec::new(),
            parent: Vec::new(),
            end: Vec::new(),
            children: Vec::new(),
            histories: Vec::new(),
            initial: Vec::new(),
            targets: Vec::new(),
            collections: machine.children.keys().map(String::as_str).collect(),
            spawn_entry: Vec::new(),
            spawn_exit: Vec::new(),
            spawn_effects: Vec::new(),
            after_blocks: Vec::new(),
            child_events: Vec::new(),
            children_events: vec![Vec::new(); machine.children.len()],
            ref_events: Vec::new(),
            collection_errors: machine
                .children
                .values()
                .map(|d| {
                    ir.machines
                        .get(&d.machine)
                        .map_or(&[][..], |m| m.errors.as_slice())
                })
                .collect(),
            has_always: Vec::new(),
            idle: RefCell::new(Vec::new()),
            on_events: Vec::new(),
            on_any: Vec::new(),
            combinations: Cell::new(combinations),
        };
        c.add(&machine.root, None);
        let index: HashMap<&str, u32> = c
            .states
            .iter()
            .enumerate()
            .map(|(i, s)| (s.id.as_str(), i as u32))
            .collect();
        let collection = |name: &str| c.collections.iter().position(|&n| n == name);
        let spawns = |effects: &[Effect]| -> Vec<usize> {
            let mut out: Vec<usize> = effects
                .iter()
                .filter_map(|e| match e {
                    Effect::Spawn { children, .. } => collection(children),
                    _ => None,
                })
                .collect();
            out.sort_unstable();
            out.dedup();
            out
        };
        let n = c.states.len();
        let mut child_events: Vec<Vec<&str>> = vec![Vec::new(); n];
        let mut children_events = c.children_events.clone();
        let mut ref_events: IndexMap<(&str, &str), Vec<&str>> = IndexMap::new();
        for i in 0..n {
            let s = c.states[i];
            c.children.push(
                s.states
                    .iter()
                    .filter(|x| !x.kind.is_history())
                    .map(|x| index[x.id.as_str()])
                    .collect(),
            );
            c.histories.push(
                s.states
                    .iter()
                    .filter(|x| x.kind.is_history())
                    .map(|x| index[x.id.as_str()])
                    .collect(),
            );
            c.initial
                .push(s.initial.as_deref().and_then(|x| index.get(x).copied()));
            c.targets.push(
                s.transitions
                    .iter()
                    .map(|t| t.target.as_deref().and_then(|x| index.get(x).copied()))
                    .collect(),
            );
            c.spawn_entry.push(spawns(&s.entry));
            c.spawn_exit.push(spawns(&s.exit));
            c.spawn_effects
                .push(s.transitions.iter().map(|t| spawns(&t.effects)).collect());
            let mut blocks = Vec::new();
            for (k, t) in s.transitions.iter().enumerate() {
                match &t.trigger {
                    Trigger::After { .. } if !blocks.iter().any(|&(g, _)| g == t.group) => {
                        blocks.push((t.group, k));
                    }
                    Trigger::Emitted { from, event } => {
                        let push = |list: &mut Vec<&'a str>| {
                            if !list.contains(&event.as_str()) {
                                list.push(event.as_str());
                            }
                        };
                        match from.kind {
                            AddressKind::Child => {
                                if let Some(&st) = from.state.as_deref().and_then(|x| index.get(x))
                                {
                                    push(&mut child_events[st as usize]);
                                }
                            }
                            AddressKind::Children => {
                                if let Some(k) = collection(&from.name) {
                                    push(&mut children_events[k]);
                                }
                            }
                            AddressKind::Ref => {
                                let names = ref_events
                                    .entry((from.machine.as_str(), event.as_str()))
                                    .or_default();
                                if !names.contains(&from.name.as_str()) {
                                    names.push(from.name.as_str());
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            c.after_blocks.push(blocks);
            c.has_always.push(
                s.transitions
                    .iter()
                    .any(|t| matches!(t.trigger, Trigger::Always)),
            );
            c.on_events.push(
                s.transitions
                    .iter()
                    .filter_map(|t| match &t.trigger {
                        Trigger::Event { event } => Some(event.as_str()),
                        _ => None,
                    })
                    .collect(),
            );
            c.on_any.push(
                s.transitions
                    .iter()
                    .any(|t| matches!(t.trigger, Trigger::Any)),
            );
        }
        c.child_events = child_events;
        c.children_events = children_events;
        // Only names of one machine can hold the same run, and only those a trigger expects
        // this event through change what is selected.
        c.ref_events = ref_events
            .into_iter()
            .map(|((_, e), names)| (e, names))
            .collect();
        c
    }

    fn add(&mut self, s: &'a ir::State, parent: Option<u32>) {
        let i = self.states.len();
        self.states.push(s);
        self.parent.push(parent);
        self.end.push(0);
        for c in &s.states {
            self.add(c, Some(i as u32));
        }
        self.end[i] = self.states.len() as u32;
    }

    fn kind(&self, s: u32) -> StateKind {
        self.states[s as usize].kind
    }

    fn transition(&self, (s, i): T) -> &'a ir::Transition {
        &self.states[s as usize].transitions[i]
    }

    /// Whether `a` is a proper ancestor of `x`.
    fn contains(&self, a: u32, x: u32) -> bool {
        a < x && x < self.end[a as usize]
    }

    fn default_entry(&self, s: u32, out: &mut Vec<u32>) {
        match self.kind(s) {
            StateKind::Compound => {
                if let Some(c) = self.initial[s as usize] {
                    out.push(c);
                    self.default_entry(c, out);
                }
            }
            StateKind::Parallel => {
                for &c in &self.children[s as usize] {
                    out.push(c);
                    self.default_entry(c, out);
                }
            }
            _ => {}
        }
    }

    /// `domain(t)`: the root for a transition of the root, else the nearest compound proper
    /// ancestor of the source that is a proper ancestor of the target.
    fn domain(&self, source: u32, target: u32) -> u32 {
        if source == 0 {
            return 0;
        }
        let mut a = self.parent[source as usize].unwrap_or(0);
        while a != 0 && !(self.kind(a) == StateKind::Compound && self.contains(a, target)) {
            a = self.parent[a as usize].unwrap_or(0);
        }
        a
    }

    /// `exitSet(t)`: the active states below the domain, in document order.
    fn exit_set(&self, active: &[u32], t: T) -> Vec<u32> {
        let Some(target) = self.targets[t.0 as usize][t.1] else {
            return Vec::new();
        };
        let d = self.domain(t.0, target);
        active
            .iter()
            .copied()
            .filter(|&x| self.contains(d, x))
            .collect()
    }

    /// The states from just below `d` down to `r`, outermost first.
    fn path(&self, d: u32, r: u32) -> Vec<u32> {
        let mut path = vec![r];
        let mut cur = r;
        while let Some(p) = self.parent[cur as usize] {
            if p == d {
                break;
            }
            path.push(p);
            cur = p;
        }
        path.reverse();
        path
    }

    /// `entrySet(t)`, for a transition with a target, history states included, with the
    /// history records of `cfg` (as they are after the microstep recorded).
    fn entry_set(&self, cfg: &Config, t: T, out: &mut Vec<u32>) {
        let Some(target) = self.targets[t.0 as usize][t.1] else {
            return;
        };
        let d = self.domain(t.0, target);
        if !self.kind(target).is_history() {
            let path = self.path(d, target);
            out.extend(&path);
            self.default_entry(target, out);
            for &p in &path[..path.len() - 1] {
                if self.kind(p) == StateKind::Parallel {
                    for &c in &self.children[p as usize] {
                        if !path.contains(&c) {
                            out.push(c);
                            self.default_entry(c, out);
                        }
                    }
                }
            }
            return;
        }
        let p = self.parent[target as usize].unwrap_or(0);
        let targets: Vec<u32> = match cfg.history.iter().find(|(h, _)| *h == target) {
            Some((_, r)) => r.clone(),
            None if self.kind(p) == StateKind::Compound => {
                self.initial[p as usize].into_iter().collect()
            }
            None => self.children[p as usize].clone(),
        };
        let mut set = Vec::new();
        for r in targets {
            set.extend(self.path(d, r));
            self.default_entry(r, &mut set);
        }
        set.sort_unstable();
        set.dedup();
        let mut more = Vec::new();
        for &q in &set {
            if self.kind(q) == StateKind::Parallel {
                for &c in &self.children[q as usize] {
                    let covered = set
                        .iter()
                        .chain(&more)
                        .any(|&x| x == c || self.contains(c, x));
                    if !covered {
                        more.push(c);
                        self.default_entry(c, &mut more);
                    }
                }
            }
        }
        out.extend(set);
        out.extend(more);
    }

    /// Whether `s` is in a final state, in `active`.
    fn in_final(&self, active: &[u32], s: u32) -> bool {
        if active.binary_search(&s).is_err() {
            return false;
        }
        match self.kind(s) {
            StateKind::Final => true,
            StateKind::Compound => self.children[s as usize]
                .iter()
                .any(|&c| self.kind(c) == StateKind::Final && active.binary_search(&c).is_ok()),
            StateKind::Parallel => self.children[s as usize]
                .iter()
                .all(|&c| self.in_final(active, c)),
            _ => false,
        }
    }

    fn set_spawned(cfg: &mut Config, collections: &[usize]) {
        for &k in collections {
            cfg.spawned[k] = true;
        }
    }

    /// Forgets that `s`'s work was consumed: it is left, or entered again and restarted.
    fn restart(cfg: &mut Config, s: u32) {
        cfg.consumed.retain(|&(x, _)| x != s);
    }

    /// `enterStates`: enters `list` (in document order); stops at a top-level final state,
    /// which ends the run.
    fn enter_states(&self, cfg: &mut Config, list: &[u32]) {
        for &s in list {
            if let Err(at) = cfg.active.binary_search(&s) {
                cfg.active.insert(at, s);
            }
            Self::set_spawned(cfg, &self.spawn_entry[s as usize]);
            Self::restart(cfg, s);
            if self.kind(s) == StateKind::Final {
                let parent = self.parent[s as usize].unwrap_or(0);
                if parent == 0 {
                    cfg.end = Some(s);
                    return;
                }
                self.complete(cfg, s);
            }
        }
    }

    /// `complete(f)`: queues the completions a final state's entry causes.
    fn complete(&self, cfg: &mut Config, f: u32) {
        let mut s = f;
        let parent = self.parent[f as usize].unwrap_or(0);
        if self.kind(parent) == StateKind::Compound {
            cfg.queue.push(parent);
            s = parent;
        }
        while let Some(p) = self.parent[s as usize] {
            if self.kind(p) != StateKind::Parallel || !self.in_final(&cfg.active, p) {
                break;
            }
            s = p;
            cfg.queue.push(s);
        }
    }

    /// `microstep(T)`, on a running configuration.
    fn microstep(&self, cfg: &mut Config, ts: &[T]) {
        let mut exits: Vec<u32> = ts
            .iter()
            .flat_map(|&t| self.exit_set(&cfg.active, t))
            .collect();
        exits.sort_unstable();
        exits.dedup();
        // recordHistory, before anything is exited.
        for &s in &exits {
            for &h in &self.histories[s as usize] {
                let record: Vec<u32> = if self.kind(h) == StateKind::History {
                    self.children[s as usize]
                        .iter()
                        .copied()
                        .filter(|c| cfg.active.binary_search(c).is_ok())
                        .collect()
                } else {
                    cfg.active
                        .iter()
                        .copied()
                        .filter(|&x| {
                            self.contains(s, x)
                                && matches!(self.kind(x), StateKind::Atomic | StateKind::Final)
                        })
                        .collect()
                };
                match cfg.history.binary_search_by_key(&h, |(k, _)| *k) {
                    Ok(at) => cfg.history[at].1 = record,
                    Err(at) => cfg.history.insert(at, (h, record)),
                }
            }
        }
        for &s in exits.iter().rev() {
            Self::restart(cfg, s);
            Self::set_spawned(cfg, &self.spawn_exit[s as usize]);
            if let Ok(at) = cfg.active.binary_search(&s) {
                cfg.active.remove(at);
            }
            cfg.queue.retain(|&q| q != s);
        }
        for &t in ts {
            Self::set_spawned(cfg, &self.spawn_effects[t.0 as usize][t.1]);
        }
        let mut entries = Vec::new();
        for &t in ts {
            self.entry_set(cfg, t, &mut entries);
        }
        entries.sort_unstable();
        entries.dedup();
        self.enter_states(cfg, &entries);
    }

    /// A configuration as a node: one that has finished keeps only where it ended, and one
    /// with nothing to settle (no `always` among its active states, no `done` among its
    /// queued completions) is stable at once, with no edge for its settling.
    fn normalize(&self, mut cfg: Config) -> Config {
        if cfg.end.is_some() {
            cfg.history.clear();
            cfg.consumed.clear();
            cfg.spawned.iter_mut().for_each(|f| *f = false);
            cfg.queue.clear();
            cfg.settling = false;
            return cfg;
        }
        let always = cfg.active.iter().any(|&s| self.has_always[s as usize]);
        let done = cfg.queue.iter().any(|&q| {
            self.states[q as usize]
                .transitions
                .iter()
                .any(|t| matches!(t.trigger, Trigger::Done))
        });
        cfg.settling = always || done;
        if !cfg.settling {
            cfg.queue.clear();
        }
        cfg
    }

    // -----------------------------------------------------------------------------------------
    // Selection.

    /// The candidates of state `s` for a search, in document order.
    fn candidates(&self, s: u32, c: &Cand<'_>) -> Vec<usize> {
        let ts = &self.states[s as usize].transitions;
        let pick = |f: &dyn Fn(&Trigger) -> bool| -> Vec<usize> {
            ts.iter()
                .enumerate()
                .filter(|(_, t)| f(&t.trigger))
                .map(|(i, _)| i)
                .collect()
        };
        match c {
            Cand::Event(n) => {
                let on = pick(&|t| matches!(t, Trigger::Event { event } if event == n));
                if on.is_empty() {
                    pick(&|t| matches!(t, Trigger::Any))
                } else {
                    on
                }
            }
            Cand::Emitted(sender, n) => pick(&|t| match t {
                Trigger::Emitted { from, event } if event == n => match (sender, from.kind) {
                    (SenderRef::Child(st), AddressKind::Child) => {
                        from.state.as_deref() == Some(self.states[*st as usize].id.as_str())
                    }
                    (SenderRef::Refs(names), AddressKind::Ref) => {
                        names.contains(&from.name.as_str())
                    }
                    (SenderRef::Children(k), AddressKind::Children) => from.name == *k,
                    _ => false,
                },
                _ => false,
            }),
            Cand::Always => pick(&|t| matches!(t, Trigger::Always)),
            Cand::SpawnDone(k) => {
                pick(&|t| matches!(t, Trigger::SpawnDone { children } if children == k))
            }
            Cand::SpawnFail(k, e) => {
                let named = pick(
                    &|t| matches!(t, Trigger::SpawnFail { children, error } if children == k && error == e),
                );
                if named.is_empty() {
                    pick(
                        &|t| matches!(t, Trigger::SpawnFail { children, error } if children == k && error == "*"),
                    )
                } else {
                    named
                }
            }
            Cand::InvokeDone => pick(&|t| matches!(t, Trigger::InvokeDone)),
            Cand::InvokeFail(e) => {
                let named = pick(&|t| matches!(t, Trigger::InvokeFail { error } if error == e));
                if named.is_empty() {
                    pick(&|t| matches!(t, Trigger::InvokeFail { error } if error == "*"))
                } else {
                    named
                }
            }
            Cand::After(g) => ts
                .iter()
                .enumerate()
                .filter(|(_, t)| matches!(t.trigger, Trigger::After { .. }) && t.group == *g)
                .map(|(i, _)| i)
                .collect(),
            Cand::Done => pick(&|t| matches!(t, Trigger::Done)),
        }
    }

    /// The outcomes of trying `states` in order, each one's candidates in document order: a
    /// guarded candidate is taken (its guard assumed to hold) or skipped (assumed to fail), an
    /// unguarded one is taken. The last outcome takes nothing, if every candidate can be
    /// skipped.
    fn search(&self, states: impl Iterator<Item = u32>, c: &Cand<'_>) -> Vec<Choice> {
        let mut out = Vec::new();
        let mut assumed = Vec::new();
        for s in states {
            for i in self.candidates(s, c) {
                let t = (s, i);
                if self.transition(t).guard.is_some() {
                    let mut taken = assumed.clone();
                    taken.push((t, true));
                    out.push(Choice {
                        taken: Some(t),
                        assumed: taken,
                    });
                    assumed.push((t, false));
                } else {
                    out.push(Choice {
                        taken: Some(t),
                        assumed,
                    });
                    return out;
                }
            }
        }
        out.push(Choice {
            taken: None,
            assumed,
        });
        out
    }

    /// `selectByWalks`, with every guard outcome: each distinct `T` (after `removeConflicts`),
    /// with the fewest guard outcomes that lead to it. `T` may be empty.
    fn select_by_walks(&self, cfg: &Config, c: &Cand<'_>) -> Vec<Selected> {
        let walks: Vec<Vec<Choice>> = cfg
            .active
            .iter()
            .copied()
            .filter(|&s| matches!(self.kind(s), StateKind::Atomic | StateKind::Final))
            .map(|leaf| {
                let mut chain = vec![leaf];
                let mut cur = leaf;
                while let Some(p) = self.parent[cur as usize] {
                    chain.push(p);
                    cur = p;
                }
                self.search(chain.into_iter(), c)
            })
            .collect();
        let mut results: IndexMap<Vec<T>, Vec<(T, bool)>> = IndexMap::new();
        let mut enabled = Vec::new();
        let mut assumed = Vec::new();
        let mut leaves = 0;
        self.combine(
            cfg,
            &walks,
            &mut enabled,
            &mut assumed,
            &mut results,
            &mut leaves,
        );
        if self.exhausted() {
            // `explore` stops after this node: nothing to gain from the rest.
            return Vec::new();
        }
        results.into_iter().collect()
    }

    /// Whether the budget of combinations has run out.
    fn exhausted(&self) -> bool {
        self.combinations.get() == 0
    }

    /// Every consistent combination of the walks' outcomes: one guard has one outcome in one
    /// selection, so two walks that search the same state see the same guards. Each
    /// combination after the first of a selection (`leaves` counts them) spends one of the
    /// budget: a selection with no choice to make costs nothing.
    fn combine(
        &self,
        cfg: &Config,
        walks: &[Vec<Choice>],
        enabled: &mut Vec<T>,
        assumed: &mut Vec<(T, bool)>,
        results: &mut IndexMap<Vec<T>, Vec<(T, bool)>>,
        leaves: &mut usize,
    ) {
        if self.exhausted() {
            return;
        }
        let Some((first, rest)) = walks.split_first() else {
            if *leaves > 0 {
                self.combinations.set(self.combinations.get() - 1);
            }
            *leaves += 1;
            let kept = self.remove_conflicts(&cfg.active, enabled);
            let mut a = assumed.clone();
            a.sort_unstable();
            a.dedup();
            match results.get_mut(&kept) {
                Some(have) if have.len() <= a.len() => {}
                Some(have) => *have = a,
                None => {
                    results.insert(kept, a);
                }
            }
            return;
        };
        for choice in first {
            let consistent = choice
                .assumed
                .iter()
                .all(|(t, v)| !assumed.iter().any(|(u, w)| u == t && w != v));
            if !consistent {
                continue;
            }
            let (n_enabled, n_assumed) = (enabled.len(), assumed.len());
            if let Some(t) = choice.taken
                && !enabled.contains(&t)
            {
                enabled.push(t);
            }
            assumed.extend(choice.assumed.iter().copied());
            self.combine(cfg, rest, enabled, assumed, results, leaves);
            enabled.truncate(n_enabled);
            assumed.truncate(n_assumed);
        }
    }

    /// `removeConflicts`.
    fn remove_conflicts(&self, active: &[u32], enabled: &[T]) -> Vec<T> {
        let mut kept: Vec<(T, Vec<u32>)> = Vec::new();
        for &t1 in enabled {
            let x1 = self.exit_set(active, t1);
            let mut preempted = false;
            let mut replaced = Vec::new();
            for (k, (t2, x2)) in kept.iter().enumerate() {
                if x1.iter().any(|s| x2.contains(s)) {
                    if self.contains(t2.0, t1.0) {
                        replaced.push(k);
                    } else {
                        preempted = true;
                        break;
                    }
                }
            }
            if !preempted {
                for k in replaced.into_iter().rev() {
                    kept.remove(k);
                }
                kept.push((t1, x1));
            }
        }
        kept.into_iter().map(|(t, _)| t).collect()
    }

    // -----------------------------------------------------------------------------------------
    // Successors.

    fn successors(&self, cfg: &Config) -> Vec<Successor> {
        let mut out = Vec::new();
        if cfg.end.is_some() {
            return out;
        }
        if cfg.settling {
            self.settle(cfg, &mut out);
            return out;
        }
        // The machine's events.
        for name in self.machine.events.keys() {
            self.by_walks(
                cfg,
                Input::Event(name.clone()),
                &Cand::Event(name),
                &mut out,
            );
        }
        // Invoke outcomes, and what invoked children emit.
        for &s in &cfg.active {
            let state = self.states[s as usize];
            let Some(inv) = &state.invoke else { continue };
            if cfg.consumed.contains(&(s, INVOKE)) {
                continue;
            }
            self.one_state(
                cfg,
                s,
                INVOKE,
                Input::InvokeDone {
                    state: StateIndex(s),
                },
                &Cand::InvokeDone,
                &mut out,
            );
            let errors: &[String] = match inv.kind {
                InvokeKind::Task => self.ir.tasks.get(&inv.src).map(|t| &t.errors[..]),
                InvokeKind::Machine => self.ir.machines.get(&inv.src).map(|m| &m.errors[..]),
            }
            .unwrap_or(&[]);
            for e in errors {
                self.one_state(
                    cfg,
                    s,
                    INVOKE,
                    Input::InvokeFail {
                        state: StateIndex(s),
                        error: e.clone(),
                    },
                    &Cand::InvokeFail(e),
                    &mut out,
                );
            }
            for &event in &self.child_events[s as usize] {
                self.by_walks(
                    cfg,
                    Input::Emitted {
                        from: Sender::Child(StateIndex(s)),
                        event: event.to_string(),
                    },
                    &Cand::Emitted(SenderRef::Child(s), event),
                    &mut out,
                );
            }
        }
        // Timers.
        for &s in &cfg.active {
            for &(group, index) in &self.after_blocks[s as usize] {
                if cfg.consumed.contains(&(s, group)) {
                    continue;
                }
                self.one_state(
                    cfg,
                    s,
                    group,
                    Input::After {
                        state: StateIndex(s),
                        index,
                    },
                    &Cand::After(group),
                    &mut out,
                );
            }
        }
        // Peers' events, from every set of names that may hold the sender.
        for (event, names) in &self.ref_events {
            // Every nonempty set of names: `explore` refuses more than `MAX_SENDER_NAMES`.
            let n = names.len();
            for mask in 1u32..(1 << n) {
                let held: Vec<&str> = (0..n)
                    .filter(|i| mask & (1 << i) != 0)
                    .map(|i| names[i])
                    .collect();
                self.by_walks(
                    cfg,
                    Input::Emitted {
                        from: Sender::Refs(held.iter().map(|s| s.to_string()).collect()),
                        event: event.to_string(),
                    },
                    &Cand::Emitted(SenderRef::Refs(&held), event),
                    &mut out,
                );
            }
        }
        // Spawned children: what they emit, and how they end.
        for (k, &name) in self.collections.iter().enumerate() {
            if !cfg.spawned[k] {
                continue;
            }
            for &event in &self.children_events[k] {
                self.by_walks(
                    cfg,
                    Input::Emitted {
                        from: Sender::Children(name.to_string()),
                        event: event.to_string(),
                    },
                    &Cand::Emitted(SenderRef::Children(name), event),
                    &mut out,
                );
            }
            self.by_walks(
                cfg,
                Input::SpawnDone {
                    children: name.to_string(),
                },
                &Cand::SpawnDone(name),
                &mut out,
            );
            for e in self.collection_errors[k] {
                self.by_walks(
                    cfg,
                    Input::SpawnFail {
                        children: name.to_string(),
                        error: e.clone(),
                    },
                    &Cand::SpawnFail(name, e),
                    &mut out,
                );
            }
        }
        out
    }

    /// The edges of an input selected by walks: one per distinct nonempty `T`. An input that
    /// enables nothing changes nothing and has no edge.
    fn by_walks(&self, cfg: &Config, input: Input, c: &Cand<'_>, out: &mut Vec<Successor>) {
        // An input that no active state has a candidate for enables nothing: skip the search.
        if !cfg.active.iter().any(|&s| self.has_candidate(s, c)) {
            return;
        }
        let extra = self.input_assumption(cfg, &input);
        for (ts, assumed) in self.select_by_walks(cfg, c) {
            if ts.is_empty() {
                self.idle.borrow_mut().push(input.clone());
                continue;
            }
            let mut next = cfg.clone();
            self.microstep(&mut next, &ts);
            out.push(Successor::new(
                input.clone(),
                &ts,
                &assumed,
                extra.clone(),
                self.normalize(next),
            ));
        }
    }

    /// Whether the timer of the `after` block at `a` certainly expires before the one at `b`,
    /// while both run: they are blocks of one state, so `startWork` starts them together, `a`
    /// first (it is written first), and both delays are literals, `a`'s the shorter. Its due
    /// time is then earlier (`spec/execution.md`, "Time").
    fn expires_first(&self, a: T, b: T) -> bool {
        let delay = |t: T| match &self.transition(t).trigger {
            Trigger::After { delay } => match delay.kind {
                ir::ExprKind::Lit(ir::Literal::Duration(ms)) => Some(ms),
                _ => None,
            },
            _ => None,
        };
        a.0 == b.0 && a.1 < b.1 && matches!((delay(a), delay(b)), (Some(x), Some(y)) if x < y)
    }

    /// Whether state `s` has a candidate for the search `c`.
    fn has_candidate(&self, s: u32, c: &Cand<'_>) -> bool {
        match c {
            Cand::Event(n) => self.on_any[s as usize] || self.on_events[s as usize].contains(n),
            _ => !self.candidates(s, c).is_empty(),
        }
    }

    /// What an input assumes of things other than guards: that a timer expires before the
    /// others running, or that another machine does what the input says.
    fn input_assumption(&self, cfg: &Config, input: &Input) -> Option<Assumption> {
        match input {
            Input::InvokeDone { state } | Input::InvokeFail { state, .. } => {
                let invoke = self.states[state.index()].invoke.as_ref()?;
                (invoke.kind == InvokeKind::Machine).then(|| Assumption::Machine(input.clone()))
            }
            Input::Emitted { .. } | Input::SpawnDone { .. } | Input::SpawnFail { .. } => {
                Some(Assumption::Machine(input.clone()))
            }
            Input::After { state, index } => {
                let others = cfg.active.iter().any(|&s| {
                    self.after_blocks[s as usize].iter().any(|&(group, i)| {
                        (s, i) != (state.0, *index)
                            && !cfg.consumed.contains(&(s, group))
                            && !self.expires_first((state.0, *index), (s, i))
                    })
                });
                others.then_some(Assumption::TimerFirst {
                    state: *state,
                    index: *index,
                })
            }
            Input::Event(_) | Input::Always | Input::Done { .. } | Input::Settled => None,
        }
    }

    /// The edges of an invoke outcome or a timer expiry: it consumes its work, then takes the
    /// first candidate of the one state whose guard holds, if any.
    fn one_state(
        &self,
        cfg: &Config,
        s: u32,
        slot: u32,
        input: Input,
        c: &Cand<'_>,
        out: &mut Vec<Successor>,
    ) {
        let mut consumed = cfg.clone();
        let at = consumed
            .consumed
            .binary_search(&(s, slot))
            .unwrap_or_else(|e| e);
        consumed.consumed.insert(at, (s, slot));
        let extra = self.input_assumption(cfg, &input);
        for choice in self.search(std::iter::once(s), c) {
            let mut next = consumed.clone();
            let ts: Vec<T> = choice.taken.into_iter().collect();
            if !ts.is_empty() {
                self.microstep(&mut next, &ts);
                next = self.normalize(next);
            }
            out.push(Successor::new(
                input.clone(),
                &ts,
                &choice.assumed,
                extra.clone(),
                next,
            ));
        }
    }

    /// The edges of a settling node: the enabled `always` transitions, or, when none is, the
    /// first queued completion whose `done` is taken, or, when none is, the end of settling.
    fn settle(&self, cfg: &Config, out: &mut Vec<Successor>) {
        for (ts, assumed) in self.select_by_walks(cfg, &Cand::Always) {
            if !ts.is_empty() {
                let mut next = cfg.clone();
                self.microstep(&mut next, &ts);
                out.push(Successor::new(
                    Input::Always,
                    &ts,
                    &assumed,
                    None,
                    self.normalize(next),
                ));
                continue;
            }
            // No `always` is taken: the completions, in queue order.
            let mut assumed = assumed;
            let mut queue = cfg.queue.clone();
            loop {
                if queue.is_empty() {
                    let mut next = cfg.clone();
                    next.queue.clear();
                    next.settling = false;
                    out.push(Successor::new(Input::Settled, &[], &assumed, None, next));
                    break;
                }
                let p = queue.remove(0);
                let mut none = None;
                for choice in self.search(std::iter::once(p), &Cand::Done) {
                    let mut a = assumed.clone();
                    a.extend(choice.assumed.iter().copied());
                    match choice.taken {
                        Some(t) => {
                            let mut next = cfg.clone();
                            next.queue = queue.clone();
                            self.microstep(&mut next, &[t]);
                            out.push(Successor::new(
                                Input::Done {
                                    state: StateIndex(p),
                                },
                                &[t],
                                &a,
                                None,
                                self.normalize(next),
                            ));
                        }
                        None => none = Some(a),
                    }
                }
                // Discarded: every `done` guard of `p` failed (or it has none). With an
                // unguarded `done` it cannot be, and settling goes no further this way.
                match none {
                    Some(a) => assumed = a,
                    None => break,
                }
            }
        }
    }
}

/// The candidates a search tries.
enum Cand<'s> {
    Event(&'s str),
    Emitted(SenderRef<'s>, &'s str),
    Always,
    SpawnDone(&'s str),
    SpawnFail(&'s str, &'s str),
    InvokeDone,
    InvokeFail(&'s str),
    After(u32),
    Done,
}

/// [`Sender`], borrowed.
enum SenderRef<'s> {
    Child(u32),
    Refs(&'s [&'s str]),
    Children(&'s str),
}

/// One outcome of a search: the transition taken, if any, and the guard outcomes assumed.
struct Choice {
    taken: Option<T>,
    assumed: Vec<(T, bool)>,
}

/// An edge before its target is interned.
struct Successor {
    input: Input,
    transitions: Vec<TransitionRef>,
    assumptions: Vec<Assumption>,
    config: Config,
}

impl Successor {
    fn new(
        input: Input,
        ts: &[T],
        assumed: &[(T, bool)],
        extra: Option<Assumption>,
        config: Config,
    ) -> Successor {
        let tref = |&(s, index): &T| TransitionRef {
            state: StateIndex(s),
            index,
        };
        let mut assumptions: Vec<Assumption> = assumed
            .iter()
            .map(|(t, holds)| Assumption::Guard {
                transition: tref(t),
                holds: *holds,
            })
            .chain(extra)
            .collect();
        assumptions.sort_unstable();
        assumptions.dedup();
        Successor {
            input,
            transitions: ts.iter().map(tref).collect(),
            assumptions,
            config,
        }
    }
}

/// A node's configuration while exploring.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Config {
    /// The active states, sorted (document order).
    active: Vec<u32>,
    /// History records, sorted by history state.
    history: Vec<(u32, Vec<u32>)>,
    /// The consumed work of active states: (state, [`INVOKE`] or the `after` block's group),
    /// sorted.
    consumed: Vec<(u32, u32)>,
    /// Per collection, whether a `spawn` into it has run.
    spawned: Vec<bool>,
    /// The top-level final state the run finished in.
    end: Option<u32>,
    /// Whether the run is settling.
    settling: bool,
    /// The completion queue, while settling.
    queue: Vec<u32>,
}

impl Config {
    fn new(collections: usize) -> Config {
        Config {
            active: Vec::new(),
            history: Vec::new(),
            consumed: Vec::new(),
            spawned: vec![false; collections],
            end: None,
            settling: true,
            queue: Vec::new(),
        }
    }
}

struct Builder {
    /// The nodes by the hash of their configuration, which is kept once, in `configs`.
    ids: HashMap<u64, Vec<NodeId>>,
    hasher: RandomState,
    configs: Vec<Config>,
    edges: Vec<Edge>,
    max_nodes: usize,
}

impl Builder {
    fn intern(&mut self, cfg: Config) -> Result<NodeId, TooLarge> {
        let hash = self.hasher.hash_one(&cfg);
        if let Some(ids) = self.ids.get(&hash)
            && let Some(&id) = ids.iter().find(|id| self.configs[id.index()] == cfg)
        {
            return Ok(id);
        }
        if self.configs.len() >= self.max_nodes {
            return Err(TooLarge::Nodes {
                reached: self.configs.len() + 1,
            });
        }
        let id = NodeId(self.configs.len() as u32);
        self.ids.entry(hash).or_default().push(id);
        self.configs.push(cfg);
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    /// The graph of machine `M` in `src`.
    fn graph(src: &str) -> Graph {
        let out = crate::compile_source("m.road", src, &Default::default());
        assert!(!out.has_errors(), "{}", out.render());
        let ir = out.ir.unwrap();
        explore(&ir, &ir.machines["M"], DEFAULT_MAX_NODES).unwrap()
    }

    /// A node's active leaves, by their ids below the machine, `+` for a finished run.
    fn leaves(g: &Graph, n: NodeId) -> String {
        let ids: Vec<&str> = g.active(n).iter().map(|&s| g.state_id(s)).collect();
        let mut out: Vec<&str> = ids
            .iter()
            .filter(|id| !ids.iter().any(|o| o.starts_with(&format!("{id}."))))
            .map(|id| id.strip_prefix("M.").unwrap_or(id))
            .collect();
        out.sort();
        let mut s = out.join(" ");
        if g.is_final(n) {
            s.push('+');
        }
        s
    }

    /// The leaves of every stable node (finished ones included).
    fn stable(g: &Graph) -> BTreeSet<String> {
        g.nodes()
            .filter(|&n| !g.is_settling(n))
            .map(|n| leaves(g, n))
            .collect()
    }

    fn edges(g: &Graph) -> impl Iterator<Item = &Edge> {
        (0..g.edge_count() as u32).map(|e| g.edge(EdgeId(e)))
    }

    /// Every edge, as `from --input--> to`, with `?` when it assumes a guard outcome.
    fn steps(g: &Graph) -> BTreeSet<String> {
        edges(g)
            .map(|e| {
                let q = if e.assumptions.is_empty() { "" } else { "?" };
                format!(
                    "{} --{}{q}--> {}",
                    leaves(g, e.from),
                    g.describe(&e.input),
                    leaves(g, e.to)
                )
            })
            .collect()
    }

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_regions_of_a_parallel_state_take_one_event_together() {
        let g = graph(
            "machine M() {\n  event Go\n  initial P\n  parallel state P {\n    state L {\n      initial L1\n      state L1 { on Go -> L2 }\n      state L2 {}\n    }\n    state R {\n      initial R1\n      state R1 { on Go -> R2 }\n      state R2 {}\n    }\n  }\n}\n",
        );
        // Both regions move on `Go`: no configuration has only one of them moved.
        assert_eq!(stable(&g), set(&["P.L.L1 P.R.R1", "P.L.L2 P.R.R2"]));
        let go = g.edge(g.outgoing(g.start())[0]);
        assert_eq!(go.transitions.len(), 2);
        assert!(go.assumptions.is_empty());
    }

    #[test]
    fn a_guarded_region_may_move_alone() {
        let g = graph(
            "fn ok() -> bool\nmachine M() {\n  event Go\n  initial P\n  parallel state P {\n    state L {\n      initial L1\n      state L1 { on Go if ok() -> L2 }\n      state L2 {}\n    }\n    state R {\n      initial R1\n      state R1 { on Go -> R2 }\n      state R2 {}\n    }\n  }\n}\n",
        );
        assert_eq!(
            stable(&g),
            set(&["P.L.L1 P.R.R1", "P.L.L1 P.R.R2", "P.L.L2 P.R.R2"])
        );
    }

    #[test]
    fn a_guarded_branch_may_be_taken_or_skipped_and_else_is_its_alternative() {
        let g = graph(
            "fn ok() -> bool\nmachine M() {\n  event Go\n  initial A\n  state A {\n    on Go {\n      if ok() -> B\n      else    -> C\n    }\n  }\n  state B {}\n  state C {}\n}\n",
        );
        assert_eq!(steps(&g), set(&["A --Go?--> B", "A --Go?--> C"]));
        let assumed: Vec<Vec<(usize, bool)>> = g
            .outgoing(g.start())
            .iter()
            .map(|&e| {
                g.edge(e)
                    .assumptions
                    .iter()
                    .map(|x| match x {
                        Assumption::Guard { transition, holds } => (transition.index, *holds),
                        other => panic!("{other:?}"),
                    })
                    .collect()
            })
            .collect();
        // To B the guard holds; to C, the `else`, it fails.
        assert_eq!(assumed, vec![vec![(0, true)], vec![(0, false)]]);
    }

    #[test]
    fn an_ancestor_takes_the_event_when_its_childs_guard_fails() {
        let g = graph(
            "fn ok() -> bool\nmachine M() {\n  event Go\n  initial W\n  state W {\n    initial A\n    on Go -> Out\n    state A { on Go if ok() -> B }\n    state B {}\n  }\n  state Out {}\n}\n",
        );
        let s = steps(&g);
        assert!(s.contains("W.A --Go?--> W.B"));
        assert!(s.contains("W.A --Go?--> Out"));
        // From B nothing guards the way out.
        assert!(s.contains("W.B --Go--> Out"));
    }

    #[test]
    fn on_star_is_tried_only_in_a_state_with_no_on_for_the_event() {
        let g = graph(
            "fn ok() -> bool\nmachine M() {\n  event Go\n  event Other\n  initial A\n  state A {\n    on Go if ok() -> B\n    on *          -> C\n  }\n  state B {}\n  state C {}\n}\n",
        );
        let s = steps(&g);
        assert!(s.contains("A --Go?--> B"));
        assert!(s.contains("A --Other--> C"));
        // A failed guard on `Go` does not fall back to `on *`.
        assert!(!s.contains("A --Go?--> C") && !s.contains("A --Go--> C"));
    }

    #[test]
    fn shallow_history_enters_what_it_recorded_or_the_default_entry() {
        let g = graph(
            "machine M() {\n  event Go\n  event Out\n  event Back\n  event Reset\n  initial W\n  state W {\n    initial A\n    history H\n    state A { on Go -> B }\n    state B {\n      initial B1\n      state B1 { on Go -> B2 }\n      state B2 { on Reset -> H }\n    }\n    on Out -> Away\n  }\n  state Away { on Back -> W.H }\n}\n",
        );
        let s = steps(&g);
        // Left from B.B2: the shallow history state restores B, from its start.
        assert!(s.contains("W.B.B2 --Out--> Away"));
        assert!(s.contains("Away --Back--> W.B.B1"));
        // Left from A: back to A.
        assert!(s.contains("Away --Back--> W.A"));
        // From inside W, never left yet: W's default entry.
        let reset: Vec<&Edge> = edges(&g)
            .filter(|e| e.input == Input::Event("Reset".into()))
            .collect();
        assert!(reset.iter().any(|e| leaves(&g, e.to) == "W.A"));
        // Once W was left from B, a reset from inside goes back to B, which W recorded.
        assert!(reset.iter().any(|e| leaves(&g, e.to) == "W.B.B1"));
        // The record is part of the node: `Away` is two nodes, one per record.
        let away = g.nodes().filter(|&n| leaves(&g, n) == "Away").count();
        assert_eq!(away, 2);
    }

    #[test]
    fn deep_history_restores_the_leaves_or_with_no_record_the_default_entry() {
        let g = graph(
            "machine M() {\n  event Go\n  event Out\n  event Back\n  initial Start\n  state Start { on Back -> W.D }\n  state W {\n    initial A\n    deep history D\n    state A { on Go -> B }\n    state B {\n      initial B1\n      state B1 { on Go -> B2 }\n      state B2 {}\n    }\n    on Out -> Start\n  }\n}\n",
        );
        let s = steps(&g);
        // No record yet: W's default entry.
        assert!(s.contains("Start --Back--> W.A"));
        // Recorded in B2: B2 itself, not B's start.
        assert!(s.contains("W.B.B2 --Out--> Start"));
        assert!(s.contains("Start --Back--> W.B.B2"));
        // Left from B1 it restores B1: one way back per leaf W can be left from.
        let back: BTreeSet<String> = s
            .iter()
            .filter(|x| x.starts_with("Start --Back-->"))
            .cloned()
            .collect();
        assert_eq!(
            back,
            set(&[
                "Start --Back--> W.A",
                "Start --Back--> W.B.B1",
                "Start --Back--> W.B.B2"
            ])
        );
    }

    #[test]
    fn a_compound_state_completes_when_a_final_child_is_entered() {
        let g = graph(
            "fn ok() -> bool\nmachine M() {\n  event Go\n  initial W\n  state W {\n    initial A\n    state A { on Go -> F }\n    final state F {}\n    done if ok() -> Next\n  }\n  state Next {}\n}\n",
        );
        let s = steps(&g);
        // `Go` enters F; settling then takes `done`, or discards it when its guard fails.
        assert!(s.contains("W.A --Go--> W.F"));
        assert!(s.contains("W.F --done of M.W?--> Next"));
        assert!(s.contains("W.F --settled?--> W.F"));
        let settling: Vec<NodeId> = g.nodes().filter(|&n| g.is_settling(n)).collect();
        assert_eq!(settling.len(), 1);
        assert_eq!(leaves(&g, settling[0]), "W.F");
        for &e in g.outgoing(settling[0]) {
            assert!(g.edge(e).settling);
        }
    }

    #[test]
    fn a_parallel_state_completes_when_every_region_is_in_a_final_state() {
        let g = graph(
            "machine M() {\n  event Left\n  event Right\n  initial P\n  parallel state P {\n    state L {\n      initial L1\n      state L1 { on Left -> L2 }\n      final state L2 {}\n    }\n    state R {\n      initial R1\n      state R1 { on Right -> R2 }\n      final state R2 {}\n    }\n    done -> Over\n  }\n  final state Over {}\n}\n",
        );
        let s = steps(&g);
        assert!(s.contains("P.L.L2 P.R.R1 --Right--> P.L.L2 P.R.R2"));
        assert!(s.contains("P.L.L2 P.R.R2 --done of M.P--> Over+"));
        // One region in its final state does not complete P.
        assert!(stable(&g).contains("P.L.L2 P.R.R1"));
        assert!(!s.iter().any(|x| x.starts_with("P.L.L2 P.R.R1 --done")));
    }

    #[test]
    fn always_is_taken_before_done() {
        let g = graph(
            "fn ok() -> bool\nmachine M() {\n  event Go\n  initial W\n  state W {\n    initial A\n    state A { on Go -> F }\n    final state F {}\n    always if ok() -> Other\n    done -> Next\n  }\n  state Next {}\n  state Other {}\n}\n",
        );
        let s = steps(&g);
        assert!(s.contains("W.F --always?--> Other"));
        assert!(s.contains("W.F --done of M.W?--> Next"));
        // `done` is taken only when the `always` guard fails, and is not discarded.
        let done = edges(&g)
            .find(|e| matches!(e.input, Input::Done { .. }))
            .unwrap();
        assert_eq!(done.assumptions.len(), 1);
        assert!(matches!(
            done.assumptions[0],
            Assumption::Guard { holds: false, .. }
        ));
        assert!(!s.iter().any(|x| x.starts_with("W.F --settled")));
    }

    #[test]
    fn a_timer_is_consumed_when_it_expires_and_restarts_on_entry() {
        let g = graph(
            "fn ok() -> bool\nmachine M() {\n  event Again\n  initial A\n  state A {\n    after 1s if ok() -> B\n    on Again -> A\n  }\n  state B {}\n}\n",
        );
        let a = g.start();
        let after: Vec<&Edge> = g
            .outgoing(a)
            .iter()
            .map(|&e| g.edge(e))
            .filter(|e| matches!(e.input, Input::After { .. }))
            .collect();
        assert_eq!(after.len(), 2);
        // The expiry whose guard failed consumed the timer: no expiry from there.
        let failed = after.iter().find(|e| e.transitions.is_empty()).unwrap();
        assert_ne!(failed.to, a);
        let expired = failed.to;
        assert_eq!(g.active(expired), g.active(a));
        assert!(
            g.outgoing(expired)
                .iter()
                .all(|&e| !matches!(g.edge(e).input, Input::After { .. }))
        );
        // Entering A again restarts it.
        let again = g
            .outgoing(expired)
            .iter()
            .map(|&e| g.edge(e))
            .find(|e| e.input == Input::Event("Again".into()))
            .unwrap();
        assert_eq!(again.to, a);
    }

    #[test]
    fn an_invoke_has_an_outcome_for_done_and_each_declared_error_and_is_consumed() {
        let g = graph(
            "fn ok() -> bool\nerror Busy\nerror Gone\ntask work() -> int ! Busy | Gone\nmachine M() {\n  event Poke\n  initial A\n  state A {\n    invoke work() {\n      done      -> B\n      fail Busy -> C\n      fail Gone if ok() -> C\n    }\n    on Poke -> A\n  }\n  state B {}\n  state C {}\n}\n",
        );
        let s = steps(&g);
        assert!(s.contains("A --done of the invoke in M.A--> B"));
        assert!(s.contains("A --fail Busy of the invoke in M.A--> C"));
        // `Gone` whose guard fails takes no transition: it only consumes the invoke, and no
        // outcome comes after it until A is entered again.
        let gone = edges(&g)
            .find(|e| {
                matches!(&e.input, Input::InvokeFail { error, .. } if error == "Gone")
                    && e.transitions.is_empty()
            })
            .unwrap();
        assert_eq!(gone.assumptions.len(), 1);
        assert!(gone.transitions.is_empty());
        assert!(
            g.outgoing(gone.to)
                .iter()
                .all(|&e| matches!(g.edge(e).input, Input::Event(_)))
        );
    }

    #[test]
    fn an_invoked_child_emits_while_its_invoke_runs() {
        let g = graph(
            "machine Kid() {\n  emits Ping\n  initial A\n  state A { entry emit Ping }\n}\nmachine M() {\n  initial W\n  state W {\n    invoke Kid() as kid {\n      done -> Gone\n    }\n    on kid.Ping -> W\n  }\n  state Gone {}\n}\n",
        );
        let s = steps(&g);
        assert!(s.contains("W --Ping from the child of M.W?--> W"));
        assert!(s.contains("W --done of the invoke in M.W?--> Gone"));
        assert!(!s.iter().any(|x| x.starts_with("Gone ")));
    }

    #[test]
    fn spawned_childrens_outcomes_arrive_only_after_a_spawn() {
        let g = graph(
            "error Lost\nmachine Kid() -> int ! Lost {\n  initial A\n  state A { after 1s -> D }\n  final state D { output 1 }\n}\nmachine M() {\n  event Go\n  children kids: Kid by string\n  initial A\n  state A {\n    on Go spawn kids[\"a\"] = Kid()\n    on kids.done -> B\n    on kids.fail Lost -> C\n  }\n  state B {}\n  state C {}\n}\n",
        );
        let start = g.start();
        let inputs: Vec<String> = g
            .outgoing(start)
            .iter()
            .map(|&e| g.describe(&g.edge(e).input))
            .collect();
        assert_eq!(inputs, vec!["Go"]);
        // `Go` is internal: the same active states, but a node where children may run.
        let go = g.edge(g.outgoing(start)[0]);
        assert_ne!(go.to, start);
        assert_eq!(g.active(go.to), g.active(start));
        let s = steps(&g);
        assert!(s.contains("A --kids.done?--> B"));
        assert!(s.contains("A --kids.fail Lost?--> C"));
    }

    #[test]
    fn a_targetless_transition_is_a_step_to_the_same_configuration() {
        let g = graph(
            "action log()\nmachine M() {\n  event Tick\n  initial A\n  state A { on Tick do log() }\n}\n",
        );
        let e = g.edge(g.outgoing(g.start())[0]);
        assert_eq!((e.from, e.to), (g.start(), g.start()));
        assert_eq!(e.transitions.len(), 1);
    }

    #[test]
    fn a_transition_in_the_machine_body_leaves_whatever_is_active() {
        let g = graph(
            "machine M() {\n  event Go\n  event Stop\n  initial A\n  on Stop -> Out\n  state A { on Go -> B }\n  state B {}\n  final state Out {}\n}\n",
        );
        let s = steps(&g);
        assert!(s.contains("A --Stop--> Out+"));
        assert!(s.contains("B --Stop--> Out+"));
    }

    #[test]
    fn a_top_level_final_state_ends_the_run() {
        let g = graph(
            "machine M() -> string {\n  event Go\n  initial A\n  state A { on Go -> Done }\n  final state Done { output \"ok\" }\n}\n",
        );
        let done: Vec<NodeId> = g.nodes().filter(|&n| g.is_final(n)).collect();
        assert_eq!(done.len(), 1);
        assert!(g.outgoing(done[0]).is_empty());
        assert_eq!(g.final_state(done[0]), g.state_index("M.Done"));
        assert!(g.is_active(done[0], g.state_index("M.Done").unwrap()));
    }

    #[test]
    fn paths_prefer_fewer_edges_or_fewer_assumptions() {
        // Straight to C relies on a guard; round through B does not.
        let g = graph(
            "fn ok() -> bool\nmachine M() {\n  event Go\n  event Next\n  initial A\n  state A {\n    on Go if ok() -> C\n    on Next -> B\n  }\n  state B { on Next -> C }\n  state C {}\n}\n",
        );
        let c = g.state_index("M.C").unwrap();
        let short = g.shortest_path(g.start(), |n| g.is_active(n, c)).unwrap();
        assert_eq!(short.len(), 1);
        let sure = g
            .fewest_assumptions_path(g.start(), |n| g.is_active(n, c))
            .unwrap();
        assert_eq!(sure.len(), 2);
        assert!(sure.iter().all(|&e| g.edge(e).assumptions.is_empty()));
        assert_eq!(g.shortest_path(g.start(), |_| true), Some(vec![]));
        assert_eq!(g.shortest_path(g.start(), |_| false), None);
    }

    /// The assumptions of the edges for `input` (as [`Graph::describe`] gives it), each edge's
    /// as one list, in edge order.
    fn assumed_by(g: &Graph, input: &str) -> Vec<Vec<Assumption>> {
        edges(g)
            .filter(|e| g.describe(&e.input) == input)
            .map(|e| e.assumptions.clone())
            .collect()
    }

    #[test]
    fn a_timer_assumes_it_expires_first_unless_it_must() {
        // The 1 s timer is written first and is the shorter: it expires first whatever
        // happens. The 5 s one expires only if it beats it, which no run does.
        let g = graph(
            "machine M() {\n  initial A\n  state A {\n    after 1s -> B\n    after 5s -> C\n  }\n  state B { after 1s -> A }\n  final state C {}\n}\n",
        );
        let a = g.state_index("M.A").unwrap();
        assert_eq!(assumed_by(&g, "the after at #0 of M.A"), vec![vec![]]);
        assert_eq!(
            assumed_by(&g, "the after at #1 of M.A"),
            vec![vec![Assumption::TimerFirst { state: a, index: 1 }]]
        );
        // The only timer running.
        assert_eq!(assumed_by(&g, "the after at #0 of M.B"), vec![vec![]]);
        // Written the other way round, or with a delay that is not a literal, or in another
        // state, neither is certain to come first.
        for src in [
            "machine M() {\n  initial A\n  state A {\n    after 5s -> C\n    after 1s -> B\n  }\n  state B {}\n  final state C {}\n}\n",
            "fn d() -> duration\nmachine M() {\n  initial A\n  state A {\n    after d() -> B\n    after 5s -> C\n  }\n  state B {}\n  final state C {}\n}\n",
            "machine M() {\n  initial W\n  state W {\n    initial A\n    after 5s -> C\n    state A { after 1s -> B }\n    state B {}\n  }\n  final state C {}\n}\n",
        ] {
            let g = graph(src);
            let timers: Vec<&Edge> = edges(&g)
                .filter(|e| matches!(e.input, Input::After { .. }) && e.from == g.start())
                .collect();
            assert_eq!(timers.len(), 2, "{src}");
            for e in timers {
                assert!(
                    matches!(e.assumptions[..], [Assumption::TimerFirst { .. }]),
                    "{src}: {:?}",
                    e.assumptions
                );
            }
        }
    }

    #[test]
    fn what_other_machines_do_is_assumed_and_what_the_host_does_is_not() {
        let g = graph(
            "error Lost\ntask work() -> int\nmachine Kid() -> int ! Lost {\n  emits Ping\n  initial A\n  state A { entry emit Ping }\n}\nmachine M(peer: ref Kid) {\n  event Go\n  children kids: Kid by string\n  initial A\n  state A {\n    invoke Kid() as kid {\n      done      -> T\n      fail Lost -> T\n    }\n    on kid.Ping -> A\n    on peer.Ping -> A\n    on Go -> T  spawn kids[\"a\"] = Kid()\n  }\n  state T {\n    invoke work() {\n      done -> T\n    }\n    on kids.done -> A\n    on kids.fail Lost -> A\n    on kids.Ping -> A\n  }\n}\n",
        );
        // Besides the race with the other machines live at the node, which
        // `the_race_with_another_machine_is_assumed_exactly_when_one_is_live` covers.
        let own: Vec<Vec<&Assumption>> = edges(&g)
            .map(|e| {
                e.assumptions
                    .iter()
                    .filter(|a| !matches!(a, Assumption::Unpreempted { .. }))
                    .collect()
            })
            .collect();
        let mut kinds = BTreeSet::new();
        for (e, own) in edges(&g).zip(&own) {
            let d = g.describe(&e.input);
            match &e.input {
                Input::Event(_) => assert!(own.is_empty(), "{d}"),
                // A task's outcome is the host's to give.
                Input::InvokeDone { state } if g.state_id(*state) == "M.T" => {
                    assert!(own.is_empty(), "{d}")
                }
                _ => assert!(
                    matches!(own[..], [Assumption::Machine(i)] if *i == e.input),
                    "{d}: {:?}",
                    e.assumptions
                ),
            }
            kinds.insert(d);
        }
        assert_eq!(
            kinds,
            set(&[
                "Go",
                "done of the invoke in M.A",
                "fail Lost of the invoke in M.A",
                "Ping from the child of M.A",
                "peer.Ping",
                "done of the invoke in M.T",
                "kids.done",
                "kids.fail Lost",
                "kids.Ping",
            ])
        );
    }

    #[test]
    fn the_race_with_another_machine_is_assumed_exactly_when_one_is_live() {
        let child = "machine Child() {\n  initial Done\n  final state Done {}\n}\n";
        let g = graph(&format!(
            "{child}machine M() {{\n  initial A\n  state A {{\n    invoke Child() {{\n      done -> D\n    }}\n    after 1s -> T\n  }}\n  final state T {{}}\n  final state D {{}}\n}}\n"
        ));
        let a = g.state_index("M.A").unwrap();
        let race = Assumption::Unpreempted {
            senders: vec![Sender::Child(a)],
        };
        // The timer races the child; the child's own outcome does not race itself.
        assert_eq!(assumed_by(&g, "the after at #1 of M.A"), vec![vec![race]]);
        assert_eq!(
            assumed_by(&g, "done of the invoke in M.A"),
            vec![vec![Assumption::Machine(Input::InvokeDone { state: a })]]
        );
        // No other machine live: nothing to race.
        let g = graph(
            "machine M() {\n  initial A\n  state A { after 1s -> T }\n  final state T {}\n}\n",
        );
        assert_eq!(assumed_by(&g, "the after at #0 of M.A"), vec![vec![]]);
        // Two children: each one's outcome races the other.
        let g = graph(&format!(
            "{child}machine M() {{\n  initial P\n  parallel state P {{\n    state L {{\n      initial A\n      state A {{\n        invoke Child() {{\n          done -> B\n        }}\n      }}\n      final state B {{}}\n    }}\n    state R {{\n      initial A\n      state A {{\n        invoke Child() {{\n          done -> B\n        }}\n      }}\n      final state B {{}}\n    }}\n  }}\n}}\n"
        ));
        let (l, r) = (
            g.state_index("M.P.L.A").unwrap(),
            g.state_index("M.P.R.A").unwrap(),
        );
        let first = assumed_by(&g, "done of the invoke in M.P.L.A");
        assert!(first[0].contains(&Assumption::Unpreempted {
            senders: vec![Sender::Child(r)]
        }));
        // Once R's child has ended, L's races nothing.
        assert!(first.iter().any(|a| a.len() == 1));
        assert!(!first.iter().flatten().any(|a| matches!(
            a,
            Assumption::Unpreempted { senders } if senders.contains(&Sender::Child(l))
        )));
    }

    #[test]
    fn the_fewest_assumptions_come_before_the_fewest_edges() {
        // To D: one step through a guard, two through a timer that might not come first, or
        // three that assume nothing.
        let g = graph(
            "fn ok() -> bool\nmachine M() {\n  event Go\n  event Next\n  initial A\n  state A {\n    on Go if ok() -> D\n    on Next -> B\n    after 5s -> C\n    after 1s -> A\n  }\n  state B { on Next -> E }\n  state C { on Next -> D }\n  state E { on Next -> D }\n  state D {}\n}\n",
        );
        let d = g.state_index("M.D").unwrap();
        let to_d = |n: NodeId| g.is_active(n, d);
        let short = g.shortest_path(g.start(), to_d).unwrap();
        assert_eq!(short.len(), 1);
        let sure = g.fewest_assumptions_path(g.start(), to_d).unwrap();
        let inputs: Vec<String> = sure.iter().map(|&e| g.describe(&g.edge(e).input)).collect();
        assert_eq!(inputs, ["Next", "Next", "Next"]);
        assert!(sure.iter().all(|&e| g.edge(e).assumptions.is_empty()));
        // With one assumption needed whichever way, the shorter of those: the guard.
        let g = graph(
            "fn ok() -> bool\nmachine M() {\n  event Go\n  event Next\n  initial A\n  state A {\n    on Go if ok() -> D\n    after 5s -> C\n    after 1s -> A\n  }\n  state C { on Next -> D }\n  state D {}\n}\n",
        );
        let d = g.state_index("M.D").unwrap();
        let path = g
            .fewest_assumptions_path(g.start(), |n| g.is_active(n, d))
            .unwrap();
        assert_eq!(path.len(), 1);
        assert!(matches!(
            g.edge(path[0]).assumptions[..],
            [Assumption::Guard { holds: true, .. }]
        ));
        assert_eq!(g.fewest_assumptions_path(g.start(), |_| true), Some(vec![]));
        assert_eq!(g.fewest_assumptions_path(g.start(), |_| false), None);
    }

    #[test]
    fn an_input_whose_guards_may_all_fail_may_take_nothing() {
        let g = graph(
            "fn ok() -> bool\nmachine M() {\n  event Go\n  event Stop\n  event Both\n  initial P\n  parallel state P {\n    state L {\n      initial L1\n      state L1 {\n        on Go if ok() -> L2\n        on Both if ok() -> L2\n      }\n      state L2 {}\n    }\n    state R {\n      initial R1\n      state R1 {\n        on Stop -> R2\n        on Both -> R2\n      }\n      state R2 {}\n    }\n  }\n}\n",
        );
        let start = g.start();
        // `Go`'s only candidate is guarded; `Stop`'s is not; `Both` has an unguarded one in R.
        assert!(g.takes_nothing(start, &Input::Event("Go".into())));
        assert!(!g.takes_nothing(start, &Input::Event("Stop".into())));
        assert!(!g.takes_nothing(start, &Input::Event("Both".into())));
        // An event no active state has a candidate for has no outcome at all.
        let l2 = g.state_index("M.P.L.L2").unwrap();
        let moved = g.nodes().find(|&n| g.is_active(n, l2)).unwrap();
        assert!(!g.takes_nothing(moved, &Input::Event("Go".into())));
        assert!(!g.takes_nothing(moved, &Input::Event("Stop".into())));
    }

    #[test]
    fn the_cost_of_a_path_is_its_number_of_assumptions() {
        // One step to T relies on three guards; two steps rely on one each.
        let g = graph(
            "fn a() -> bool\nfn b() -> bool\nfn c() -> bool\nfn d() -> bool\nfn e() -> bool\nmachine M() {\n  event Go\n  event Next\n  initial A\n  state A {\n    on Go {\n      if a() -> Z\n      if b() -> Z\n      if c() -> T\n    }\n    on Next if d() -> B\n  }\n  state B { on Next if e() -> T }\n  state Z {}\n  state T {}\n}\n",
        );
        let t = g.state_index("M.T").unwrap();
        let path = g
            .fewest_assumptions_path(g.start(), |n| g.is_active(n, t))
            .unwrap();
        let inputs: Vec<String> = path.iter().map(|&e| g.describe(&g.edge(e).input)).collect();
        assert_eq!(inputs, ["Next", "Next"]);
    }

    #[test]
    fn too_many_sender_names_or_combinations_are_not_explored() {
        let names: Vec<String> = (1..=17).map(|i| format!("p{i}")).collect();
        let params: Vec<String> = names.iter().map(|n| format!("{n}: ref P")).collect();
        let ons: String = names
            .iter()
            .map(|n| format!("    on {n}.Ping -> A\n"))
            .collect();
        let src = format!(
            "machine P() {{\n  emits Ping\n  initial A\n  state A {{ entry emit Ping }}\n}}\nmachine M({}) {{\n  initial A\n  state A {{\n{ons}  }}\n}}\n",
            params.join(", ")
        );
        let out = crate::compile_source("m.road", &src, &Default::default());
        assert!(!out.has_errors(), "{}", out.render());
        let ir = out.ir.unwrap();
        assert_eq!(
            explore(&ir, &ir.machines["M"], DEFAULT_MAX_NODES).unwrap_err(),
            TooLarge::Senders {
                event: "Ping".into(),
                names: 17
            }
        );
        // Seven regions, each with a guarded `Go`: 128 combinations for the first input,
        // more than a budget of one node allows.
        let regions: String = (0..7)
            .map(|i| format!("    state R{i} {{\n      initial A\n      state A {{ on Go if ok() -> B }}\n      state B {{}}\n    }}\n"))
            .collect();
        let src = format!(
            "fn ok() -> bool\nmachine M() {{\n  event Go\n  initial P\n  parallel state P {{\n{regions}  }}\n}}\n"
        );
        let out = crate::compile_source("m.road", &src, &Default::default());
        let ir = out.ir.unwrap();
        assert_eq!(
            explore(&ir, &ir.machines["M"], 1).unwrap_err(),
            TooLarge::Combinations {
                budget: COMBINATIONS_PER_NODE
            }
        );
        assert!(explore(&ir, &ir.machines["M"], DEFAULT_MAX_NODES).is_ok());
    }

    #[test]
    fn the_budget_stops_exploring() {
        let out = crate::compile_source(
            "m.road",
            "machine M() {\n  event Go\n  initial A\n  state A { on Go -> B }\n  state B {}\n}\n",
            &Default::default(),
        );
        let ir = out.ir.unwrap();
        assert_eq!(
            explore(&ir, &ir.machines["M"], 1).unwrap_err(),
            TooLarge::Nodes { reached: 2 }
        );
    }
}
