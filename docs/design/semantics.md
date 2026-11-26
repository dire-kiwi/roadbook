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

An abridged IR for `Checkout` (`/* … */` marks what is left out; the spike emits an earlier draft of this shape, without `type` on expressions, end positions in spans or the hash):

```jsonc
{
  "irVersion": "0.1",
  "sources": [{ "path": "examples/shop/checkout.road" }, { "path": "examples/shop/types.road" }],
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
      "events": {},
      "root": {
        "id": "Checkout", "name": "Checkout", "kind": "compound", "doc": "Charge the customer's card, …",
        "span": { /* the machine's */ }, "initial": "Checkout.Charging",
        "states": [
          {
            "id": "Checkout.Charging", "name": "Charging", "kind": "atomic",
            "doc": "One attempt against the payment gateway.",
            "span": { "file": 0, "line": 12, "col": 3, "endLine": 21, "endCol": 4 },
            "initial": null, "states": [], "entry": [], "exit": [],
            "invoke": { "kind": "task", "src": "chargeCard", "args": [/* order, user */],
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

Roadbook machines are statecharts with the semantics of SCXML, the W3C standard that xstate also follows. Anyone who knows xstate will find no surprises. The rules, briefly:

1. **One event at a time.** A machine has a queue. It takes one event, does everything that event causes, and only then takes the next (run to completion).
2. **A step.** Find the active states that have a transition for the event, innermost first. Evaluate guards in document order and take the first that holds. Exit states from the innermost outward, run the transition's effects, enter states from the outermost inward. Within each: `set` assignments first, then `do` actions, in the order written.
3. **Settling.** After a step, `always` transitions and `done` transitions are taken until none applies. The machine is then stable and takes the next event.
4. **Unhandled events are dropped and recorded.** An event with no enabled transition changes nothing. It appears in the trace as dropped, so it is visible. A machine can opt into `strict`, which turns a dropped event into a failure.
5. **Parallel states** are active in all regions at once. Each region may take a transition for the same event. If two would exit the same state, the one in the earlier region wins.
6. **Final.** Reaching a final child completes the parent, which enables the parent's `done` transition. Reaching a top-level final state ends the machine with its `output` or `fail`.

### Work is tied to states

This is where Roadbook is stricter than xstate, and it is the part that matters most for program flow.

- **An `invoke` lives exactly as long as its state.** Entering the state starts the task. Leaving the state for any reason (its own outcome, an event on a parent, a timeout) cancels it. Nothing started by a state can outlive it.
- **A task's outcome is an event like any other.** It is queued and handled by rule 1. Errors route by tag to the matching `fail` branch. Because every declared error must be handled, an invoke cannot fail silently.
- **Timers belong to states.** `after 30s` starts when the state is entered and is cancelled when it is left.
- **Actions are awaited.** Each `do` action finishes before the next thing happens, and actions cannot fail. Long or fallible work belongs in a task, where its failure has a visible branch.
- **Defects are not flow.** A bug in host code (a thrown exception, a panic) is not one of the declared errors. It fails the whole machine run, cancels everything it started, and propagates to whoever ran it: the parent machine's invoke, or the HTTP adapter.

### Child machines

`invoke Checkout(order, user)` starts a child instance with those arguments. Its `output` arrives as `done` and its `fail` as the matching `fail` branch. Leaving the invoking state cancels the child and everything the child started. From the parent's side a child machine and a task are indistinguishable, which is what lets a test replace an entire sub-machine with a single function.

Machines that exchange messages while they run, rather than only arguments and a result, are covered on the [actors and messaging](actors.md) page.

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
const snapshot = yield* store.load(orderId)                       // decoded with the generated schema
const next = yield* OrderLive.resume(snapshot, { _tag: "PaymentReceived", amount })
yield* store.save(orderId, next)
```

xstate offers the same pattern, with `actor.getPersistedSnapshot()` and `createActor(machine, { snapshot })`, and with the pure `transition(machine, snapshot, event)` for servers that keep no actors at all.

