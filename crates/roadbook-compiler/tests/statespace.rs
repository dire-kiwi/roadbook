//! Soundness of `statespace::explore`: every run in a conformance trace is a path of its
//! machine's graph.
//!
//! For each case, the chart is compiled and every machine that a run of the trace runs is
//! explored. Each run's records are replayed: the configurations it passes through between
//! microsteps must be nodes' active sets, and each microstep's `transition` records an edge
//! between two such nodes, for the same input. The replay follows the set of nodes the run can
//! be at (several, since nodes differ by what the trace does not show, such as a spawn flag),
//! so the history records and the consumed invokes and timers of the nodes are checked too: a
//! wrong one leaves the run with no edge to follow.
//!
//! What cannot be replayed, and why:
//! - A run that ends with a cancel stops being checked at the cancel. Its earlier microsteps
//!   are checked; the cancel's exits are not a microstep, and the graph has no cancel.
//! - A run that ends with a defect stops being checked at the `defect` record, and the
//!   microstep that was under way is not checked: a host defect in an action stops the step's
//!   records partway through, so the configuration it would have reached is not in the trace.
//!   The graph has no defects.
//! - A run restored from a snapshot starts from every stable node with the snapshot's active
//!   states and history records (the graph does not say which invokes are consumed, so it may
//!   start from more nodes than one). A restored snapshot that no such node matches would be
//!   skipped; [`UNREACHABLE_SNAPSHOTS`] lists them.
//!
//! Wherever a run waits for an input (before one, when it is suspended or cancelled, and at
//! the end of its trace) it has settled, so the nodes it may be at must include a stable one.
//!
//! A call another run makes of the machine is its `on c` input, and the outcome of a call the
//! machine made is its invoke's, as for a task. A `replied` record is not a microstep.
//!
//! An event a peer emits is matched by the `ref` names that hold the sender: the run's
//! arguments (`started`'s `input`, a restored snapshot's `input` and `vars`) and the `set`
//! records of its variables say which name holds which run id, and the event's `from` names
//! the sender. Where no name is known to hold it (a handle that came inside a record, say),
//! the event is matched by its name alone, whichever names the edge assumed.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use roadbook_compiler::statespace::{
    self, Graph, Input, NodeId, Sender, StateIndex, TransitionRef,
};
use roadbook_compiler::{FsLoader, Options, compile};
use serde_json::Value;

/// Runs restored from a snapshot that no node of the graph has: `(case, run, why)`.
const UNREACHABLE_SNAPSHOTS: &[(&str, &str, &str)] = &[];

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// As `roadbook.toml` compiles them: one chart reads a variable before it is assigned, on
/// purpose, for the `unassigned` defect.
fn options() -> Options {
    Options {
        allow_unassigned: vec!["spec/conformance/charts/defects.road".into()],
        ..Options::default()
    }
}

#[test]
fn every_conformance_run_is_a_path_of_its_graph() {
    let repo = repository();
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(repo.join("spec/conformance/cases"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    let mut failures = Vec::new();
    let mut unreachable = Vec::new();
    let (mut cases, mut runs, mut microsteps) = (0, 0, 0);
    for dir in &dirs {
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        let case: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("case.json")).unwrap()).unwrap();
        let chart = format!("spec/conformance/{}", case["chart"].as_str().unwrap());
        let mut loader = FsLoader::new(&repo);
        let out = compile(&mut loader, &chart, &options()).unwrap();
        let Some(ir) = out.ir else {
            panic!("{name}: {chart} does not compile:\n{}", out.render());
        };
        let trace = std::fs::read_to_string(dir.join("trace.jsonl")).unwrap();
        let mut by_run: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        for line in trace.lines() {
            let r: Value = serde_json::from_str(line).unwrap();
            let run = r["run"].as_str().unwrap().to_string();
            by_run.entry(run).or_default().push(r);
        }
        cases += 1;
        let mut graphs: HashMap<String, Graph> = HashMap::new();
        for (run, records) in &by_run {
            let first = &records[0];
            let machine = match first["kind"].as_str() {
                Some("started") => first["machine"].as_str().unwrap(),
                Some("restored") => first["machine"].as_str().unwrap(),
                other => panic!("{name} {run}: the first record is {other:?}"),
            };
            let g = graphs.entry(machine.to_string()).or_insert_with(|| {
                statespace::explore(&ir, &ir.machines[machine], statespace::DEFAULT_MAX_NODES)
                    .unwrap_or_else(|e| panic!("{name}: {machine}: {e}"))
            });
            let mut replay = Replay::new(g);
            match replay.run(records) {
                Ok(()) => {
                    runs += 1;
                    microsteps += replay.microsteps;
                }
                Err(Skip::Unreachable) => unreachable.push((name.clone(), run.clone())),
                Err(Skip::Fail(e)) => failures.push(format!("{name} {run} ({machine}): {e}")),
            }
        }
    }
    eprintln!("{cases} cases, {runs} runs, {microsteps} microsteps replayed");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    let expected: Vec<(String, String)> = UNREACHABLE_SNAPSHOTS
        .iter()
        .map(|(c, r, _)| (c.to_string(), r.to_string()))
        .collect();
    assert_eq!(unreachable, expected, "restored runs with no matching node");
}

