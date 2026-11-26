//! The driver: one future per run, one FIFO per run, and the core's commands carried out in
//! order (`spec/execution.md#the-driver`).

use std::collections::{BTreeMap, VecDeque};
use std::future::poll_fn;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::task::{Context, Poll};

use roadbook_core::{
    Command, Defect, Input, Object, Program, Record, RunResult, Snapshot, SrcKind, Status,
};
use serde_json::{Map, Value};

use crate::defect::defect_from_panic;
use crate::dynamic::{Host, Invocation, Outcome};
use crate::env::{BoxFuture, Config, TraceRecord};
use crate::shared::{Finished, InvokeContext, RunHandle, Shared, with_context};

/// How a run ended, as [`crate::dynamic::start`] reports it.
#[derive(Debug, Clone, PartialEq)]
pub enum RunEnd {
    /// It reached a top-level final state with an output (`null` for none).
    Done(Value),
    /// It reached a top-level final state with `fail`.
    Failed(Value),
    /// It was cancelled from outside.
    Cancelled,
    /// It ended with a defect.
    Defect(Defect),
}

/// A running invoke: its future and, for a machine invoke, the context it is polled in.
struct Slot<'a> {
    fut: BoxFuture<'a, Outcome>,
    ctx: Option<Arc<InvokeContext>>,
}

pub(crate) struct Driver<'a> {
    program: &'a Program,
    host: &'a dyn Host,
    start: Option<(&'a str, Object)>,
    config: Config,
    id: String,
    parent: Option<String>,
    shared: Arc<Shared>,
    queue: VecDeque<Input>,
    invokes: BTreeMap<u64, Slot<'a>>,
    timers: BTreeMap<u64, BoxFuture<'static, ()>>,
    snap: Snapshot,
    final_state: Option<String>,
    cancel_requested: bool,
    cancel_taken: bool,
}

/// Calls a host function with no invoke context, catching a panic as a host defect.
fn call_function(host: &dyn Host, name: &str, args: Vec<Value>) -> Result<Value, Defect> {
    with_context(None, || {
        catch_unwind(AssertUnwindSafe(|| host.function(name, args)))
            .unwrap_or_else(|p| Err(defect_from_panic(p)))
    })
}

fn blank() -> Snapshot {
    Snapshot {
        machine: String::new(),
        ir: String::new(),
        status: Status::Running,
        active: Vec::new(),
        input: Map::new(),
        vars: Map::new(),
        invokes: Vec::new(),
        timers: Vec::new(),
        next_invoke: 1,
        next_timer: 1,
        result: None,
    }
}

impl<'a> Driver<'a> {
    pub fn root(
        program: &'a Program,
        machine: &'a str,
        input: Object,
        host: &'a dyn Host,
        config: &Config,
        shared: Arc<Shared>,
    ) -> Self {
        let id = config.root_id();
        Self::new(
            program,
            machine,
            input,
            host,
            config.clone(),
            id,
            None,
            shared,
        )
    }

