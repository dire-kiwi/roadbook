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

States are named by their ids in the IR, transitions by their state's id and their index in its `transitions`, and values are in their JSON form ([values](execution.md#values)), with what the chart marks `sensitive` redacted ([redaction](#redaction)). Every member listed is present; none is ever omitted, except `as` of `invoke.started`, written only where the table says.

| `kind` | Members | Written |
| --- | --- | --- |
| `started` | `machine`, `ir`, `input` | First, when the run starts. `ir` is the machine's `hash` from the IR. `input` holds the arguments, keyed by parameter name. |
| `restored` | `machine`, `ir`, `migrated`, `snapshot` | First, instead of `started`, when the run is restored from a saved snapshot ([restoring](execution.md#restoring-a-snapshot)). `ir` is the machine's `hash`. `snapshot` is the [saved snapshot](execution.md#the-saved-snapshot) it was restored from, after any migration and without the snapshots of child runs nested in it, so its `run` is the id the run had when it was saved. A child run restored from a snapshot nested in its parent's ([restoring a child run](execution.md#restoring-a-child-run)) begins with its own `restored` record, whose `migrated` is `null`. `migrated` is the hash the snapshot had before a migration, or `null`. The `spawn.started`, then `invoke.started` and `timer.started` records that follow it, with the ids the snapshot gives, restart the work that was running. |
| `entered` | `state` | When a state is entered, before its entry effects. The root is entered first and exited last. |
| `exited` | `state` | When a state is left, after its invokes and timers were cancelled and its exit effects ran. |
| `event` | `event` | When the run takes an input that enables a transition, before anything it causes. `event` is the input as the core received it (below). |
| `dropped` | `event` | Instead of `event`, when the input enables no transition. Nothing else happens. In a strict machine an external event, a message, a call, a raised event or a delayed raise's expiry that enables no transition is an `unhandled` [defect](execution.md#defects) instead, and has no `dropped` record; a peer's end (`ended`) is dropped there too. |
| `stale` | `event` | Instead of `event`, when the input is the outcome of an invoke or a spawned child, the expiry of a timer (an `after` timer's, or a delayed raise's that was cancelled or replaced), an event emitted by a child run, or an event emitted by a task, whose invoke, spawned child (or spawned task) or timer is no longer running: it was cancelled, stopped or replaced after the input had been queued, or (for an event emitted by a child run) its outcome has been taken. A task's emit is never stale for the last reason: a task's emits are queued before its outcome, never after. Nothing else happens. |
| `transition` | `from`, `index`, `to` | When a transition is taken, before the exits it causes. `to` is the list of the transition's target ids, as the IR's `targets`: one for most transitions, several for a transition with several targets, and `[]` for one with no target. A history state's id stands for what it restores: the `entered` records after it say which states those are. Whether the transition is internal is not recorded: the IR says so, and the `exited` and `entered` records after it show what it left and entered. `always` and `done` transitions are traced the same way, with no `event` record before them. When one microstep takes transitions in several regions of a parallel state, their `transition` records come one after another, before all their exits ([a microstep](execution.md#a-microstep)). |
| `set` | `var`, `value` | When an assignment runs, in an exit, transition or entry effect list; and when a state's parameter or variable gets its value as the state is entered, after its `entered` record and before its entry effects: one record for each parameter, then for each variable that starts with a value, in the order they are declared, or for each local a history state restores ([data local to a state](execution.md#data-local-to-a-state)). `var` is a machine variable's name, or, for a state's parameter or variable, the state's id, a dot and its name: `PasswordReset.Confirming.tries`. A state that declares no locals writes none, so the traces of machines without them are unchanged. |
| `action` | `action`, `args` | When an action is run, before the driver runs it. `args` is keyed by parameter name. |
| `sent` | `message`, `to`, `value` | When a `send` effect runs, before the driver queues the message. `message` is the message's id in the sending run, from 1, shared with `emitted` and `undelivered`; `to` the receiving run's id; `value` the event, a tagged record. |
| `emitted` | `message`, `value` | When an `emit` effect runs, before the driver queues the event for its receivers. Which runs received it is in their records, not here. |
| `undelivered` | `message`, `to`, `value` | A `send` that reached no run. Written by the core in place of `sent`, with `to` `null`, when the invoke the child is addressed through is not running (it has not started yet, or its outcome has been taken), or when the collection a `send name[key].E` goes through has no running child with that key. Written by the driver right after the `sent` record, with the same members, when the receiving run has ended. |
| `invoke.started` | `invoke`, `state`, `src`, `args`, and `as` when `state` has several invokes | When a task or child machine is started, or a call is made. A task that emits is started the same way; nothing records whether it was given a live emitter. `invoke` is its id in the run, from 1. `as` is the `as` name of the invoke of `state` that was started, so a reader can tell the state's invokes apart; a state with one invoke never has it, whether or not that invoke has an `as`. For a call, `src` is the address and the call, `inventory.reserve`, and `args` is keyed by the call's parameter names. |
| `invoke.cancelled` | `invoke`, `state`, `src` | When a running invoke is cancelled because its state is left. An invoke whose outcome arrived is not cancelled. |
| `replied` | `to`, `invoke`, `call`, `outcome`, `value` | When a `reply` effect runs, before the driver answers the caller, and right after the `dropped` record of a call that enabled no transition. `to` is the caller's run id and `invoke` the caller's invoke; `outcome` is `done` or `fail`, and `value` the result (`null` for a call that returns nothing) or the error. A dropped call is answered `fail` with `{"_tag": "Unanswered"}`. |
| `spawn.started` | `spawn`, `children`, `key`, `src`, `args` | When a spawned child is started, after the step that spawned it has settled, or again on restoring. `spawn` is its id in the run, from the sequence invokes take theirs from; `children` the collection, `key` its key, `src` its machine, or its task for a collection of tasks, `args` its arguments keyed by parameter name. A spawned task has no run, so no group of records of its own. |
| `spawn.stopped` | `spawn`, `children`, `key` | When a running spawned child is stopped: by `stop`, by a `spawn` with its key, or because its parent finished or was cancelled. A child whose outcome has been taken is not stopped; one that ended with its outcome still queued is, and that outcome is then stale. |
| `timer.started` | `timer`, `state`, `index`, `delay` | When an `after` timer is started. `index` is the transition of its `after` block that comes first, and `delay` the evaluated delay in milliseconds. |
| `timer.cancelled` | `timer`, `state`, `index` | When a running timer is cancelled because its state is left. |
| `raised` | `value` | When a `raise` effect without a delay runs, as it puts the event on the internal queue. `value` is the event, a tagged record. Its `event` (or `dropped`) record comes later in the same step, with the input `raised` ([raised events](execution.md#raised-events)). |
| `raise.scheduled` | `timer`, `name`, `delay`, `value` | When a delayed raise starts, before the driver starts its timer, and again on restoring one that was not yet due. `timer` is its id, from the sequence `after` timers take theirs from; `name` its `as` name or `null`; `delay` the evaluated delay in milliseconds (the time that remains, on restoring); `value` the event. Its expiry is an `after` input with that `timer`. |
| `raise.cancelled` | `timer`, `name` | When a pending delayed raise is cancelled: by `cancel`, by a delayed raise with the same name, or because the run finished or was cancelled. |
| `done` | `state`, `output` | Last, when the run reaches a top-level final state with an output. `output` is `null` for a machine that returns nothing. |
| `failed` | `state`, `error` | Last, when the run reaches a top-level final state with `fail`. |
| `cancelled` | none | Last, when the run was cancelled from outside, after every state was exited. |
| `defect` | `defect` | Last, when the run ended with a [defect](execution.md#defects). When an action of the step that would have ended the run had a host defect, it comes right after that action's record, in place of the `done`, `failed` or `cancelled` record and the records between. |
| `suspended` | none | Last, written by the driver, when it stopped an idle run it had saved ([suspending](execution.md#suspending-and-resume)), and by each child run of its tree, which stops with it. The run has not ended: it goes on, as another run, from its saved snapshot. |

Exactly one of `done`, `failed`, `cancelled` and `defect` ends every run that ends. A trace of a run still in progress has none, and neither has one of a Rust run whose future was dropped ([ending a run](execution.md#ending-a-run)) or of a suspended run, which ends with `suspended`.

### Inputs in records

The `event` member of `event`, `dropped` and `stale` records is the input exactly as the core received it:

| Input | Form |
| --- | --- |
| External event | `{"kind": "event", "value": {"_tag": "Check", "amount": 5}}` |
| Message from another run | `{"kind": "message", "from": "r1", "message": 2, "value": {"_tag": "Pause"}}` |
| Emitted by a child run | `{"kind": "emitted", "from": "r1.1", "invoke": 1, "message": 1, "value": {"_tag": "Progress", "offset": 100}}` (`invoke` is the id of the invoke or spawned child) |
| Emitted by a peer | `{"kind": "emitted", "from": "r1", "invoke": null, "message": 3, "value": {"_tag": "Reserved", "orderId": "o1"}}` |
| Emitted by a task | `{"kind": "task.emitted", "invoke": 3, "value": {"_tag": "Progress", "percent": 50}}` (`invoke` is the id of the invoke or spawned task) |
| Call from another run | `{"kind": "call", "from": "r2", "invoke": 1, "call": "reserve", "args": {"orderId": "o1", "items": […]}}` |
| Invoke succeeded | `{"kind": "invoke.done", "invoke": 1, "value": …}` |
| Invoke failed | `{"kind": "invoke.fail", "invoke": 1, "error": {"_tag": "GatewayUnavailable"}}` |
| Invoke ended with a defect | `{"kind": "invoke.defect", "invoke": 1, "defect": {"kind": "host", "message": "…"}}` |
| Spawned child done | `{"kind": "spawn.done", "spawn": 4, "value": …}` |
| Spawned child failed | `{"kind": "spawn.fail", "spawn": 4, "error": {"_tag": "Lost"}}` |
| Spawned child ended with a defect | `{"kind": "spawn.defect", "spawn": 4, "defect": {"kind": "host", "message": "…"}}` |
| Timer expired | `{"kind": "after", "timer": 1}` (also when `timer.fire` queued it on restoring, and for a delayed raise, whose `raise.scheduled` record has that `timer`) |
| Raised by the run | `{"kind": "raised", "value": {"_tag": "Ping"}}` (taken from the internal queue in the step that raised it, after its `raised` record) |
| A peer ended | `{"kind": "ended", "from": "r1", "status": "Done"}` (`status` is `Done`, `Failed`, `Cancelled`, `Defect` or `Suspended`) |

A message or emitted event names its sender by `from` and the sender's `message` id, so every received input points at exactly one `sent` or `emitted` record: the one in run `from`'s group with that `message`. A task's emit has neither: a task is not a run and writes no records, so its input points only at the `invoke.started` or `spawn.started` record, in the run's own group, with that id.

A raised event has no sender: it points at the latest `raised` record before it in the run's own group with that value, in the same step. A delayed raise's expiry points at the `raise.scheduled` record with its `timer`.

A peer's end has no record of its own in the peer's group: it points at the record that ends run `from`'s group, `done`, `failed`, `cancelled`, `defect` or `suspended`, which `status` names (`Done`, `Failed`, `Cancelled`, `Defect`, `Suspended`). It is an `event` or a `dropped` record, never `stale`, and only in the group of a run whose machine has an `on name.ended` for the peer's machine ([delivery](execution.md#delivering-messages)): a holder that has none writes nothing. Nothing in it is redacted.

The two other inputs, `cancel` and `defect`, are not written as `event` records: the `cancelled` or `defect` record that ends the run shows them. An `invoke.defect` or `spawn.defect` input is written as an `event` record (or `stale`), followed by the run's `defect` record.

### Defects

`{"kind": "host", "message": …}`, `{"kind": "unassigned", "var": …}`, `{"kind": "overflow", "expr": …}`, `{"kind": "undeclared", "error": …}`, `{"kind": "limit"}`, `{"kind": "unhandled", "event": …}` or `{"kind": "answered", "call": …}`; see [execution.md](execution.md#defects).

## Messages

Records are enough to draw a sequence view across runs. A `send` is one `sent` record in the sender's group, with `to`, and one `event`, `dropped` or `stale` record in the receiver's, whose input has `from` and the same `message`; or `sent` followed by `undelivered` if the receiver had ended, or `undelivered` alone if there was no child to send to. An `emit` is one `emitted` record, and one input record in the group of each run it reached. **A task's emit** has no record of its own: it is the one input record, `event`, `dropped` or `stale`, with a `task.emitted` input, in the group of the run that invoked or spawned the task. One the run's machine does not hear, or one made by a task invoked without `as`, leaves no record at all. A `sent` with no receiving record and no `undelivered` is a message that never reached a transition: it was still queued when the receiver ended and was discarded with the queue ([ending a run](execution.md#ending-a-run)), or it was kept for a child that never claimed its invoke (a stub) and discarded when the invoke ended.

A **peer's end** is the record that ends the peer's group, and one `event` or `dropped` record with an `ended` input, `from` the peer, in the group of each holder that hears it. It is the last input record from that peer in the holder's group.

A **call** is an `invoke.started` record in the caller's group whose `src` names the address and the call, and an `event` or `dropped` record in the callee's whose input has `kind` `call`, `from` the caller and the same `invoke`. Its answer is a `replied` record in the callee's group with `to` the caller and that `invoke`, and the `invoke.done` or `invoke.fail` input record in the caller's (or `stale`, if the caller had left the state). A call to a callee that was not open, or that ended before answering, has only the caller's records: its `invoke.fail` input carries `Unanswered`, and the callee writes nothing about it.

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
{"from":"PlaceOrder.Authenticating","index":0,"kind":"transition","run":"r1","t":10,"to":["PlaceOrder.Processing"]}
{"kind":"exited","run":"r1","state":"PlaceOrder.Authenticating","t":10}
{"kind":"set","run":"r1","t":10,"value":{"id":"u1","role":"Customer"},"var":"user"}
{"kind":"entered","run":"r1","state":"PlaceOrder.Processing","t":10}
{"kind":"entered","run":"r1","state":"PlaceOrder.Processing.Validating","t":10}
{"delay":30000,"index":1,"kind":"timer.started","run":"r1","state":"PlaceOrder.Processing","t":10,"timer":1}
…
{"event":{"kind":"after","timer":1},"kind":"event","run":"r1","t":30010}
{"from":"PlaceOrder.Processing","index":1,"kind":"transition","run":"r1","t":30010,"to":["PlaceOrder.TimedOut"]}
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

## Redaction

A chart marks the fields, parameters and variables whose values must not appear in a trace as `sensitive` ([language](../docs/design/language.md#sensitive-values)), and the IR carries the mark ([`sensitive`](ir.md#fields-and-parameters)). Every value a record shows of one of them is replaced by the JSON string `"<redacted>"`. The records keep the members and the form above: only values change, and a trace of a chart that marks nothing is exactly what it was before marks existed.

**Where.** The core redacts each record as it emits it ([execution](execution.md#redaction)), so both runtimes write the same bytes, and no driver, sink or exporter ever holds the real value of a record. Records are the only thing redacted: the core's snapshot, the commands (an action's and an invoke's arguments, the value of a `send` or `emit`), what host code receives and returns, and **saved snapshots keep the real values**, since a run restored from one must go on with them. A saved snapshot is as sensitive as the data it holds; store it accordingly. Only the copy of it in a `restored` record is redacted, and that copy cannot be restored.

**By type.** A value is redacted by its declared type, from the IR, through aliases (in any file or package). A type **holds** a sensitive value if it is a record or error with a sensitive field, or with a field whose type holds one, or a list, optional or map of such a type, or an alias of one; or an instance of a generic type that holds one whatever its type arguments (a field of a type parameter's type holds none of its own), or one with a type argument that holds one. Then:

- a value of a sensitive field, parameter or variable is `"<redacted>"` whole, whatever its type, `null`, a list or a record included;
- a value of a type that holds no sensitive value is left as it is: `string`, `int`, `float`, `bool`, `duration`, `json`, an enum, an `extern` type, a `ref`, and records and lists without marks at any depth. A trace of a chart that marks nothing is unchanged;
- a record or error that holds one: each member that is one of its sensitive fields is `"<redacted>"`, each other declared field is redacted by its type, `_tag` is left as it is, and **any other member is `"<redacted>"`**;
- a list that holds one: each item, by the item type. An optional: `null` is left as it is, a value is redacted by the inner type. A map: each value, by the value type, its keys left as they are;
- an instance of a generic type (`Page<Login>`): as its declaration, with each type parameter replaced by its argument;
- **a value without the shape of a type that holds one** (not an object where such a record or map is expected, not an array where such a list is) is `"<redacted>"` whole.

The last two rules are for values a runtime is not meant to see at all: host code that bypasses the generated types (raw JSON in Rust's dynamic API) can give one, and what it gives is not shown. Records' argument objects (`input`, `args`, a snapshot's `input` and `vars`, and each state's member of its `locals` and `historyLocals`) are redacted like records whose fields are the parameters or variables: a member that is none of them is `"<redacted>"`.

**Which type.** Each record member is redacted as the value of:

| Record | Member | Type |
| --- | --- | --- |
| `started` | `input` | each by its parameter of the machine |
| `restored` | `snapshot` | its `input` by the machine's parameters, its `vars` by its variables, each member of `locals` by the parameters and variables of its state, each member of `historyLocals` likewise, state by state, each `spawned` child's `args` by the parameters of its collection's machine or task, each `scheduled` raise's `value` as the run's own event of its `_tag`, a `result`'s `output` by the machine's result type and its `error` by its tag; nothing else in it is a value |
| `set` | `value` | the variable, or for a `var` of the form `<state id>.<name>` that state's parameter or variable (a variable the machine or the state does not declare: `"<redacted>"`) |
| `action` | `args` | each by its parameter of the action |
| `invoke.started` | `args` | each by its parameter of the task or machine, or for a call invoke (`src` `inventory.reserve`) of the call on the `ref`'s machine |
| `spawn.started` | `args` | each by its parameter of the machine or task (`src`); `key` is shown as it is: the compiler rejects a sensitive key (`RB0320`) |
| `sent`, `undelivered` | `value` | the receiving machine's event of its `_tag` (the address's machine) |
| `emitted` | `value` | the run's own emitted event of its `_tag` |
| `raised`, `raise.scheduled` | `value` | the run's own event or internal event of its `_tag` |
| `done` | `output` | the machine's result type (for a machine that returns nothing, anything but `null` is `"<redacted>"`) |
| `failed` | `error` | the error type its `_tag` names |
| `replied` | `value` | for `done`, the call's result type (for a call that returns nothing, anything but `null` is `"<redacted>"`); for `fail`, the error type its `_tag` names |
| `event`, `dropped`, `stale` | `event` | by the input's kind, below |
| `defect` | `defect` | an `unhandled` defect's `event` as an `event` record's input, below; nothing else in a defect is a value |

The `undelivered` record a driver writes repeats the value of the `sent` record before it, which the core has redacted.

In an input, an `event`, `message` or `raised` value is redacted as the run's own event (or internal event) of its `_tag`; a `call`'s `args` each by its parameter of the run's own call of that name; an `invoke.fail` error as the error type its `_tag` names; an `invoke.done` value as the result type of the running invoke's task, machine or call (for one that returns nothing, anything but `null` is `"<redacted>"`); a `spawn.fail` error as the error type its `_tag` names, and a `spawn.done` value as the result type of the spawned child's machine or task (one whose child is no longer in the snapshot, a stale one, is `"<redacted>"` whole, as for `invoke.done`); a `task.emitted` value as the emitted event of its `_tag` in the `emits` of the task that its invoke or spawned task runs, `task.E`. An `ended` input holds no value, and is shown as it is. A tagged value (an event, an error) is treated as a record of its declaration, if that holds a sensitive value. One of a `_tag` nothing declares has every member but `_tag` redacted, and one that is not an object with a string `_tag` is `"<redacted>"` whole. Three inputs come from something the receiving run cannot always name:

- an `emitted` value is redacted under the emitted event of its `_tag` of **every** machine of the IR that emits one whose fields are exactly the value's members (but `_tag`), one after the other in IR order. A holder of a `ref` does not know which machine sent it, and a parent may have left the invoking state; the sender's own declaration is always among them, since a core builds the event with every field it declares. If none fits, every member but `_tag` is redacted.
- a `stale` `task.emitted` value, whose invoke or spawned task is no longer in the snapshot, is redacted under the emitted event of its `_tag` of **every** task of the IR that emits one whose fields are exactly the value's members (but `_tag`), one after the other in IR order, as an `emitted` value is under every machine's. If none fits, every member but `_tag` is redacted. The run ignores it anyway, and a chart that marks nothing has it shown as it is.
- a `stale` `invoke.done` value is `"<redacted>"` whole: its invoke has ended, and with it what it ran, so its type is not known. The run ignores it anyway.

**What it does not cover.** A function's result is not sensitive: a `fn` is how a chart derives what may be shown, so `set shown = describe(password)` writes what `describe` returns. (An operator's result with a sensitive operand is sensitive: the compiler rejects `output pin + 0` as it rejects `output pin`, `RB0320`.) A host defect's `message` is whatever host code raised, a function's or task's exception included, and is written as it is; so is a defect's `expr` (a chart's own text). The runtimes' own messages about a value that does not decode name the place and the kind of JSON, never the value. A machine's hash leaves its marks out ([the machine hash](ir.md#the-machine-hash)), so two builds of a chart that differ only in marks pass the handle checks between runs: each run redacts by the IR it was compiled with, and a run built without a peer's marks shows what that peer marks.

