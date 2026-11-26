# The trace

A trace is the record of what machine runs did: one JSON object per line ([JSON Lines](https://jsonlines.org/)). Both runtimes write the same records for the same run, so one format serves debugging, the viewer's replay and coverage modes, and conformance testing. The schema is [`trace.schema.json`](trace.schema.json) (JSON Schema draft 2020-12). When each record is written is defined by [execution.md](execution.md): the core emits the records as `trace` commands, in order, and the driver writes them.

## Common members

Every record has:

| Member | Meaning |
| --- | --- |
| `t` | The driver's clock when it wrote the record, in integer milliseconds. Virtual time from 0 in conformance runs; since the Unix epoch otherwise. |
| `run` | The id of the run that wrote it. The driver names a top-level run (`r1`, `r2`, … in conformance). A child run is named `<parent run>.<invoke id>`: `r1.4` was started by invoke 4 of `r1`. A spawned child's id comes from the same sequence as invokes. |
| `parent` | The parent run's id. Present on every record of a child run (invoked or spawned), and on no record of a top-level run. |
| `kind` | The record kind, below. |

The core emits records without these members; the driver adds them. The driver also writes two kinds of record itself: `undelivered` after a `sent` record, and `suspended`.

## Records

States are named by their ids in the IR, transitions by their state's id and their index in its `transitions`, and values are in their JSON form ([values](execution.md#values)). Every member listed is present; none is ever omitted.

| `kind` | Members | Written |
| --- | --- | --- |
| `started` | `machine`, `ir`, `input` | First, when the run starts. `ir` is the machine's `hash` from the IR. `input` holds the arguments, keyed by parameter name. |
| `restored` | `machine`, `ir`, `migrated`, `snapshot` | First, instead of `started`, when the run is restored from a saved snapshot ([restoring](execution.md#restoring-a-snapshot)). `ir` is the machine's `hash`. `snapshot` is the [saved snapshot](execution.md#the-saved-snapshot) it was restored from, after any migration, so its `run` is the id the run had when it was saved. `migrated` is the hash the snapshot had before a migration, or `null`. The `spawn.started`, then `invoke.started` and `timer.started` records that follow it, with the ids the snapshot gives, restart the work that was running. |
| `entered` | `state` | When a state is entered, before its entry effects. The root is entered first and exited last. |
| `exited` | `state` | When a state is left, after its invoke and timers were cancelled and its exit effects ran. |
| `event` | `event` | When the run takes an input that enables a transition, before anything it causes. `event` is the input as the core received it (below). |
| `dropped` | `event` | Instead of `event`, when the input enables no transition. Nothing else happens. |
| `stale` | `event` | Instead of `event`, when the input is the outcome of an invoke or a spawned child, the expiry of a timer, or an event emitted by a child run, whose invoke, spawned child or timer is no longer running: it was cancelled, stopped or replaced after the input had been queued, or (for an emitted event) its outcome has been taken. Nothing else happens. |
| `transition` | `from`, `index`, `to` | When a transition is taken, before the exits it causes. `to` is the target state's id, or `null` for an internal transition. `always` and `done` transitions are traced the same way, with no `event` record before them. When one microstep takes transitions in several regions of a parallel state, their `transition` records come one after another, before all their exits ([a microstep](execution.md#a-microstep)). |
| `set` | `var`, `value` | When an assignment runs, in an exit, transition or entry effect list. |
| `action` | `action`, `args` | When an action is run, before the driver runs it. `args` is keyed by parameter name. |
| `sent` | `message`, `to`, `value` | When a `send` effect runs, before the driver queues the message. `message` is the message's id in the sending run, from 1, shared with `emitted` and `undelivered`; `to` the receiving run's id; `value` the event, a tagged record. |
| `emitted` | `message`, `value` | When an `emit` effect runs, before the driver queues the event for its receivers. Which runs received it is in their records, not here. |
| `undelivered` | `message`, `to`, `value` | A `send` that reached no run. Written by the core in place of `sent`, with `to` `null`, when the state the child is addressed through has no running invoke (it has not started yet, or its outcome has been taken), or when the collection a `send name[key].E` goes through has no running child with that key. Written by the driver right after the `sent` record, with the same members, when the receiving run has ended. |
| `invoke.started` | `invoke`, `state`, `src`, `args` | When a task or child machine is started. `invoke` is its id in the run, from 1. |
| `invoke.cancelled` | `invoke`, `state`, `src` | When a running invoke is cancelled because its state is left. An invoke whose outcome arrived is not cancelled. |
| `spawn.started` | `spawn`, `children`, `key`, `src`, `args` | When a spawned child is started, after the step that spawned it has settled, or again on restoring. `spawn` is its id in the run, from the sequence invokes take theirs from; `children` the collection, `key` its key, `src` its machine, `args` its arguments keyed by parameter name. |
| `spawn.stopped` | `spawn`, `children`, `key` | When a running spawned child is stopped: by `stop`, by a `spawn` with its key, or because its parent finished or was cancelled. A child whose outcome has been taken is not stopped; one that ended with its outcome still queued is, and that outcome is then stale. |
| `timer.started` | `timer`, `state`, `index`, `delay` | When an `after` timer is started. `index` is the transition of its `after` block that comes first, and `delay` the evaluated delay in milliseconds. |
| `timer.cancelled` | `timer`, `state`, `index` | When a running timer is cancelled because its state is left. |
| `done` | `state`, `output` | Last, when the run reaches a top-level final state with an output. `output` is `null` for a machine that returns nothing. |
| `failed` | `state`, `error` | Last, when the run reaches a top-level final state with `fail`. |
| `cancelled` | none | Last, when the run was cancelled from outside, after every state was exited. |
| `defect` | `defect` | Last, when the run ended with a [defect](execution.md#defects). When an action of the step that would have ended the run had a host defect, it comes right after that action's record, in place of the `done`, `failed` or `cancelled` record and the records between. |
| `suspended` | none | Last, written by the driver, when it stopped an idle run it had saved ([suspending](execution.md#suspending-and-resume)). The run has not ended: it goes on, as another run, from its saved snapshot. |

Exactly one of `done`, `failed`, `cancelled` and `defect` ends every run that ends. A trace of a run still in progress has none, and neither has one of a Rust run whose future was dropped ([ending a run](execution.md#ending-a-run)) or of a suspended run, which ends with `suspended`.

### Inputs in records

The `event` member of `event`, `dropped` and `stale` records is the input exactly as the core received it:

| Input | Form |
| --- | --- |
| External event | `{"kind": "event", "value": {"_tag": "Check", "amount": 5}}` |
| Message from another run | `{"kind": "message", "from": "r1", "message": 2, "value": {"_tag": "Pause"}}` |
| Emitted by a child run | `{"kind": "emitted", "from": "r1.1", "invoke": 1, "message": 1, "value": {"_tag": "Progress", "offset": 100}}` (`invoke` is the id of the invoke or spawned child) |
| Emitted by a peer | `{"kind": "emitted", "from": "r1", "invoke": null, "message": 3, "value": {"_tag": "Reserved", "orderId": "o1"}}` |
| Invoke succeeded | `{"kind": "invoke.done", "invoke": 1, "value": …}` |
| Invoke failed | `{"kind": "invoke.fail", "invoke": 1, "error": {"_tag": "GatewayUnavailable"}}` |
| Invoke ended with a defect | `{"kind": "invoke.defect", "invoke": 1, "defect": {"kind": "host", "message": "…"}}` |
| Spawned child done | `{"kind": "spawn.done", "spawn": 4, "value": …}` |
| Spawned child failed | `{"kind": "spawn.fail", "spawn": 4, "error": {"_tag": "Lost"}}` |
| Spawned child ended with a defect | `{"kind": "spawn.defect", "spawn": 4, "defect": {"kind": "host", "message": "…"}}` |
| Timer expired | `{"kind": "after", "timer": 1}` (also when `timer.fire` queued it on restoring) |

A message or emitted event names its sender by `from` and the sender's `message` id, so every received input points at exactly one `sent` or `emitted` record: the one in run `from`'s group with that `message`.

The two other inputs, `cancel` and `defect`, are not written as `event` records: the `cancelled` or `defect` record that ends the run shows them. An `invoke.defect` or `spawn.defect` input is written as an `event` record (or `stale`), followed by the run's `defect` record.

### Defects

`{"kind": "host", "message": …}`, `{"kind": "unassigned", "var": …}`, `{"kind": "overflow", "expr": …}`, `{"kind": "undeclared", "error": …}` or `{"kind": "limit"}`; see [execution.md](execution.md#defects).

## Messages

Records are enough to draw a sequence view across runs. A `send` is one `sent` record in the sender's group, with `to`, and one `event`, `dropped` or `stale` record in the receiver's, whose input has `from` and the same `message`; or `sent` followed by `undelivered` if the receiver had ended, or `undelivered` alone if there was no child to send to. An `emit` is one `emitted` record, and one input record in the group of each run it reached. A `sent` with no receiving record and no `undelivered` is a message that never reached a transition: it was still queued when the receiver ended and was discarded with the queue ([ending a run](execution.md#ending-a-run)), or it was kept for a child that never claimed its invoke (a stub) and discarded when the invoke ended.

## Order

**Within a run**, records are in the order the core emitted them, which is fixed by the run's inputs. **Across runs**, a live trace interleaves records as they are written, and that interleaving depends on how the host schedules the runs: on the Effect scheduler, or on how a Rust driver polls a child's future. It is not part of the format. A `sent` or `emitted` record is always written before the records of the runs that receive it.

To compare traces, and in every conformance case, records are **grouped by run**. The groups are ordered by run id, and each keeps its records in their order. Run ids are compared segment by segment, splitting at the dots: the first segment as a string, the others as numbers, and an id before any id it is a prefix of. That is a pre-order walk of the run tree with each run's children in the order their invokes were started, the top-level runs by id: `r1`, `r1.4`, `r1.4.2`, `r1.7`, `r2`, `r2.1`.

Grouping is used instead of a defined global schedule because it asks nothing of either driver's concurrency. A global order would force both drivers to run a child's step synchronously inside the parent's command loop, or to share one deterministic scheduler, neither of which an Effect fiber or a Rust future gives without contortion. Grouped, each run's records only need to be in that run's own order, which both get for free, and `t` still places records from different runs relative to each other. Ordering groups by id rather than by creation time means the order does not depend on when the host actually created a child either.

## Canonical form

For comparison each record is written in the [canonical JSON](ir.md#canonical-json) of RFC 8785 (in Rust, `roadbook_ir::jcs::to_canonical_string`; in TypeScript, `JSON.stringify` with keys sorted), followed by a line feed. A trace file is these lines and nothing else: no blank lines, a line feed after the last record, UTF-8. Two traces are equal when their bytes are.

## Example

A `PlaceOrder` request that times out while its `Checkout` child is waiting to retry, grouped by run (from the conformance case [`place-order-timeout`](conformance/cases/place-order-timeout/trace.jsonl), with `input` and some records left out):

```jsonl
{"input":{…},"ir":"sha256:2481…","kind":"started","machine":"PlaceOrder","run":"r1","t":0}
{"kind":"entered","run":"r1","state":"PlaceOrder","t":0}
{"kind":"entered","run":"r1","state":"PlaceOrder.Authenticating","t":0}
{"args":{"token":"tok-1"},"invoke":1,"kind":"invoke.started","run":"r1","src":"authenticate","state":"PlaceOrder.Authenticating","t":0}
{"event":{"invoke":1,"kind":"invoke.done","value":{"id":"u1","role":"Customer"}},"kind":"event","run":"r1","t":10}
{"from":"PlaceOrder.Authenticating","index":0,"kind":"transition","run":"r1","t":10,"to":"PlaceOrder.Processing"}
{"kind":"exited","run":"r1","state":"PlaceOrder.Authenticating","t":10}
{"kind":"set","run":"r1","t":10,"value":{"id":"u1","role":"Customer"},"var":"user"}
{"kind":"entered","run":"r1","state":"PlaceOrder.Processing","t":10}
{"kind":"entered","run":"r1","state":"PlaceOrder.Processing.Validating","t":10}
{"delay":30000,"index":1,"kind":"timer.started","run":"r1","state":"PlaceOrder.Processing","t":10,"timer":1}
…
{"event":{"kind":"after","timer":1},"kind":"event","run":"r1","t":30010}
{"from":"PlaceOrder.Processing","index":1,"kind":"transition","run":"r1","t":30010,"to":"PlaceOrder.TimedOut"}
{"invoke":4,"kind":"invoke.cancelled","run":"r1","src":"Checkout","state":"PlaceOrder.Processing.Paying","t":30010}
…
{"kind":"done","output":{"body":{"code":"timeout"},"status":504},"run":"r1","state":"PlaceOrder.TimedOut","t":30010}
{"input":{…},"ir":"sha256:fa1a…","kind":"started","machine":"Checkout","parent":"r1","run":"r1.4","t":30}
…
{"index":0,"kind":"timer.cancelled","parent":"r1","run":"r1.4","state":"Checkout.Backoff","t":30010,"timer":1}
{"kind":"exited","parent":"r1","run":"r1.4","state":"Checkout.Backoff","t":30010}
{"kind":"exited","parent":"r1","run":"r1.4","state":"Checkout","t":30010}
{"kind":"cancelled","parent":"r1","run":"r1.4","t":30010}
```

## Later

Fields marked `sensitive` in a chart will be redacted in records. That does not change the records above.