enum Skip {
    Unreachable,
    Fail(String),
}

impl From<String> for Skip {
    fn from(s: String) -> Skip {
        Skip::Fail(s)
    }
}

/// One run's replay on its graph.
struct Replay<'g> {
    g: &'g Graph,
    /// The nodes the run may be at.
    current: BTreeSet<NodeId>,
    /// The active states, from the `entered` and `exited` records.
    active: BTreeSet<StateIndex>,
    started: bool,
    /// The microstep whose records are being read: its transitions.
    pending: Option<Vec<TransitionRef>>,
    /// Whether the pending microstep has entered a state (its exits are over), or has only
    /// targetless transitions (it exits nothing).
    entering: bool,
    internal: bool,
    /// Whether the last record was a `transition`.
    in_transitions: bool,
    /// The input the next microstep is for; `None` for settling.
    input: Option<Input>,
    /// The run has started to exit everything: it is finishing or cancelled.
    ending: bool,
    invokes: HashMap<u64, StateIndex>,
    timers: HashMap<u64, (StateIndex, usize)>,
    spawns: HashMap<u64, String>,
    /// The run id each parameter and variable holds, for those that hold a string.
    holders: HashMap<String, String>,
    microsteps: usize,
}

impl<'g> Replay<'g> {
    fn new(g: &'g Graph) -> Replay<'g> {
        Replay {
            g,
            current: BTreeSet::new(),
            active: BTreeSet::new(),
            started: false,
            pending: None,
            entering: false,
            internal: true,
            in_transitions: false,
            input: None,
            ending: false,
            invokes: HashMap::new(),
            timers: HashMap::new(),
            spawns: HashMap::new(),
            holders: HashMap::new(),
            microsteps: 0,
        }
    }

    fn state(&self, id: &Value) -> Result<StateIndex, String> {
        let id = id.as_str().ok_or("a state id is not a string")?;
        self.g
            .state_index(id)
            .ok_or_else(|| format!("no state {id} in the graph"))
    }

    fn run(&mut self, records: &[Value]) -> Result<(), Skip> {
        for r in records {
            let kind = r["kind"].as_str().unwrap_or("");
            let was_in_transitions = std::mem::replace(&mut self.in_transitions, false);
            match kind {
                "started" => self.hold(&r["input"]),
                "set" => {
                    let var = r["var"].as_str().unwrap_or("").to_string();
                    match r["value"].as_str() {
                        Some(id) => self.holders.insert(var, id.to_string()),
                        None => self.holders.remove(&var),
                    };
                }
                "restored" => self.restore(&r["snapshot"])?,
                "transition" => {
                    if !was_in_transitions {
                        self.close()?;
                        self.pending = Some(Vec::new());
                        self.entering = false;
                        self.internal = true;
                    }
                    let t = TransitionRef {
                        state: self.state(&r["from"])?,
                        index: r["index"].as_u64().unwrap() as usize,
                    };
                    if !r["to"].is_null() {
                        self.internal = false;
                    }
                    self.pending.as_mut().unwrap().push(t);
                    self.in_transitions = true;
                }
                "exited" => {
                    if !self.ending && (self.pending.is_none() || self.entering || self.internal) {
                        // Exits outside a microstep's exits: the run is finishing (after it
                        // entered its top-level final state) or cancelled.
                        self.close()?;
                        self.ending = true;
                    }
                    let s = self.state(&r["state"])?;
                    self.active.remove(&s);
                }
                "entered" => {
                    self.entering = true;
                    let s = self.state(&r["state"])?;
                    self.active.insert(s);
                }
                "event" | "dropped" | "stale" => {
                    self.close()?;
                    self.settled()?;
                    let event = &r["event"];
                    let ek = event["kind"].as_str().unwrap_or("");
                    if matches!(ek, "invoke.defect" | "spawn.defect") {
                        return Ok(());
                    }
                    if kind == "stale" {
                        continue;
                    }
                    let input = self.input_of(event)?;
                    if kind == "event" {
                        self.input = Some(input);
                    } else if matches!(
                        input,
                        Input::InvokeDone { .. } | Input::InvokeFail { .. } | Input::After { .. }
                    ) {
                        // Dropped, but consumed.
                        self.follow(&input, &[])?;
                    }
                }
                "invoke.started" => {
                    let s = self.state(&r["state"])?;
                    self.invokes.insert(r["invoke"].as_u64().unwrap(), s);
                }
                "timer.started" => {
                    let s = self.state(&r["state"])?;
                    let index = r["index"].as_u64().unwrap() as usize;
                    self.timers.insert(r["timer"].as_u64().unwrap(), (s, index));
                }
                "spawn.started" => {
                    let c = r["children"].as_str().unwrap().to_string();
                    self.spawns.insert(r["spawn"].as_u64().unwrap(), c);
                }
                "done" | "failed" => {
                    if !self.ending {
                        return Err(format!("{kind} without exits").into());
                    }
                    if let Some(n) = self.current.iter().find(|&&n| !self.g.is_final(n)) {
                        return Err(format!("the run finished, but node {} has not", n.0).into());
                    }
                    return Ok(());
                }
                // A cancel comes from outside, between macrosteps: the run had settled.
                "cancelled" => {
                    self.settled()?;
                    return Ok(());
                }
                "defect" => return Ok(()),
                "suspended" => {
                    self.close()?;
                    self.settled()?;
                    return Ok(());
                }
                _ => {}
            }
        }
        if !self.ending {
            self.close()?;
            self.settled()?;
        }
        Ok(())
    }

    /// Records which run each name of `values` holds (an object of names to values).
    fn hold(&mut self, values: &Value) {
        for (name, v) in values.as_object().into_iter().flatten() {
            if let Some(id) = v.as_str() {
                self.holders.insert(name.clone(), id.to_string());
            }
        }
    }

    /// Starts from the stable nodes that match a restored snapshot.
    fn restore(&mut self, snap: &Value) -> Result<(), Skip> {
        let mut active = Vec::new();
        for id in snap["active"].as_array().unwrap() {
            active.push(self.state(id)?);
        }
        // A snapshot with no history state recorded may leave the member out.
        let mut history = Vec::new();
        let empty = serde_json::Map::new();
        for (h, record) in snap["history"].as_object().unwrap_or(&empty) {
            let h = self.state(&Value::String(h.clone()))?;
            let mut r = Vec::new();
            for id in record.as_array().unwrap() {
                r.push(self.state(id)?);
            }
            history.push((h, r));
        }
        history.sort();
        self.hold(&snap["input"]);
        self.hold(&snap["vars"]);
        // The work the snapshot holds: a timer already due is fired with no `timer.started`.
        let none = Vec::new();
        let list = |key: &str| snap[key].as_array().unwrap_or(&none).clone();
        for tm in list("timers") {
            let s = self.state(&tm["state"])?;
            let index = tm["index"].as_u64().unwrap() as usize;
            self.timers.insert(tm["id"].as_u64().unwrap(), (s, index));
        }
        for inv in list("invokes") {
            let s = self.state(&inv["state"])?;
            self.invokes.insert(inv["id"].as_u64().unwrap(), s);
        }
        for ch in list("spawned") {
            let c = ch["children"].as_str().unwrap().to_string();
            self.spawns.insert(ch["id"].as_u64().unwrap(), c);
        }
        self.current = self
            .g
            .nodes()
            .filter(|&n| {
                !self.g.is_settling(n)
                    && !self.g.is_final(n)
                    && self.g.active(n) == active.as_slice()
                    && self.g.history(n) == history.as_slice()
            })
            .collect();
        if self.current.is_empty() {
            return Err(Skip::Unreachable);
        }
        self.active = active.into_iter().collect();
        self.started = true;
        Ok(())
    }

    /// The input of an `event` or `dropped` record, as the graph names it.
    fn input_of(&self, event: &Value) -> Result<Input, String> {
        let tag = || event["value"]["_tag"].as_str().unwrap_or("").to_string();
        let error = || event["error"]["_tag"].as_str().unwrap_or("").to_string();
        let invoke = |key: &str| -> Result<StateIndex, String> {
            let id = event[key].as_u64().unwrap();
            self.invokes
                .get(&id)
                .copied()
                .ok_or_else(|| format!("no invoke {id}"))
        };
        let spawn = |key: &str| -> Result<String, String> {
            let id = event[key].as_u64().unwrap();
            self.spawns
                .get(&id)
                .cloned()
                .ok_or_else(|| format!("no spawn {id}"))
        };
        Ok(match event["kind"].as_str().unwrap_or("") {
            "event" | "message" => Input::Event(tag()),
            // A call from another run, taken among the `on c` transitions.
            "call" => Input::Call(event["call"].as_str().unwrap_or("").to_string()),
            "invoke.done" => Input::InvokeDone {
                state: invoke("invoke")?,
            },
            "invoke.fail" => Input::InvokeFail {
                state: invoke("invoke")?,
                error: error(),
            },
            "after" => {
                let id = event["timer"].as_u64().unwrap();
                let &(state, index) = self.timers.get(&id).ok_or(format!("no timer {id}"))?;
                Input::After { state, index }
            }
            "spawn.done" => Input::SpawnDone {
                children: spawn("spawn")?,
            },
            "spawn.fail" => Input::SpawnFail {
                children: spawn("spawn")?,
                error: error(),
            },
            // A peer's end: a peer's input named `ended`, from the names known to hold it.
            "ended" => {
                let sender = event["from"].as_str().unwrap_or("");
                let mut names: Vec<String> = self
                    .holders
                    .iter()
                    .filter(|(_, id)| *id == sender)
                    .map(|(n, _)| n.clone())
                    .collect();
                names.sort();
                Input::Emitted {
                    from: Sender::Refs(names),
                    event: statespace::ENDED.to_string(),
                }
            }
            "emitted" => {
                let from = match event["invoke"].as_u64() {
                    // The names known to hold the sender, if any.
                    None => {
                        let sender = event["from"].as_str().unwrap_or("");
                        let mut names: Vec<String> = self
                            .holders
                            .iter()
                            .filter(|(_, id)| *id == sender)
                            .map(|(n, _)| n.clone())
                            .collect();
                        names.sort();
                        Sender::Refs(names)
                    }
                    Some(id) => match (self.invokes.get(&id), self.spawns.get(&id)) {
                        (Some(&s), _) => Sender::Child(s),
                        (_, Some(c)) => Sender::Children(c.clone()),
                        _ => return Err(format!("no invoke or spawn {id}")),
                    },
                };
                Input::Emitted { from, event: tag() }
            }
            other => return Err(format!("unexpected input {other}")),
        })
    }

    /// Whether an edge's input is the trace's. A peer's event matches the edge whose names
    /// are those of the names known to hold the sender that a trigger expects it through, or,
    /// with no name known to hold it, whichever names the edge assumed.
    fn same_input(&self, edge: &Input, trace: &Input) -> bool {
        match (edge, trace) {
            (
                Input::Emitted {
                    from: Sender::Refs(assumed),
                    event: a,
                },
                Input::Emitted {
                    from: Sender::Refs(holders),
                    event: b,
                },
            ) => {
                if a != b {
                    return false;
                }
                if holders.is_empty() {
                    return true;
                }
                let expected = self.expected_names(a);
                let want: BTreeSet<&String> =
                    holders.iter().filter(|n| expected.contains(*n)).collect();
                let have: BTreeSet<&String> = assumed.iter().collect();
                want == have
            }
            _ => edge == trace,
        }
    }

    /// Every name the graph's edges expect a peer's `event` through.
    fn expected_names(&self, event: &str) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for e in 0..self.g.edge_count() {
            if let Input::Emitted {
                from: Sender::Refs(names),
                event: x,
            } = &self.g.edge(statespace::EdgeId(e as u32)).input
                && x == event
            {
                out.extend(names.iter().cloned());
            }
        }
        out
    }

