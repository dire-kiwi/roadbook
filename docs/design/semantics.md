# Semantics, the IR and the trace

Two runtimes have to behave identically, and the diagram has to match both. This page defines the three things that make that possible: the IR every tool reads, the execution rules, and the trace every runtime writes.

## The IR

The compiler's only output is one JSON document per entry file: the intermediate representation. Everything downstream reads it and nothing downstream reads `.road` source.

```
                         ┌─▶ TypeScript code generator ─▶ place_order.road.ts
 *.road ─▶ compiler ─▶ IR ├─▶ Rust macro                ─▶ types, traits, embedded IR
          (checks)       ├─▶ viewer                    ─▶ place_order.html
                         ├─▶ exporters                 ─▶ Mermaid, SCXML
                         └─▶ both runtimes interpret it
```

Because both runtimes interpret the same IR the viewer draws, the picture cannot drift from the behaviour. The IR's content hash is stamped into the generated code and into every trace, so the viewer can tell when a trace came from a different version of the chart.

The IR is fully resolved: state ids are absolute (`PlaceOrder.Processing.Paying`), targets are ids, names are checked, every expression carries its type, and every declaration, state, variable, event, invoke and transition carries its source span and doc comment. It is versioned (`irVersion`) and has a published JSON Schema, so third-party tools can read it and a runtime can refuse an IR it does not understand. The field-by-field reference is [`spec/ir.md`](../../spec/ir.md) and the schema is [`spec/ir.schema.json`](../../spec/ir.schema.json).

An abridged IR for `Checkout` (`/* … */` marks what is left out):

```jsonc
{
  "irVersion": "0.2",
  "sources": [{ "path": "examples/shop/checkout.road", "package": null },
              { "path": "examples/shop/types.road", "package": null }],
  "types": {
    "CardDeclined": { "kind": "error", "fields": [{ "name": "reason", "type": "string", "doc": "" }],
                      "doc": "", "span": { "file": 1, "line": 15, "col": 1, "endLine": 15, "endCol": 44 } }
  },
  "functions": { "retryDelay": { "params": [{ "name": "attempt", "type": "int", "doc": "" }], "returns": "duration",
                                 "doc": "", "span": { /* … */ } } },
  "actions":   {},
  "tasks":     { "chargeCard": { "params": [/* order, user */], "returns": "Receipt",
                                 "errors": ["CardDeclined", "GatewayUnavailable"], "doc": "", "span": { /* … */ } } },
  "machines": {
    "Checkout": {
      "doc": "Charge the customer's card, retrying while the payment gateway is down.",
      "span": { "file": 0, "line": 4, "col": 1, "endLine": 32, "endCol": 2 },
      "params": [/* order, user */], "returns": "Receipt", "errors": ["CardDeclined", "GatewayUnavailable"],
      "vars": [{ "name": "attempts", "type": "int",
                 "init": { "kind": "lit", "value": 0, "type": "int", "text": "0" },
                 "doc": "", "span": { "file": 0, "line": 5, "col": 3, "endLine": 5, "endCol": 24 } }],
      "events": {}, "emits": {},
      "root": {
        "id": "Checkout", "name": "Checkout", "kind": "compound", "doc": "Charge the customer's card, …",
        "span": { /* the machine's */ }, "initial": "Checkout.Charging",
        "states": [
          {
            "id": "Checkout.Charging", "name": "Charging", "kind": "atomic",
            "doc": "One attempt against the payment gateway.",
            "span": { "file": 0, "line": 12, "col": 3, "endLine": 21, "endCol": 4 },
            "initial": null, "states": [], "entry": [], "exit": [],
            "invoke": { "kind": "task", "src": "chargeCard", "as": null, "args": [/* order, user */],
                        "span": { "file": 0, "line": 13, "col": 5, "endLine": 20, "endCol": 6 } },
            "transitions": [
              { "trigger": { "kind": "invoke.done" }, "group": 0, "guard": null,
                "target": "Checkout.Charged",
                "effects": [{ "kind": "set", "var": "receipt",
                              "value": { "kind": "ref", "path": ["result"], "type": "Receipt", "text": "result" } }],
                "span": { "file": 0, "line": 14, "col": 7, "endLine": 14, "endCol": 64 } },
              /* fail CardDeclined, group 1 */
              { "trigger": { "kind": "invoke.fail", "error": "GatewayUnavailable" }, "group": 2,
                "guard": { "kind": "op", "op": "<", "args": [/* attempts, 3 */], "type": "bool", "text": "attempts < 3" },
                "target": "Checkout.Backoff", "effects": [], "span": { /* … */ } },
              { "trigger": { "kind": "invoke.fail", "error": "GatewayUnavailable" }, "group": 2,
                "guard": null, "target": "Checkout.GaveUp", "effects": [], "span": { /* … */ } }
            ],
            "output": null, "fail": null
          }
          /* Backoff, Charged, Declined, GaveUp */
        ],
        "entry": [], "exit": [], "invoke": null, "transitions": [], "output": null, "fail": null
      },
      "hash": "sha256:…"
    }
  }
}
```

