# Execution

This page is the algorithm both runtimes implement: what the pure core does with a snapshot and an input, what it asks the driver to do, and how the driver carries that out. It is precise enough that two independent implementations produce the same [trace](trace.md) byte for byte, and the [conformance cases](conformance/) hold them to it. The design rationale is in [semantics](../docs/design/semantics.md); this page only says what happens.

It covers atomic, compound, parallel and final states, events, guards, `invoke` of tasks and machines, `after`, `always` and `done`, [messages between runs](#messages-between-runs) (`send`, `emit`, `on name.Event` and `ref`, milestone 5), [calls between runs](#calls-between-runs) (`call`, `invoke name.call` and `reply`), [saving and restoring](#saving-and-restoring) a run (milestone 6) together with its [child runs](#saved-child-runs), [history states](#history-states), [spawned children](#spawned-children) (keyed `spawn`), [tasks that emit](#tasks-that-emit) (`emits` on a task, stream tasks and spawned tasks), [events a machine raises](#raised-events) (`raise`, delayed `raise … after`, `cancel` and `internal event`), and [data local to a state](#data-local-to-a-state) (a state's parameters and variables). Parallel states, history states, spawned children, tasks that emit, raised events and state locals change nothing for a machine that has none: every rule below reduces to the one it replaced, and the traces of such machines are unchanged.

## Terms

- A **run** is one instance of a machine, from `start` until it finishes. A run's **status** is `running`, `done`, `failed`, `cancelled` or `defect`; only `running` is not final.
- The **active states** are the states the run is in. They form a tree from the root (the machine itself, a compound state whose id is the machine's name): an active compound state has exactly one active child, and an active parallel state has all its children active. Without parallel states the tree is a chain from the root down to one atomic or final state. The root is active for the whole run.
- A **history state** is a state of kind `history` (shallow) or `deepHistory` (deep): a pseudo-state in the `states` of a compound or parallel state other than the root, its **parent**. It is never active and has no children, effects, work or transitions; it is only ever a transition's target ([history states](#history-states)). On this page the **children** of a state are the states in its `states` that are not history states, so a history state is never a region, an initial state, an active state or an active leaf.
- A **region** is a child of a parallel state. A region may be a state of any kind: compound, parallel, atomic or final.
- An **active leaf** is an active state with no active child: an atomic or final state. Without parallel states there is exactly one.
- **Document order** is the order of a pre-order walk of the IR's state tree: a state comes before its children, and children are in `states` order. **Innermost first** means reverse document order; for a chain it is leaf to root.
- An **internal event** is one of the machine's `internalEvents` (`internal event E`): an event the machine raises itself, which host code and other runs cannot send. The machine's **own events** are its `events` and its `internalEvents`.
- A **raised event** is one of the machine's own events that a `raise` effect puts on the run's **internal queue**, to be taken before the step ends ([raised events](#raised-events)). A **delayed raise** (`raise E after d as n`) is a timer that belongs to the run, not to a state: when it expires its event is taken as an input. Its optional `name` lets a `cancel` effect stop it.
- A state's **locals** are its parameters (`params` in the IR) and its variables (`vars`): data that exists only while the state is active. Each time the state is entered its parameters get the values the transition that enters it passes, its **arguments**, and its variables their initial values; when it is left they are gone ([data local to a state](#data-local-to-a-state)). The root has none: the machine's parameters and variables are `input` and `vars`.
- An **input** is what the driver gives the core: an external event, a message, an emitted event or a call from another run, an event a task emitted, a peer's end, an invoke's or a spawned child's outcome, a timer's expiry (an `after` timer's or a delayed raise's), or a request to cancel or to fail with a defect.
- A **top-level run** is one that host code started (`run`, `spawn`), as opposed to a **child run**, which an `invoke` or a `spawn` effect of its parent started ([child runs](#child-runs)). A **peer** of a run is a top-level run it holds a `ref` to.
- A **spawned child** is a child run that a `spawn` effect started into one of the machine's `children` collections, under a key ([spawned children](#spawned-children)), or, for a collection of tasks, a **spawned task**: a task started the same way, which is not a run. Unlike an invoked child it belongs to the run, not to a state: it runs until it ends by itself, until a `stop` effect stops it, until a `spawn` with the same key replaces it, or until its parent ends.
- A **message** is an event one run sends to another with `send`. An **emitted event** is one a run emits with `emit`, for its invoker and the runs that hold a `ref` to it. Both are queued for the receiver like any input ([messages between runs](#messages-between-runs)). A **task's emit** is one of the events a task declares in its `emits`, which the task's host implementation gives the driver while it runs, for the run that invoked it with `as` or spawned it ([tasks that emit](#tasks-that-emit)).
- A **call** is a request one run, the **caller**, makes of a peer, the **callee**, with `invoke name.c(…)`, where `c` is one of the callee's `calls`. The callee takes it as an input and answers it with `reply`; the answer is the outcome of the caller's invoke ([calls between runs](#calls-between-runs)).
- A **step** is what the core does with one input (or with `start`). It is a **macrostep** in SCXML's terms: one **microstep** for the input's transitions, if any, then the microsteps of [settling](#settling), then [starting work](#starting-work). A microstep takes a set of transitions together: one, except when one input or one round of `always` enables transitions in several regions of a parallel state ([selecting](#selecting-a-transition)).
- `parent(s)` is the state whose `states` contains `s`. A **proper ancestor** of `s` is `parent(s)`, its parent, and so on up to the root. A **proper descendant** is the converse.
- A state is **in a final state** when it is a final state; when it is a compound state whose active child is a final state; or when it is a parallel state whose children are all active and all in a final state. An atomic state never is, nor is a state that is not active. (This is SCXML's `isInFinalState`.)

## The core

The core is two pure, synchronous functions. They do no I/O and read no clock.

```
start(ir, machine, run, input, fns)                  -> (snapshot, commands)
step(ir, snapshot, input, fns)                       -> (snapshot, commands)
restore(ir, saved, run, now, migrated, fns)          -> (snapshot, commands)
```

`restore` starts a run again from a saved snapshot; it is specified under [saving and restoring](#restoring-a-snapshot).

`ir` is the whole IR document, `machine` the name of the machine to run and `run` the run's id ([run ids](#the-driver)), which the core needs only to name its child runs in `sent` records. `input` (for `start`) is the machine's arguments as an object keyed by parameter name. `fns(name, args)` calls the host's pure function `name` with positional argument values and returns its value, or a `host` [defect](#defects) if the host code threw, panicked or returned a value that is not of the declared type. The binding layer turns the positional list into whatever the host implementation takes (an object of named parameters in TypeScript, arguments in Rust), decodes the result, and catches what the host code throws (TypeScript `try`/`catch`, Rust `std::panic::catch_unwind`), so nothing ever unwinds through the core. `snapshot` is never modified in place: `step` returns a new one.

`commands` is a list the driver carries out in order. It is the core's only way to affect the world. Trace records are commands too, so the trace interleaves with the work exactly as the core emitted it.

### Snapshot

A snapshot is plain JSON, so both runtimes have the same shape and a snapshot can be stored and compared.

```jsonc
{
  "machine": "Checkout",
  "run": "r1",                                   // the run's id
  "ir": "sha256:fa1a…",                         // the machine's hash from the IR
  "status": "running",                           // running | done | failed | cancelled | defect
  "active": ["Checkout", "Checkout.Backoff"],    // active state ids, in document order
  "input": { "order": { /* … */ }, "user": { /* … */ } },
  "vars": { "attempts": 1 },                     // assigned variables only
  "locals": {},                                  // the active states' parameters and variables
  "invokes": [],                                 // running invokes
  "calls": [],                                   // calls taken and not answered
  "spawned": [],                                 // running spawned children
  "timers": [{ "id": 1, "state": "Checkout.Backoff", "index": 0, "delay": 200 }],
  "scheduled": [],                              // pending delayed raises
  "nextInvoke": 2,
  "nextTimer": 2,
  "nextMessage": 1,
  "history": {},
  "historyLocals": {},
  "result": null
}
```

| Member | Meaning |
| --- | --- |
| `machine`, `ir` | The machine's name and its `hash`. |
| `run` | The run's id, as given to `start`. |
| `status` | The run's status. |
| `active` | The active states' ids in document order. `[]` once the run is `done`, `failed` or `cancelled`. When the run ends with a `defect`, left as it was in the snapshot the defect was applied to, to show roughly where it stopped (`[]` if that snapshot had already finished). |
| `input` | The arguments, keyed by parameter name. Parameters are read from here. A `ref` is the id of the run it refers to ([values](#values)). |
| `vars` | Every variable that has a value, keyed by name. A variable with no value yet has no member, which is different from an optional variable holding `null`. An optional variable always has a member, since it starts as `null` when it has no initial value. |
| `locals` | The locals of the active states, keyed by state id, in document order: one member for each active state that declares a parameter or a variable, an object keyed by name holding those of its locals that have a value, as `vars` does (a parameter always has one). A state that declares none has no member. `{}` once the run is `done`, `failed` or `cancelled`; when it ends with a `defect`, left as it was, as `active` is ([data local to a state](#data-local-to-a-state)). |
| `invokes` | `[{"id", "state", "src"}]`: the invokes that are running, in the order they were started. `state` is the id of the state that owns it, `src` the task's or machine's name, or for a call the address and the call, `inventory.reserve`. An entry whose state has several invokes also has `as`, the `as` of the one it runs: `{"id", "state", "src", "as"}`. The entry's **invoke** is the invoke of its state with that `as`, or the state's one invoke for an entry without `as`. |
| `calls` | `[{"from", "invoke", "call"}]`: the calls the run has taken and not answered yet, its **pending calls**, in the order it took them. `from` is the caller's run id, `invoke` the id of the caller's invoke, and `call` the call's name ([calls between runs](#calls-between-runs)). |
| `spawned` | `[{"id", "children", "key", "args"}]`: the spawned children that are running, spawned tasks among them, in the order they were started. `children` is the name of the collection, `key` the child's key and `args` its arguments, keyed by parameter name, as they were evaluated when it was spawned ([spawned children](#spawned-children)). |
| `timers` | `[{"id", "state", "index", "delay"}]`: the `after` timers that are running, in the order they were started. `index` is the position in the state's `transitions` of the first transition of the timer's `after` block, and `delay` the evaluated delay in milliseconds. |
| `scheduled` | `[{"id", "name", "delay", "value"}]`: the run's pending **delayed raises**, in the order they were started ([raised events](#raised-events)). `id` is a timer id, from the sequence `after` timers take theirs from, `name` the raise's `as` name or `null`, `delay` the evaluated delay in milliseconds, and `value` the event, a tagged record of one of the machine's own events. |
| `nextInvoke`, `nextTimer` | The id the next invoke or timer will get. Both start at 1. Ids are never reused within a run, which is what lets the core recognise a stale outcome. `after` timers and delayed raises take their ids from the one sequence `nextTimer`, so a timer id names one or the other. Invokes and spawned children take their ids from the one sequence `nextInvoke`, so an id names one or the other, and the child run of either, when it runs a machine, is `<run>.<id>`. |
| `nextMessage` | The id the next `send` or `emit` effect will get, from 1. A message is named in traces by its sender's run id and this id. |
| `history` | What each history state has recorded ([history states](#history-states)), keyed by the history state's id: a list of state ids, in document order. A history state whose parent no microstep has exited yet has no member. Starts as `{}`. |
| `historyLocals` | The locals each history state recorded with the states it will re-enter, keyed by the history state's id, then by the id of such a state that declares a local, then by name ([history states](#history-states)). A history state whose record holds no state with locals has no member. Starts as `{}`. |
| `result` | `null` while running and when cancelled. `{"output": value}` when done (`value` is `null` for a machine that returns nothing), `{"error": value}` when failed, `{"defect": defect}` when the run ended with a [defect](#defects). |

A **saved snapshot**, the form host code stores, is this snapshot with a format version and the due time of each timer and delayed raise added ([saving and restoring](#saving-and-restoring)).

Within a macrostep the core also keeps three transient values that are not part of the snapshot: the set of states **entered during this macrostep**, the **internal queue**, which holds, in the order they were queued, the **completions** of compound and parallel states that reached a final state and have not been exited since and the **raised events** of `raise` effects ([raised events](#raised-events)), and the **pending spawns**, the children spawned during this macrostep that have not started yet (`[{"children", "key", "args"}]`, in the order spawned).

### Inputs

| Input | Meaning |
| --- | --- |
| `{"kind": "event", "value": {"_tag": "Cancel", …}}` | An external event: one of the machine's `events`, encoded as a tagged record (see [values](#values)), given by host code. |
| `{"kind": "message", "from": "r2", "message": 1, "value": {"_tag": "Reserve", …}}` | A message: run `r2` sent one of this machine's `events` with `send`, as its message 1. |
| `{"kind": "emitted", "from": "r1.3", "invoke": 3, "message": 2, "value": {"_tag": "Progress", …}}` | An emitted event: run `r1.3` emitted one of its machine's `emits` as its message 2. `invoke` is the id of this run's invoke or spawned child that is `r1.3` when this run is its parent, and `null` when this run holds a `ref` to it. |
| `{"kind": "task.emitted", "invoke": 3, "value": {"_tag": "Progress", …}}` | A task's emit: the task of this run's invoke or spawned task 3 emitted one of its task's `emits` while it ran ([tasks that emit](#tasks-that-emit)). Given only for an invoke with `as` or a spawned task, and only for an event the run's machine hears from that task. |
| `{"kind": "call", "from": "r2", "invoke": 1, "call": "reserve", "args": {…}}` | A call: run `r2` called this machine's call `reserve` with its invoke 1. `args` is keyed by the call's parameter names. |
| `{"kind": "ended", "from": "r1", "status": "Done"}` | A peer's end: run `r1`, which this run holds a `ref` to, has closed. `status`, a variant of the built-in enum `Ended`, says how: `Done`, `Failed`, `Cancelled` or `Defect` for a run that ended with status `done`, `failed`, `cancelled` or `defect`, and `Suspended` for one that was [suspended](#suspending-and-resume). Given only to a run whose machine hears `ended` from `r1`'s machine ([messages between runs](#messages-between-runs)), at most once per peer. |
| `{"kind": "invoke.done", "invoke": 1, "value": v}` | Invoke 1 succeeded with `v` (`null` for a task, machine or call that returns nothing). |
| `{"kind": "invoke.fail", "invoke": 1, "error": e}` | Invoke 1 failed with the declared error `e`, a tagged record, or, for a call, with `{"_tag": "Unanswered"}`. |
| `{"kind": "invoke.defect", "invoke": 1, "defect": d}` | Invoke 1 ended with a [defect](#defects): a host defect in a task, or a child run that ended with a defect. |
| `{"kind": "spawn.done", "spawn": 4, "value": v}` | Spawned child 4 ended `done` with output `v` (`null` for a machine or task that returns nothing). |
| `{"kind": "spawn.fail", "spawn": 4, "error": e}` | Spawned child 4 ended `failed` with the declared error `e`. |
| `{"kind": "spawn.defect", "spawn": 4, "defect": d}` | Spawned child 4 ended with a defect: a child run that ended with one, or a host defect in a spawned task. |
| `{"kind": "after", "timer": 1}` | Timer 1 expired: an `after` timer, or a delayed raise. |
| `{"kind": "cancel"}` | Cancel the run from outside. |
| `{"kind": "defect", "defect": d}` | The driver met a defect while carrying out this run's commands: a host defect in an action, or a defect in the cancel step of a child run it was cancelling ([child runs](#child-runs)). Unlike every other input, it is taken even when the run has already ended, unless it ended with a defect. |

The driver only ever passes events the machine declares, with values of the declared types: an external event or a message is one of the machine's `events`, never one of its `internalEvents`. The core may assume its inputs are well formed.

One more form appears in records and defects, never as an input from the driver: `{"kind": "raised", "value": {"_tag": "Ping", …}}`, a raised event as the core takes it from the internal queue ([raised events](#raised-events)).

### Commands

| Command | The driver… |
| --- | --- |
| `{"kind": "trace", "record": r}` | writes trace record `r`, after adding `t`, `run` and, for a child run, `parent` ([trace](trace.md)). |
| `{"kind": "action", "action": "audit", "args": {…}}` | runs the action and waits for it to finish. `args` is keyed by parameter name. |
| `{"kind": "invoke.start", "invoke": 1, "state": "…", "src": "chargeCard", "srcKind": "task", "to": null, "as": null, "args": {…}, "saved": null}` | starts the task (`srcKind` `task`) or machine (`srcKind` `machine`) and, when it ends, queues its outcome as an `invoke.*` input with the same id. It does not wait for it. `as` is the invoke's `as` name or `null`: a child run invoked with one delivers what it emits to this run ([delivery](#delivering-messages)), and a task invoked with one is given a live emitter ([the emitter](#the-emitter)), which a task that declares no `emits` never uses. With `srcKind` `call`, it makes the call `src` of the peer whose run id is `to` (`null` for the other kinds), whose answer is the outcome ([carrying out a call](#carrying-out-calls)). `saved` is `null`, except in the commands of `restore`, where it is the saved snapshot of the invoke's child run when the saved snapshot holds one: the child run that claims the invoke restores it instead of starting ([restoring a child run](#restoring-a-child-run)). |
| `{"kind": "reply", "to": "r2", "invoke": 1, "call": "reserve", "outcome": "done", "value": v}` | answers the call that run `to` made with its invoke 1: `outcome` `done` with the result `v` (`null` for a call that returns nothing), or `fail` with the error `v`. It does not wait for the caller ([carrying out a call](#carrying-out-calls)). |
| `{"kind": "invoke.cancel", "invoke": 1}` | cancels invoke 1 and waits until it has stopped: at once for a task, after its cancel step for a [child run](#child-runs). Its outcome is not queued after this; one already queued stays queued. |
| `{"kind": "timer.start", "timer": 1, "delay": 200}` | starts a timer (an `after` timer or a delayed raise: the driver does not tell them apart) that queues `{"kind": "after", "timer": 1}` after `delay` milliseconds. |
| `{"kind": "timer.cancel", "timer": 1}` | cancels timer 1. Its expiry is not queued after this; one already queued stays queued. |
| `{"kind": "timer.fire", "timer": 1}` | queues `{"kind": "after", "timer": 1}` on this run's queue at once, without starting a timer. Only `restore` emits it, for a restored timer that was already due ([restoring](#restoring-a-snapshot)). |
| `{"kind": "send", "to": "r2", "invoke": null, "message": 1, "value": v}` | queues the message for run `to` and does not wait for it: for the child run of this run's invoke or spawned child `invoke`, or, when `invoke` is `null`, for the peer `to` ([delivery](#delivering-messages)). |
| `{"kind": "emit", "message": 2, "value": v}` | queues the emitted event for this run's parent, if it invoked this run with `as` or spawned it, and for every run that holds a `ref` to this run, of those whose machine has an `on` for it ([messages between runs](#messages-between-runs)). It does not wait for them. |
| `{"kind": "spawn.start", "spawn": 4, "src": "Session", "srcKind": "machine", "args": {…}, "saved": null}` | starts machine `src` (`srcKind` `machine`) or task `src` (`srcKind` `task`) as spawned child 4 of this run and, when it ends, queues its outcome as a `spawn.*` input with the same id. It does not wait for it. The child run delivers what it emits to this run, and the task is given a live emitter ([the emitter](#the-emitter)). `saved` is as for `invoke.start`, and always `null` for a task. |
| `{"kind": "spawn.stop", "spawn": 4}` | stops spawned child 4 and waits until it has stopped, exactly as `invoke.cancel` does: at once for a task, after its cancel step for a child run. Its outcome is not queued after this; one already queued stays queued. |

A trace record is always emitted immediately before the command it describes. Records are listed in [trace.md](trace.md). Two records are written by the driver instead of the core: `undelivered`, when it finds that the receiver of a `send` has ended ([delivery](#delivering-messages)), and `suspended`, when it stops a run it has saved ([suspending](#suspending-and-resume)).

### Redaction

Every `trace` command carries its record **redacted** ([trace.md](trace.md#redaction)): each value of a field, parameter or variable the IR marks `sensitive` is `"<redacted>"`. The pseudocode below writes `emit trace r` for the record before redaction; the core redacts it as it emits it, by the types trace.md lists, so both runtimes emit the same records. An input is redacted when the step that takes it begins, before anything else: an `invoke.done` input's type is that of the invoke's task or machine while the invoke is in the snapshot, which it no longer is once taken (a stale one, whose invoke is not in it, is redacted whole), and a `task.emitted` input's is the event of its `_tag` in the `emits` of the task its invoke or spawned task runs ([trace.md](trace.md#redaction) says what a stale one is redacted by). Redacting copies: the snapshot, the other commands and the values host code gets are never changed, and a value of a type that holds no sensitive field is the same value. A program whose IR marks nothing never redacts anything.

## Starting a machine

```
start(ir, name, run, input, fns):
  m    = ir.machines[name]
  snap = { machine: name, run, ir: m.hash, status: "running", active: [], input, vars: {}, locals: {},
           invokes: [], calls: [], spawned: [], timers: [], scheduled: [], nextInvoke: 1, nextTimer: 1, nextMessage: 1,
           history: {}, historyLocals: {}, result: null }
  emit trace { kind: "started", machine: name, ir: m.hash, input }
  for v in m.vars, in order:
    if v.init != null:          snap.vars[v.name] = eval(v.init)
    elif v.type ends with "?":  snap.vars[v.name] = null
  enterStates([m.root] + defaultEntry(m.root), {}, {})
  settle()
  if snap.status == "running": startWork()
  return (snap, commands)
```

A variable's initial value may read the parameters and the variables declared before it; reading a later one is a runtime error (`unassigned`). A variable with no initial value starts unassigned, unless its type is optional (`string?`, whose IR type string ends in `?`, or an alias of an optional type, aliases expanded transitively): an optional variable with no initial value starts as `null`, as the [language](../docs/design/language.md#definite-assignment) says, and reading it is never `unassigned`. Initial values are not traced (a state's variables' are: [data local to a state](#data-local-to-a-state)).

`defaultEntry(s)` is the list of states entered below `s` when `s` is entered with no more specific target, in document order:

```
defaultEntry(s):
  if s.kind == "compound": c = s.initial; return [c] + defaultEntry(c)
  if s.kind == "parallel": return the concatenation of [c] + defaultEntry(c) for each child c of s, in order
  return []
```

So starting enters the root, then its initial child, and so on down, outermost first, entering every region of a parallel state on the way, the first region and everything below it before the second: the root's `entry` effects run before any other state's. Starting takes no transition, so it writes no `transition` record. Default entry never enters a state that has parameters (the compiler rejects one as an `initial` or as a region), so starting passes no arguments: the states it enters that declare variables get their initial values.

## Taking an input

```
step(ir, snap, input, fns):
  m = ir.machines[snap.machine]
  if input.kind == "defect":
    if snap.status != "defect": failWithDefect(input.defect)
    return (snap, commands)
  if snap.status != "running": return (snap, [])
  sc = null                                       # the delayed raise an "after" input consumes
  switch input.kind:
    "cancel":  cancelRun(); return
    "invoke.done", "invoke.fail", "invoke.defect":
      inv = the entry of snap.invokes with id input.invoke
      if none: emit trace { kind: "stale", event: input }; return
      remove inv from snap.invokes
      if input.kind == "invoke.defect":
        emit trace { kind: "event", event: input }
        failWithDefect(input.defect); return
      if input.kind == "invoke.fail" and input.error._tag is not a declared error of inv.src:
        runtime error { kind: "undeclared", error: input.error._tag }
      T = selectOutcome(inv, input)
    "spawn.done", "spawn.fail", "spawn.defect":
      ch = the entry of snap.spawned with id input.spawn
      if none: emit trace { kind: "stale", event: input }; return
      remove ch from snap.spawned
      if input.kind == "spawn.defect":
        emit trace { kind: "event", event: input }
        failWithDefect(input.defect); return
      if input.kind == "spawn.fail" and input.error._tag is not a declared error of the
                                         machine or task of collection ch.children:
        runtime error { kind: "undeclared", error: input.error._tag }
      T = selectSpawnOutcome(ch, input)
    "after":
      tm = the entry of snap.timers with id input.timer
      if tm != null:
        remove tm from snap.timers
        T = selectAfter(tm)
      else:
        sc = the entry of snap.scheduled with id input.timer      # a delayed raise
        if none: emit trace { kind: "stale", event: input }; return
        remove sc from snap.scheduled
        T = selectEvent(sc.value)
    "event", "message":
      T = selectEvent(input.value)
    "call":
      T = selectCall(input.call)
    "emitted":
      if input.invoke != null and neither snap.invokes nor snap.spawned has an entry with id input.invoke:
        emit trace { kind: "stale", event: input }; return
      T = selectEmitted(input)
    "task.emitted":
      if neither snap.invokes nor snap.spawned has an entry with id input.invoke:
        emit trace { kind: "stale", event: input }; return
      T = selectEmitted(input)
    "ended":
      T = selectEnded(input)
  if T is empty:
    if m.strict and (input.kind in ("event", "message", "call") or sc != null):
      runtime error { kind: "unhandled", event: input }
    emit trace { kind: "dropped", event: input }
    if input.kind == "call": answer(input.from, input.invoke, input.call, "fail", { _tag: "Unanswered" })
    return
  emit trace { kind: "event", event: input }
  if input.kind == "call":
    append { from: input.from, invoke: input.invoke, call: input.call } to snap.calls
  microstep(T)
  settle()
  if snap.status == "running": startWork()
```

- An invoke's outcome or a timer's expiry **consumes** it (a delayed raise's expiry consumes the raise): it is removed from the snapshot before a transition is chosen, whether or not one is. Leaving the state afterwards therefore does not cancel it again.
- An outcome or expiry whose id is not in the snapshot is **stale**: its state was left (and the invoke or timer cancelled) after the driver had already queued it, or, for a delayed raise, a `cancel`, a raise with the same name or the end of the run cancelled it. It is traced as `stale` and changes nothing.
- An emitted event from a child run whose invoke or spawned child is no longer in the snapshot is **stale** too: it was cancelled or stopped after the child had emitted it, as a child cancelled by its parent does when its exit effects `emit`. Taking an emitted event does not consume the invoke or the spawned child. An emitted event from a peer (`invoke` `null`), a message and a peer's end (`ended`) are never stale.
- **A task's emit** (`task.emitted`) is stale in the same way, when its invoke or spawned task is no longer in the snapshot: the state was left (and the invoke cancelled), or the spawned task was stopped or replaced, after the emit had been queued. Unlike a child run's emitted event, a task's emit is never stale because its outcome was taken: a driver queues a task's emits before its outcome and none after it ([the emitter](#the-emitter)), so the run takes every one of them first. Taking one does not consume anything either ([tasks that emit](#tasks-that-emit)).
- A spawned child's outcome consumes it like an invoke's: the child is removed from `spawned` before a transition is chosen, whether or not one is. An outcome of a child that was stopped or replaced after the driver had queued it is stale.
- An input that enables no transition is **dropped**: traced as `dropped`, and nothing else happens. Settling is skipped, since nothing changed. A consumed invoke or timer stays consumed. A dropped call is the exception: it is answered at once with `Unanswered`, so its caller does not wait for it ([calls between runs](#calls-between-runs)).
- **A delayed raise's expiry** selects the raise's event exactly as the same external event would be selected (`on *` included), with `event` bound to its value: the run raised it, so it is one of its own events, internal or not.
- **In a strict machine** (`strict` in the IR), an external event, a message, a call, or the expiry of a delayed raise, that enables no transition is instead a runtime error, `unhandled`, whose `event` is the input ([defects](#defects)); so is a raised event taken from the internal queue ([raised events](#raised-events)). The machine raised those itself, and could have cancelled a delayed one, so one that no transition takes is a mistake in the chart, not a race. A call that fails a strict callee this way is answered `Unanswered` by the callee's driver as the run ends, like any call pending when its callee ends ([calls between runs](#calls-between-runs)). Every other input that enables no transition is dropped as in any machine: an emitted event, from a peer or from a child run (invoked or spawned), a task's emit (`task.emitted`), a peer's end (`ended`: a strict machine is not required to have an `on name.ended`), an invoke's outcome whose guarded branches all fail, a spawned child's outcome that no transition takes (`spawn.done` or `spawn.fail`), and an `after` timer's expiry whose guarded branches all fail. They arrive when another run or the clock decides, so failing on them would make an ordinary race fatal. A transition with no target and no effects (`ignore`) is an enabled transition like any other, so an input it takes is traced as `event`, with a `transition` record whose `to` is `[]`, and is never `unhandled`. A `send` from a strict machine that reaches no run is `undelivered`, as from any machine ([delivery](#delivering-messages)): strictness is about what a machine receives.
- A **call** that enables a transition becomes **pending**: it is appended to `snap.calls` after the `event` record and before the microstep, so a `reply` among the transition's effects finds it.
- A `dropped` or `stale` record replaces the `event` record; an input is traced by exactly one of the three.
- **A defect in the step that ended the run.** The core finishes the run (or cancels it) inside the step, so the snapshot a step returns can already be `done`, `failed` or `cancelled` while the driver is still carrying out that step's commands, every action of the step among them: say the effects of the transition into a top-level final state, the exit effects `finish` runs (the root's among them), or the exit effects of `cancelRun`. If any of those actions has a host defect, the driver stops at it as for any action ([defects](#defects)), so the `done`, `failed` or `cancelled` record after it is never written, and gives the core the `defect` input. That is why a `defect` input is taken in any status but `defect`: the run's status becomes `defect`, its result the defect, and the `defect` record is its last. `failWithDefect` finds no invokes or timers left to cancel, and `active` stays `[]`. A second `defect` input, for a run that already ended with one, changes nothing and writes nothing.

## Selecting a transition

A transition is **enabled** if its trigger matches the input and its guard holds. A `null` guard holds. Selection gives the list `T` of transitions the input takes, in order: empty when it enables none, one transition, or, with parallel states, one per region that takes the input. Guards are evaluated in the order given here, and evaluation stops at the first enabled transition of each search, so a guard after it is never evaluated (which matters because a guard can call host functions).

### Searching from the active leaves

An external event, a message, an emitted event, a task's emit, a call, a peer's end, a spawned child's outcome and `always` are selected by **walks**. Each state has **candidates** for the input, given below, tried in document order; the state **offers** the first candidate whose guard holds, or nothing.

```
selectByWalks(candidates):
  enabled = []
  for a in the active leaves, in document order:
    for s in [a] + the proper ancestors of a, innermost first:
      t = the first of candidates(s), in document order, whose guard holds
      if t != null:
        if t is not in enabled: append t to enabled
        break                       # this leaf's walk is over
  return removeConflicts(enabled)
```

Without parallel states there is one active leaf and one walk: the active states, innermost first, and the first enabled transition is taken, as before. With parallel states each region's leaves walk outward on their own, so one input can be taken in several regions. Walks that reach the same state search it again; its guards give the same result, since they are pure and nothing changes during selection, and a state that offered a transition offers the same one, which `enabled` holds once, at the position of the first walk that found it. (An implementation may remember what a state offered: searching again calls the same host functions with the same arguments, so the distinct calls made are the same either way, which is all a conformance case's `fns` table can tell.)

**Conflicts.** The **exit set** of a transition with a target is the active states that are proper descendants of its [domain](#a-microstep); a transition with no target has an empty one. Two transitions **conflict** when their exit sets share a state: taking both would exit it twice.

```
removeConflicts(enabled):
  kept = []
  for t1 in enabled, in order:
    preempted = false; replaced = []
    for t2 in kept, in order:
      if exitSet(t1) and exitSet(t2) share a state:
        if t1's state is a proper descendant of t2's state: append t2 to replaced
        else: preempted = true; break
    if not preempted:
      remove every transition in replaced from kept
      append t1 to kept
  return kept
```

This is SCXML's `removeConflictingTransitions`. Of two conflicting transitions the one found first, by the walk of an earlier leaf (the earlier region), is kept, unless the later one belongs to a state inside the earlier one's state: then the inner one is kept, as innermost first would choose in a chain. A transition with no target never conflicts. `kept` is in the order of the leaves whose walks found its transitions.

### Candidates

**An external event, a message or a raised event** `{"_tag": N, …}`: the candidates of `s` are its transitions with trigger `{"kind": "event", "event": N}`. If `s` has none, they are its `any` (`on *`) transitions instead. So an `on *` is not tried in a state that has an `on N` whose guards all failed. `on *` never matches an invoke outcome, an `after` timer or an emitted event. A message is one of the machine's `events`, so it is selected exactly as the same event from host code would be; only its record shows who sent it. A raised event, taken from the internal queue or as a delayed raise's expiry, is one of the machine's own events, `events` or `internalEvents`, and is selected the same way, `on *` included. `T = selectByWalks(those candidates)`, also written `selectEvent(value)`.

**An emitted event** `{"_tag": N, …}` from run `input.from`: the candidates of `s` are its transitions with trigger kind `emitted`, `event` `N`, and an address `from` that names the sender:

- an address of kind `child` names it when `input.invoke` is not `null` and the entry of `snap.invokes` with id `input.invoke` has `state` equal to the address's `state`, and its invoke has `as` equal to the address's `name`. (That entry exists: the input would otherwise have been stale.)
- an address of kind `ref` names it when `input.invoke` is `null` and the address's `name` is a parameter whose value, or a variable whose value, is the run id `input.from`. (An address names a parameter or variable of type `ref M`, through aliases: never an optional `ref` or a list of them, which the compiler rejects.) A variable with no value names no sender; reading it here is not an `unassigned` defect.

- an address of kind `children` names it when `input.invoke` is not `null` and the entry of `snap.spawned` with id `input.invoke` has `children` equal to the address's `name`. `key` is bound to that entry's `key`.

`T = selectByWalks(those candidates)`. There is no fallback to `on *`. So `on upload.Progress` is taken only for a `Progress` from the child that `upload` names, and from the run that started it, not from an earlier child of the same state: a child restarted by re-entering its state has a new invoke id. `on sessions.Progress` is taken for a `Progress` from any running child of `sessions`. An address of kind `task`, or of kind `children` for a collection of tasks, never names the sender of an `emitted` input, since a task is not a run.

**A task's emit** `{"_tag": N, …}` from invoke or spawned task `input.invoke`: the candidates of `s` are its transitions with trigger kind `emitted`, `event` `N`, and an address `from` that names the task's work:

- an address of kind `task` names it when the entry of `snap.invokes` with id `input.invoke` has `state` equal to the address's `state`, and its invoke has `as` equal to the address's `name`, the test an address of kind `child` makes for an `emitted` input.
- an address of kind `children`, for a collection whose `kind` is `task`, names it when the entry of `snap.spawned` with id `input.invoke` has `children` equal to the address's `name`. `key` is bound to that entry's `key`.

(One of those entries exists: the input would otherwise have been stale.) An address of kind `child` or `ref`, or of kind `children` for a collection of machines, never names a task. `T = selectByWalks(those candidates)`, also written `selectEmitted`. There is no fallback to `on *`. So `on up.Progress` is taken only for a `Progress` from the task that the state's current invoke `up` runs, not from another invoke of the state, and `on uploads.Progress` for one from any running spawned task of `uploads`.

**A peer's end** from run `input.from`: the candidates of `s` are its transitions with trigger kind `ended` and an address `from` (always of kind `ref`) that names that run: the address's `name` is a parameter or variable whose value is the run id `input.from`, or, for one of a list type (`ref M[]`), a list that has `input.from` among its items. (An address of an `ended` trigger names a parameter or variable of type `ref M`, `ref M?` or `ref M[]`, through aliases.) A value that is `null`, or a variable with no value, names no run; reading it here is not an `unassigned` defect. The value is read when the input is taken, so a `ref` variable names whichever run it holds then: an end of a run that no address names at that moment is dropped, also one the variable held earlier. `T = selectByWalks(those candidates)`, written `selectEnded`. There is no fallback to `on *`, and nothing is consumed: an `ended` is never stale.

**A spawned child's outcome** for the entry `ch` just removed from `snap.spawned`: for `spawn.done`, the candidates of `s` are its transitions with trigger `{"kind": "spawn.done", "children": ch.children}`. For `spawn.fail` with error tag `E`, they are its transitions with trigger kind `spawn.fail`, `children` `ch.children` and `error` `E`, or, if `s` has none, those with `error` `"*"`. `T = selectByWalks(those candidates)`, written `selectSpawnOutcome`. There is no fallback to `on *`.

**A call** of the machine's call `c`: the candidates of `s` are its transitions with trigger `{"kind": "call", "call": c}`. `T = selectByWalks(those candidates)`. There is no fallback to `on *`, which matches only events. With parallel states one call can be taken in several regions, like an event; it is still one pending call, which only one `reply` can answer.

**`always`**: the candidates of `s` are its `always` transitions. `T = selectByWalks(those candidates)`, used by [settling](#settling).

The other inputs search one state and take at most one transition, so `T` is empty or holds the first enabled candidate:

**An invoke outcome** for the entry `inv`: only the state `inv.state` is searched, and only its transitions whose trigger's `as` is the `as` of `inv`'s invoke (`null` for a state's one invoke without `as`), so an outcome is offered only to its own invoke's outcomes. For `invoke.done` the candidates are those `invoke.done` transitions. For `invoke.fail` with error tag `E`, they are those `invoke.fail` transitions with `error` `E`, or, if there are none, those with `error` `"*"`. Guards are tried in document order. The state's other invokes are not touched: unless the transition taken leaves the state, they keep running.

**A timer expiry** for timer `tm`: the candidates are the transitions of `tm.state` with trigger `after` and the same `group` as the transition at `tm.index`, in document order.

**`done`** of state `p` (compound or parallel): `p`'s `done` transitions in document order.

`event` is in scope in the guard and effects of an `on` transition (including `on *`, `on name.Event` and `on name.ended`), `result` in an `invoke.done` or `spawn.done` transition and `error` in an `invoke.fail` or `spawn.fail` transition, bound to the input's `value` or `error`. `key` is in scope in an `emitted` transition whose address is of kind `children` and in a `spawn.done` or `spawn.fail` transition, bound to the key of the child the input came from. In an `on c` transition, which takes a call, `event` is bound to the input's `args`, a record of the call's parameters, and `call` to the **pending-call value** `{"from": input.from, "invoke": input.invoke}`. For an emitted event `event` has the type `Machine.Name` of the sender's event ([type references](ir.md#type-references)), and its value is the tagged record the sender emitted; for a task's emit, the type `task.Name`, and its value is the input's `value`. In an `ended` transition `event` is bound to `{"status": input.status}`, a record with no `_tag` and the one field `status`, of the built-in enum `Ended` ([type references](ir.md#type-references)); a chart reads it only as `event.status`. Whether a chart may read `event` in an `on *` is the compiler's business. The implicit names `event`, `result`, `error`, `call` and `key` are reserved and parameter and variable names are unique, so a name in a `ref` means exactly one thing. (`attempt`, which only a retry's delay reads, never reaches the IR: the compiler replaces it with the retry's counter variable.)

## A microstep

A microstep takes the transitions of `T` together, as SCXML's microstep does: first every exit they cause, then every transition's effects, then every entry. Between the effects and the entries it evaluates the transitions' arguments and discards the locals of the states it exited ([data local to a state](#data-local-to-a-state)).

```
microstep(T):                         # each t in T belongs to a state, t.source
  for t in T, in order:
    emit trace { kind: "transition", from: t.source.id, index: t's index in t.source.transitions, to: t.targets }
  exits = the union of exitSet(t) for t in T          # computed before anything is exited
  recordHistory(exits)
  for s in exits, innermost first: exitState(s)
  for t in T, in order: runEffects(t.effects)
  args = {}                                           # state id -> argument values
  for t in T, in order:
    for a in t.args, in order: args[a.state] = eval of a.values, left to right
  for s in exits: remove the member s.id from snap.locals, if it has one
  restored = the union of snap.historyLocals[x.id] for each target x of each t in T
             that is a history state and has a member in snap.historyLocals
  entries = the union of entrySet(t) for t in T with targets
  enterStates(entries in document order, args, restored)

domain(t):                            # for t with targets
  if t.source is the root: return the root
  if t.internal: return t.source      # every target is a proper descendant of the source
  return the nearest proper ancestor of t.source that is a compound state (the root is one)
         and a proper ancestor of every target

exitSet(t):
  if t.targets == []: return []       # no state is left or entered
  d = domain(t)
  if d.kind == "parallel":            # only an internal transition's domain can be one
    regions = the children of d that are targets or have a target below them, or every
              child of d if a target is a history state of d
    return the active states that are regions or descendants of one
  return the active states that are proper descendants of d

entrySet(t):
  set = []
  for r in targets(t):                # the states the targets stand for, in order
    set = set ∪ (the states from just below domain(t) down to r) ∪ defaultEntry(r)
  for q in set, if q.kind == "parallel":          # once: a region added with its default
    for c in the children of q, if neither c nor any descendant of c is in set:     # entry
      set = set ∪ [c] ∪ defaultEntry(c)           # leaves no parallel state incomplete
  return set

targets(t):
  the concatenation, for each target x of t in order, of [x] if x is not a history state,
  and of what entering x restores if it is one (history states, below)
```

With one transition and one target that is not a history state this is: the `transition` record; the exits, innermost first; the effects; the arguments, if the transition has any; the entries, outermost first, which are the path from just below the domain to the target, the target's default entry, and every other region of a parallel state on the path by default entry. A transition with no target contributes only its `transition` record and its effects. The transitions of `T` never conflict, so their exit sets are disjoint, and so are their entry sets. Exiting innermost first across regions exits the later region's states before the earlier region's ([exiting](#exiting-and-entering-a-state)), while the effects run in the order of `T`, the earlier region's first.

**Several targets.** A transition can have more than one target (`-> P.A.X, P.B.Y`), to enter a parallel state with a chosen state in more than one region, as SCXML's `target="a b"` does. The compiler guarantees that no target is another's ancestor or descendant and that any two meet at a parallel state: their nearest common ancestor is parallel, so they are in different regions of it, at any depth (a history state counting as a child of its parent). The entry set is then the union of the paths to every target, and a region of a parallel state on those paths that none of them is in is entered by default. The `transition` record's `to` lists the targets in the order the IR does.

**A transition is external unless it is internal.** An **external** transition, `internal` `false`, is SCXML's external one. Unless the source is the root, its domain is the least common compound ancestor of its source and targets, which is a proper ancestor of all of them, so the source itself is always exited and the targets always entered. **An external transition's domain is never a parallel state**: a parallel state is skipped when looking for it, as SCXML's `findLCCA` skips it, so a transition between two regions, or from a region to a state inside itself, leaves the parallel state and enters it again with all its regions:

| Transition | Domain | Exited | Entered |
| --- | --- | --- | --- |
| `A -> B`, siblings | their parent | `A` and its active descendants | `B` and its default entry |
| `A -> A` (self) | `A`'s parent | `A` and its active descendants | `A` and its default entry |
| `A -> A.C` (into its own child) | `A`'s parent | `A` and its active descendants | `A`, `C` and `C`'s default entry |
| `A.C -> A` (to its own ancestor) | `A`'s parent | `A` and its active descendants | `A` and its default entry |
| `P.R.X -> P.R.Y`, within region `R` of parallel `P` | `R` | `X` and its active descendants | `Y` and its default entry |
| `P.R.X -> P.S.Y`, from region `R` to region `S` | the nearest compound proper ancestor of `P` | `P` and all its active descendants | `P`, `R` and every other region but `S` by default entry, and `S`, then the path down to `Y`, and `Y`'s default entry |
| `P.R -> P.R.Y`, a region into its own child | the nearest compound proper ancestor of `P` | `P` and all its active descendants | `P`, the path to `Y` and its default entry, and every other region by default entry |
| `X -> P.S.Y`, from outside `P` | the least common compound ancestor | as for any transition | `P`, every region but `S` by default entry, the path down to `Y` and its default entry |
| `X -> P.R.X2, P.S.Y`, two targets | the least common compound ancestor | as for any transition | `P`, the paths down to `X2` and to `Y` and their default entries, and every region but `R` and `S` by default entry |
| `X -> C.H`, `H` a history state of `C`, from outside `C` | the least common compound ancestor | as for any transition | `C`, and what `H` recorded, or `C`'s default entry |
| `C.A -> C.H`, from inside `C` | `C` (if compound) | `A` and its active descendants, not `C` | what `H` recorded when `C` was last exited, or `C`'s default entry, not `C` itself |
| `C -> C.H`, written in `C` | the least common compound ancestor of `C` | `C` and its active descendants, recording them | `C`, and what `H` just recorded: the states `C` was in |

An **internal** transition, `internal` `true` (written `-> .X`), keeps its source: its domain is the source itself, so only the states below the source change. Every target of one is a proper descendant of its source, and its source is never the root (the compiler writes `-> .X` in the machine body as an external transition, which is the same thing there). This is SCXML's `type="internal"`, and XState's default for a target written `.X`, except where the source is a parallel state: SCXML's rule applies only to a compound source, and from a parallel one takes the transition as external. Here a parallel source is kept too, and **only the regions the targets are in** are exited and entered again; the other regions keep going, as in XState v6:

| Transition | Domain | Exited | Entered |
| --- | --- | --- | --- |
| `S -> .A`, `S` compound, now in `B` | `S` | `B` and its active descendants | `A` and its default entry |
| `S -> .A`, now in `A` | `S` | `A` and its active descendants | `A` and its default entry: the target restarts, the source does not |
| `S -> .A.C`, a deeper target | `S` | `S`'s active descendants | `A`, `C` and `C`'s default entry |
| `P.R -> .Y`, a region into its own child | `R` | `R`'s active descendants | `Y` and its default entry; `P` and the other regions are kept |
| `P -> .R.Y`, written in parallel `P` | `P` | `R` and its active descendants | `R`, the path down to `Y` and its default entry; every other region is kept |
| `P -> .R`, written in parallel `P` | `P` | `R` and its active descendants | `R` and its default entry: the one region is reset |
| `P -> .R.X, .S.Y`, two targets | `P` | `R`, `S` and their active descendants | `R` and `S` with the paths down to `X` and `Y`; every other region is kept |
| `C -> .H`, `H` a history state of `C` | `C` | `C`'s active descendants, not `C`, so nothing is recorded | what `H` recorded when `C` was last exited, or `C`'s default entry |

Since an internal transition's domain is its source, the source's own invoke, timers, `entry` and `exit` are untouched by it, as are every state above it and, for a parallel source, every region no target is in. An internal transition inside one region, `P.R -> .Y`, does not exit `P`, so it no longer conflicts with a transition in another region taken by the same input: both are taken, in one microstep.

The entries are in document order, so the regions of a parallel state are entered one after the other, each with everything below it, in the order the regions are written.

Re-entering a state restarts its work: its invoke is cancelled and started again with a new id, and its timers likewise. **A transition with no target** leaves and enters no state: nothing is cancelled or restarted, and only its effects run.

The root is never a target. A transition written in the machine body (an `on`, `after` or `always` of the root) has the root as its domain: everything below the root is exited and the targets entered, but the root itself is neither exited nor re-entered, so its entry and exit effects do not run again and its own timers keep running.

### Exiting and entering a state

```
exitState(s):
  for each entry inv of snap.invokes with state s.id, by ascending id:
    remove it; emit trace { kind: "invoke.cancelled", invoke: inv.id, state: s.id, src: inv.src }
    emit invoke.cancel { invoke: inv.id }
  for each entry tm of snap.timers with state s.id, by ascending id:
    remove it; emit trace { kind: "timer.cancelled", timer: tm.id, state: s.id, index: tm.index }
    emit timer.cancel { timer: tm.id }
  runEffects(s.exit)
  remove s from active                # its locals stay until the microstep discards them
  remove every completion of s, and of an ancestor of s, from the internal queue
  emit trace { kind: "exited", state: s.id }

enterStates(list, args, restored):
  for s in list, in order:
    add s to active (active stays in document order)
    emit trace { kind: "entered", state: s.id }
    bindLocals(s, args, restored)
    runEffects(s.entry)
    add s to the states entered during this macrostep
    if s.kind == "final":
      if parent(s) is the root: finish(s); return
      complete(s)

bindLocals(s, args, restored):        # s was just entered
  if s.params == [] and s.vars == []: return
  snap.locals[s.id] = {}              # a member in document order among snap.locals
  if restored has a member s.id:      # entered again from a history record
    for each local n of s, its parameters in order then its variables in order,
        if restored[s.id] has a member n:
      bind(s, n, restored[s.id][n])
    return
  for p in s.params, in order, and v in args[s.id], in order: bind(s, p.name, v)
  for v in s.vars, in order:
    if v.init != null:          bind(s, v.name, eval(v.init))
    elif v's type is optional:  bind(s, v.name, null)     # as for a machine variable

bind(s, n, v):
  snap.locals[s.id][n] = v
  emit trace { kind: "set", var: s.id + "." + n, value: v }

complete(f):                          # a final state f was just entered; parent(f) is not the root
  s = f
  if parent(f).kind == "compound":
    append the completion of parent(f) to the internal queue
    s = parent(f)
  while parent(s).kind == "parallel" and parent(s) is in a final state:
    s = parent(s)
    append the completion of s to the internal queue

runEffects(effects):
  for e in effects with kind "set", in order:
    v = eval(e.value)
    if e.state == null:                           # a machine variable
      snap.vars[e.var] = v
      emit trace { kind: "set", var: e.var, value: v }
    else:                                         # a variable of state e.state
      snap.locals[e.state][e.var] = v
      emit trace { kind: "set", var: e.state + "." + e.var, value: v }
  for e in effects with kind "do", "send", "emit", "reply", "spawn", "stop", "raise" or "cancel", in order:
    switch e.kind:
      "do":
        args = the action's parameter names zipped with eval of e.args, left to right
        emit trace { kind: "action", action: e.action, args }
        emit action { action: e.action, args }
      "send":
        if e.to.kind == "children" and e.key == null:   # send sessions.*.E: every running child
          v = eval(e.value)
          for ch in snap.spawned with children e.to.name, in order:
            id = snap.nextMessage; snap.nextMessage += 1
            to = snap.run + "." + ch.id
            emit trace { kind: "sent", message: id, to, value: v }
            emit send { to, invoke: ch.id, message: id, value: v }
          continue
        id = snap.nextMessage; snap.nextMessage += 1
        if e.to.kind == "child":
          inv = the entry of snap.invokes with state e.to.state whose invoke has as e.to.name
          v = eval(e.value)
          if inv == null:
            emit trace { kind: "undelivered", message: id, to: null, value: v }
            continue
          to = snap.run + "." + inv.id; invoke = inv.id
        elif e.to.kind == "children":                  # send sessions[k].E
          k = eval(e.key); v = eval(e.value)
          ch = the entry of snap.spawned with children e.to.name and key k
          if ch == null:
            emit trace { kind: "undelivered", message: id, to: null, value: v }
            continue
          to = snap.run + "." + ch.id; invoke = ch.id
        else:
          to = eval({ kind: "ref", path: [e.to.name], state: null }); invoke = null
          v = eval(e.value)
        emit trace { kind: "sent", message: id, to, value: v }
        emit send { to, invoke, message: id, value: v }
      "emit":
        id = snap.nextMessage; snap.nextMessage += 1
        v = eval(e.value)
        emit trace { kind: "emitted", message: id, value: v }
        emit emit { message: id, value: v }
      "reply":
        c = eval(e.call)                          # a pending-call value { from, invoke }
        p = the entry of snap.calls with from == c.from and invoke == c.invoke
        if p == null: runtime error { kind: "answered", call: the call named by e.call's type }
        v = e.value == null ? null : eval(e.value)
        remove p from snap.calls
        answer(p.from, p.invoke, p.call, e.outcome, v)
      "spawn":
        k = eval(e.key)
        args = the parameter names of e.src (a machine or a task) zipped with eval of e.args, left to right
        stopChild(e.children, k)
        append { children: e.children, key: k, args } to the pending spawns
      "stop":
        stopChild(e.children, eval(e.key))
      "raise":
        if e.delay == null:                       # raise E: on the internal queue
          v = eval(e.value)
          emit trace { kind: "raised", value: v }
          append the raised event v to the internal queue
        else:                                     # raise E after d [as n]: a delayed raise
          d = eval(e.delay); v = eval(e.value)
          if e.name != null: cancelRaise(e.name)
          id = snap.nextTimer; snap.nextTimer += 1
          append { id, name: e.name, delay: d, value: v } to snap.scheduled
          emit trace { kind: "raise.scheduled", timer: id, name: e.name, delay: d, value: v }
          emit timer.start { timer: id, delay: d }
      "cancel":
        cancelRaise(e.name)

cancelRaise(n):
  sc = the entry of snap.scheduled with name n
  if sc != null:
    remove sc from snap.scheduled
    emit trace { kind: "raise.cancelled", timer: sc.id, name: n }
    emit timer.cancel { timer: sc.id }

answer(to, invoke, call, outcome, value):
  emit trace { kind: "replied", to, invoke, call, outcome, value }
  emit reply { to, invoke, call, outcome, value }

stopChild(children, k):
  remove every entry with children and key k from the pending spawns
  ch = the entry of snap.spawned with children and key k
  if ch != null:
    remove ch from snap.spawned
    emit trace { kind: "spawn.stopped", spawn: ch.id, children, key: k }
    emit spawn.stop { spawn: ch.id }
```

So a state's work stops before its own exit effects run, and the `exited` record comes last. All `set` effects of a list run before its other effects, whatever order the IR lists them in, and each `set` sees the assignments before it. Then the `do`, `send`, `emit`, `reply`, `spawn`, `stop`, `raise` and `cancel` effects run in the order the IR lists them. None of them can change variables, so every one of them in a list sees the same values, and a message or a reply carries what its transition assigned.

- **A reply** reads its pending-call value before it evaluates its result or error. A value whose call is not pending (it was answered already, by this effect list or an earlier step, or the run was restored) is the runtime error `answered`, whose `call` is the call's name.
- **Message ids.** Every `send` and `emit` effect that runs takes the next message id, also one that is undelivered. With the run id it names the message in every record about it: the sender's `sent`, `emitted` or `undelivered`, and the receiver's `event`, `dropped` or `stale`.
- **A send to a child** goes to the run started by the invoke of the address's `state` whose `as` is the address's `name`, if it is running now. If it is not, the message is **undelivered**: traced with `to` `null`, and nothing is sent. This happens when the send runs in the step that enters the invoking state, before its work starts (the `entry` of a state inside it, say: the compiler rejects only the invoking state's own `entry`), and when the invoke's outcome has already been taken while the state stays active (an outcome transition with no target, or an outcome that enabled no transition).
- **A send to a peer** reads the `ref` parameter or variable the address names, exactly as an expression `ref` would, so a variable with no value is an `unassigned` defect. The address is read before the value is evaluated.
- **Delivery is the driver's.** The core does not know whether the receiver is still running. If it is not, the driver writes `undelivered` after the `sent` record ([delivery](#delivering-messages)), with the `sent` record's value, which the core redacted: never the `send` command's value.
- **A send to a spawned child** by key evaluates the key, then the value, and goes to the running child of the collection with that key. If there is none, the message is **undelivered**, traced with `to` `null`: no child has that key, it was stopped, or it was spawned in this macrostep and has not started yet ([spawned children](#spawned-children)). **`send sessions.*.E`** evaluates the value once and sends it to every running child of the collection, in the order they were started, each as a message of its own with its own id and `sent` record. With no running child it writes nothing; it takes no message id then.
- **`spawn` and `stop`** are described under [spawned children](#spawned-children), and **`raise` and `cancel`** under [raised events](#raised-events).

In a microstep, then, the order is: the `transition` records, one per transition; for each state exited, innermost first, its invokes cancelled, its timers cancelled, its exit effects, `exited`; the transitions' effects, transition by transition; the transitions' arguments, transition by transition; for each state entered, in document order, `entered`, a `set` record for each of its locals that gets a value, and its entry effects. No work is started during a microstep.

### History states

A history state `h` with parent `p` records where `p` was each time a microstep exits `p`, with the locals of the states it will re-enter, and a transition that targets `h` enters those states again, with those locals.

```
recordHistory(exits):                 # before any state of exits is exited
  for s in exits, for each history state h in s.states:
    if h.kind == "history":
      snap.history[h.id] = the active children of s, in document order
      kept = the active children of s
    if h.kind == "deepHistory":
      snap.history[h.id] = the active leaves that are proper descendants of s, in document order
      kept = the active proper descendants of s
    saved = { q.id: a copy of snap.locals[q.id] for q in kept, in document order,
              if snap.locals has a member q.id }
    if saved is empty: remove the member h.id from snap.historyLocals, if it has one
    else:              snap.historyLocals[h.id] = saved

restores(h):                          # for a history state h with parent p
  if snap.history has h.id: return snap.history[h.id]
  if p.kind == "compound":  return [p.initial]
  return the children of p                        # every region
```

- **What is recorded.** A shallow history state records its parent's active children: one for a compound parent, every region for a parallel one. A deep one records the active leaves below its parent, atomic and final states, in every region. Each record replaces the one before, its locals with it.
- **Locals.** With the states, a record keeps the locals of every state that entering it will re-enter from the record: for a shallow one the children it records, for a deep one every active state below its parent, the leaves and the states between them and the parent. They are copied as they are before the microstep exits anything, so an `exit` effect that assigns a local of its own state does not change the record. The states that entering the record enters below those by default entry (a shallow record's child's `initial` chain or regions) are not in it, and start afresh. It is recorded from the active states as they are before the microstep exits anything, so the record is where `p` was at the end of the previous microstep. Only a microstep records: `finish` and `cancelRun` exit states without recording, and `failWithDefect` exits none. A runtime error in a step discards what the step recorded with the rest of the step.
- **Entering.** A target `h` stands for `restores(h)` in `entrySet`'s `targets(t)` ([a microstep](#a-microstep)). Entering `h` enters what it recorded, or, with no record, the states directly below `p` that `p`'s default entry starts with. Each of those states gets its default entry: a shallow record's child its `initial` chain or every region, a deep record's leaves nothing more. Every state between the domain and them is entered too, and a region of a parallel state on the way that none of them is in gets its default entry, as for any target. A deep record holds a leaf in every region below `p`, so no region below `p` is entered by default. `h` itself is never entered: there is no `entered` record for it, and it is never in `active`. The `transition` record's `to` has `h`'s id.
- **The domain** is `domain(t)` with `h` as the target, as for any state: for an external transition, the nearest compound proper ancestor of the source that is a proper ancestor of `h`; for an internal one, the source. It does not depend on what `h` recorded. From outside `p` that is above `p`, so `p` is entered. From inside a compound `p` it is `p`, so `p` is neither exited nor recorded by this transition, and what is entered is what `p` recorded when it was last exited. (SCXML computes the domain from the states the history restores, which differs only for a transition from inside `p`.) From inside a parallel `p` the domain is above `p`, which is exited, recorded and entered again as recorded.
- **Restored states are entered**, with their `entry` effects, their `entered` records and, once the step has settled, new invokes and timers, as on any entry. An entered final state completes its parent as usual (`complete`). A restored state that declares locals gets back the ones it had when it was recorded, each with a `set` record, in place of its arguments and initial values (`bindLocals`); a variable that had no value then has none now. Nothing else is restored: the machine's variables keep their values, and the work that ran in those states before, an invoked child run among it, is not resumed.
- **With no record**, entering `h` enters `p`'s default entry, which never enters a state that has parameters, so nothing needs arguments: its variables get their initial values. `p` itself, when the transition enters it, gets its arguments from the transition like any state on a target's path.
- **Order.** A microstep records before it computes its entries, so a transition that exits `p` and targets `h` (from outside `p`, or from inside a parallel `p`) enters what `p` was a moment before: the same states, entered again.
- **An internal transition to a history state of its own source**, `C -> .H`, does not exit `C`, so `C` records nothing and what is entered is what `C` recorded when it was last exited. From a parallel source, `P -> .H` with `H` a history state of `P` exits and enters every region.

### Data local to a state

A state's parameters and variables, its [locals](#terms), live in `snap.locals[s.id]` while `s` is active. The compiler resolves every name, so a reader never looks a name up by scope: a `ref` expression or a `set` effect whose `state` is not `null` names a local of that state, and one whose `state` is `null` names the machine's parameter or variable (or, for a `ref`, an implicit name). The compiler guarantees that such a state is the expression's own state or one of its ancestors, so it is active, or is being exited in this microstep, whenever the expression is evaluated.

- **Lifetime.** `bindLocals` creates a state's member of `snap.locals` when the state is entered, after its `entered` record and before its `entry` effects, and the microstep that exits it removes the member after the transitions' effects and arguments have been evaluated, just before the entries. So a state's `exit` effects read its locals, and so do the effects and arguments of the transitions that leave it: `-> Backoff(attempt + 1)` written in `Backoff` passes one more than the value it leaves with, and `-> Confirming(token)` passes a token on. `finish` and `cancelRun` exit states outside a microstep and clear `snap.locals` once every state is exited; `failWithDefect` exits nothing and leaves it as it is.
- **Entering.** Parameters are bound first, in order, to the arguments, then each variable, in order, to its initial value, or to `null` if it has none and its type is optional (by the rule [starting a machine](#starting-a-machine) gives for the machine's variables); a variable with neither starts with no value, and has no member. An initial value is evaluated when it is bound, so it may read the state's parameters, its variables declared before it, the locals of the states around it (entered before it, outermost first) and the machine's parameters and variables. Each binding writes a `set` record whose `var` is the state's id, a dot and the local's name (`PasswordReset.Confirming.tries`): a variable's name is lower-case and has no dot, so it never collides with a machine variable's. Machine variables' initial values are not traced; a state's locals are, since its arguments come from the step.
- **Arguments.** A transition's `args` holds, for each state it enters that has parameters, the expressions of its arguments, in the order the states are entered. They are evaluated after every transition of the microstep has run its effects, transition by transition in the order of `T`, each list left to right: so they see what the transitions' `set`s assigned, as a `send` does, and the locals of the states being exited. The compiler guarantees that a transition has arguments for exactly the states with parameters it enters along the paths written in its targets, and that default entry never enters a state with parameters (no `initial` and no region has any), so every state with parameters that is entered gets its arguments, either from the transition or from a history record ([history states](#history-states)).
- **Assigning.** A `set` effect with a `state` assigns that state's local in `snap.locals[state]`, which exists: the compiler allows a `set` only of a variable of the state the effect list belongs to or of a state around it. An assignment in a transition to a local of a state the transition exits is discarded with that state's locals (the compiler warns, `RB0916`).
- **Reading** a local with no value is the runtime error `unassigned`, whose `var` is the qualified name, `PasswordReset.Confirming.token`, as in the `set` record ([defects](#defects)). A runtime error or host defect while evaluating an argument or an initial value makes the step atomic, as any does.
- **Re-entering** a state, by a transition that exits it and enters it again (`-> Self(n)`), discards its locals and binds new ones, its initial values evaluated again. An internal transition (`-> .Child(x)`) does not exit its source, which keeps its locals; only the states it enters get new ones.

A machine whose states declare no locals has `locals` `{}` and `historyLocals` `{}` throughout, and its transitions have no `args`, so it writes the records it wrote before.

## Settling

After the input's microstep (and after the initial entry in `start`), the core takes eventless transitions and works through the internal queue until neither gives a transition:

```
settle():
  count = 0
  loop:
    if snap.status != "running": return
    T = the enabled always transitions (selectByWalks, conflicts removed)
    while T is empty and the internal queue is not empty:
      x = take the front of the internal queue
      if x is the completion of a state p:
        T = [the first enabled done transition of p], or [] if there is none
      else:                                       # x is a raised event v
        input = { kind: "raised", value: v }
        T = selectEvent(v)
        if T is empty:
          if m.strict: runtime error { kind: "unhandled", event: input }
          emit trace { kind: "dropped", event: input }
        else:
          emit trace { kind: "event", event: input }
    if T is empty: return
    count = count + 1
    if count > 1000: runtime error { kind: "limit" }
    microstep(T)
```

- `always` transitions take priority over the internal queue. Its front is considered only when no `always` is enabled, and completions and raised events are taken in the order they were queued, as SCXML takes its internal queue (where `done.state.*` events go too). A raised event that is taken writes an `event` record before its transitions; one that enables none is `dropped`, or `unhandled` in a strict machine, and takes no microstep. The `always` transitions enabled in several regions are taken together, in one microstep, as an input's are.
- A compound state is complete when one of its final children is entered. A parallel state is complete when it is in a final state: when, after a final state below it is entered, every region is in a final state (`complete`). The entry that completes the last region queues the parallel state, right after that region's own completion, so a region's `done` is tried before its parallel state's. A region that is itself a final state is in a final state from the moment it is entered; a region that is an atomic state never is, so a parallel state with one never completes. A parallel state inside a region of another completes it in turn when the outer one's other regions are in a final state too. A region in a final state can leave it again without its parallel state being exited, by an internal transition written in the region or in the parallel state (or one of their ancestors' in a deeper chart): that exits the final state, and the region is no longer in one.
- A state's completion is queued once per entry, and removed from the queue when the state is exited, or when a state below it is (`exitState`), so a state in the queue is always active and still in a final state: a state whose final child an internal transition exits is no longer complete, and if the transition enters a final state again, `complete` queues it again, at the back. A completion is therefore never taken for a state that was exited after it completed, even if it was entered again in the same macrostep: an `always` that leaves and re-enters it discards the completion. When a completion is taken from the queue, the state's `done` transitions are tried once: if none of their guards holds, it is discarded without a trace record and is not retried later.
- A run that takes more than 1000 microsteps while settling one input has a runtime error `limit`, so a guarded `always` loop, or a raise that raises itself again, cannot hang the host. (The compiler rejects unguarded `always` loops.) The microsteps of raised events count towards it like any other.

## Starting work

After settling, if the run is still running, the core starts the children spawned during this macrostep, in the order they were spawned, then the invokes and timers of every state that was **entered during this macrostep and is still active**, in document order (outermost first):

```
startWork():
  for p in the pending spawns, in order:
    id = snap.nextInvoke; snap.nextInvoke += 1
    append { id, children: p.children, key: p.key, args: p.args } to snap.spawned
    c = the collection p.children; src = c.src
    emit trace { kind: "spawn.started", spawn: id, children: p.children, key: p.key, src, args: p.args }
    emit spawn.start { spawn: id, src, srcKind: c.kind, args: p.args, saved: null }
  clear the pending spawns
  for s in active, in document order, if s was entered during this macrostep:
    for iv in s.invokes, in order:
      id = snap.nextInvoke; snap.nextInvoke += 1
      to, src, args = startArgs(iv)
      if s.invokes has more than one entry:
        append { id, state: s.id, src, as: iv.as } to snap.invokes
        emit trace { kind: "invoke.started", invoke: id, state: s.id, src, as: iv.as, args }
      else:
        append { id, state: s.id, src } to snap.invokes
        emit trace { kind: "invoke.started", invoke: id, state: s.id, src, args }
      emit invoke.start { invoke: id, state: s.id, src: iv.src, srcKind: iv.kind,
                          to, as: iv.as, args, saved: null }
    for each after block of s, in document order of its first transition (index i):
      delay = eval(s.transitions[i].trigger.delay)
      id = snap.nextTimer; snap.nextTimer += 1
      append { id, state: s.id, index: i, delay } to snap.timers
      emit trace { kind: "timer.started", timer: id, state: s.id, index: i, delay }
      emit timer.start { timer: id, delay }

startArgs(iv):
  if iv.kind == "call":                           # invoke inventory.reserve(…)
    to = eval({ kind: "ref", path: [iv.to.name], state: null })
    src = iv.to.name + "." + iv.src
    params = the parameters of the call iv.src of machine iv.to.machine
  else:
    to = null; src = iv.src; params = the parameters of iv.src
  args = the names of params zipped with eval of iv.args, left to right
  return (to, src, args)
```

- A call's peer is read before its arguments are evaluated, as a `send`'s address is, so a `ref` variable with no value is an `unassigned` defect. Its `src`, in the snapshot and the records, is `inventory.reserve`: the address's name, a dot, and the call's.
- Within one state the invokes start in the order they are written, and before the timers. States are taken in document order, so the work of several regions of a parallel state starts region by region, in the order the regions are written, and gets its invoke and timer ids in that order. So a state's invokes have consecutive ids, in the order written.
- A delay is a `duration`, so it is never negative: a host function declared to return `duration` that gives a negative one has returned a value that is not of its type, a `host` defect ([values](#values)). A delay of 0 is a timer like any other: its expiry is queued by the driver's timer, never by the core (except by a `timer.fire` command of `restore`), and it comes after every input already queued.
- Work starts after every entry effect of the macrostep, including those of states entered while settling. So an `after` delay or an invoke argument sees the variables as the step left them: `Checkout`'s `Backoff` sets `attempts` on entry and its `after retryDelay(attempts)` reads the new value.
- A state that is entered and left again within one macrostep (`Confirming` in `PlaceOrder`, which an `always` leaves at once) never starts its work. This is SCXML's rule for `invoke`, applied to timers as well.
- A state that was already active before the input and is not exited keeps its work running.

Time spent is the driver's business: the core records only the delay. A timer's expiry is due `delay` milliseconds after the driver carries out `timer.start`.

## Finishing

Entering a final state whose parent is the root finishes the run, in the middle of the microstep that entered it. It is always the last state that microstep enters: its transition's domain is the root, so any other transition taken with it has no target (one with a target would have conflicted with it).

```
finish(f):
  if f.output != null: status = "done";   value = eval(f.output)
  elif f.fail != null: status = "failed"; value = eval(f.fail)
  else:                status = "done";   value = null
  for s in active, innermost first: exitState(s)        # f, then the root
  snap.locals = {}
  stopChildren()
  cancelRaises()
  snap.result = status == "done" ? { output: value } : { error: value }
  snap.status = status
  clear the internal queue
  emit trace status == "done" ? { kind: "done", state: f.id, output: value }
                              : { kind: "failed", state: f.id, error: value }
```

The output or error is evaluated right after `f`'s entry effects, before any exit effect runs, so it may read `f`'s parameters (`final state Rejected(reason: string) { output problem(422, reason) }`). Then every active state is exited, innermost first, with its exit effects, as on any exit (but no history is recorded: the run is over): this is when the root's `exit` effects run, and when timers on the root are cancelled. Then the spawned children still running are stopped, after every exit effect ([spawned children](#spawned-children)), and then the pending delayed raises are cancelled (`cancelRaises`, [raised events](#raised-events)), a delayed raise an exit effect started among them. A message an exit effect sends a child is queued before the stop, but the stop is a cancel request, which a run takes before anything queued, so whether the child takes the message first is up to the scheduler, as for any message to a run cancelled in the same step ([conformance](conformance/README.md#how-a-runner-works)); a chart cannot rely on it. Settling stops and no work is started. The run's result is the output (an Effect success, a Rust `Ok`) or the error (the Effect error channel, a Rust `Err` of the machine's error enum).

## Cancelling from outside

The input `{"kind": "cancel"}` exits every active state, innermost first (in reverse document order, so the later regions of a parallel state first), exactly as `exitState` does: each state's invoke and timers are cancelled and its exit effects run. No history is recorded, and `history` keeps what it had. Then the spawned children still running are stopped and the pending delayed raises cancelled, as when the run finishes, the status becomes `cancelled` and a `cancelled` record is written. What exit effects raised is thrown away with the internal queue. There is no `event` record for a cancel.

```
cancelRun():
  for s in active, innermost first: exitState(s)
  snap.locals = {}
  stopChildren()
  cancelRaises()
  clear the internal queue
  snap.status = "cancelled"
  emit trace { kind: "cancelled" }
```

This was decided on 27 November 2026 ([semantics](../docs/design/semantics.md#cancelling-from-outside)): exit effects are the only cleanup hook a chart has, so they must be quick. A parent leaving an invoking state cancels its child run this way, so the child's exit effects run too, and the parent waits for them ([child runs](#child-runs)). The cancel step is the run's last; a host defect in one of its exit actions turns it into a defect ([taking an input](#taking-an-input)).

## Messages between runs

A run talks to another only through queues. The core's part is small: a `send` or `emit` effect writes a record and gives the driver a `send` or `emit` command ([effects](#exiting-and-entering-a-state)), and a message or an emitted event comes back to the receiver's core as an input like any other ([taking an input](#taking-an-input), [selecting](#selecting-a-transition)). The driver does the delivering ([delivering messages](#delivering-messages)).

- **Sending is queuing.** The sender's driver pushes the input onto the receiver's queue and carries on with the next command. A sender never waits for the receiver and never runs the receiver's transitions; the receiver takes the input later, one at a time, like any other.
- **Who receives a `send`.** For an address of kind `child`, the child run of the invoke that is running in the address's `state`. For an address of kind `ref`, the run whose id the parameter or variable holds. For an address of kind `children`, the running child of the collection with the `send`'s key, or every running child of it for `send name.*.E` ([spawned children](#spawned-children)).
- **Who receives an `emit`.** The run's parent, if it invoked the run with `as` or spawned it; an invoker without `as` has no name to receive them under, and they are not delivered to it. And every run that holds a `ref` to the emitting run ([refs and handles](#refs-and-handles)). Of these, a run receives an emitted event `E` from a run of machine `M` only if its own machine **hears** it: has a transition, in any state, with trigger kind `emitted`, `event` `E` and an address whose `machine` is `M`, whatever the address's kind (`child`, `ref` or `children`) and name. The filter applies to a spawned child's parent as to an invoker: a parent whose machine has no `emitted` transition for `E` from the child's machine, `on sessions.E` or any other, is not sent it. Every other run is not sent it and writes nothing for it. The filter is decided from the chart alone, never from the states a run is in: a run that hears an event and has no enabled transition for it when it takes it, because its guard fails or it is in another state, writes `dropped`. A run never receives its own emitted events.
- **Who receives a task's emits.** Only the run that invoked the task with `as` or spawned it, by the same filter: a run's machine **hears** `E` from task `t` when it has a transition, in any state, with trigger kind `emitted`, `event` `E` and an address whose `task` is `t` (kind `task`, or `children` of a collection of `t`), whatever the address's name. A `hears` map keyed by the sender's name holds machines and tasks without clashing, since a task's name is lower-case and a machine's upper-case. A task's emits never go to a run that holds a `ref` to the invoking run, nor to the invoking run's [listeners](#host-listeners) ([tasks that emit](#tasks-that-emit)).
- **A run holds a `ref`** to each run whose handle it was given as an argument: the value of a parameter whose type is `ref M`, or contains one (`ref M?`, `ref M[]`, an alias of one). A machine gets a `ref` only as an argument (from host code, or from a parent that passes one of its own to an invoke), never from a result, an event, a call's arguments or a field, so the runs it holds are fixed when it starts. A `ref` variable holds one of them.
- **A peer's end.** When a run closes, however it closes (it finishes, fails, is cancelled, ends with a defect, or is suspended), the runs that hold a `ref` to it and **hear** its end are told, with one `ended` input each, the last input they get from it ([ending a run](#ending-a-run)). A run's machine hears `ended` from a run of machine `M` when it has a transition, in any state, with trigger kind `ended` and an address whose `machine` is `M`, whatever the address's name: writing `on inventory.ended` is the subscription, and there is no other. `ended` is lower case, so it is never the name of an event, and the filter keeps it alongside the events it hears. The filter is decided from the chart alone, as for emitted events: a holder that hears `ended` and has no enabled transition for it when it takes it, because its guard fails or it is in another state, writes `dropped`; one that does not hear it is not sent it and writes nothing. A run held under two names (a parameter and a variable, or two parameters) is one subscription, so its end is one input, which either name can take. Messages sent to a closed run are still `undelivered` and calls to it `Unanswered`. Only a peer's end is told: a child's end is its parent's invoke or spawn outcome ([child runs](#child-runs)), and the output, error or defect of a peer is not part of what its holders get.
- **Ending a run.** The exit effects that run when a run finishes or is cancelled are effects like any others, and can send and emit. Whatever is still queued for a run when it ends, messages and emitted events included, is discarded without a record ([ending a run](#ending-a-run)).
- **Defects.** A runtime error inside the core discards every command of the step, so nothing the step sent or emitted is delivered and no record of it is written; the snapshot the defect is applied to keeps its `nextMessage`. A host defect in an action stops the step's commands, so the sends and emits after it are neither delivered nor traced. `failWithDefect` runs no exit effects, so it sends and emits nothing itself; a child run it cancels runs its own cancel step, whose emitted events find the parent already ended.

## Calls between runs

A **call** asks a peer for an answer that only the caller receives ([asking a peer](../docs/design/actors.md#asking-a-peer)). The callee declares it (`call reserve(…) -> R ! E`, a key of its `calls`); a holder of a `ref` to it makes it with `invoke inventory.reserve(…)`, takes it with `on reserve`, and answers it with `reply`, in the step that took it or a later one.

- **The caller's side is an invoke.** A call invoke has an id, `invoke.started` and `invoke.cancelled` records, and an outcome taken as `invoke.done` or `invoke.fail`, exactly as a task does ([starting work](#starting-work)): it belongs to its state, is cancelled when the state is left, and its outcome is stale after that. The core gives the driver `invoke.start` with `srcKind` `call`, the call's name as `src`, and the callee's run id as `to`.
- **`Unanswered`.** Besides its declared errors, every call can fail with the built-in error `{"_tag": "Unanswered"}`, which has no fields: when the callee was not open when the call was made, when it ended before answering, and when it took the call with no enabled transition. `fail Unanswered` and `fail *` take it like a declared error; it is never `undeclared`.
- **The callee's side is an input.** The call reaches the callee's queue as `{"kind": "call", …}` and is selected like an event, among the `on c` transitions ([candidates](#candidates)). A call that enables a transition is **pending**: it is in `snap.calls` until a `reply` answers it, and `call` names it in the transition's guard and effects; a variable of type `call c` can keep it for a later step. A call that enables no transition is dropped, and the core answers it at once with `Unanswered`; in a strict machine it is the runtime error `unhandled` instead, and the callee's driver answers it `Unanswered` as the run ends.
- **Replying.** `reply x done v` or `reply x fail e` removes the call from `snap.calls`, writes a `replied` record and gives the driver a `reply` command ([effects](#exiting-and-entering-a-state)). Each call is answered once: a `reply` through a value whose call is no longer pending is the runtime error `answered`. A pending-call value never leaves the run: it is no argument, field, event or result.
- **The answer is the caller's outcome.** The callee's driver pushes `invoke.done` or `invoke.fail` for the caller's invoke onto the caller's queue, unless the caller has closed or cancelled that invoke ([carrying out calls](#carrying-out-calls)). When a callee ends with calls pending or queued, its driver answers them `Unanswered`.
- **Ending.** `finish`, `cancelRun` and `failWithDefect` leave `snap.calls` as it is, so the snapshot of a run that ended with calls pending still lists them: they are the calls its driver answers `Unanswered` as it ends. Saving such a run gives them, and restoring it is refused as `ended` first.
- **Saving.** A run with pending calls is not idle, so suspend waits until it has answered them, and a saved snapshot that has them is not restored (`calls`): their callers are gone. A caller holds a `ref`, so it is never restored either ([restoring](#restoring-a-snapshot)).
- **Messages are unchanged.** A call is not a message: `on *` never takes one, and a machine's `events` and `calls` are separate. A request and a reply made of a `send` and an `emit` still work as before.

## Spawned children

A machine declares keyed collections of children, `children sessions: Session by SessionId` (or of tasks, `children uploads: upload by string`), and starts, addresses and stops them by key with the effects `spawn sessions[k] = Session(args)`, `send sessions[k].E`, `send sessions.*.E` and `stop sessions[k]` ([language](../docs/design/language.md#many-children)). The rules:

- **A collection belongs to the run.** It is declared on the machine, not on a state, so a spawned child is not stopped when the state whose effect spawned it is left. It runs until it ends by itself, until a `stop` effect stops it, until a `spawn` with the same key replaces it, or until its parent run ends.
- **Keys.** A key is a `string`, an `int` or an enum variant (or an alias of one), and keys are compared by value: two keys are the same when their JSON forms are equal. A collection has at most one child per key, running or pending; different collections may use the same keys.
- **Spawning.** A `spawn` effect evaluates the key, then the arguments, left to right; stops the child that has that key in the collection, if any, exactly as `stop` would (a running one with a `spawn.stopped` record, a pending one by dropping it); and adds the new child to the **pending spawns**. Nothing starts yet: like an invoke, a spawned child starts once the macrostep has settled, before the states' work ([starting work](#starting-work)), and gets its id from `nextInvoke` then. A child spawned in a macrostep that ends the run, or that a later effect of the same macrostep stops or replaces, never starts and leaves no record.
- **Replacing** is stopping and spawning: the old child's cancel step runs (the driver waits for it, as for `stop`), and the new child starts with the step's other work. What the old child emitted or its outcome, if the driver had already queued them, is stale.
- **Stopping.** `stop sessions[k]` evaluates the key and stops the running child with that key: it is removed from `spawned`, a `spawn.stopped` record is written, and the driver cancels it as it cancels an invoked child, waiting for its cancel step ([child runs](#child-runs)), so nothing after it in the step happens before the child's exit effects have run. A pending child with that key is dropped instead, with no record. With no child of that key, `stop` does nothing and writes nothing.
- **Sending.** `send sessions[k].E` goes to the running child with key `k`, and is undelivered (`to` `null`) when there is none, a pending child included: a child spawned in this macrostep has not started, as an invoked child has not started in the step that enters its state. `send sessions.*.E` goes to every running child of the collection ([effects](#exiting-and-entering-a-state)).
- **Receiving.** A spawned child delivers what it emits to its parent, with no `as`: the collection names it. As for any receiver, the parent is sent only the events its machine hears ([messages between runs](#messages-between-runs)). `on sessions.E` is taken for an `E` from any running child of `sessions`, with `key` bound to that child's key. An emitted event from a child that has since been stopped or replaced, or whose outcome has been taken, is stale.
- **Ending by itself.** When a spawned child ends, its outcome comes back as a `spawn.done`, `spawn.fail` or `spawn.defect` input. The child is removed from `spawned` when the input is taken (it can still be sent to before then, and the driver then writes `undelivered`). `on sessions.done` is taken for `spawn.done` (with `result` and `key`), `on sessions.fail E` and `on sessions.fail *` for `spawn.fail` (with `error` and `key`), searched from the active leaves like an event ([selecting](#candidates)). An outcome no transition takes is dropped, in a strict machine too, like an invoke's outcome: a chart is not required to handle its children's outcomes. A failure with an error the child's machine does not declare is an `undeclared` defect, as for an invoke.
- **Defects travel up.** A spawned child that ends with a defect ends its parent with the same defect, as an invoked child does: defects are not flow. A defect in a child's cancel step, while its parent carries out `spawn.stop`, is a defect of the parent too, as for `invoke.cancel`. (Whether a parent should instead supervise its children and survive their defects is [not decided yet](../docs/design/actors.md#supervision); this is the current default.)
- **When the parent ends.** A parent that finishes or is cancelled exits its states first, every exit effect included, then stops every spawned child still running, in the order they were started, each with a `spawn.stopped` record ([finishing](#finishing)); its trace then ends with `done`, `failed` or `cancelled`. A parent that ends with a defect stops them with no record, as `failWithDefect` cancels its invokes. Pending spawns are dropped.
- **Ids.** A spawned child's id comes from the same sequence as invokes, so its child run is `<parent run>.<id>` and its records sort among the invoked children's by id ([order](trace.md#order)).
- **Nothing more.** A chart cannot ask how many children a collection has or which keys it holds: there are no expressions over collections. A parent that needs to know keeps its own count in a variable, or has its children emit.
- **Collections of tasks.** A collection whose `kind` is `task` holds **spawned tasks**, and every rule above holds for them, with these differences. The `spawn.start` command has `srcKind` `task`, and the driver starts the task's host implementation as it starts an invoked task, with no invoke context and so no child run, no run id and no records of its own ([the emitter](#the-emitter)). `spawn.stop` drops or interrupts it at once, as `invoke.cancel` does a task. What it emits reaches the parent as `task.emitted` inputs, never `emitted` ones ([tasks that emit](#tasks-that-emit)). Its outcome is a `spawn.done`, `spawn.fail` or `spawn.defect` input as a child run's is: a failure with an error the task does not declare is an `undeclared` defect, and a host defect in it is the parent's defect. A chart never sends to one (the compiler rejects it), so the `send` rules above never meet a spawned task.

## Tasks that emit

A task may declare events it emits while it runs (`task upload(file: File) -> Url ! UploadFailed emits Progress { percent: int }`, the task's [`emits`](ir.md#functions-actions-and-tasks)), and its host implementation emits them through an **emitter** the driver gives it ([the emitter](#the-emitter)). A **stream task** is one that returns nothing, emits, and runs until its invoke is cancelled; it is an ordinary task, and nothing below treats it apart. The rules:

- **Who is given them.** The run that invoked the task with `as` (an invoke whose `as` is not `null`), or spawned it into a collection of tasks, and only the events its machine hears from that task ([messages between runs](#messages-between-runs)). An invoke without `as` has no name to receive them under: its emitter is inert, and nothing it emits is queued. Nothing a task emits goes to another run, a run's listeners, or the trace, until the run takes it.
- **The input.** Each emit that is given to the run is the input `{"kind": "task.emitted", "invoke": id, "value": v}`: `id` is the id of the invoke or spawned task (the two share one sequence), and `v` the event, a tagged record of one of the task's `emits`. It has no sender run and no message id, and is pushed onto the run's own queue like an outcome.
- **Taking it.** It is selected from the active leaves ([candidates](#candidates)): through an address of kind `task` whose `state` and `name` name the invoke, or through a collection of tasks that holds the spawned task, binding `key`. One that enables no transition is `dropped`, in a strict machine too. Taking it consumes nothing: the invoke or spawned task goes on running.
- **Staleness.** One whose invoke or spawned task is no longer in the snapshot is `stale`: the state was left, or the spawned task was stopped or replaced, after the emit had been queued ([taking an input](#taking-an-input)). It is never stale because the task's outcome was taken, since nothing a task emits is queued after its outcome. An emit taken before a queued outcome can itself leave the state: that cancels the invoke, and the outcome is then the stale input.
- **Order.** A task's emits are queued in the order it made them, all of them before its outcome, so a run takes every emit of a task that ended before it takes the task's outcome. Nothing orders them with what other sources push onto the queue.
- **Cancelled work queues nothing.** Once the driver has carried out `invoke.cancel` or `spawn.stop` for the task, or the core has taken its outcome, the emitter is closed and queues nothing more; what it queued before is stale when taken.
- **Saving.** Nothing about a task's emits is saved. A task, emitting or not, invoked or spawned, is work in flight, so a run with a stream task running is never idle and suspend waits until it ends or its state is left ([suspending](#suspending-and-resume)). Restoring starts it again from the beginning ([restoring](#restoring-a-snapshot)), and it emits again as it runs.

## Raised events

A machine can send itself one of its own events with `raise` ([language](../docs/design/language.md#events-a-machine-raises)): at once, on the internal queue, as SCXML's `<raise>`; or after a delay, as a timer that belongs to the run, as SCXML's `<send delay>` to itself. An event declared `internal event E` is in the machine's `internalEvents`, not its `events`: only `raise` can produce it, so host code and other runs cannot send it, and it is in no interface. `raise` can raise any of the machine's own events, internal or not. The rules:

- **`raise E`** evaluates its value, writes a `raised` record and appends the event to the internal queue ([effects](#exiting-and-entering-a-state)). It is taken in the same step, while the step settles ([settling](#settling)), after every `always` that is enabled and after what was queued before it (a completion, another raised event), and before the step ends: so before any input the driver gives, an external event already queued among them. It is selected as the same external event would be ([candidates](#candidates)), in several regions at once if it enables transitions there, and its `event` record has the input `{"kind": "raised", "value": v}`. One that enables no transition is `dropped`, and in a strict machine the runtime error `unhandled`.
- **Not tied to a state.** A raised event stays queued when the state whose effect raised it is exited, unlike a completion: it belongs to the run. A raise in a final state's `entry` is queued before that state's completion, since entry effects run before `complete`.
- **During `start`**, a raise in an initial entry is taken while `start` settles.
- **When the step ends the run**, the internal queue is cleared: what `finish` and `cancelRun` raise in exit effects is written as `raised` and never taken. The queue is always empty between steps, so it is never saved.
- **`raise E after d`** (with or without `as n`) is a **delayed raise**. It evaluates the delay, then the value; if it has a name, cancels the pending raise of that name, as `cancel n` would; takes a timer id from `nextTimer`; adds `{id, name, delay, value}` to `snap.scheduled`; writes `raise.scheduled`; and gives the driver `timer.start` at once, not in [starting work](#starting-work), since no state owns it. Its timer is a timer like any other for the driver ([time](#time)).
- **Its expiry** is the input `{"kind": "after", "timer": id}`, queued by the driver like any timer's. Taking it consumes the raise (it is removed from `scheduled`) and selects its event as an external event is selected, `event` bound to its value ([taking an input](#taking-an-input)). It is an ordinary input, so external inputs queued before it are taken first. One that enables no transition is `dropped`, and in a strict machine `unhandled`. Leaving the state that started it changes nothing: it fires whatever states are active then.
- **Replacing.** A delayed raise whose name is pending replaces the pending one: a `raise.cancelled` record and `timer.cancel` for the old one, then the new one starts with a new id. So `raise Check after 500ms as debounce` in an `on Key` restarts the delay on every key. An unnamed delayed raise never replaces one, and cannot be cancelled.
- **`cancel n`** cancels the pending delayed raise named `n`: it is removed from `scheduled`, with a `raise.cancelled` record and `timer.cancel`. With none pending it does nothing and writes nothing, as `stop` of a key with no child. A cancelled raise whose expiry the driver had already queued is pending no longer, so that expiry is `stale` when taken.
- **When the run ends.** A run that finishes or is cancelled cancels every pending delayed raise, after its exit effects and after stopping its spawned children, in the order they were started, each with `raise.cancelled` and `timer.cancel` ([finishing](#finishing), [cancelling](#cancelling-from-outside)); a child run cancelled by its parent so too. A run that ends with a defect cancels them with no record, as its timers (`failWithDefect`).
- **Idle and saving.** A delayed raise is a timer, so a run waiting on one is idle ([suspending](#suspending-and-resume)), and suspend cancels its timer with no record, as it does an `after` timer's. It is saved in `scheduled`, with its due time, and restored with the time that remains ([restoring](#restoring-a-snapshot)).

```
cancelRaises():                       # the run finishes or is cancelled
  for sc in snap.scheduled, in order:
    emit trace { kind: "raise.cancelled", timer: sc.id, name: sc.name }
    emit timer.cancel { timer: sc.id }
  snap.scheduled = []
```

A machine with no `raise` has an internal queue of completions only, in the order they were queued before raised events existed, and an empty `scheduled`, so nothing above changes it.

## Defects

A defect ends the run at once. It is never routed to a `fail` branch, and no exit effect runs, since the machine can no longer be trusted to run its own code. A defect is an object:

| Defect | Cause |
| --- | --- |
| `{"kind": "host", "message": "…"}` | Host code failed: a function, action or task threw or panicked, or returned a value that is not of its declared type (a negative `duration`, say), or a task emitted an event that does not encode ([the emitter](#the-emitter)). `message` is set as described under [catching host defects](#catching-host-defects); in conformance cases it is given by the script. |
| `{"kind": "unassigned", "var": "receipt"}` | A variable was read before it had a value. For a state's variable `var` is its qualified name, `Order.Confirming.code` ([data local to a state](#data-local-to-a-state)). The compiler rejects such a read (`RB0403`, definite assignment), so this is a runtime backstop, reachable only where that check is skipped, as for files listed in `allow-unassigned`. |
| `{"kind": "overflow", "expr": "attempts + 1"}` | Integer arithmetic left the `int` range, or float arithmetic produced a value that is not finite. `expr` is the `text` of the operator's IR node. |
| `{"kind": "undeclared", "error": "Oops"}` | A task or child machine failed with an error it does not declare. |
| `{"kind": "limit"}` | Settling took more than 1000 microsteps. |
| `{"kind": "unhandled", "event": {"kind": "event", "value": {"_tag": "Knock", …}}}` | A strict machine was given an external event, a message, a call or the expiry of a delayed raise that enabled no transition, or raised an event that enabled none ([taking an input](#taking-an-input), [settling](#settling)). `event` is the input, an `event`, a `message`, a `call`, an `after` or a `raised`, exactly as an `event` record would have it, so a message keeps its sender and message id. |
| `{"kind": "answered", "call": "reserve"}` | A `reply` was given a pending-call value whose call is not pending: it was answered already, or the run was restored ([calls between runs](#calls-between-runs)). |

**Inside the core.** Runtime errors (`unassigned`, `overflow`, `undeclared`, `limit`, `unhandled`, `answered`) and a `host` defect returned by `fns` while evaluating an expression happen inside `start` or `step`. The call is then **atomic**: every command it had produced is discarded, and it returns `failWithDefect(d)` applied to the snapshot it was given, so even an invoke the failed step had consumed is cancelled again (a driver ignores that). There is no `event` record for the input.

**A defect in `start`** works the same way, applied to the initial snapshot (`active` `[]`, `vars` `{}`, no invokes or timers): `start` still returns a snapshot, with status `defect`, and two commands, the `started` record and the `defect` record. The `started` record is kept so the trace still says what the run was. The driver then ends the run like any other that ends with a defect: a root run raises the defect to whoever called `run`, a child run reports it to its parent as `invoke.defect`. A host defect in an action of `start`'s commands (a root `entry` effect, say) is an ordinary action defect, below.

**A host defect in an action** is found by the driver while carrying out a step's commands (or `start`'s). It stops carrying them out, writes none of the remaining records, and gives the core `{"kind": "defect", "defect": {"kind": "host", "message": …}}`, also when the step had already ended the run ([taking an input](#taking-an-input)). **A host defect in a task** arrives as `invoke.defect`, and is ignored as stale if that invoke was already cancelled. **A child run that ends with a defect** is reported to its parent the same way, with the child's defect object unchanged, so the parent's run ends with a defect too and the defect travels up to whoever ran the root. **A defect in a child's cancel step**, while its parent carries out `invoke.cancel`, is met by the parent's driver like a defect in an action: it gives the parent `{"kind": "defect", "defect": d}` with the child's defect `d` unchanged ([child runs](#child-runs)).

```
failWithDefect(d):
  for inv in snap.invokes: emit invoke.cancel { invoke: inv.id }    # no trace records
  for ch in snap.spawned:  emit spawn.stop { spawn: ch.id }
  for tm in snap.timers:   emit timer.cancel { timer: tm.id }
  for sc in snap.scheduled: emit timer.cancel { timer: sc.id }
  snap.invokes = []; snap.spawned = []; snap.timers = []; snap.scheduled = []
  clear the pending spawns
  snap.status = "defect"; snap.result = { defect: d }
  emit trace { kind: "defect", defect: d }
```

Cancelling a child run here (an invoked one or a spawned one) is an ordinary [cancellation](#cancelling-from-outside) of the child, which runs the child's exit effects, and the driver waits for it as for any `invoke.cancel`. A defect in that cancel step is written to the child's trace and changes nothing in the parent, which has already ended: the parent's driver ignores it and goes on with the remaining cancels and the `defect` record ([the driver](#the-driver)). A driver ignores an `invoke.cancel`, `spawn.stop` or `timer.cancel` for an id that has already ended, or whose start it never carried out (because a step was cut short by an action's defect). After writing the record the driver raises the defect to whoever ran the run, as the host language does: `Effect.die` with a `Defect` value holding the defect object in TypeScript, and `std::panic::resume_unwind` with a `roadbook::Defect` payload in Rust.

### Catching host defects

The driver catches everything host code throws or panics, so that a defect becomes an input and the trace gets its `defect` record:

- **Where.** Around every call of a function (in the binding layer, so `fns` returns the defect to the core), every action, and every invoke's implementation, for the whole time the driver polls or awaits it. In Rust that is `std::panic::catch_unwind` around a function call and `FutureExt::catch_unwind` (or the equivalent `catch_unwind` around each `poll`) around an action's or an invoke's future, with `AssertUnwindSafe`, since the run is ended anyway. In TypeScript it is `try`/`catch` around a function and the defect channel (`Effect.die`, a thrown exception inside an Effect) of an action's or an invoke's Effect.
- **The message.** A Rust panic payload that is a `&'static str` or a `String` is the message. Any other payload gives the message `Box<dyn Any>`, which is what Rust's own panic hook prints for one. In TypeScript, a defect that is an `Error` gives its `message`, and any other value `String(value)`. A value that is not of its declared type (a decoding failure) gives a message chosen by the binding layer, which no conformance case compares.
- **A defect that is already a Roadbook defect** is passed on unchanged rather than wrapped: a Rust payload that downcasts to `roadbook::Defect`, or a TypeScript `Defect` value. That is what a child run raises when it ends with a defect, so `{"kind": "unassigned", "var": "receipt"}` in a child reaches its parent as `invoke.defect` with that same object, and so on up to the root.

## Expressions

Expressions are evaluated by their IR node, recursively. Every node's `type` is known, and evaluation uses it where the JSON form of a value would be ambiguous.

| Node | Value |
| --- | --- |
| `lit` | `value`, read according to `type` (an `int`, a float, a string, a bool, or a duration in milliseconds). |
| `ref` | With `state` `null`, `path[0]` is a parameter (from `input`), a variable (from `vars`; reading one with no value is `unassigned`), or the implicit name `event`, `result`, `error`, `call` or `key`. With `state` a state's id, `path[0]` is a parameter or variable of that state, from `snap.locals[state]` (reading one with no value is `unassigned`, named `state + "." + path[0]`). Each further name reads that field of the value. A parameter or variable of a `ref` type reads as the run id it holds. |
| `field` | Evaluate `of`, then read field `name`. |
| `call` | Evaluate `args` left to right, then `fns(fn, args)`. |
| `record` | An object with the fields in order. Its `type` may be an instance of a generic record (`Page<Order>`); the fields are the same. If `type` is an error, it also has `"_tag": type`. If `type` is an event (the value of a `send`, `emit` or `raise`: `Machine.Name`, or the name of one of the current machine's `emits`, `events` or `internalEvents`), it also has `"_tag"` with the event's name, the part after the dot: `send upload.Pause` sends `{"_tag": "Pause"}`. |
| `enum` | The string `variant`. |
| `op` | See below. |

Operators, by `op`. An operand's type is its IR `type` with aliases expanded, transitively, the arguments of a generic alias substituted: `<` on a `Money` after `type Money = int` compares integers, and so does `+` on a `First<int>` after `type First<T> = T`. The IR keeps the alias's name, so a runtime that dispatches on the type string must expand it first.

- `and`, `or`: evaluate the left side; if it decides the result (`false` for `and`, `true` for `or`), that is the result and the right side is **not evaluated**. Otherwise the result is the right side. `not` negates.
- `==`, `!=`: on two values of the left operand's type, compared by walking that type, as [equality](#equality) says. `!=` is the negation of `==`.
- `<`, `<=`, `>`, `>=`: on `int`, `float` and `duration` numerically, and on `string` by Unicode scalar values, lexicographically (which is Rust's `str` order; in TypeScript compare code points, not UTF-16 code units).
- `+`, `-`, `*`, and `neg` (unary minus), on `int` and `float` only (the language has no arithmetic on durations): when the node's `type` is `int`, exact integer arithmetic, and a result outside the runtime's `int` range is `overflow` (see [values](#values) for the range). When `type` is `float`, IEEE 754 double arithmetic; a result that is infinite or NaN is `overflow`.

Both operands of a binary operator are evaluated, left first, except for the short circuit above. Function calls are the only part of evaluation the host can observe, so these rules fix which calls are made: a conformance case lists every call its run makes and a call that is not listed fails the case.

## Values

Every Roadbook value has one JSON form. It is used for the machine's input, task and machine results, errors, events, function arguments and results, action arguments, snapshots, traces and conformance scripts.

| Type | JSON |
| --- | --- |
| `string` | A string. |
| `int` | An integer number. Read in any form (`2.0` is the integer 2, `-0.0` is 0), since a JSON number's form does not matter; written in integer form. |
| `float` | A number, in canonical form when written: `2.0` is written `2`, `-0.0` is written `0`. Always finite. |
| `bool` | `true` or `false`. |
| `duration` | A non-negative integer number of milliseconds. A host duration finer than a millisecond is truncated to whole milliseconds when it enters the core. A negative one (which TypeScript can produce and Rust's `Duration` cannot) is not a `duration`, and neither is one longer than 2^53 − 1 milliseconds (Rust's `Duration::MAX`, a common "never", or an infinite TypeScript `Duration`), which a saved snapshot could not hold: the binding layer rejects both as a `host` defect, in both runtimes. |
| `json` | The value itself. |
| A record (`type R { … }`) | An object with every field, by name. An optional field with no value is `null`, never omitted. |
| An error (`error E { … }`) | An object with `"_tag": "E"` and every field: `{"_tag": "CardDeclined", "reason": "expired"}`, or `{"_tag": "GatewayUnavailable"}` for an error with no fields. |
| An event (`event N { … }`) | The same as an error: `{"_tag": "Check", "amount": 5}`. |
| An enum | The variant's name: `"Customer"`. |
| `T?` | `null`, or the `T`. (`T??` cannot be told apart from `T?` and should be rejected by the compiler.) |
| `T[]` | An array. |
| `map<K, V>` | An object with a member per key, each value in `V`'s form: `{"a": 1, "b": 2}`. The order of its members means nothing (canonical JSON sorts them), and any string is a key, `"__proto__"` included: a JavaScript runtime keeps it as an own data member, never as the object's prototype. |
| An instance `G<A, …>` of a generic record or alias | The form of `G`'s declaration with each parameter replaced by its argument: `Page<int>` is `{"items": [1, 2], "next": null}`. |
| An alias | The aliased type's form. |
| An `extern` type | Whatever the language binding's encoder produces. The core never looks inside it. Conformance cases do not use extern types. |
| `ref M` | The id of the run it refers to, a string: `"r2"`. The binding layer gives host code a handle to that run instead ([refs](#refs-and-handles)). |
| `call c` | A pending-call value: `{"from": "r2", "invoke": 1}`, the caller's run id and the id of its invoke. Host code never sees one ([calls between runs](#calls-between-runs)). |

Arguments, inputs and the `args` of actions and invokes are objects keyed by parameter name. Values are decoded against their declared type by the binding layer before they reach the core, so the core can tell an `int` from a `float` without looking at the JSON.

**Integers.** The language's `int` is 64-bit. The Rust runtime holds it as `i64`, and its overflow bound is `i64`'s. The TypeScript runtime holds it as a `number`, and its overflow bound is ±(2^53 − 1), the range in which a double is exact: an arithmetic result outside it is `overflow`, and an input integer outside it is rejected by the decoder as a host defect. Between those bounds the two runtimes differ, and traces of values there are not comparable (canonical JSON rounds them to the nearest double). Conformance cases keep every integer within ±(2^53 − 1) and do not test overflow.

### Equality

`==` compares two values of one type, its left operand's IR `type`, by walking the type, aliases expanded at each level and the arguments of a generic instance substituted. It never compares the host form of a value (object identity in TypeScript, the JSON spelling of a number):

- **`int`, `float`, `duration`** are equal when they are the same number, whatever the JSON spelling: `1` and `1.0` are the same `int`, and floats compare as IEEE 754 doubles, so `0.0 == -0.0`. A `float` is always finite, so NaN does not arise.
- **`string`, `bool` and enums** are equal when they are the same JSON value. Strings are equal when their Unicode scalar values are.
- **A record or an error** (an instance of a generic record included) is equal to another when each field the type declares is equal by the field's type. A field a value lacks is `null`. Two values of one error type have the same `_tag`, so it is not compared.
- **An event** (`Machine.Name`, `task.Name`, or by its bare name one of the current machine's `events`, `internalEvents` or `emits`) compares as a record of its declared fields.
- **`T[]`** values are equal when they have the same length and are equal element by element, in order, by `T`.
- **`map<K, V>`** values are equal when they have the same keys and the values at each key are equal by `V`. The order of their members does not matter.
- **`T?`**: `null` is equal only to `null`; two values that are not `null` compare by `T`.

The compiler allows `==` only where every part of the type is one of these (`RB0303`): never on `json`, an `extern` type, a `ref` or a pending call. A value that is not of its type (which a hand-written IR could give), and a value of a type the walk does not cover, compares as JSON: numbers numerically, arrays element by element, objects by their keys and the values at each whatever the order, and anything else as the same JSON value.

## The driver

The driver gives each run a queue and a clock, carries out the core's commands with the host's concurrency, and ends the run.

```
run(machine, input):
  (snap, commands) = core.start(ir, machine, runId, input, fns)
  snap = carryOut(snap, commands)
  while snap.status == "running":
    input = take()                  # a cancel request if there is one, else the front of the queue
    (snap, commands) = core.step(ir, snap, input, fns)
    snap = carryOut(snap, commands)
  end the run with snap.result      # see "Ending a run"

carryOut(snap, commands):
  for c in commands, in order:
    carry out c                     # an action or an invoke.cancel is waited for
    if that met a defect d:         # a host defect in an action, or a defect in a child's cancel step
      if snap.status == "defect":   # already ended with a defect (failWithDefect's cancels): d changes nothing
        continue
      (snap, rest) = core.step(ir, snap, { kind: "defect", defect: d }, fns)
      carry out rest                # cancels and the defect record; a defect met here is ignored
      return snap
  return snap
```

- **Commands are carried out in order, one at a time.** An action is awaited before the next command, and so is an `invoke.cancel` ([child runs](#child-runs)). Starting an invoke or a timer only registers it; its outcome comes back later through the queue.
- **A defect stops the commands.** The commands after the one that met a defect are not carried out and their records are not written; the core's answer to the `defect` input is carried out instead. That answer is the same whether or not the step had already ended the run. The one exception is a run whose snapshot has already ended with a defect: the core would not take the input ([taking an input](#taking-an-input)), so the driver does not give it, ignores the defect and carries out the remaining commands. That happens when a child's cancel step has a defect while the driver carries out the cancels of `failWithDefect`, and the commands after it are that function's other cancels and the run's own `defect` record ([defects](#defects)).
- **Outcomes of a task.** Success gives `invoke.done` with the value. A failure with a tagged error gives `invoke.fail` with it (the core checks that it is one the task declares, and makes any other an `undeclared` defect). A defect gives `invoke.defect` ([catching host defects](#catching-host-defects)). For a spawned task they are `spawn.done`, `spawn.fail` and `spawn.defect`, with `spawn` in place of `invoke`.
- **Run ids.** The driver names each top-level run; a conformance runner names them `r1`, `r2`, … in the order the case starts them. A child run's id is its parent's id, a dot, and the id of the invoke that started it: `r1.4` is the run started by invoke 4 of run `r1`. A `ref` is a run id, so the ids of top-level runs that can reach one another must differ: the default names (`r1`, `r2`, … in the order runs start in the process) do, and a driver given a fixed id (Rust `Config::run_id`, a `RootRunId` of one's own) must not be given the same one twice. A fixed id must be a run id as [`snapshot.schema.json`](snapshot.schema.json) writes one, `^[A-Za-z0-9_-]+(\.[1-9][0-9]*)*$`, or the run's saved snapshot would be refused as `invalid`: a runtime refuses any other when it is given (Rust's `Config::run_id` panics; in TypeScript the run dies with a host defect as it starts, before any record).

### One queue per run

Every source of input pushes onto one first-in, first-out queue per run, at the moment the input arrives. The queue is unbounded: nothing is ever refused because a queue is full ([full queues](../docs/design/actors.md#full-queues)).

| Source | Pushes |
| --- | --- |
| A caller's `send` | the external event |
| Another run's `send` | a `message` input ([delivery](#delivering-messages)) |
| Another run's `emit` | an `emitted` input, for its parent (its invoker or spawner) and the runs that hold a `ref` to it, of those that hear it |
| A task, or a machine invoke that is not a child run | its outcome, when its future or fiber ends |
| A task's emitter | a `task.emitted` input, for the run that invoked it with `as` or spawned it, of the events that run's machine hears from it ([the emitter](#the-emitter)) |
| A child run | the outcome of the invoke that started it, when the child ends |
| A timer | its expiry, when its sleep completes |
| Another run's `invoke` of a call | a `call` input ([carrying out a call](#carrying-out-calls)) |
| A callee's `reply`, or its driver when it ends | the outcome of the call's invoke |
| A peer's driver when the peer closes, or this run's own driver when the peer had closed already | an `ended` input, for the runs subscribed to it that hear its end |

The run takes inputs from the front, one at a time, and takes the next only once the previous step's commands have all been carried out. A cancel request is the one exception: it is not queued, and is taken before anything that is.

- **Nothing reaches the core except through the queue.** An Effect driver can make the queue an Effect `Queue` that senders, invoke fibers and timer fibers offer to. A Rust driver's loop `select`s over the event channel, the set of invoke futures and the timers only to find out what has arrived: it pushes every input it finds onto the run's queue (a `VecDeque`, say) and gives the core the front of that queue, never an input straight from the source that produced it. So an outcome that completes while the run is busy, or in the same poll as an event, waits behind what arrived before it, and may be stale by the time it is taken. Inputs that one poll finds ready together are pushed in an order the driver chooses; nothing outside conformance depends on that order, and a conformance runner never lets it arise.
- **Several inputs at once.** A driver lets a caller append a list of inputs to a run's queue in one operation, so that the run takes no step between them: `Queue.offerAll` on the run's queue in Effect, and in Rust a method that pushes them all onto the queue while the run is between steps. A conformance runner delivers a `together` step this way ([conformance](conformance/README.md#how-a-runner-works)).
- **An invoke is forgotten once its outcome is taken.** When the core takes an invoke's outcome (whether it writes `event`, `dropped` or `stale`), the driver drops that invoke's future or interrupts its fiber if it is still there, and closes its emitter. Normally it has already ended; it has not when its outcome was appended by a caller, as a conformance runner does for a scripted invoke in a `together`, and dropping it then ensures it never queues a second outcome.
- **Cancelled work queues nothing.** Once the driver has carried out `invoke.cancel`, `spawn.stop` or `timer.cancel`, the invoke, spawned child or timer has been dropped or interrupted and pushes nothing more, and a task's emitter is closed. An outcome, emit or expiry it pushed before stays queued and reaches the core, which writes it as `stale`.
- **Cancellation takes effect between steps.** A cancel request does not interrupt the commands of a step in progress. When they are done the driver gives the core `{"kind": "cancel"}`, carries out the resulting commands, and discards anything still queued.

### Child runs

A machine invoke's implementation is host code: in TypeScript the value given for it in `machines`, in Rust a `Handlers` method whose body usually calls the child machine's `run`. When that code runs the chart, the driver has to know, so that the child writes to the parent's trace on the parent's clock and is cancelled gracefully. Both drivers do it with an **invoke context**.

- **The context.** When the driver carries out `invoke.start` or `spawn.start` with `srcKind` `machine`, it runs the implementation with an ambient invoke context holding the trace sink, the clock and timer, the parent's run id, the invoke id, a fresh cancel token, the messages the parent has sent to the invoke so far (none yet), the saved snapshot to restore when the command's `saved` is not `null`, and, when the command is `spawn.start` or its `as` is not `null`, a way to push onto the parent's queue the emitted events the parent's machine hears from the invoked or spawned machine ([messages between runs](#messages-between-runs)). In TypeScript the context is a `Context.Reference` provided to the invoke's fiber. In Rust it is a task-local, or a thread-local that the driver sets around each `poll` of the invoke's future and restores afterwards, which works on any executor. All other host code the driver runs (functions, actions, tasks) runs with no invoke context; so does a child run's own work, except that each of its machine invokes gets a context of its own.
- **Claiming it.** A machine's `run` looks for an invoke context. If it finds one whose token is unclaimed, it claims the token and becomes the **child run**: its run id is `<parent run id>.<invoke id>`, its records carry `parent`, and it uses the context's sink, clock and timer. Its queue starts with the messages the context holds, in the order they were sent (after a restored child's overdue expiries: [restoring a child run](#restoring-a-child-run)), and from then on the parent's sends go straight to its queue. When the context holds a saved snapshot, the child run restores it instead of starting ([restoring a child run](#restoring-a-child-run)). Otherwise it is a root run. Host code that does not call `run` (a stub) never claims the token, which is how the parent's driver tells a stub from a child run. The parent's core cannot tell them apart and does not need to.
- **Its outcome.** A child run ends like any run: on `done` its `run` returns the output, on `failed` the error, and on `defect` it raises the defect ([catching host defects](#catching-host-defects)). The parent's driver turns that into `invoke.done`, `invoke.fail` or `invoke.defect` as for a task, with the output, error or defect object unchanged, and pushes it onto the parent's queue; for a spawned child, into `spawn.done`, `spawn.fail` or `spawn.defect` with the same members and `spawn` in place of `invoke`.
- **Spawned children** of a collection of machines are child runs like any other: the host's implementation of the machine runs with an invoke context whose invoke id is the spawn's id, and `spawn.stop` is carried out exactly as `invoke.cancel`. A stubbed machine spawned is a stub like an invoked one. Below, "invoke" includes them. A spawned task, `spawn.start` with `srcKind` `task`, runs with no invoke context, exactly as an invoked task does ([the emitter](#the-emitter)).
- **Cancelling it.** On `invoke.cancel` the parent's driver signals the token. If the token was never claimed (a task or a stub), it drops the future or interrupts the fiber at once and is done. If it was claimed, the child takes the signal as a cancel request, between its steps like any other, runs its cancel step (exit effects and the `cancelled` record) and reports through the token that it has finished. The parent keeps driving the child until then (in Rust, it keeps polling the child's future), and only then drops it. Carrying out `invoke.cancel` waits for this, like an action, so nothing after it in the parent's step (the exit effects of the state being left, the rest of the transition) happens before the child's exit effects have run. A child that has already ended, however it ended, reports plain "finished" at once (never a defect), and its outcome is not queued after the signal; one already queued is stale.
- **A defect in the cancel step** is reported through the token too: the child writes its `defect` record and ends, and the parent's driver meets that defect while carrying out `invoke.cancel` and gives its own core `{"kind": "defect", "defect": d}`, with the child's `d` unchanged, as it would for a defect in an action. A cancelled task or stub has no cancel step, and whatever it does while being dropped or interrupted is ignored.
- **In Effect** the signal is interruption. The parent interrupts the invoke's fiber and waits for it to end (`FiberMap.remove`, or `Fiber.interrupt`, which returns the fiber's exit). A child's `run` runs each step uninterruptibly and its cancel step in an `Effect.onInterrupt` around the step loop, so interrupting it during a step waits for that step to finish, and the interruption completes only after the cancel step. A defect in the cancel step shows in the fiber's exit as a die with a `Defect`, which the parent uses only if the token was claimed.
- **In Rust** the token is a small shared value (a cancel flag with a waker, and a slot for "finished" or "finished with defect `d`"). The child's run loop checks it before each take, and the parent, after signalling it, polls the child's future until the slot is filled. The child's future may never complete after that; the parent drops it.
- **Suspending it.** When the tree of a top-level run stops because it is idle ([suspending](#suspending-and-resume)), the parent's driver signals each child run's token with a **stop** instead of a cancel. The child takes it between steps, as a cancel request, but runs no cancel step and no exit effect: it stops as a suspended run does (it cancels its timers with no record, writes `suspended` and keeps its last snapshot), stops its own child runs the same way first, and reports through the token that it has stopped. The parent keeps driving it until then, as for a cancel. Its outcome is never queued. In Effect the stop is interruption with the token marked as stopping, so the child's `onInterrupt` stops it instead of running its cancel step; in Rust it is a stop flag beside the cancel flag.

### The emitter

When the driver carries out `invoke.start` with `srcKind` `task`, or `spawn.start` with `srcKind` `task`, it gives the task's host implementation an **emitter** along with its arguments ([TypeScript](../docs/design/typescript.md), [Rust](../docs/design/rust.md)). Every task gets one, whether or not it declares `emits`.

- **Live or inert.** The emitter is **live** when the invoke has `as` (the command's `as` is not `null`) or the command is `spawn.start`. A live emitter holds the run's queue, the invoke's (or spawned task's) id, and the **heard** set: the events of the task's `emits` that the run's machine hears from that task ([messages between runs](#messages-between-runs)), which a runtime works out from the IR when it loads it. For an invoke without `as` the emitter is **inert**: emitting through it queues nothing, but the event is still encoded.
- **`emit(event)`** does, in order:
  1. Encode the host value to its JSON form by the event's declared fields ([values](#values)). A value that does not encode (a non-finite `float`, an `int` out of range, a missing field) is the **task's** host defect: the task fails with it, so its outcome is `invoke.defect` (or `spawn.defect`). In TypeScript `emit` dies with a `Defect`; in Rust it panics with a `roadbook::Defect` payload, which the driver's catch around the task's poll turns into the defect ([catching host defects](#catching-host-defects)). This is the rule for a host `send` through a handle ([refs and handles](#refs-and-handles)): the encoding failure is the emitting code's.
  2. If the emitter is inert, if the driver has carried out `invoke.cancel` or `spawn.stop` for it, if its outcome has already been queued, or if the run has closed: do nothing.
  3. If the event's `_tag` is not in the heard set: do nothing, and write no record.
  4. Otherwise push `{"kind": "task.emitted", "invoke": id, "value": json}` onto the run's queue.

  It never waits, since queues are unbounded, and it never fails but for the encoding defect.
- **Order.** A task's emits are pushed in the order it made them, and all of them before its outcome, since the outcome is pushed when the task's Effect or future ends, after its last emit. A Rust driver that finds a task's emits and its end in one poll pushes the emits first, as it does for a child run's emitted events ([order per pair](#delivering-messages)).
- **Closing.** The emitter is closed once the driver has carried out `invoke.cancel` or `spawn.stop` for its work, once the work's outcome has been queued, and once the core has taken its outcome and the driver has forgotten the invoke ([one queue per run](#one-queue-per-run)), and when the run closes. Cancelled work queues nothing: an emit queued before stays queued, and is stale when taken. An implementation may keep its emitter after that, and call it from anywhere (another fiber, another thread): it does nothing then.
- **No records, no listeners.** The driver writes nothing for an emit. The run writes the `event`, `dropped` or `stale` record when it takes the input ([inputs in records](trace.md#inputs-in-records)), and an emit that step 2 or 3 discards leaves no trace at all. A task's emits are never given to the run's [host listeners](#host-listeners), which hear only the run's own `emit` effects.
- **Saving and restoring.** A task is never saved: on restore it starts again with its arguments evaluated again, or, for a spawned task, with its saved `args`, and with a new emitter ([restoring](#restoring-a-snapshot)). Emits queued and not yet taken are not saved, as nothing queued is.
- **Durable runs.** A driver that journals the inputs a run takes and replays them (TypeScript's [durable driver](../docs/design/typescript.md#durable-runs)) runs a task, invoked or spawned, as one activity, as it does a task that does not emit. Its emits are pushed onto the run's queue as it runs, and those the run takes are journalled as inputs and given again on replay, like any other input. A `task.emitted` input has no identity of its own (no sender run and no message id), so the replay's dedupe never drops one. After a crash a task that was still running starts again, at least once as before, and its new emits are new inputs, so the chart sees the work start over. Emits queued and not taken at the crash are lost, at most once for those; a task that had finished gives its stored outcome without running again.

### Refs and handles

The core holds a `ref` as a run id ([values](#values)). Host code holds a **handle**: Rust's `Ref` (`inventory::Ref`, from `actor.handle()`), and its TypeScript counterpart, a value from a spawned run's actor. A handle can be cloned and passed on, and it is how host code reaches a run.

- **Encoding.** The binding layer turns a handle into its run's id when it encodes a value for the core (a machine's arguments), and a run id back into the handle when it decodes a value for host code (the arguments of an invoke, an action, a task or a function). A machine can only get a `ref` from its arguments, so every run id its core gives back, in a `send` or in arguments, is one of the handles the run was started with. The driver keeps those for as long as the run is open. It needs no registry of runs by id: a run reaches exactly the runs it was given. What a run needs to emit is the other way round, the runs subscribed to it ([delivery](#delivering-messages)), which its handle can keep.
- **Where handles come from.** A top-level run that host code `spawn`s has one; host code passes it in the arguments of the runs that are to talk to it. A child run has none: its parent reaches it through its invoke, and nothing else can. The tools' handles that a runtime gives for every run as it starts (Rust's `Config::on_run`, TypeScript's `RunObserver`) can queue inputs on a child run, but cannot cancel it or suspend it: a child run ends only by itself or when its parent leaves the invoking state, since its parent learns how it ended only through the invoke. The same holds for a top-level run of `run`, whose caller gets an output or an error, never a cancellation or a snapshot.
- **Host code sends** through a handle as through an actor: the run gets an ordinary `event` input, with no sender. An event that does not encode (a non-finite `float`) is the sender's host defect, never the receiver's: nothing is queued, and the host code that sent it fails (in TypeScript `send` dies with a `Defect`, in Rust it panics with a `roadbook::Defect` payload), so inside an action or a task it ends the run carrying that out with the defect, as any host defect there does. Host code receives what a run emits through its actor, not through a handle ([host listeners](#host-listeners)).
- **Equality.** The language defines no `==` on `ref`s. The core compares run ids only to tell which address an emitted event is from ([selecting](#selecting-a-transition)).

### Delivering messages

- **Open and closed.** A run is **open** from just before its driver calls `start` (or `restore`) until the core returns a snapshot whose status is not `running`, from `start`, `restore` or any `step`, or until it is [suspended](#suspending-and-resume); it is then **closed** for good. A run closes before the commands of its last step are carried out, so a message that another run sends it while it carries them out is undelivered, and an emitted event is not delivered to it. Checking that a receiver is open and pushing onto its queue should be one operation with respect to its closing; a driver where a push can slip in just before the receiver closes loses that message with the receiver's queue, without a record. No conformance case can tell these apart.
- **Subscriptions.** Before it calls `start`, the driver subscribes the run to the emitted events of every run it holds a `ref` to: each run whose handle is in its arguments, once however many times it appears there (a parameter of type `ref M`, `ref M?`, `ref M[]` or an alias of one). Each subscription keeps the events of `M` that the run's machine hears ([messages between runs](#messages-between-runs)), and `ended` when it hears the end of a run of `M`, which a runtime works out from the IR when it loads it; it can be empty. When the run closes, it is unsubscribed.
- **Subscribing to a peer that has ended.** A run's end is known only once its driver has carried out the commands of its last step: it closes before them, and a host defect in one of them turns `done` into `defect`. So the point that matters is when the peer's driver **tells its end** ([ending a run](#ending-a-run)), not when it closes. Checking whether the peer has told its end and subscribing are one operation with respect to that telling, as pushing onto a queue is with respect to closing. Either the subscription is in place when the peer tells its end, and the peer's driver pushes the `ended` input (a holder that subscribes after the peer closed but before it told its end also gets what the last step emits, first), or the peer had told its end already. In that case the run is not subscribed and, if it hears `ended`, its own driver pushes `{"kind": "ended", "from": <the peer's id>, "status": <how it ended>}` onto its queue right after it has carried out the commands of its start step (unless that step closed the run). So the start step's sends to the peer are `undelivered` and its calls `Unanswered` first, in the same order as for a peer that ends later, and the result never depends on whether the peer ended just before or just after the holder subscribed. Without this a holder could miss its peer's end, or be told twice. A handle therefore keeps how its run ended for as long as the handle exists.
- **Carrying out `send`.** The driver builds the input `{"kind": "message", "from": <this run's id>, "message": …, "value": …}`.
  - With an `invoke`, it is for the child run of that invoke. If a run has claimed the invoke's token and is open, the driver pushes the input onto its queue. If the token has not been claimed yet (the host code has not called `run` yet), the driver keeps the input in the invoke context for the run that claims it. If the run that claimed it has closed, the driver writes `{"kind": "undelivered", "message": …, "to": …, "value": …}` (adding `t`, `run` and `parent` as for any record), whose `value` is that of the `sent` record the core emitted right before the command: redacted ([redaction](#redaction)), unlike the command's. If the invoke ends or is cancelled without its token ever being claimed (a stub, a task-like implementation), the inputs kept for it are discarded without a record.
  - With `invoke` `null`, it is for the peer `to`, one of this run's handles. If that run is open, the driver pushes the input onto its queue; otherwise it writes the `undelivered` record.
- **Carrying out `emit`.** If this run is a child run whose `invoke.start` had an `as`, or one started by `spawn.start`, its parent's machine hears the event, and its parent is open, the driver pushes `{"kind": "emitted", "from": <this run's id>, "invoke": <its invoke id>, "message": …, "value": …}` onto the parent's queue. For every open run subscribed to this run whose subscription keeps the event, it pushes the same with `invoke` `null`. It writes nothing for receivers that are closed, and an emit with no receiver has only its `emitted` record. Pushing to the receivers is not an ordered operation the trace can show: they take their inputs independently.
- **Order per pair.** Everything one run pushes onto another's queue arrives in the order its driver carried out the commands, so two messages from A to B are taken in the order they were sent, and so are an emitted event and a message. A child run's emitted events reach its parent's queue before its outcome, which is pushed when the child's run ends, after the commands of its last step; a Rust driver that finds a child's emitted events and its end in one poll pushes the emitted events first. Likewise a peer's `ended` comes after everything else it pushes onto a holder's queue: what the commands of its last step send and emit, then its `Unanswered` answers to the holder's calls, then `ended` ([ending a run](#ending-a-run)). There is no order between what two different runs push.
- **Cancelled children.** "Cancelled work queues nothing" holds for a child run's emitted events as for its outcome: once the parent has carried out `invoke.cancel`, the child pushes nothing more. What it emitted before that, in its cancel step included, is in the parent's queue, where it is stale ([taking an input](#taking-an-input)), unless the parent has closed. A cancel request still goes before everything queued, messages included, and whatever a run has queued when it closes is discarded.
- **Stubs.** A stubbed machine never claims its token, so it receives nothing and emits nothing. Messages for it are kept until its invoke ends, then discarded.

### Host listeners

A **listener** is host code that receives what a run emits. It is not a run: the core never sees it, it has no queue the core takes from, and it writes no record. A run has any number of listeners, which subscribe and unsubscribe while it runs.

- **What a listener hears.** Every event the run emits, of its machine's `emits`, whatever any machine hears: the filter on emitted events ([messages between runs](#messages-between-runs)) is about runs, which write `dropped` for what they cannot take, and does not apply to listeners. A listener of a run hears that run's events only, never those of its child runs, invoked or spawned (a child's emitted events are for its parent, which hears them by the same filter as any run), never those of its tasks (a task's emits are inputs of the run, [the emitter](#the-emitter)), and never the messages it sends. It hears the real values, `sensitive` fields included: a listener is host code, like an action given the same values, and only trace records are [redacted](#redaction), so a runner that compares what a listener heard with the `emitted` records redacts it first.
- **When.** The driver gives the event to each listener of the run while it carries out the `emit` command, after pushing it to the run's invoker and subscribers, and never waits for a listener. So a listener gets an event after its `emitted` record has been written, and only if the command was carried out: not an `emit` after an action's host defect in the same step, nor one of a step that a runtime error inside the core discarded ([defects](#defects)). A listener gets the run's events in the order of its `emitted` records. Nothing orders them with anything else: with what other runs take, with the run's snapshots as host code sees them, or with another listener's.
- **The trace.** Listeners change nothing in it. An emit still has its `emitted` record whoever receives it, and one whose only receivers are listeners has only that record. `dropped` and `stale` are records of receiving runs and `undelivered` is written only for a `send`, so a listener causes none of them. So a listener subscribed before a run's first step until its end gets exactly the `value`s of the run's `emitted` records, in order; a conformance runner checks that on every case ([how a runner works](conformance/README.md#how-a-runner-works)).
- **Subscribing late.** A listener gets only what the run emits after it has subscribed: nothing is kept for listeners that are not there yet, and nothing is replayed. A listener of an actor hears nothing its run's start (or restore) step emits: in TypeScript the actor exists only once that step has been carried out, and in Rust, where `spawn` can return before the run's task has taken it, a subscription made before then hears from the step after it, so that both runtimes agree. The tools' handle of a run (Rust `Config::on_run`, TypeScript `RunObserver`), which is given before the run's `started` record, can subscribe before the start step; it gives the events as JSON, not typed. It does so for child runs and runs of `run` too, which have no actor.
- **Ending.** A run's listeners are ended when the run ends, after the commands of its last step have been carried out, so what the exit effects of that step emit (a final state's, a cancel step's) reaches them first. They are ended too when the run is suspended. A listener learns how the run ended from the actor's result, not from its listening.
- **No backpressure.** As queues are ([one queue per run](#one-queue-per-run)), what a listener has not taken yet is kept without bound: the driver neither waits for a listener nor drops anything for one. A listener that stops taking events keeps them in memory until it unsubscribes or the run ends.
- **Saving and restoring.** Listeners are not saved ([the saved snapshot](#the-saved-snapshot)). A restored run starts with none and gives no event it emitted before it was saved; restoring runs no entry effects, so its restore step emits nothing.
- **Replaying.** A driver that rebuilds a run by giving its core inputs it has taken before (TypeScript's [durable driver](../docs/design/typescript.md#durable-runs)) gives the events of those steps to no listener again, as it carries out none of their actions again: a listener hears an event when its `emitted` record is written, and a replay writes none (unless it is asked to write its trace again, when its listeners hear the replayed steps' events with their records). A listener of a durable run as a whole, which outlasts its attempts (`DurableMachine.emitted`), hears each of its message ids once. Listeners get the events of steps taken live; the trace's `emitted` records are the run's whole history.
- **TypeScript.** `actor.emitted` is a `Stream` of the machine's emitted events (`D["emits"]`), decoded as host code receives events. Each run of the stream subscribes as it starts and unsubscribes when it ends or is interrupted, and ends when the run ends or is suspended. The tools' `RunHandle.listen(f)` calls `f` with each event's JSON as the driver carries out the `emit`, until the function it returns is called or the run ends; `f` must not throw.
- **Rust.** `actor.subscribe()` gives a `Subscription` of the machine's `Emitted` enum, and the tools' `RunHandle::subscribe()` one of JSON values. `next().await` gives the next event, or `None` once the run has ended and every event before that has been taken; `try_next()` gives one only if it is there. Dropping a subscription unsubscribes it.

### Carrying out calls

- **Making a call.** For `invoke.start` with `srcKind` `call`, the driver builds `{"kind": "call", "from": <this run's id>, "invoke": …, "call": <src>, "args": …}` and keeps with it a way to answer: a push of an outcome for that invoke onto this run's queue. If the peer `to` is open, it pushes the call onto the peer's queue, as one operation with respect to the peer's closing, as for a message. Otherwise it pushes `{"kind": "invoke.fail", "invoke": …, "error": {"_tag": "Unanswered"}}` onto this run's own queue at once, as a task that fails at once would. It writes no record either way, and runs no host code: a call has no implementation to bind.
- **Taking a call.** When the callee's driver gives a `call` input to its core, it keeps the call's way to answer, until it carries out a `reply` for it.
- **Carrying out `reply`.** The driver answers the call `to`/`invoke`: it pushes `{"kind": "invoke.done", "invoke": …, "value": …}` for `done`, or `{"kind": "invoke.fail", "invoke": …, "error": …}` for `fail`, onto the caller's queue, and forgets the call. It pushes nothing if the caller has closed or has carried out `invoke.cancel` for that invoke: cancelled work queues nothing, and the callee is not told. Like delivering a message, checking that the caller is open and its invoke not cancelled and pushing should be one operation.
- **Abandoning a call.** A caller that carries out `invoke.cancel` for a call only forgets its way to answer; nothing reaches the callee, which may still take the call and reply to it, to no effect. An answer the callee had already pushed is stale, as for a task.
- **Ending with calls.** When a callee ends, however it ends, its driver answers every call it was given and has not carried out a `reply` for, in the order it took them, then every call still in its queue, in queue order, with `{"_tag": "Unanswered"}`, as it would carry out a `reply` with `fail`. It writes no record for them. It does this as it discards its queue ([ending a run](#ending-a-run)), after the commands of its last step, so a `reply` among them reaches its caller first. The calls it was given and has not answered are the pending calls of its last snapshot, except when a defect cut a step short (a runtime error in the step that took a call, or an action's host defect before a `reply` command): which is why the driver keeps them itself.
- **Order.** A call and a reply are pushes like a message, so they keep the [order per pair](#delivering-messages): two calls from one run to another are taken in the order they were made, and a callee's answers reach a caller in the order its driver carried out the replies.

### Ending a run

A run ends when its status is no longer `running`. On `done` the result is the output (an Effect success, a Rust `Ok`), on `failed` the error (the Effect error channel, a Rust `Err` of the machine's error enum), and on `defect` the defect is raised ([defects](#defects)). A cancelled run has no result: a child run reports through its token, a root run as follows.

When a run ends, for whatever reason, the driver answers its pending and queued calls with `Unanswered` ([carrying out calls](#carrying-out-calls)), then tells its holders that hear its end ([a peer's end](#messages-between-runs)): it pushes `{"kind": "ended", "from": <this run's id>, "status": s}` onto the queue of every open run subscribed to it whose subscription keeps `ended`, where `s` is `Done`, `Failed`, `Cancelled` or `Defect` for the status the run ended with. A run whose driver is dropped before it ends (a hard stop, in Rust) tells them `Cancelled`, and one that ends before its start step (a `ref` argument that is not a handle of the right kind, an argument that does not encode) tells them `Defect`. Then it discards anything still in its queue, messages and emitted events included and without a record, unsubscribes it from the emitted events and ends of its peers, and drops every invoke and timer of the run it still holds. Normally there are none, since the core cancelled them; there can be when an action's defect stopped a step before the `invoke.cancel` or `timer.cancel` of work the step had already removed from the snapshot, which `failWithDefect` therefore cannot see. A child run dropped this way gets a cancel signal first, as on `invoke.cancel`.

- **Telling the holders.** The `ended` inputs are pushed after the commands of the last step have been carried out and after the `Unanswered` answers, so for each holder `ended` is the last input from this run, behind what the last step sent and emitted to it and the answers to its calls. A run that ends with a defect tells them too (`Defect`), whether the defect came from its core, a host defect in an action or a child: the defect discards the step's commands, not the driver's end of the run. A suspended run tells them with `Suspended` ([suspending](#suspending-and-resume)): it is closed for good, and a run restored from its snapshot is a new run with a new id, which no holder holds. Only top-level runs have holders, so a child run tells no one (a child run can still be a holder, through the `ref`s in its arguments). Nothing is written for it in this run's trace; each holder writes an `event` or `dropped` record when it takes the input ([inputs in records](trace.md#inputs-in-records)).
- **One end per pair.** A holder gets at most one `ended` from each peer, from the peer's driver or, for a peer that had told its end before it subscribed, from its own ([subscribing to a peer that has ended](#delivering-messages)). A `ref` variable pointed at a run whose end the holder has already taken (or dropped) is not told again: nothing in the core remembers that a peer has ended, and no expression asks it.

- **TypeScript.** Interrupting the fiber that runs a root `run`, or closing the scope of a `spawn`ed actor, is a graceful cancel. As for a child, each step runs uninterruptibly and the cancel step runs in `onInterrupt`, so the exit effects run and the trace ends with `cancelled` before the interruption completes; the run's Effect then ends interrupted.
- **Rust.** Dropping the future of a root `run` is a **hard stop**: nothing more runs, no exit effect and no final record (the trace simply stops, as a run's in progress does), and the invokes, child runs and timers it holds are dropped with it. Its calls are still answered `Unanswered` and its holders told, with `Cancelled`, so that no holder waits on a peer that is gone. Graceful cancellation from outside goes through the actor: `spawn` the run, then `actor.cancel().await` gives it a cancel request and returns once its cancel step has finished, after which `actor.result()` reports that it was cancelled. A child run is never dropped while it runs: its parent cancels it through the token.

### Time

- **Records.** `t` in a trace record is the driver's clock when it writes the record, in integer milliseconds: since the Unix epoch in production, and virtual milliseconds from 0 in conformance runs.
- **Timers.** A timer's expiry is due `delay` milliseconds after the driver carries out `timer.start`. The driver reads the time and waits only through the clock and timer it was given (in TypeScript a `Timer` service whose default sleeps on Effect's `Clock`, in Rust the `Timer` trait), telling the timer the run id, the timer id and the delay when it starts one and the ids when it cancels one. It never completes a timer itself, not even one with delay 0 (a `timer.fire` command queues an expiry without any timer). A test can therefore give it a virtual clock and fire timers itself, as a conformance runner does.
- **Due times.** The driver remembers, for each running timer (an `after` timer's or a delayed raise's), when it is **due**: the clock when it carried out the `timer.start`, plus the command's `delay`, capped at 2^53 − 1, the largest due time a saved snapshot holds (a delay near the longest `duration` would pass it). A timer restored by `timer.fire` keeps the due time it was saved with. A [saved snapshot](#the-saved-snapshot) records these.

How a conformance runner drives virtual time, scripted outcomes and quiescence, and what it may rely on the driver for, is specified in [conformance/README.md](conformance/README.md#how-a-runner-works).

## Saving and restoring

A run can be saved as plain data and started again from it later, in another process or in the other runtime: the request handler of [saving and restoring](../docs/design/semantics.md#saving-and-restoring) loads a snapshot, applies one event, and saves the result. Four operations do it:

- **save** gives a top-level run's [saved snapshot](#the-saved-snapshot), as its last step left it, with the snapshots of its running child runs nested in it ([saved child runs](#saved-child-runs)). The run carries on.
- **suspend** waits until a top-level run and all its child runs are idle, then stops them and gives its saved snapshot ([suspending](#suspending-and-resume)).
- **restore** checks a saved snapshot and starts a new top-level run from it ([restoring](#restoring-a-snapshot)).
- **resume** is restore, then one event, then suspend: the request handler's operation.

Host code saves only top-level runs, since it holds no handle to a child run; a child run is saved inside its parent's snapshot.

### The saved snapshot

A saved snapshot is the core's [snapshot](#snapshot), as the run's last step left it, with three additions: `snapshotVersion`, which is `4`; a `due` member on each timer and each delayed raise, from the driver ([time](#time)); and a `child` member on each entry of `invokes` and `spawned` whose child run is saved with it ([saved child runs](#saved-child-runs)). Its `calls` member is written only when the run has pending calls, its `scheduled` member only when it has pending delayed raises, its `locals` member only when it is not `{}` and its `historyLocals` member likewise, in any version, and a saved snapshot without one has none, so a snapshot saved before calls, delayed raises or state locals existed needs no new version. Its schema is [`snapshot.schema.json`](snapshot.schema.json). It holds the **real values**, those of `sensitive` parameters and variables and fields too: a restored run goes on with them, so nothing in a saved snapshot is [redacted](#redaction). Only the copy of it in a `restored` record is.

```jsonc
{
  "snapshotVersion": 4,
  "machine": "Order",
  "run": "r1",                                  // the id the run had when it was saved
  "ir": "sha256:6ae1…",
  "status": "running",
  "active": ["Order", "Order.AwaitingPayment"],
  "input": { "id": "o1" },
  "vars": { "paid": 0, "tracking": "" },
  "invokes": [],
  "spawned": [],
  "timers": [{ "id": 1, "state": "Order.AwaitingPayment", "index": 0, "delay": 259200000, "due": 259200000 }],
  "nextInvoke": 1,
  "nextTimer": 2,
  "nextMessage": 1,
  "history": {},
  "result": null
}
```

- **Versions 1 to 3.** Version 3 is the same without any `child` member, as written before child runs were saved; version 2 is also without `spawned`, as written before keyed spawn; and version 1 is also without `history`, as written before history states. A runtime restores one as if it had `"spawned": []` (and `"history": {}`), every child run it started begins again, as when it was written, and the `restored` record holds it as it was given. Save and suspend always write version 4. `calls`, `scheduled`, `locals` and `historyLocals` may be in any version: each is written only when not empty. A machine with no history state and no `children` keeps its hash from before both, so its older snapshots still restore.
- **`due`** is an absolute time on the driver's clock, in integer milliseconds: since the Unix epoch in production, virtual time in conformance. A deadline is kept rather than the time remaining because the run is in its state the whole time it is saved: an order that waits three days for payment expires three days after it began waiting, however long it spent in a store. A host can also find when to wake a saved run from its earliest `due`.
- **What is not in it.** Inputs queued but not yet taken, by any run of the tree, a task's emits among them (a request handler never has any, since it saves an idle tree); the state of work that is not a running child run (a task, a spawned task, a call, a stub, a machine whose implementation has not claimed its invoke yet, a child run that has ended), which starts again ([restoring](#restoring-a-snapshot)); messages kept for a child that has not claimed its invoke; subscriptions; and listeners.
- **The same in both runtimes.** Values are in their JSON form ([values](#values)), `active` is in document order, and `invokes`, `spawned`, `timers` and `scheduled` are in the order they were started, so the two runtimes save the same run as the same document, and either restores what the other saved. Conformance compares saved snapshots as [canonical JSON](ir.md#canonical-json).
- **Numbers.** A JSON number's form does not matter. Wherever a saved snapshot has an integer (`snapshotVersion`, the ids, a timer's `index`, `delay` and `due`, a delayed raise's `delay` and `due`, an `int` or `duration` value), one written as a float with no fraction (`1.0`, `10000.0`, `-0.0`) is that integer, as JSON Schema's `integer` and JavaScript read it; a runtime always writes its integers in integer form (`2`, and `-0` as `0`).
- **A copy.** What save and suspend give host code, and what restore takes from it, is the runtime's no longer: changing a saved snapshot afterwards changes no run.

### Saved child runs

A child run is saved inside its parent's saved snapshot, as the `child` member of the `invokes` or `spawned` entry that started it, and its own child runs inside its snapshot the same way, to any depth. `child` is a saved snapshot of version 4 itself, the child run's: its `run` is the child's id, `<parent's run>.<entry's id>`, its `ir` the hash of the child's machine, and its timers have their due times.

```jsonc
"spawned": [{
  "id": 1, "children": "sessions", "key": "a", "args": { "user": "a" },
  "child": { "snapshotVersion": 4, "machine": "Session", "run": "r1.1", "ir": "sha256:91c0…",
             "status": "running", "active": ["Session", "Session.Idle"], "input": { "user": "a" },
             "vars": {}, "invokes": [], "spawned": [],
             "timers": [{ "id": 1, "state": "Session.Idle", "index": 0, "delay": 60000, "due": 60000 }],
             "nextInvoke": 1, "nextTimer": 2, "nextMessage": 1, "history": {}, "result": null }
}]
```

- **Which entries have one.** An entry has `child` when its work is a child run that has claimed its invoke ([child runs](#child-runs)) and is still open, or, in the snapshot suspend gives, one that suspend stopped with its tree. It has none when its work is a task (invoked or spawned), a call or a stub, a machine whose implementation has not called `run` yet, or a child run that has closed (one that has ended and whose outcome its parent has not taken yet). On restore those start again, as tasks do ([restoring](#restoring-a-snapshot)).
- **As each run's last step left it.** save gives the top-level run's snapshot as its last step left it, and in each of its entries with a child run that child's snapshot as the child's last step left it, at the time save is called, assembled the same way below it. The runs of a tree take their steps independently, so what is in flight between them when save is called (a message the parent has sent that the child has not taken, an emitted event on its way up) is in no snapshot, as nothing queued for a top-level run is. suspend loses nothing, since it stops a tree only when nothing is queued anywhere in it ([suspending](#suspending-and-resume)).
- **Ids and timers.** Every id in a child's snapshot is its own run's: its invokes, timers, delayed raises and messages are numbered by its own `nextInvoke`, `nextTimer` and `nextMessage`. Its timers' and delayed raises' due times are on the clock the tree shares.
- **The same in both runtimes.** A nested snapshot follows every rule of a saved snapshot, so the two runtimes save the same tree as the same document.

### Restoring a snapshot

Restoring checks the saved snapshot against the IR the runtime was given, refuses it with a reason if it does not fit, and otherwise starts a new top-level run from it. Nothing is written to the trace before the checks pass. They are made for the whole tree, the saved snapshot and every snapshot nested in it, in this order, each reason for the whole tree before the next:

| Reason | Refused when |
| --- | --- |
| `invalid` | It does not match `snapshot.schema.json`, or its `snapshotVersion` is not `1`, `2`, `3` or `4` (a version 1 snapshot has no `history` and no `spawned`, a version 2 one has `history`, a version 3 one both, and a version 4 one both and may have `child` members, each a snapshot of version 4). |
| `machine` | The IR has no machine named `machine`, or it is not the machine restore was asked to restore (TypeScript `OrderLive.restore` restores only `Order`). |
| `ir` | Its `ir` is not the machine's hash in the IR, or the `ir` of a snapshot nested in it is not the hash of the machine its entry starts (an `invokes` entry's `src` when that is a machine, a `spawned` entry's collection's machine when the collection is one of the machine's and its `kind` is `machine`; a nested snapshot in any other entry is not compared here, and is `invalid` below), and no migration was given; or the migration's result has such a hash too. |
| `migration` | The migration failed: it threw (anything at all), panicked or returned an error. (Its result is checked from the top, as a snapshot of the machine being restored, the one the old snapshot names when restore was not asked for one: a result that is not a snapshot is `invalid`, one of another machine `machine`, and one that still has another hash `ir`.) |
| `ended` | Its `status` is not `running` (a nested snapshot that is not running is `invalid` instead). A run that ended with a defect is `ended` whatever its defect's `kind`: the schema requires only that it has one, so a kind this version does not name is not `invalid`. |
| `ref` | The machine, or the machine an entry with a nested snapshot starts (as for `ir`), has a parameter of type `ref M`, `ref M?` or `ref M[]`, or an alias of one ([not yet specified](#not-yet-specified)). |
| `calls` | Its `calls` is not empty (a nested snapshot with `calls` is `invalid` instead: a child run has no handle, so it takes no calls). The callers that wait for those calls belong to the process that saved it, and are gone. A run with pending calls is not idle, so suspend never gives such a snapshot; save can. |
| `invalid` | It does not fit the machine: its `status` is `running` but its `result` is not `null` (a run has a result only once it has ended); `active` is not a configuration in document order (the root first, each other state's parent active, one active child for each active compound state, every child for each active parallel state, and no history state); an `invokes` entry's state is not active, or the entry has `as` though its state has at most one invoke, or has none though its state has several, or its state has no invoke with that `as` (for an entry without one, no invoke), or the entry's invoke is not of `src`, or two entries have the same state and invoke; a `timers` entry's state is not active, or its `index` is not the first transition of an `after` block of that state, or two entries have the same state and index; a `spawned` entry's `children` is not a collection of the machine, its `key` does not decode as the collection's key type, its `args` do not have exactly the parameters of the collection's machine or task or do not decode as their types, or two entries of one collection have the same key; a `scheduled` entry's `value` does not decode as one of the machine's own events (`events` or `internalEvents`), its `name` is neither `null` nor the `name` of a delayed `raise` effect of the machine, or two entries have the same name that is not `null`; an id is repeated (among `invokes` and `spawned` together, or among `timers` and `scheduled` together) or not below `nextInvoke` or `nextTimer`; `input` does not have exactly the machine's parameters, or `vars` has a name that is not one of its variables, or lacks an optional variable; `locals` does not have exactly one member for each active state that declares a parameter or a variable, or one of its members has a name that is not a local of its state, lacks one of the state's parameters or lacks an optional variable of it; a `history` key is not the id of one of its history states, or its value is not one that history state could have recorded (for a `history` state, one child of a compound parent or every child of a parallel parent; for a `deepHistory` state, the leaves of a configuration below its parent: atomic or final states which, with their ancestors up to and including the parent, have one child for each compound state and every child for each parallel state among them; in document order either way); a `historyLocals` key is not the id of a history state with a member in `history`, or its value does not have exactly one member for each state that declares a local among those its record re-enters (for a `history` state, the children it recorded; for a `deepHistory` state, the leaves it recorded and their ancestors below its parent), or one of those members does not fit its state as a `locals` member must; or a `history` record re-enters a state that declares a local and `historyLocals` has no member for its history state; or a value does not decode as its declared type, as a machine's arguments must. A record, an error or an event decodes only with exactly the members its type declares (and an error's or event's `_tag`): one with a member its type does not declare is `invalid`. And for each snapshot nested in it: its entry is an `invokes` entry whose `src` is not a machine (a task or a call), or a `spawned` entry of a collection of tasks; its `machine` is not the machine its entry starts; its `run` is not this snapshot's `run`, a dot and the entry's `id`; its `status` is not `running`; it has `calls`; or it does not fit its own machine by every rule of this row, the snapshots nested in it included. |

**Migration.** When the hashes differ, the host may give a **migration**: a function from the old saved snapshot to a new one, both plain JSON (TypeScript `(saved: Json) => Json`, Rust `Fn(serde_json::Value) -> Result<serde_json::Value, String>`). It is called once for the whole tree, and only when a hash in it differs. In a tree a runtime saved, the top-level snapshot's hash differs whenever a nested one's does, since a machine's hash covers every machine it invokes or spawns ([the machine hash](ir.md#the-machine-hash)); `migrated` is the top-level snapshot's hash before the migration either way. It is given the whole tree and gives the whole tree, rewriting the nested snapshots too. Its result is checked from the top as if it had been given instead, except that a second migration is never applied. It is how a deployment that changed a chart (renamed a variable, added a state) keeps its saved runs: the migration rewrites `ir`, the names, and whatever else must change. It needs no IR of the old chart; it sees only the data.

**The new run.** The restored run is a new top-level run with an id of its driver's choosing ([run ids](#the-driver)), not the `run` in the snapshot, which named it in the process that saved it. It has no peers. The driver opens it and calls the core:

```
restore(ir, saved, run, now, migrated, fns):
  m    = ir.machines[saved.machine]           # every check above has passed
  shown = saved without the child members of its invokes and spawned entries
  snap = shown without snapshotVersion, with run = run, no due on the timers or the scheduled
         raises, calls = [], scheduled = [] if it has none, locals = {} and historyLocals = {} if
         it has none, history = {} if it has none (version 1), and spawned = [] if it has
         none (versions 1 and 2)
  emit trace { kind: "restored", machine: saved.machine, ir: m.hash, migrated, snapshot: shown }
  for ch in saved.spawned, in order:
    c = the collection ch.children; src = c.src
    emit trace { kind: "spawn.started", spawn: ch.id, children: ch.children, key: ch.key, src, args: ch.args }
    emit spawn.start { spawn: ch.id, src, srcKind: c.kind, args: ch.args, saved: ch.child, or null if it has none }
  for s in snap.active, in document order:
    for each entry inv of saved.invokes with state s.id, by ascending id:
      iv = inv's invoke
      to, src, args = startArgs(iv)               # never a call: a caller holds a ref, so is refused
      if inv has as:
        emit trace { kind: "invoke.started", invoke: inv.id, state: s.id, src: inv.src, as: inv.as, args }
      else:
        emit trace { kind: "invoke.started", invoke: inv.id, state: s.id, src: inv.src, args }
      emit invoke.start { invoke: inv.id, state: s.id, src: iv.src, srcKind: iv.kind,
                          to, as: iv.as, args, saved: inv.child, or null if it has none }
    for each entry tm of saved.timers with state s.id, by ascending id, if tm.due > now:
      emit trace { kind: "timer.started", timer: tm.id, state: s.id, index: tm.index, delay: tm.due - now }
      emit timer.start { timer: tm.id, delay: tm.due - now }
  for each entry sc of saved.scheduled, in order, if sc.due > now:
    emit trace { kind: "raise.scheduled", timer: sc.id, name: sc.name, delay: sc.due - now, value: sc.value }
    emit timer.start { timer: sc.id, delay: sc.due - now }
  for each entry tm of saved.timers and saved.scheduled together with tm.due <= now,
      by ascending due, then ascending id:
    emit timer.fire { timer: tm.id }
  return (snap, commands)
```

`now` is the driver's clock, `run` the new run's id, and `migrated` the hash the snapshot had before a migration, or `null` when there was none. Each timer and delayed raise of the core snapshot keeps the `delay` it was saved with, the delay its `after` evaluated to, though a restarted timer's `timer.started` or `raise.scheduled` record and command give the time that remains. `snapshot` in the `restored` record is the saved snapshot as restored, after any migration, without the snapshots nested in it: each child run restored from one writes its own `restored` record.

- **The spawned children go on.** Each child in `spawned` is started again, with its id and key and the arguments it was spawned with (they were evaluated from the event that spawned it, which is gone, so they are kept), as a new child run `<new run id>.<id>`, before the states' work, as in [starting work](#starting-work). One with a `child` snapshot is restored from it and goes on where it was ([restoring a child run](#restoring-a-child-run)); one without starts from the beginning, a spawned task always, with its saved arguments and a new emitter. A child that had ended when the run was saved, but whose outcome had not been taken yet, is still in `spawned`, has no `child`, and runs again, as an invoke in that position does.
- **The work that was running restarts.** Each invoke in `invokes` is started again, with the same id and with its arguments evaluated now, from the snapshot's variables and locals (a function in them is called again). A task runs again from the start, so a machine meant to be saved should wait in states with no `invoke`, or use tasks that are safe to repeat ([semantics](../docs/design/semantics.md#saving-and-restoring)). A task that emits gets a new emitter and emits again as it runs: what it emitted before the save, taken or not, is not given again. A child machine is a new child run `<new run id>.<invoke id>`: one with a `child` snapshot is restored from it and goes on where it was ([restoring a child run](#restoring-a-child-run)), with what its parent had sent it and it had taken; one without runs again from its `start`, its own progress and the messages its parent had sent it not saved. An invoke whose outcome had already been taken is not in `invokes` and is not restarted, though its state is active; in a state with several invokes, the others are restarted all the same.
- **Timers keep their deadlines.** A timer not yet due is started again, with the same id, for the time that remains, so it expires at its saved `due`. So is a delayed raise, after every state's work, with a `raise.scheduled` record; one already due joins the timers already due, in one order. A timer already due is not started: `timer.fire` queues its expiry at once, before anything else can reach the run's queue, the earliest due first. Its deadline passed while the run was saved, so it is taken before any event given after restoring: an order restored after its payment deadline expires before it sees the payment. Since ids are kept, `nextInvoke` and `nextTimer` do not change; they cannot collide with anything, since the run id is new. A restored child run's timers are restored the same way, by its own `restore`.
- **Nothing else happens.** No state is entered or exited, no effect runs and the run does not settle: the snapshot was saved between steps, already settled. The run then takes inputs like any run.
- **Defects.** A runtime error or a host defect while evaluating an invoke's arguments makes `restore` atomic, like `start`: it returns `failWithDefect(d)` applied to `snap`, with two commands, the `restored` record and the `defect` record. The `invoke.cancel`, `spawn.stop` and `timer.cancel` commands of `failWithDefect` are for work never started (the `spawn.start` commands before the defect are discarded with the rest), which the driver ignores. The run then ends with the defect like any run.
- **The driver** carries out the commands and drives the run as `run` does ([the driver](#the-driver)). The due times it keeps are `now + delay` for the timers it starts, which is their saved `due`, and the saved `due` for those whose expiry `timer.fire` queued, delayed raises' as `after` timers'.

### Restoring a child run

When the driver carries out an `invoke.start` or `spawn.start` whose `saved` is not `null`, the invoke context it runs the implementation with holds that snapshot, and the run that claims the invoke ([child runs](#child-runs)) restores it instead of starting:

- **It is restored as a child run.** Its id is `<parent run id>.<invoke id>` as always, and its driver calls `restore(ir, saved, <its id>, now, null, fns)` in place of `start`. It writes `restored` in place of `started` (with `parent`, and `migrated` `null`: a migration is the top-level run's, which its own `restored` record names), restarts its own work as above, the child runs nested in its snapshot included, and goes on. Its input and its variables are its snapshot's: the arguments the implementation gives `run`, the invoke's evaluated again, are not used. Its queue starts with the expiries its own `restore` queued with `timer.fire`, then the messages the context holds, in the order they were sent: the timers' deadlines passed while it was saved, before anything was sent to it after restoring.
- **It checks nothing more.** The top-level restore made every check, for the whole tree, before anything started. But if the claiming run's machine is not the snapshot's `machine`, or its hash is not the snapshot's `ir` (the host bound another machine to the invoke, or one built from another chart), it ends with a `host` defect before its first record, as a run whose arguments do not encode does, and its parent takes that as the invoke's defect.
- **Host code around it.** What the implementation does before it calls `run` is done again, as a task is. An implementation that never claims the invoke (a stub) never sees the snapshot, which is discarded: it runs again from the start.

### Suspending and resume

A run is **idle** when it is between steps, its queue is empty, no cancel request is waiting, it has no pending call (`snap.calls` is empty), and the work of every entry of `snap.invokes` and `snap.spawned` is a child run that has claimed its invoke, is open, and is idle itself: it waits only for events, messages and timers (delayed raises among them), nothing it started is in flight but child runs that wait the same way, and no caller waits for it. A task, a call, a stub or a machine whose implementation has not claimed its invoke is work in flight, so a run that waits for one is not idle, and neither is one whose child run has closed, since that child's outcome is on its way. A task that emits is work in flight like any task, invoked or spawned: a run with a stream task running is never idle, so suspend waits until the task ends or the run leaves its state (or stops the spawned task). A run with no invoke and no spawned child is idle exactly as before. The **tree** of a top-level run is the run and its child runs, to any depth, and a top-level run is idle exactly when every run of its tree is.

**suspend** waits until the run is idle or has ended, taking inputs meanwhile as always (a cancel request among them: one that arrives while a step runs is taken after that step, so a run asked to cancel and then to suspend is cancelled, exit effects and all, and suspend gives its `cancelled` snapshot): the outcomes of its invokes and the expiries of its timers, which keep running, so a chart's own `after` bounds how long it waits for a task. The driver checks when suspend is called, and after the commands of each step (or `start`'s, or `restore`'s) of any run of the tree have been carried out. Then:

- If the run is idle, the driver **stops** its tree: it closes every run of the tree ([delivery](#delivering-messages)), deciding that the tree is idle and closing its runs as one operation with respect to what is pushed onto their queues, so that an input that arrived while one was open is taken (the tree is then not idle), never discarded. Each child run then stops as its token tells it ([child runs](#child-runs)), and the top-level run last: it cancels its timers and its delayed raises' timers through its timer (with no record: they are in the snapshot), discards its queue (empty), writes `{"kind": "suspended"}`, tells its holders that hear its end, with `{"kind": "ended", "from": <its id>, "status": "Suspended"}` ([ending a run](#ending-a-run)), and gives the saved snapshot, with its child runs' last snapshots nested in it. Every status is still `running`: the runs are not over, they go on from the snapshot when it is restored. No exit effect runs, in any run of the tree. A child run tells no one, since only top-level runs have holders.
- If the run has ended with `done`, `failed` or `cancelled`, the driver gives its saved snapshot, whose `status` and `result` say so, and writes nothing. A run that ended with a defect raises it, as `run` does, and gives no snapshot.

A timer that falls due while suspend waits is taken like any input; one still running when the run stops is in the snapshot. An event given to the run while suspend waits is taken if it arrives before the run stops.

**resume**`(saved, event?, migration?)` restores the snapshot, gives the run `event` if there is one, suspends it, and gives the saved snapshot that suspend gives. Expiries that `timer.fire` queued are taken before the event. Both inputs are checked before anything runs or is written: first the snapshot, as restore checks it (a refusal), then the event, which must be one of the events the machine accepts, with exactly its fields, each of its type, as an event given to a run must encode. An event that is not is the caller's error, as for an event host code gives a running run that does not encode: a host defect raised to the caller (TypeScript dies with it, Rust resumes unwinding with it); no run starts, nothing is traced and no run id is used. This is the request handler's operation: it awaits the tasks the event starts (so a snapshot resume gives has no running invokes) and saves the timers with their deadlines. It waits for the tree to be idle, not for the children to end: a hub whose sessions wait for events is resumed per request, its sessions saved in its snapshot and restored where they were. A child that runs a task, or a stub, is waited for, as a task is. A defect is raised; a refused snapshot is an error of its own, before any run starts ([TypeScript](../docs/design/typescript.md#saving-and-restoring), [Rust](../docs/design/rust.md#saving-and-restoring)).

## Not yet specified

The formats leave room for these; their rules will be added here.

- **Restoring a run that holds `ref`s.** Its `input` names peers by the run ids of the process that saved it. Restoring it needs host code to give a handle for each of those ids, and the binding layer to rewrite them in `input` and `vars` by their types; until then such a snapshot is refused (`ref`).
- **Saving what is in flight** between the runs of a tree, so that save, like suspend, loses nothing: a message a parent has sent that its child has not taken, an emitted event or an outcome on its way up.
- **Host code making a call** through a handle, and a call to a child run (invoked with `as`, or spawned) rather than to a peer.