    /// Checks the start, the first time the run reaches a boundary between microsteps.
    fn check_start(&mut self) -> Result<(), String> {
        if self.started {
            return Ok(());
        }
        self.started = true;
        let start = self.g.start();
        let active: Vec<StateIndex> = self.active.iter().copied().collect();
        if self.g.active(start) != active.as_slice() {
            return Err(format!(
                "the run starts in {}, the graph in {}",
                self.names(&active),
                self.names(self.g.active(start))
            ));
        }
        self.current = BTreeSet::from([start]);
        Ok(())
    }

    /// Ends the pending microstep: the run must have reached, by an edge with its transitions,
    /// a node with the active states it now has.
    fn close(&mut self) -> Result<(), String> {
        self.check_start()?;
        let Some(ts) = self.pending.take() else {
            return Ok(());
        };
        let input = self.input.take();
        let active: Vec<StateIndex> = self.active.iter().copied().collect();
        let mut next = BTreeSet::new();
        for &n in &self.current {
            for &e in self.g.outgoing(n) {
                let edge = self.g.edge(e);
                let same_input = match &input {
                    Some(i) => !edge.settling && self.same_input(&edge.input, i),
                    None => {
                        edge.settling && matches!(edge.input, Input::Always | Input::Done { .. })
                    }
                };
                if same_input
                    && edge.transitions == ts
                    && self.g.active(edge.to) == active.as_slice()
                {
                    next.insert(edge.to);
                }
            }
        }
        if next.is_empty() {
            let from: Vec<String> = self
                .current
                .iter()
                .map(|&n| self.names(self.g.active(n)))
                .collect();
            return Err(format!(
                "microstep {} ({}) to {}: no edge from {}",
                self.microsteps + 1,
                match &input {
                    Some(i) => self.g.describe(i),
                    None => "settling".into(),
                },
                self.names(&active),
                from.join(" or ")
            ));
        }
        self.current = next;
        self.microsteps += 1;
        Ok(())
    }