Two rules make this safe. A snapshot records the IR hash it was taken with, and restoring it into a changed chart is refused unless a migration is supplied. And a snapshot taken while a task is running restarts that task on restore, so a machine meant to be saved should wait in states that have no `invoke`, or use tasks that are safe to repeat. Surviving a crash in the middle of a task without repeating it is durable execution, which is a later driver and not part of this.

## A pure core and a driver

Each runtime is split in two.

The **core** is a pure, synchronous function. It does no I/O and has no clock:

```
step(ir, snapshot, event, fns) -> (snapshot', commands[])
```

`snapshot` is the set of active states plus the variables. `commands` is a list of things for the outside world to do: run an action, start or cancel a task, start or cancel a timer, record a trace entry. `fns` gives the core the host's pure functions, which guards and expressions may call.

The **driver** owns the queue and carries out the commands using the host's concurrency: Effect fibers and scopes in TypeScript, futures in Rust. It feeds task outcomes and timer expiries back into the core as events.

This split pays for itself three ways. The core is small enough to implement twice and compare. Tests of the core need no async and no fake clock. And the TypeScript core runs in the browser, which is what powers the viewer's simulate mode.

## The trace

Every runtime can emit a trace: an ordered stream of JSON records describing what a machine instance did. One format serves debugging, the viewer's replay and coverage modes, and conformance testing.

```jsonl
{"t":0,"run":"r1","machine":"PlaceOrder","ir":"sha256:9f2c…","kind":"started","input":{"token":"…"}}
{"t":0,"run":"r1","kind":"entered","state":"PlaceOrder.Authenticating"}
{"t":0,"run":"r1","kind":"invoke.started","state":"PlaceOrder.Authenticating","src":"authenticate"}
{"t":12,"run":"r1","kind":"event","event":{"kind":"invoke.done","src":"authenticate"}}
{"t":12,"run":"r1","kind":"transition","from":"PlaceOrder.Authenticating","to":"PlaceOrder.Processing","index":0,"set":{"user":{"id":"u1","role":"Customer"}}}
{"t":12,"run":"r1","kind":"entered","state":"PlaceOrder.Processing"}
{"t":30012,"run":"r1","kind":"event","event":{"kind":"after","state":"PlaceOrder.Processing"}}
{"t":30012,"run":"r1","kind":"invoke.cancelled","state":"PlaceOrder.Processing.Paying","src":"Checkout","child":"r2"}
{"t":30012,"run":"r1","kind":"done","state":"PlaceOrder.TimedOut","output":{"status":504,"body":{"code":"timeout"}}}
```

Records name states and transitions by the ids in the IR, so the viewer can light up exactly what ran. A child machine's records carry their own `run` id and the parent's, so a trace of a whole request nests the way the charts do.

Values in a trace go through the generated schemas. Fields can be marked `sensitive` in the chart to be redacted. Trace output is off by default and goes wherever the application points it: a file, stdout, an OpenTelemetry exporter (each state becomes a span, each transition a span event), or a live connection to the viewer.

## Conformance

The two cores are kept identical by a shared test suite rather than by hope. A conformance case is a chart, a script and the expected trace:

```jsonc
{
  "chart": "retry.road",
  "machine": "Checkout",
  "input": { "order": { /* … */ }, "user": { /* … */ } },
  "fns": { "retryDelay": [{ "args": [1], "returns": 200 }] },
  "script": [
    { "task": "chargeCard", "fail": { "_tag": "GatewayUnavailable" } },
    { "advance": 200 },
    { "task": "chargeCard", "done": { "chargeId": "c1", "amountCents": 4200 } }
  ],
  "expect": "retry.trace.jsonl"
}
```

Both runtimes run every case with scripted task outcomes and virtual time, and must produce the same trace byte for byte. The suite lives in `spec/conformance/` and is the definition of correct behaviour. A semantic change is a change to these files, reviewed like any other.