    pub fn child(
        program: &'a Program,
        machine: &'a str,
        input: Object,
        host: &'a dyn Host,
        ctx: &InvokeContext,
    ) -> Self {
        let id = format!("{}.{}", ctx.parent, ctx.invoke);
        Self::new(
            program,
            machine,
            input,
            host,
            ctx.config.clone(),
            id,
            Some(ctx.parent.clone()),
            ctx.token.clone(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new(
        program: &'a Program,
        machine: &'a str,
        input: Object,
        host: &'a dyn Host,
        config: Config,
        id: String,
        parent: Option<String>,
        shared: Arc<Shared>,
    ) -> Self {
        Driver {
            program,
            host,
            start: Some((machine, input)),
            config,
            id,
            parent,
            shared,
            queue: VecDeque::new(),
            invokes: BTreeMap::new(),
            timers: BTreeMap::new(),
            snap: blank(),
            final_state: None,
            cancel_requested: false,
            cancel_taken: false,
        }
    }

    pub async fn drive(self) -> RunEnd {
        self.drive_reporting().await.0
    }

    /// Runs to the end. Also says whether the run took a cancel request, in which case a
    /// defect it ended with happened in its cancel step.
    pub async fn drive_reporting(mut self) -> (RunEnd, bool) {
        if let Some(observer) = &self.config.observer {
            observer(
                &self.id,
                RunHandle {
                    shared: self.shared.clone(),
                },
            );
        }
        let (machine, input) = self.start.take().expect("a driver runs once");
        let host = self.host;
        let (snap, commands) = self
            .program
            .start(machine, input, &mut |n, a| call_function(host, n, a))
            .unwrap_or_else(|e| panic!("{e}"));
        self.snap = snap;
        self.carry_out(commands).await;
        self.publish();
        while self.snap.status == Status::Running {
            let input = self.take().await;
            let (invoke, timer) = (input.invoke_id(), input.timer_id());
            let (snap, commands) = self
                .program
                .step(&self.snap, input, &mut |n, a| call_function(host, n, a));
            self.snap = snap;
            // The core has taken this outcome or expiry: forget its source.
            if let Some(i) = invoke {
                self.invokes.remove(&i);
            }
            if let Some(t) = timer {
                self.timers.remove(&t);
            }
            self.carry_out(commands).await;
            self.publish();
        }
        self.end().await
    }

    fn publish(&self) {
        let leaf = self.final_state.as_ref().or(self.snap.active.last());
        if let Some(leaf) = leaf {
            self.shared.set_state(leaf);
        }
    }

    /// The next input: a cancel request if there is one, else the front of the queue.
    async fn take(&mut self) -> Input {
        poll_fn(|cx| {
            self.poll_sources(cx);
            if self.cancel_requested {
                self.cancel_taken = true;
                return Poll::Ready(Input::Cancel);
            }
            match self.queue.pop_front() {
                Some(input) => Poll::Ready(input),
                None => Poll::Pending,
            }
        })
        .await
    }

    /// Finds out what has arrived and pushes it onto the queue: the inbox (events and inputs
    /// sent from outside), then invoke outcomes and timer expiries, by ascending id.
    fn poll_sources(&mut self, cx: &mut Context<'_>) {
        if self.shared.drain(&mut self.queue, cx.waker()) {
            self.cancel_requested = true;
        }
        let mut ended = Vec::new();
        for (&id, slot) in self.invokes.iter_mut() {
            let polled = with_context(slot.ctx.as_ref(), || {
                catch_unwind(AssertUnwindSafe(|| slot.fut.as_mut().poll(cx)))
            });
            match polled {
                Ok(Poll::Pending) => {}
                Ok(Poll::Ready(outcome)) => ended.push((id, outcome)),
                Err(p) => ended.push((id, Outcome::Defect(defect_from_panic(p)))),
            }
        }
        for (invoke, outcome) in ended {
            self.invokes.remove(&invoke);
            self.queue.push_back(match outcome {
                Outcome::Done(value) => Input::InvokeDone { invoke, value },
                Outcome::Fail(error) => Input::InvokeFail { invoke, error },
                Outcome::Defect(defect) => Input::InvokeDefect { invoke, defect },
            });
        }
        let mut fired = Vec::new();
        for (&id, t) in self.timers.iter_mut() {
            if t.as_mut().poll(cx).is_ready() {
                fired.push(id);
            }
        }
        for timer in fired {
            self.timers.remove(&timer);
            self.queue.push_back(Input::After { timer });
        }
    }

    async fn carry_out(&mut self, commands: Vec<Command>) {
        for c in commands {
            if let Err(defect) = self.carry(c).await {
                let host = self.host;
                let (snap, rest) =
                    self.program
                        .step(&self.snap, Input::Defect { defect }, &mut |n, a| {
                            call_function(host, n, a)
                        });
                self.snap = snap;
                for c in rest {
                    // A defect met here is ignored.
                    let _ = self.carry(c).await;
                }
                return;
            }
        }
    }

    /// Carries out one command. `Err` is a defect it met: a host defect in an action, or a
    /// defect in the cancel step of a child run.
    async fn carry(&mut self, c: Command) -> Result<(), Defect> {
        match c {
            Command::Trace { record } => {
                self.write(record);
                Ok(())
            }
            Command::Action { action, args } => {
                let host = self.host;
                let created = with_context(None, || {
                    catch_unwind(AssertUnwindSafe(|| host.action(&action, args)))
                });
                let mut fut = created.map_err(defect_from_panic)?;
                poll_fn(|cx| {
                    let polled = with_context(None, || {
                        catch_unwind(AssertUnwindSafe(|| fut.as_mut().poll(cx)))
                    });
                    match polled {
                        Ok(Poll::Ready(r)) => Poll::Ready(r),
                        Ok(Poll::Pending) => {
                            self.poll_sources(cx);
                            Poll::Pending
                        }
                        Err(p) => Poll::Ready(Err(defect_from_panic(p))),
                    }
                })
                .await
            }
            Command::InvokeStart {
                invoke,
                src,
                src_kind,
                args,
                ..
            } => {
                let ctx = (src_kind == SrcKind::Machine).then(|| {
                    Arc::new(InvokeContext {
                        config: self.config.clone(),
                        parent: self.id.clone(),
                        invoke,
                        token: Arc::new(Shared::default()),
                    })
                });
                let call = Invocation {
                    kind: src_kind,
                    src,
                    args,
                    run: self.id.clone(),
                    invoke,
                };
                let host = self.host;
                let created = with_context(ctx.as_ref(), || {
                    catch_unwind(AssertUnwindSafe(|| host.invoke(call)))
                });
                let fut = created.unwrap_or_else(|p| {
                    let d = defect_from_panic(p);
                    Box::pin(async move { Outcome::Defect(d) })
                });
                self.invokes.insert(invoke, Slot { fut, ctx });
                Ok(())
            }
            Command::InvokeCancel { invoke } => match self.invokes.remove(&invoke) {
                Some(slot) => self.cancel_slot(slot).await,
                None => Ok(()),
            },
            Command::TimerStart { timer, delay } => {
                let fut = self.config.timer.start(&self.id, timer, delay);
                self.timers.insert(timer, fut);
                Ok(())
            }
            Command::TimerCancel { timer } => {
                if self.timers.remove(&timer).is_some() {
                    self.config.timer.cancel(&self.id, timer);
                }
                Ok(())
            }
        }
    }

    /// Stops a running invoke. A task's or stub's future is dropped at once. A child run is
    /// signalled through its token and polled until it reports that its cancel step is
    /// done; only then is its future dropped.
    async fn cancel_slot(&mut self, mut slot: Slot<'a>) -> Result<(), Defect> {
        let Some(ctx) = slot.ctx.clone() else {
            return Ok(());
        };
        if !ctx.token.is_claimed() || ctx.token.finished().is_some() {
            return Ok(());
        }
        ctx.token.request_cancel();
        let how = poll_fn(|cx| {
            if let Some(f) = ctx.token.finished() {
                return Poll::Ready(f);
            }
            let polled = with_context(Some(&ctx), || {
                catch_unwind(AssertUnwindSafe(|| slot.fut.as_mut().poll(cx)))
            });
            if let Some(f) = ctx.token.finished() {
                return Poll::Ready(f);
            }
            match polled {
                Ok(Poll::Pending) => {
                    self.poll_sources(cx);
                    Poll::Pending
                }
                // The host code ended without its run reporting: nothing more to wait for.
                _ => Poll::Ready(Finished::Ok),
            }
        })
        .await;
        drop(slot);
        match how {
            Finished::Ok => Ok(()),
            Finished::Defect(d) => Err(d),
        }
    }

    fn write(&mut self, record: Record) {
        if let Record::Done { state, .. } | Record::Failed { state, .. } = &record {
            self.final_state = Some(state.clone());
        }
        let record = TraceRecord {
            t: self.config.timer.now(),
            run: self.id.clone(),
            parent: self.parent.clone(),
            record,
        };
        self.config.sink.record(&record);
    }

    /// Ends the run: discards the queue, stops whatever it still holds (a child run is
    /// cancelled through its token first), and reports through the token.
    async fn end(mut self) -> (RunEnd, bool) {
        self.queue.clear();
        for (_, slot) in std::mem::take(&mut self.invokes) {
            let _ = self.cancel_slot(slot).await;
        }
        for (timer, _) in std::mem::take(&mut self.timers) {
            self.config.timer.cancel(&self.id, timer);
        }
        let end = match self.snap.result.clone() {
            Some(RunResult::Output(v)) => RunEnd::Done(v),
            Some(RunResult::Error(e)) => RunEnd::Failed(e),
            Some(RunResult::Defect(d)) => RunEnd::Defect(d),
            None => RunEnd::Cancelled,
        };
        self.shared.finish(match (&end, self.cancel_taken) {
            (RunEnd::Defect(d), true) => Finished::Defect(d.clone()),
            _ => Finished::Ok,
        });
        (end, self.cancel_taken)
    }
}
