//! What the benchmarks in `benches/runtime.rs` run: the example charts (`examples/shop`,
//! `examples/large`, `examples/orders` with `examples/inventory`), each as
//!
//! - a [`Script`]: the inputs of one whole run, recorded once, that the core
//!   (`Program::start` and `Program::step`) replays with no driver around it, and
//! - a whole run through the driver and the generated `Handlers`, with handlers that answer at
//!   once ([`shop`], [`large`], [`orders`]).
//!
//! The scenarios are the same at both levels, so the difference is what the driver, the
//! generated code and the trace sink cost on top of the core.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;

use roadbook::{BoxFuture, Config, Timer, TraceSink};
use roadbook_compiler::{Options, loader::FsLoader};
use roadbook_core::{Command, Input, Object, Program, Snapshot, Status};
use serde_json::Value;

pub mod large;
pub mod orders;
pub mod shop;

/// The repository root, the project root of every example chart.
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Compiles an example chart (a path from the repository root) as `roadbook::include!` does.
pub fn program(chart: &str) -> Program {
    let root = root();
    let mut loader =
        FsLoader::new(&root).with_package("inventory", root.join("examples/inventory"));
    let c = roadbook_compiler::compile(&mut loader, chart, &Options::default())
        .unwrap_or_else(|e| panic!("cannot read {chart}: {}", e.reason));
    let Some(ir) = c.ir.clone() else {
        panic!("{chart} does not compile:\n{}", c.render())
    };
    Program::new(ir).expect("the compiler's IR runs")
}

/// One whole run as the core sees it: the machine, its arguments, and every input it takes,
/// in order. Replaying it ([`Script::replay`]) gives the same run each time, since the ids
/// in the inputs are the ones the core hands out.
pub struct Script {
    pub program: Program,
    pub machine: &'static str,
    pub input: Object,
    pub inputs: Vec<Input>,
    /// Host functions, by name.
    pub functions: fn(&str, &[Value]) -> Value,
}

/// How a scripted invoke ends: its outcome as an input, given the invoke's id.
pub type Respond = fn(src: &str, args: &Object, nth: usize) -> Outcome;

/// A scripted invoke's outcome.
pub enum Outcome {
    Done(Value),
    Fail(Value),
}

/// What [`Script::record`] needs: the run and how the world answers it.
pub struct Scenario {
    pub chart: &'static str,
    pub machine: &'static str,
    pub input: Object,
    /// Events and other inputs from outside, each taken when the run is otherwise idle.
    pub outside: Vec<Input>,
    pub respond: Respond,
    pub functions: fn(&str, &[Value]) -> Value,
    /// Whether a timer fires when the run is idle and nothing more comes from outside.
    pub fire_timers: bool,
}

impl Script {
    /// Runs `scenario` once on the core alone, as a driver whose host answers every invoke at
    /// once would, and keeps the inputs. The run must end `done`.
    pub fn record(scenario: Scenario) -> Script {
        let program = program(scenario.chart);
        let functions = scenario.functions;
        let mut fns = |n: &str, a: Vec<Value>| Ok(functions(n, &a));
        let (mut snap, mut commands) = program
            .start(scenario.machine, "r1", scenario.input.clone(), &mut fns)
            .expect("the machine is in the chart");
        let mut queue: VecDeque<Input> = VecDeque::new();
        let mut outside: VecDeque<Input> = scenario.outside.into();
        let mut timers: Vec<u64> = Vec::new();
        let mut calls: Vec<String> = Vec::new();
        let mut inputs = Vec::new();
        while snap.status == Status::Running {
            for c in &commands {
                match c {
                    Command::InvokeStart {
                        invoke, src, args, ..
                    } => {
                        let nth = calls.iter().filter(|s| *s == src).count();
                        calls.push(src.clone());
                        queue.push_back(match (scenario.respond)(src, args, nth) {
                            Outcome::Done(value) => Input::InvokeDone {
                                invoke: *invoke,
                                value,
                            },
                            Outcome::Fail(error) => Input::InvokeFail {
                                invoke: *invoke,
                                error,
                            },
                        });
                    }
                    Command::TimerStart { timer, .. } => timers.push(*timer),
                    Command::TimerCancel { timer } => timers.retain(|t| t != timer),
                    _ => {}
                }
            }
            let next = queue
                .pop_front()
                .or_else(|| outside.pop_front())
                .or_else(|| {
                    (scenario.fire_timers && !timers.is_empty()).then(|| Input::After {
                        timer: timers.remove(0),
                    })
                });
            let Some(next) = next else {
                panic!("{} is stuck in {:?}", scenario.machine, snap.active)
            };
            inputs.push(next.clone());
            (snap, commands) = program.step(&snap, next, &mut fns);
        }
        assert_eq!(
            snap.status,
            Status::Done,
            "{} ended with {:?}",
            scenario.machine,
            snap.result
        );
        Script {
            program,
            machine: scenario.machine,
            input: scenario.input,
            inputs,
            functions,
        }
    }

    /// Starts the run and takes every input, giving each step's snapshot and commands to
    /// `each`.
    pub fn replay(&self, mut each: impl FnMut(&Snapshot, &[Command])) -> Snapshot {
        let functions = self.functions;
        let mut fns = |n: &str, a: Vec<Value>| Ok(functions(n, &a));
        let (mut snap, commands) = self
            .program
            .start(self.machine, "r1", self.input.clone(), &mut fns)
            .expect("the machine is in the chart");
        each(&snap, &commands);
        for input in &self.inputs {
            let (next, commands) = self.program.step(&snap, input.clone(), &mut fns);
            each(&next, &commands);
            snap = next;
        }
        snap
    }

    /// Every step's result, as the core gives it, after the step's own input.
    pub fn steps(&self) -> Vec<(Snapshot, Vec<Command>)> {
        let mut out = Vec::new();
        self.replay(|s, c| out.push((s.clone(), c.to_vec())));
        out
    }
}

/// An object from a `json!` object literal.
pub fn object(v: Value) -> Object {
    match v {
        Value::Object(m) => m,
        _ => panic!("not an object"),
    }
}

/// An event input.
pub fn event(v: Value) -> Input {
    Input::Event { value: v }
}

/// A timer that never fires: every run in these benchmarks ends without one, unless it
/// uses [`AtOnce`]. The clock stands still, so traces are the same on every run.
pub struct Never;

impl Timer for Never {
    fn now(&self) -> u64 {
        0
    }
    fn start(&self, _: &str, _: u64, _: u64) -> BoxFuture<'static, ()> {
        Box::pin(std::future::pending())
    }
}

/// A timer whose every timer is due at once.
pub struct AtOnce;

impl Timer for AtOnce {
    fn now(&self) -> u64 {
        0
    }
    fn start(&self, _: &str, _: u64, _: u64) -> BoxFuture<'static, ()> {
        Box::pin(std::future::ready(()))
    }
}

/// A config with `timer` and `sink`.
pub fn config(timer: impl Timer + 'static, sink: Arc<dyn TraceSink>) -> Config {
    Config::new().timer(Arc::new(timer)).sink(sink)
}

/// The sinks a whole run is measured with: none, and canonical JSON lines thrown away.
pub fn sinks() -> [(&'static str, Arc<dyn TraceSink>); 2] {
    [
        ("no-trace", Arc::new(roadbook::NoTrace)),
        (
            "json-lines",
            Arc::new(roadbook::JsonLines::new(std::io::sink())),
        ),
    ]
}

/// A current-thread runtime.
pub fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("a runtime")
}