    /// Before an input: the run has settled, so a settling node must be able to stop.
    fn settled(&mut self) -> Result<(), String> {
        let mut next = BTreeSet::new();
        for &n in &self.current {
            if !self.g.is_settling(n) {
                next.insert(n);
                continue;
            }
            for &e in self.g.outgoing(n) {
                if self.g.edge(e).input == Input::Settled {
                    next.insert(self.g.edge(e).to);
                }
            }
        }
        if next.is_empty() {
            return Err("the run settled, but the graph does not".into());
        }
        self.current = next;
        Ok(())
    }

    /// An input that takes no transition but changes the node (it consumed its work).
    fn follow(&mut self, input: &Input, ts: &[TransitionRef]) -> Result<(), String> {
        let mut next = BTreeSet::new();
        for &n in &self.current {
            for &e in self.g.outgoing(n) {
                let edge = self.g.edge(e);
                if edge.input == *input && edge.transitions == ts {
                    next.insert(edge.to);
                }
            }
        }
        if next.is_empty() {
            return Err(format!(
                "no edge for {} taking nothing",
                self.g.describe(input)
            ));
        }
        self.current = next;
        Ok(())
    }

    fn names(&self, states: &[StateIndex]) -> String {
        let ids: Vec<&str> = states.iter().map(|&s| self.g.state_id(s)).collect();
        format!("[{}]", ids.join(", "))
    }
}