Every member is always present: an absent value is `null`, a missing doc comment `""`, an empty list `[]`. Transitions are in document order, and a trace names one by its state's id and its index in `transitions`. The branches of one block are consecutive and share a `group`. Expressions are a small tree (`lit`, `ref`, `field`, `call`, `op`, `record`, `enum`), and every node carries its `type` and `text`, its canonical source form, so tools can display it without a pretty-printer.

The hash is SHA-256 over the [canonical JSON](../../spec/ir.md#canonical-json) (RFC 8785) of the machine with its docs, spans and source text removed, together with the declarations and child machines it uses ([definition](../../spec/ir.md#the-machine-hash)). Reformatting a chart or editing its comments leaves the hash alone, and any change in behaviour or in a type it uses changes it.

## Execution model

Roadbook machines are statecharts with the semantics of SCXML, the W3C standard that xstate also follows. Anyone who knows xstate will find no surprises. The rules, briefly (the precise algorithm is [`spec/execution.md`](../../spec/execution.md)):

1. **One event at a time.** A machine has a queue. It takes one event, does everything that event causes, and only then takes the next (run to completion).
2. **A step.** Find the active states that have a transition for the event, innermost first. Evaluate guards in document order and take the first that holds. Exit states from the innermost outward, run the transition's effects, enter states from the outermost inward. Within each: `set` assignments first, then `do` actions, in the order written. A transition with a target always leaves its source state and enters its target, even when the target is the source itself or one of its children (an external transition, in SCXML's terms), so the states it re-enters restart their work. The one exception is a transition written in the machine body, which does not leave the machine itself. A transition with no target is internal: nothing is left, entered or restarted.
3. **Settling.** After a step, `always` transitions and `done` transitions are taken until none applies, an enabled `always` before any `done`. The machine is then stable and takes the next event.
4. **Unhandled events are dropped and recorded.** An event with no enabled transition changes nothing. It appears in the trace as dropped, so it is visible. A machine will be able to opt into `strict`, which turns a dropped event of its own into a failure; that is decided but not built ([strict machines](language.md#strict-machines)).
5. **Parallel states** are active in all regions at once. Entering one enters every region, in the order they are written; leaving it exits them all, the last region first. Each region looks for a transition for the event on its own, from its innermost state outward, and the transitions found are taken together, in one step: all their exits, then all their effects in region order, then all their entries. If two would exit the same state, the one in the earlier region wins, unless the other belongs to a state inside the first one's, which wins as the inner state always does. A transition from one region into another leaves the parallel state and enters it again. `always` transitions in several regions are likewise taken together, and the parallel state is done (its `done` is enabled) when every region is in a final state.
6. **Final.** Reaching a final child completes the parent, which enables the parent's `done` transition. Reaching a top-level final state ends the machine with its `output` or `fail`, evaluated on entering it; then every active state is exited, so the machine's own `exit` effects run last.

### Work is tied to states

This is where Roadbook is stricter than xstate, and it is the part that matters most for program flow.

- **An `invoke` lives exactly as long as its state.** Entering the state starts the task. Leaving the state for any reason (its own outcome, an event on a parent, a timeout) cancels it. Nothing started by a state can outlive it. Work starts once the step has settled, and only for states still active then, so a state that an `always` leaves at once never starts its task (SCXML's rule for invokes). Its arguments see every entry effect of the step.
- **A task's outcome is an event like any other.** It is queued and handled by rule 1. Errors route by tag to the matching `fail` branch. Because every declared error must be handled, an invoke cannot fail silently. An outcome that was already queued when its state was left is stale: it is recorded in the trace and ignored.
- **Timers belong to states.** `after 30s` starts when the state is entered and is cancelled when it is left. Its delay is evaluated when it starts, after the entry effects.
- **Cancelling a machine from outside exits its states.** Each state's work is cancelled and its `exit` effects run, innermost first, as on any exit. A parent leaving an invoking state cancels a child machine this way.
- **Actions are awaited.** Each `do` action finishes before the next thing happens, and actions cannot fail. Long or fallible work belongs in a task, where its failure has a visible branch.
- **Defects are not flow.** A bug in host code (a thrown exception, a panic) is not one of the declared errors. It fails the whole machine run, cancels everything it started, and propagates to whoever ran it: the parent machine's invoke, or the HTTP adapter. No `exit` effect runs. Runtime errors in the chart's own expressions (integer overflow, and reading an unassigned variable where the compile-time check `RB0403` is skipped, as for files listed in `allow-unassigned`) are defects too.

### Cancelling from outside

A machine cancelled from outside (a cancel from host code, or a parent leaving the state that invoked it) exits every active state, innermost first: each state's work is cancelled and its exit effects run, and the run ends `cancelled`. Exit effects are the only cleanup hook a chart has, and one cancel step serves both cases, so this stays as it is ([`spec/execution.md`](../../spec/execution.md#cancelling-from-outside)).

Exit effects then run while the machine is being interrupted, so they must be quick. Slow or fallible cleanup belongs in a task in a state of its own, or in the flow that leads to a final state; business cancellation is an event (`on Cancel -> Cancelled`), not a cancel from outside. Nothing enforces "quick": the TypeScript driver runs the cancel step uninterruptibly, so an exit action that hangs hangs the cancel, while in Rust host code can stop waiting and drop the run.

Three paths already end a run without exit effects: a defect, `suspend`, and dropping a Rust run or actor. TypeScript has no such hard stop; one can be added when someone needs it.

### Child machines

`invoke Checkout(order, user)` starts a child instance with those arguments. Its `output` arrives as `done` and its `fail` as the matching `fail` branch. Leaving the invoking state cancels the child and everything the child started; the child's exit effects run, and the parent waits for them before it carries on leaving the state. From the parent's chart a child machine and a task are indistinguishable, which is what lets a test replace an entire sub-machine with a single function. Only the driver tells them apart, because a child run claims the invoke context its parent's driver provides ([child runs](../../spec/execution.md#child-runs)).

### Messages between machines

Machines that exchange messages while they run, rather than only arguments and a result, are designed on the [actors and messaging](actors.md) page and specified in [`spec/execution.md`](../../spec/execution.md#messages-between-runs). In terms of the rules above:

- **A message is an input like any other.** `send` queues an event for another machine and carries on; the receiver takes it from its own queue by rule 1. `emit` queues one for the invoker (if it named the child with `as`) and for every machine holding a reference. Each is one more source pushing onto the receiver's single queue, so a message waits behind what arrived before it, and two from one sender arrive in the order sent.
- **A child's emitted events are tied to its invoke**, as its outcome is: once the invoking state is left, what the child emitted is stale. Leaving the state still cancels the child and waits for its exit effects, which may send and emit.
- **A send that reaches nothing is recorded**, as an unhandled event is: a message to a machine that has ended, or to a child that is not running, is `undelivered` in the trace.
- **The core stays pure.** A `send` or `emit` is a command, like starting a task; the driver delivers it. The core knows only its own run id and the ids its arguments hold, so it needs no registry of machines.

## Definitions and instances

A chart is a definition. Running it creates an instance. The two have very different costs, and the difference answers the question of what happens under load.

**The definition is built once.** The compiled IR and the bound implementations (`PlaceOrderLive` in TypeScript, the `App` that implements `Handlers` in Rust) are created at start-up and shared by every run. Nothing is parsed or validated per request.

**An instance is small.** It is the set of active state ids, the variables, and whatever the active states have started. `run` creates one, drives it to a final state and drops it. It does not need a queue or a handle, and in Rust it is a single future that borrows the handlers. A thousand concurrent requests are a thousand small instances of one definition, isolated from each other, each cancellable on its own and each with its own trace.

xstate works the same way: `createMachine` once, then `createActor(machine, { input })` per request and `toPromise(actor)` for the result. "One actor per machine" means one actor per running instance, never one per definition.

Which lifetime an instance has depends on what it models:

| It models | Lifetime | How |
| --- | --- | --- |
| One request or one job | The request | `run(input)`. The common case for an HTTP endpoint. |
| A service inside the process: a connection manager, an inventory, a rate limiter | The process | `spawn` once at start-up. Request machines reach it through a `ref` parameter or host code calls `send`. |
| An entity that outlives a request: an order over several days, a checkout session across several HTTP calls | The entity | One instance per id, either kept in memory in a keyed collection, or saved between events as below. |

### Saving and restoring

For the third row, an instance does not have to stay in memory. A snapshot is plain data with a schema, and the core's `step` function is pure, so a request handler can load the snapshot for an id, apply one event, wait for the machine to settle, save the new snapshot and reply:

```ts
const snapshot = yield* store.load(orderId)                       // a Roadbook.Saved, plain JSON
const next = yield* OrderLive.resume(snapshot, { _tag: "PaymentReceived", amount })
yield* store.save(orderId, next)
```

`resume` restores the run from the snapshot, gives it the event, and runs it until it is **idle**: its queue is empty and none of the tasks or child machines it started is still running. Then it stops the run, without running any exit effect, and returns the new snapshot. So the request waits for the work the event started (the shipping task a payment starts, say), but not for timers: those are saved with their deadlines, and a timer whose deadline passed while the run was saved fires as soon as it is restored, before the event. The first request of an entity spawns it and suspends it the same way (`actor.suspend`); a run that reaches a final state is returned with that status and its result. The precise rules, and the JSON Schema of the saved form, are in [`spec/execution.md`](../../spec/execution.md#saving-and-restoring).

xstate offers the same pattern, with `actor.getPersistedSnapshot()` and `createActor(machine, { snapshot })`, and with the pure `transition(machine, snapshot, event)` for servers that keep no actors at all.

Three rules make this safe.

- **A snapshot records the IR hash it was taken with**, and restoring it into a changed chart is refused unless a migration is supplied: a host function from the old snapshot to the new one, as plain data. Snapshots are the same JSON in both runtimes, so one saved by a TypeScript service restores in a Rust one.
- **A snapshot taken while a task is running restarts that task on restore**, with its arguments evaluated again, and a child machine running then starts again from the beginning. So a machine meant to be saved should wait in states that have no `invoke`, or use tasks that are safe to repeat; `resume` itself never saves a running task, since it waits for them. Surviving a crash in the middle of a task without repeating it is durable execution, which is a later driver and not part of this.
- **A restored run is a new run.** It gets a new run id, and what was queued for the old one is not in the snapshot. A machine that holds a `ref` to a peer cannot be restored yet, since the peer's id belongs to the process that saved it.

## A pure core and a driver

Each runtime is split in two.

The **core** is a pure, synchronous function. It does no I/O and has no clock:

```
step(ir, snapshot, event, fns) -> (snapshot', commands[])
```

`snapshot` is the set of active states plus the variables, and the ids of the invokes and timers that are running, so that a stale outcome can be recognised. `commands` is a list of things for the outside world to do: run an action, start or cancel a task, start or cancel a timer, send a message or an emitted event, record a trace entry. `fns` gives the core the host's pure functions, which guards and expressions may call. A `start` function creates the first snapshot, and cancelling from outside is one more input to `step`. Both are specified, with the snapshot's JSON form, in [`spec/execution.md`](../../spec/execution.md).

The **driver** owns the queue and carries out the commands using the host's concurrency: Effect fibers and scopes in TypeScript, futures in Rust. Each run has one first-in, first-out queue: external events, messages and emitted events from other runs, task outcomes, timer expiries and child runs' outcomes are all pushed onto it as they arrive, and the core takes them from it one at a time. A child run shares its parent's trace and clock through an ambient invoke context, and is cancelled through it so that its exit effects run.

This split pays for itself three ways. The core is small enough to implement twice and compare. Tests of the core need no async and no fake clock. And the TypeScript core runs in the browser, which is what powers the viewer's simulate mode.

## The trace

Every runtime can emit a trace: an ordered stream of JSON records describing what a machine instance did. One format serves debugging, the viewer's replay and coverage modes, and conformance testing. The record kinds are listed in [`spec/trace.md`](../../spec/trace.md) and checked by [`spec/trace.schema.json`](../../spec/trace.schema.json). An excerpt of a `PlaceOrder` run that times out while its `Checkout` child waits to retry:

```jsonl
{"input":{"draft":{…},"token":"tok-1"},"ir":"sha256:2481…","kind":"started","machine":"PlaceOrder","run":"r1","t":0}
{"kind":"entered","run":"r1","state":"PlaceOrder","t":0}
{"kind":"entered","run":"r1","state":"PlaceOrder.Authenticating","t":0}
{"args":{"token":"tok-1"},"invoke":1,"kind":"invoke.started","run":"r1","src":"authenticate","state":"PlaceOrder.Authenticating","t":0}
{"event":{"invoke":1,"kind":"invoke.done","value":{"id":"u1","role":"Customer"}},"kind":"event","run":"r1","t":10}
{"from":"PlaceOrder.Authenticating","index":0,"kind":"transition","run":"r1","t":10,"to":"PlaceOrder.Processing"}
{"kind":"exited","run":"r1","state":"PlaceOrder.Authenticating","t":10}
{"kind":"set","run":"r1","t":10,"value":{"id":"u1","role":"Customer"},"var":"user"}
{"kind":"entered","run":"r1","state":"PlaceOrder.Processing","t":10}
…
{"event":{"kind":"after","timer":1},"kind":"event","run":"r1","t":30010}
{"from":"PlaceOrder.Processing","index":1,"kind":"transition","run":"r1","t":30010,"to":"PlaceOrder.TimedOut"}
{"invoke":4,"kind":"invoke.cancelled","run":"r1","src":"Checkout","state":"PlaceOrder.Processing.Paying","t":30010}
…
{"kind":"done","output":{"body":{"code":"timeout"},"status":504},"run":"r1","state":"PlaceOrder.TimedOut","t":30010}
{"input":{"order":{…},"user":{…}},"ir":"sha256:fa1a…","kind":"started","machine":"Checkout","parent":"r1","run":"r1.4","t":30}
…
{"kind":"cancelled","parent":"r1","run":"r1.4","t":30010}
```

Records name states and transitions by the ids in the IR, so the viewer can light up exactly what ran. A child machine's records carry their own `run` id and the parent's: the child started by invoke 4 of run `r1` is `r1.4`, so a trace of a whole request nests the way the charts do. Within one run, records are in the order things happened. Across runs, the interleaving depends on how the host schedules fibers or futures, so traces are compared with each run's records grouped together, in run order.

Values in a trace go through the generated schemas. Fields can be marked `sensitive` in the chart to be redacted. Trace output is off by default and goes wherever the application points it: a file, stdout, an OpenTelemetry exporter (each state becomes a span, each transition a span event), or a live connection to the viewer.

## Conformance

The two cores are kept identical by a shared test suite rather than by hope. A conformance case is a chart, a script and the expected trace:

```jsonc
{
  "description": "Two gateway failures with growing backoff, then a successful charge.",
  "chart": "charts/shop/checkout.road",
  "machine": "Checkout",
  "input": { "order": { /* … */ }, "user": { /* … */ } },
  "stub": [],
  "fns": { "retryDelay": [{ "args": [1], "returns": 200 }, { "args": [2], "returns": 400 }] },
  "script": [
    { "task": "chargeCard", "fail": { "_tag": "GatewayUnavailable" } },
    { "advance": 200 },
    { "task": "chargeCard", "fail": { "_tag": "GatewayUnavailable" } },
    { "advance": 400 },
    { "task": "chargeCard", "done": { "chargeId": "c1", "amountCents": 4200 } }
  ],
  "status": "done"
}
```

Both runtimes run every case with scripted task outcomes and virtual time, and must produce the same trace (`trace.jsonl` next to the case, one canonical JSON record per line) byte for byte. A machine listed in `stub` is scripted like a task, which is how a test replaces a whole sub-machine; any other invoked machine runs from its chart. The suite lives in [`spec/conformance/`](../../spec/conformance/) and is the definition of correct behaviour. A semantic change is a change to these files, reviewed like any other.