/// Every machine of every example chart explores within the default budget, and quickly. Run
/// with `--nocapture` for the sizes and times.
#[test]
fn every_example_explores_within_the_budget() {
    let repo = repository();
    let mut charts = Vec::new();
    let mut dirs = vec![repo.join("examples")];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().filter_map(Result::ok) {
            let p = entry.path();
            if p.is_dir() && p.file_name().is_some_and(|n| n != "node_modules") {
                dirs.push(p);
            } else if p.extension().is_some_and(|e| e == "road") {
                charts.push(p);
            }
        }
    }
    charts.sort();
    for chart in charts {
        let path = chart
            .strip_prefix(&repo)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let mut loader =
            FsLoader::new(&repo).with_package("inventory", repo.join("examples/inventory"));
        let out = compile(&mut loader, &path, &Options::default()).unwrap();
        let Some(ir) = &out.ir else {
            panic!("{path} does not compile:\n{}", out.render());
        };
        let started = std::time::Instant::now();
        let mut sizes = Vec::new();
        for (name, m) in &ir.machines {
            // A chart's IR holds the machines it imports too.
            let g = statespace::explore(ir, m, statespace::DEFAULT_MAX_NODES)
                .unwrap_or_else(|e| panic!("{path}: {name}: {e}"));
            sizes.push(format!("{name} {}/{}", g.node_count(), g.edge_count()));
        }
        eprintln!(
            "{path}: {:.1} ms (nodes/edges: {})",
            started.elapsed().as_secs_f64() * 1000.0,
            sizes.join(", ")
        );
    }
}
