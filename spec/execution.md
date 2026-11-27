# Execution

This page is the algorithm both runtimes implement: what the pure core does with a snapshot and an input, what it asks the driver to do, and how the driver carries that out. It is precise enough that two independent implementations produce the same [trace](trace.md) byte for byte, and the [conformance cases](conformance/) hold them to it. The design rationale is in [semantics](../docs/design/semantics.md); this page only says what happens.

It covers atomic, compound, parallel and final states, events, guards, `invoke` of tasks and machines, `after`, `always` and `done`, [messages between runs](#messages-between-runs) (`send`, `emit`, `on name.Event` and `ref`, milestone 5), [calls between runs](#calls-between-runs) (`call`, `invoke name.call` and `reply`), [saving and restoring](#saving-and-restoring) a run (milestone 6), [history states](#history-states), and [spawned children](#spawned-children) (keyed `spawn`). Parallel states, history states and spawned children change nothing for a machine that has none: every rule below reduces to the one it replaced, and the traces of such machines are unchanged.

## Terms

- A **run** is one instance of a machine, from `start` until it finishes. A run's **status** is `running`, `done`, `failed`, `cancelled` or `defect`; only `running` is not final.
- The **active states** are the states the run is in. They form a tree from the root (the machine itself, a compound state whose id is the machine's name): an active compound state has exactly one active child, and an active parallel state has all its children active. Without parallel states the tree is a chain from the root down to one atomic or final state. The root is active for the whole run.
- A **history state** is a state of kind `history` (shallow) or `deepHistory` (deep): a pseudo-state in the `states` of a compound or parallel state other than the root, its **parent**. It is never active and has no children, effects, work or transitions; it is only ever a transition's target ([history states](#history-states)). On this page the **children** of a state are the states in its `states` that are not history states, so a history state is never a region, an initial state, an active state or an active leaf.
- A **region** is a child of a parallel state. A region may be a state of any kind: compound, parallel, atomic or final.
- An **active leaf** is an active state with no active child: an atomic or final state. Without parallel states there is exactly one.
- **Document order** is the order of a pre-order walk of the IR's state tree: a state comes before its children, and children are in `states` order. **Innermost first** means reverse document order; for a chain it is leaf to root.
- An **input** is what the driver gives the core: an external event, a message, an emitted event or a call from another run, an invoke's or a spawned child's outcome, a timer's expiry, or a request to cancel or to fail with a defect.
- A **top-level run** is one that host code started (`run`, `spawn`), as opposed to a **child run**, which an `invoke` or a `spawn` effect of its parent started ([child runs](#child-runs)). A **peer** of a run is a top-level run it holds a `ref` to.
- A **spawned child** is a child run that a `spawn` effect started into one of the machine's `children` collections, under a key ([spawned children](#spawned-children)). Unlike an invoked child it belongs to the run, not to a state: it runs until it ends by itself, until a `stop` effect stops it, until a `spawn` with the same key replaces it, or until its parent ends.
- A **message** is an event one run sends to another with `send`. An **emitted event** is one a run emits with `emit`, for its invoker and the runs that hold a `ref` to it. Both are queued for the receiver like any input ([messages between runs](#messages-between-runs)).
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
  "invokes": [],                                 // running invokes
  "calls": [],                                   // calls taken and not answered
  "spawned": [],                                 // running spawned children
  "timers": [{ "id": 1, "state": "Checkout.Backoff", "index": 0, "delay": 200 }],
  "nextInvoke": 2,
  "nextTimer": 2,
  "nextMessage": 1,
  "history": {},
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
| `invokes` | `[{"id", "state", "src"}]`: the invokes that are running, in the order they were started. `state` is the id of the state that owns it, `src` the task's or machine's name, or for a call the address and the call, `inventory.reserve`. |
| `calls` | `[{"from", "invoke", "call"}]`: the calls the run has taken and not answered yet, its **pending calls**, in the order it took them. `from` is the caller's run id, `invoke` the id of the caller's invoke, and `call` the call's name ([calls between runs](#calls-between-runs)). |
| `spawned` | `[{"id", "children", "key", "args"}]`: the spawned children that are running, in the order they were started. `children` is the name of the collection, `key` the child's key and `args` its arguments, keyed by parameter name, as they were evaluated when it was spawned ([spawned children](#spawned-children)). |
| `timers` | `[{"id", "state", "index", "delay"}]`: the `after` timers that are running, in the order they were started. `index` is the position in the state's `transitions` of the first transition of the timer's `after` block, and `delay` the evaluated delay in milliseconds. |
| `nextInvoke`, `nextTimer` | The id the next invoke or timer will get. Both start at 1. Ids are never reused within a run, which is what lets the core recognise a stale outcome. Invokes and spawned children take their ids from the one sequence `nextInvoke`, so an id names one or the other, and the child run of either is `<run>.<id>`. |
| `nextMessage` | The id the next `send` or `emit` effect will get, from 1. A message is named in traces by its sender's run id and this id. |
| `history` | What each history state has recorded ([history states](#history-states)), keyed by the history state's id: a list of state ids, in document order. A history state whose parent no microstep has exited yet has no member. Starts as `{}`. |
| `result` | `null` while running and when cancelled. `{"output": value}` when done (`value` is `null` for a machine that returns nothing), `{"error": value}` when failed, `{"defect": defect}` when the run ended with a [defect](#defects). |

A **saved snapshot**, the form host code stores, is this snapshot with a format version and each timer's due time added ([saving and restoring](#saving-and-restoring)).

Within a macrostep the core also keeps three transient values that are not part of the snapshot: the set of states **entered during this macrostep**, the **completion queue** of compound states that reached a final child and have not been exited since, and the **pending spawns**, the children spawned during this macrostep that have not started yet (`[{"children", "key", "args"}]`, in the order spawned).

### Inputs

| Input | Meaning |
| --- | --- |
| `{"kind": "event", "value": {"_tag": "Cancel", …}}` | An external event: one of the machine's `events`, encoded as a tagged record (see [values](#values)), given by host code. |
| `{"kind": "message", "from": "r2", "message": 1, "value": {"_tag": "Reserve", …}}` | A message: run `r2` sent one of this machine's `events` with `send`, as its message 1. |
| `{"kind": "emitted", "from": "r1.3", "invoke": 3, "message": 2, "value": {"_tag": "Progress", …}}` | An emitted event: run `r1.3` emitted one of its machine's `emits` as its message 2. `invoke` is the id of this run's invoke or spawned child that is `r1.3` when this run is its parent, and `null` when this run holds a `ref` to it. |
| `{"kind": "call", "from": "r2", "invoke": 1, "call": "reserve", "args": {…}}` | A call: run `r2` called this machine's call `reserve` with its invoke 1. `args` is keyed by the call's parameter names. |
| `{"kind": "invoke.done", "invoke": 1, "value": v}` | Invoke 1 succeeded with `v` (`null` for a task, machine or call that returns nothing). |
| `{"kind": "invoke.fail", "invoke": 1, "error": e}` | Invoke 1 failed with the declared error `e`, a tagged record, or, for a call, with `{"_tag": "Unanswered"}`. |
| `{"kind": "invoke.defect", "invoke": 1, "defect": d}` | Invoke 1 ended with a [defect](#defects): a host defect in a task, or a child run that ended with a defect. |
| `{"kind": "spawn.done", "spawn": 4, "value": v}` | Spawned child 4 ended `done` with output `v` (`null` for a machine that returns nothing). |
| `{"kind": "spawn.fail", "spawn": 4, "error": e}` | Spawned child 4 ended `failed` with the declared error `e`. |
| `{"kind": "spawn.defect", "spawn": 4, "defect": d}` | Spawned child 4 ended with a defect. |
| `{"kind": "after", "timer": 1}` | Timer 1 expired. |
| `{"kind": "cancel"}` | Cancel the run from outside. |
| `{"kind": "defect", "defect": d}` | The driver met a defect while carrying out this run's commands: a host defect in an action, or a defect in the cancel step of a child run it was cancelling ([child runs](#child-runs)). Unlike every other input, it is taken even when the run has already ended, unless it ended with a defect. |

The driver only ever passes events the machine declares, with values of the declared types. The core may assume its inputs are well formed.

### Commands

| Command | The driver… |
| --- | --- |
| `{"kind": "trace", "record": r}` | writes trace record `r`, after adding `t`, `run` and, for a child run, `parent` ([trace](trace.md)). |
| `{"kind": "action", "action": "audit", "args": {…}}` | runs the action and waits for it to finish. `args` is keyed by parameter name. |
| `{"kind": "invoke.start", "invoke": 1, "state": "…", "src": "chargeCard", "srcKind": "task", "to": null, "as": null, "args": {…}}` | starts the task (`srcKind` `task`) or machine (`srcKind` `machine`) and, when it ends, queues its outcome as an `invoke.*` input with the same id. It does not wait for it. `as` is the invoke's `as` name or `null`: a child run invoked with one delivers what it emits to this run ([delivery](#delivering-messages)). With `srcKind` `call`, it makes the call `src` of the peer whose run id is `to` (`null` for the other kinds), whose answer is the outcome ([carrying out a call](#carrying-out-calls)). |
| `{"kind": "reply", "to": "r2", "invoke": 1, "call": "reserve", "outcome": "done", "value": v}` | answers the call that run `to` made with its invoke 1: `outcome` `done` with the result `v` (`null` for a call that returns nothing), or `fail` with the error `v`. It does not wait for the caller ([carrying out a call](#carrying-out-calls)). |
| `{"kind": "invoke.cancel", "invoke": 1}` | cancels invoke 1 and waits until it has stopped: at once for a task, after its cancel step for a [child run](#child-runs). Its outcome is not queued after this; one already queued stays queued. |
| `{"kind": "timer.start", "timer": 1, "delay": 200}` | starts a timer that queues `{"kind": "after", "timer": 1}` after `delay` milliseconds. |
| `{"kind": "timer.cancel", "timer": 1}` | cancels timer 1. Its expiry is not queued after this; one already queued stays queued. |
| `{"kind": "timer.fire", "timer": 1}` | queues `{"kind": "after", "timer": 1}` on this run's queue at once, without starting a timer. Only `restore` emits it, for a restored timer that was already due ([restoring](#restoring-a-snapshot)). |
| `{"kind": "send", "to": "r2", "invoke": null, "message": 1, "value": v}` | queues the message for run `to` and does not wait for it: for the child run of this run's invoke or spawned child `invoke`, or, when `invoke` is `null`, for the peer `to` ([delivery](#delivering-messages)). |
| `{"kind": "emit", "message": 2, "value": v}` | queues the emitted event for this run's parent, if it invoked this run with `as` or spawned it, and for every run that holds a `ref` to this run, of those whose machine has an `on` for it ([messages between runs](#messages-between-runs)). It does not wait for them. |
| `{"kind": "spawn.start", "spawn": 4, "src": "Session", "args": {…}}` | starts machine `src` as spawned child 4 of this run and, when it ends, queues its outcome as a `spawn.*` input with the same id. It does not wait for it. The child run delivers what it emits to this run. |
| `{"kind": "spawn.stop", "spawn": 4}` | stops spawned child 4 and waits until it has stopped, after its cancel step, exactly as `invoke.cancel` does for a child run. Its outcome is not queued after this; one already queued stays queued. |

A trace record is always emitted immediately before the command it describes. Records are listed in [trace.md](trace.md). Two records are written by the driver instead of the core: `undelivered`, when it finds that the receiver of a `send` has ended ([delivery](#delivering-messages)), and `suspended`, when it stops a run it has saved ([suspending](#suspending-and-resume)).

### Redaction

Every `trace` command carries its record **redacted** ([trace.md](trace.md#redaction)): each value of a field, parameter or variable the IR marks `sensitive` is `"<redacted>"`. The pseudocode below writes `emit trace r` for the record before redaction; the core redacts it as it emits it, by the types trace.md lists, so both runtimes emit the same records. An input is redacted when the step that takes it begins, before anything else: an `invoke.done` input's type is that of the invoke's task or machine while the invoke is in the snapshot, which it no longer is once taken (a stale one, whose invoke is not in it, is redacted whole). Redacting copies: the snapshot, the other commands and the values host code gets are never changed, and a value of a type that holds no sensitive field is the same value. A program whose IR marks nothing never redacts anything.

## Starting a machine

```
start(ir, name, run, input, fns):
  m    = ir.machines[name]
  snap = { machine: name, run, ir: m.hash, status: "running", active: [], input, vars: {},
           invokes: [], calls: [], spawned: [], timers: [], nextInvoke: 1, nextTimer: 1, nextMessage: 1,
           history: {}, result: null }
  emit trace { kind: "started", machine: name, ir: m.hash, input }
  for v in m.vars, in order:
    if v.init != null:          snap.vars[v.name] = eval(v.init)
    elif v.type ends with "?":  snap.vars[v.name] = null
  enterStates([m.root] + defaultEntry(m.root))
  settle()
  if snap.status == "running": startWork()
  return (snap, commands)
```

A variable's initial value may read the parameters and the variables declared before it; reading a later one is a runtime error (`unassigned`). A variable with no initial value starts unassigned, unless its type is optional (`string?`, whose IR type string ends in `?`, or an alias of an optional type, aliases expanded transitively): an optional variable with no initial value starts as `null`, as the [language](../docs/design/language.md#definite-assignment) says, and reading it is never `unassigned`. Initial values are not traced.

`defaultEntry(s)` is the list of states entered below `s` when `s` is entered with no more specific target, in document order:

```
defaultEntry(s):
  if s.kind == "compound": c = s.initial; return [c] + defaultEntry(c)
  if s.kind == "parallel": return the concatenation of [c] + defaultEntry(c) for each child c of s, in order
  return []
```

So starting enters the root, then its initial child, and so on down, outermost first, entering every region of a parallel state on the way, the first region and everything below it before the second: the root's `entry` effects run before any other state's. Starting takes no transition, so it writes no `transition` record.

## Taking an input

```
step(ir, snap, input, fns):
  m = ir.machines[snap.machine]
  if input.kind == "defect":
    if snap.status != "defect": failWithDefect(input.defect)
    return (snap, commands)
  if snap.status != "running": return (snap, [])
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
                                         machine of collection ch.children:
        runtime error { kind: "undeclared", error: input.error._tag }
      T = selectSpawnOutcome(ch, input)
    "after":
      tm = the entry of snap.timers with id input.timer
      if none: emit trace { kind: "stale", event: input }; return
      remove tm from snap.timers
      T = selectAfter(tm)
    "event", "message":
      T = selectEvent(input.value)
    "call":
      T = selectCall(input.call)
    "emitted":
      if input.invoke != null and neither snap.invokes nor snap.spawned has an entry with id input.invoke:
        emit trace { kind: "stale", event: input }; return
      T = selectEmitted(input)
  if T is empty:
    if m.strict and input.kind in ("event", "message", "call"):
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

- An invoke's outcome or a timer's expiry **consumes** it: it is removed from the snapshot before a transition is chosen, whether or not one is. Leaving the state afterwards therefore does not cancel it again.
- An outcome or expiry whose id is not in the snapshot is **stale**: its state was left (and the invoke or timer cancelled) after the driver had already queued it. It is traced as `stale` and changes nothing.
- An emitted event from a child run whose invoke or spawned child is no longer in the snapshot is **stale** too: it was cancelled or stopped after the child had emitted it, as a child cancelled by its parent does when its exit effects `emit`. Taking an emitted event does not consume the invoke or the spawned child. An emitted event from a peer (`invoke` `null`) and a message are never stale.
- A spawned child's outcome consumes it like an invoke's: the child is removed from `spawned` before a transition is chosen, whether or not one is. An outcome of a child that was stopped or replaced after the driver had queued it is stale.
- An input that enables no transition is **dropped**: traced as `dropped`, and nothing else happens. Settling is skipped, since nothing changed. A consumed invoke or timer stays consumed. A dropped call is the exception: it is answered at once with `Unanswered`, so its caller does not wait for it ([calls between runs](#calls-between-runs)).
- **In a strict machine** (`strict` in the IR), an external event, a message or a call that enables no transition is instead a runtime error, `unhandled`, whose `event` is the input ([defects](#defects)). A call that fails a strict callee this way is answered `Unanswered` by the callee's driver as the run ends, like any call pending when its callee ends ([calls between runs](#calls-between-runs)). Every other input that enables no transition is dropped as in any machine: an emitted event, from a peer or from a child run (invoked or spawned), an invoke's outcome whose guarded branches all fail, a spawned child's outcome that no transition takes (`spawn.done` or `spawn.fail`), and a timer's expiry whose guarded branches all fail. They arrive when another run or the clock decides, so failing on them would make an ordinary race fatal. A transition with no target and no effects (`ignore`) is an enabled transition like any other, so an input it takes is traced as `event`, with a `transition` record whose `to` is `null`, and is never `unhandled`. A `send` from a strict machine that reaches no run is `undelivered`, as from any machine ([delivery](#delivering-messages)): strictness is about what a machine receives.
- A **call** that enables a transition becomes **pending**: it is appended to `snap.calls` after the `event` record and before the microstep, so a `reply` among the transition's effects finds it.
- A `dropped` or `stale` record replaces the `event` record; an input is traced by exactly one of the three.
- **A defect in the step that ended the run.** The core finishes the run (or cancels it) inside the step, so the snapshot a step returns can already be `done`, `failed` or `cancelled` while the driver is still carrying out that step's commands, every action of the step among them: say the effects of the transition into a top-level final state, the exit effects `finish` runs (the root's among them), or the exit effects of `cancelRun`. If any of those actions has a host defect, the driver stops at it as for any action ([defects](#defects)), so the `done`, `failed` or `cancelled` record after it is never written, and gives the core the `defect` input. That is why a `defect` input is taken in any status but `defect`: the run's status becomes `defect`, its result the defect, and the `defect` record is its last. `failWithDefect` finds no invokes or timers left to cancel, and `active` stays `[]`. A second `defect` input, for a run that already ended with one, changes nothing and writes nothing.

## Selecting a transition

A transition is **enabled** if its trigger matches the input and its guard holds. A `null` guard holds. Selection gives the list `T` of transitions the input takes, in order: empty when it enables none, one transition, or, with parallel states, one per region that takes the input. Guards are evaluated in the order given here, and evaluation stops at the first enabled transition of each search, so a guard after it is never evaluated (which matters because a guard can call host functions).

### Searching from the active leaves

An external event, a message, an emitted event, a call, a spawned child's outcome and `always` are selected by **walks**. Each state has **candidates** for the input, given below, tried in document order; the state **offers** the first candidate whose guard holds, or nothing.

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

**An external event or a message** `{"_tag": N, …}`: the candidates of `s` are its transitions with trigger `{"kind": "event", "event": N}`. If `s` has none, they are its `any` (`on *`) transitions instead. So an `on *` is not tried in a state that has an `on N` whose guards all failed. `on *` never matches an invoke outcome, a timer or an emitted event. A message is one of the machine's own events, so it is selected exactly as the same event from host code would be; only its record shows who sent it. `T = selectByWalks(those candidates)`.

**An emitted event** `{"_tag": N, …}` from run `input.from`: the candidates of `s` are its transitions with trigger kind `emitted`, `event` `N`, and an address `from` that names the sender:

- an address of kind `child` names it when `input.invoke` is not `null` and the entry of `snap.invokes` with id `input.invoke` has `state` equal to the address's `state`. (That entry exists: the input would otherwise have been stale.)
- an address of kind `ref` names it when `input.invoke` is `null` and the address's `name` is a parameter whose value, or a variable whose value, is the run id `input.from`. (An address names a parameter or variable of type `ref M`, through aliases: never an optional `ref` or a list of them, which the compiler rejects.) A variable with no value names no sender; reading it here is not an `unassigned` defect.

- an address of kind `children` names it when `input.invoke` is not `null` and the entry of `snap.spawned` with id `input.invoke` has `children` equal to the address's `name`. `key` is bound to that entry's `key`.

`T = selectByWalks(those candidates)`. There is no fallback to `on *`. So `on upload.Progress` is taken only for a `Progress` from the child that `upload` names, and from the run that started it, not from an earlier child of the same state: a child restarted by re-entering its state has a new invoke id. `on sessions.Progress` is taken for a `Progress` from any running child of `sessions`.

**A spawned child's outcome** for the entry `ch` just removed from `snap.spawned`: for `spawn.done`, the candidates of `s` are its transitions with trigger `{"kind": "spawn.done", "children": ch.children}`. For `spawn.fail` with error tag `E`, they are its transitions with trigger kind `spawn.fail`, `children` `ch.children` and `error` `E`, or, if `s` has none, those with `error` `"*"`. `T = selectByWalks(those candidates)`, written `selectSpawnOutcome`. There is no fallback to `on *`.

**A call** of the machine's call `c`: the candidates of `s` are its transitions with trigger `{"kind": "call", "call": c}`. `T = selectByWalks(those candidates)`. There is no fallback to `on *`, which matches only events. With parallel states one call can be taken in several regions, like an event; it is still one pending call, which only one `reply` can answer.

**`always`**: the candidates of `s` are its `always` transitions. `T = selectByWalks(those candidates)`, used by [settling](#settling).

The other inputs search one state and take at most one transition, so `T` is empty or holds the first enabled candidate:

**An invoke outcome** for invoke `inv`: only the state `inv.state` is searched. For `invoke.done` the candidates are its `invoke.done` transitions. For `invoke.fail` with error tag `E`, they are its `invoke.fail` transitions with `error` `E`, or, if it has none, those with `error` `"*"`. Guards are tried in document order.

**A timer expiry** for timer `tm`: the candidates are the transitions of `tm.state` with trigger `after` and the same `group` as the transition at `tm.index`, in document order.

**`done`** of state `p` (compound or parallel): `p`'s `done` transitions in document order.

`event` is in scope in the guard and effects of an `on` transition (including `on *` and `on name.Event`), `result` in an `invoke.done` or `spawn.done` transition and `error` in an `invoke.fail` or `spawn.fail` transition, bound to the input's `value` or `error`. `key` is in scope in an `emitted` transition whose address is of kind `children` and in a `spawn.done` or `spawn.fail` transition, bound to the key of the child the input came from. In an `on c` transition, which takes a call, `event` is bound to the input's `args`, a record of the call's parameters, and `call` to the **pending-call value** `{"from": input.from, "invoke": input.invoke}`. For an emitted event `event` has the type `Machine.Name` of the sender's event ([type references](ir.md#type-references)), and its value is the tagged record the sender emitted. Whether a chart may read `event` in an `on *` is the compiler's business. The implicit names `event`, `result`, `error`, `call` and `key` are reserved and parameter and variable names are unique, so a name in a `ref` means exactly one thing. (`attempt`, which only a retry's delay reads, never reaches the IR: the compiler replaces it with the retry's counter variable.)

## A microstep

A microstep takes the transitions of `T` together, as SCXML's microstep does: first every exit they cause, then every transition's effects, then every entry.

```
microstep(T):                         # each t in T belongs to a state, t.source
  for t in T, in order:
    emit trace { kind: "transition", from: t.source.id, index: t's index in t.source.transitions, to: t.target }
  exits = the union of exitSet(t) for t in T          # computed before anything is exited
  recordHistory(exits)
  for s in exits, innermost first: exitState(s)
  for t in T, in order: runEffects(t.effects)
  entries = the union of entrySet(t) for t in T with a target
  enterStates(entries, in document order)

domain(t):                            # for t with a target
  if t.source is the root: return the root
  return the nearest proper ancestor of t.source that is a compound state (the root is one)
         and a proper ancestor of the target

exitSet(t):
  if t.target == null: return []      # internal: no state is left or entered
  return the active states that are proper descendants of domain(t)

entrySet(t):
  path = the states from just below domain(t) down to the target, outermost first
  set = path + defaultEntry(target)
  for p in path, other than the target, if p.kind == "parallel":
    for each child c of p, if c is not in path: set = set + [c] + defaultEntry(c)
  return set
```

That is `entrySet` for a target that is not a history state. A target that is one is [below](#history-states).

With one transition this is: the `transition` record; the exits, innermost first; the effects; the entries, outermost first. A transition with no target contributes only its `transition` record and its effects. The transitions of `T` never conflict, so their exit sets are disjoint, and so are their entry sets. Exiting innermost first across regions exits the later region's states before the earlier region's ([exiting](#exiting-and-entering-a-state)), while the effects run in the order of `T`, the earlier region's first.

**Every transition with a target is external**, in SCXML's sense. Unless the source is the root, its domain is the least common compound ancestor of its source and target, which is a proper ancestor of both, so the source itself is always exited and the target always entered. **The domain is never a parallel state**: a parallel state is skipped when looking for it, as SCXML's `findLCCA` skips it, so a transition between two regions, or from a region to a state inside itself, leaves the parallel state and enters it again with all its regions:

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
| `X -> C.H`, `H` a history state of `C`, from outside `C` | the least common compound ancestor | as for any transition | `C`, and what `H` recorded, or `C`'s default entry |
| `C.A -> C.H`, from inside `C` | `C` (if compound) | `A` and its active descendants, not `C` | what `H` recorded when `C` was last exited, or `C`'s default entry, not `C` itself |
| `C -> C.H`, written in `C` | the least common compound ancestor of `C` | `C` and its active descendants, recording them | `C`, and what `H` just recorded: the states `C` was in |

The entries are in document order, so the regions of a parallel state are entered one after the other, each with everything below it, in the order the regions are written.

Re-entering a state restarts its work: its invoke is cancelled and started again with a new id, and its timers likewise. **A transition with no target is internal**: no state is left or entered, nothing is cancelled or restarted, and only its effects run.

The root is never a target. A transition written in the machine body (an `on`, `after` or `always` of the root) has the root as its domain: everything below the root is exited and the target entered, but the root itself is neither exited nor re-entered, so its entry and exit effects do not run again and its own timers keep running.

### Exiting and entering a state

```
exitState(s):
  if snap.invokes has an entry inv with state s.id:
    remove it; emit trace { kind: "invoke.cancelled", invoke: inv.id, state: s.id, src: inv.src }
    emit invoke.cancel { invoke: inv.id }
  for each entry tm of snap.timers with state s.id, by ascending id:
    remove it; emit trace { kind: "timer.cancelled", timer: tm.id, state: s.id, index: tm.index }
    emit timer.cancel { timer: tm.id }
  runEffects(s.exit)
  remove s from active
  remove every entry s from the completion queue
  emit trace { kind: "exited", state: s.id }

enterStates(list):
  for s in list, in order:
    add s to active (active stays in document order)
    emit trace { kind: "entered", state: s.id }
    runEffects(s.entry)
    add s to the states entered during this macrostep
    if s.kind == "final":
      if parent(s) is the root: finish(s); return
      complete(s)

complete(f):                          # a final state f was just entered; parent(f) is not the root
  s = f
  if parent(f).kind == "compound":
    append parent(f) to the completion queue
    s = parent(f)
  while parent(s).kind == "parallel" and parent(s) is in a final state:
    s = parent(s)
    append s to the completion queue

runEffects(effects):
  for e in effects with kind "set", in order:
    v = eval(e.value); snap.vars[e.var] = v
    emit trace { kind: "set", var: e.var, value: v }
  for e in effects with kind "do", "send", "emit", "reply", "spawn" or "stop", in order:
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
          inv = the entry of snap.invokes with state e.to.state
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
          to = eval({ kind: "ref", path: [e.to.name] }); invoke = null
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
        args = the parameter names of e.machine zipped with eval of e.args, left to right
        stopChild(e.children, k)
        append { children: e.children, key: k, args } to the pending spawns
      "stop":
        stopChild(e.children, eval(e.key))

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

So a state's work stops before its own exit effects run, and the `exited` record comes last. All `set` effects of a list run before its other effects, whatever order the IR lists them in, and each `set` sees the assignments before it. Then the `do`, `send`, `emit`, `reply`, `spawn` and `stop` effects run in the order the IR lists them. None of them can change variables, so every one of them in a list sees the same values, and a message or a reply carries what its transition assigned.

- **A reply** reads its pending-call value before it evaluates its result or error. A value whose call is not pending (it was answered already, by this effect list or an earlier step, or the run was restored) is the runtime error `answered`, whose `call` is the call's name.
- **Message ids.** Every `send` and `emit` effect that runs takes the next message id, also one that is undelivered. With the run id it names the message in every record about it: the sender's `sent`, `emitted` or `undelivered`, and the receiver's `event`, `dropped` or `stale`.
- **A send to a child** goes to the run started by the invoke of the address's `state` that is running now. If that state has no running invoke, the message is **undelivered**: traced with `to` `null`, and nothing is sent. This happens when the send runs in the step that enters the invoking state, before its work starts (the `entry` of a state inside it, say: the compiler rejects only the invoking state's own `entry`), and when the invoke's outcome has already been taken while the state stays active (an outcome transition with no target, or an outcome that enabled no transition).
- **A send to a peer** reads the `ref` parameter or variable the address names, exactly as an expression `ref` would, so a variable with no value is an `unassigned` defect. The address is read before the value is evaluated.
- **Delivery is the driver's.** The core does not know whether the receiver is still running. If it is not, the driver writes `undelivered` after the `sent` record ([delivery](#delivering-messages)), with the `sent` record's value, which the core redacted: never the `send` command's value.
- **A send to a spawned child** by key evaluates the key, then the value, and goes to the running child of the collection with that key. If there is none, the message is **undelivered**, traced with `to` `null`: no child has that key, it was stopped, or it was spawned in this macrostep and has not started yet ([spawned children](#spawned-children)). **`send sessions.*.E`** evaluates the value once and sends it to every running child of the collection, in the order they were started, each as a message of its own with its own id and `sent` record. With no running child it writes nothing; it takes no message id then.
- **`spawn` and `stop`** are described under [spawned children](#spawned-children).

In a microstep, then, the order is: the `transition` records, one per transition; for each state exited, innermost first, its invoke cancelled, its timers cancelled, its exit effects, `exited`; the transitions' effects, transition by transition; for each state entered, in document order, `entered` and its entry effects. No work is started during a microstep.

### History states

A history state `h` with parent `p` records where `p` was each time a microstep exits `p`, and a transition that targets `h` enters those states again.

```
recordHistory(exits):                 # before any state of exits is exited
  for s in exits, for each history state h in s.states:
    if h.kind == "history":     snap.history[h.id] = the active children of s, in document order
    if h.kind == "deepHistory": snap.history[h.id] = the active leaves that are proper descendants
                                                     of s, in document order

entrySet(t), when t.target is a history state h with parent p:
  if snap.history has h.id: targets = snap.history[h.id]
  elif p.kind == "compound": targets = [p.initial]
  else:                      targets = the children of p         # every region
  set = []
  for r in targets:
    set = set ∪ (the states from just below domain(t) down to r) ∪ defaultEntry(r)
  for q in set, if q.kind == "parallel":          # once: a region added with its default
    for c in the children of q, if neither c nor any descendant of c is in set:     # entry
      set = set ∪ [c] ∪ defaultEntry(c)           # leaves no parallel state incomplete
  return set
```

- **What is recorded.** A shallow history state records its parent's active children: one for a compound parent, every region for a parallel one. A deep one records the active leaves below its parent, atomic and final states, in every region. Each record replaces the one before. It is recorded from the active states as they are before the microstep exits anything, so the record is where `p` was at the end of the previous microstep. Only a microstep records: `finish` and `cancelRun` exit states without recording, and `failWithDefect` exits none. A runtime error in a step discards what the step recorded with the rest of the step.
- **Entering.** Entering `h` enters what it recorded, or, with no record, the states directly below `p` that `p`'s default entry starts with. Each of those states gets its default entry: a shallow record's child its `initial` chain or every region, a deep record's leaves nothing more. Every state between the domain and them is entered too, and a region of a parallel state on the way that none of them is in gets its default entry, as for any target. A deep record holds a leaf in every region below `p`, so no region below `p` is entered by default. `h` itself is never entered: there is no `entered` record for it, and it is never in `active`. The `transition` record's `to` is `h`'s id.
- **The domain** is `domain(t)` with `h` as the target, as for any state: the nearest compound proper ancestor of the source that is a proper ancestor of `h`. It does not depend on what `h` recorded. From outside `p` that is above `p`, so `p` is entered. From inside a compound `p` it is `p`, so `p` is neither exited nor recorded by this transition, and what is entered is what `p` recorded when it was last exited. (SCXML computes the domain from the states the history restores, which differs only for a transition from inside `p`.) From inside a parallel `p` the domain is above `p`, which is exited, recorded and entered again as recorded.
- **Restored states are entered**, with their `entry` effects, their `entered` records and, once the step has settled, new invokes and timers, as on any entry. An entered final state completes its parent as usual (`complete`). Nothing else is restored: variables keep their values, and the work that ran in those states before, an invoked child run among it, is not resumed.
- **Order.** A microstep records before it computes its entries, so a transition that exits `p` and targets `h` (from outside `p`, or from inside a parallel `p`) enters what `p` was a moment before: the same states, entered again.
- With one transition and a recorded or default `targets` of one state, this is `entrySet` above with that state as the target, except that the domain is `h`'s.

## Settling

After the input's microstep (and after the initial entry in `start`), the core takes eventless transitions until none is enabled:

```
settle():
  count = 0
  loop:
    if snap.status != "running": return
    T = the enabled always transitions (selectByWalks, conflicts removed)
    while T is empty and the completion queue is not empty:
      p = take the first state from the completion queue
      T = [the first enabled done transition of p], or [] if there is none
    if T is empty: return
    count = count + 1
    if count > 1000: runtime error { kind: "limit" }
    microstep(T)
```

- `always` transitions take priority over `done` transitions. A `done` is considered only when no `always` is enabled, and completions are considered in the order they were queued. The `always` transitions enabled in several regions are taken together, in one microstep, as an input's are.
- A compound state is complete when one of its final children is entered. A parallel state is complete when it is in a final state: when, after a final state below it is entered, every region is in a final state (`complete`). The entry that completes the last region queues the parallel state, right after that region's own completion, so a region's `done` is tried before its parallel state's. A region that is itself a final state is in a final state from the moment it is entered; a region that is an atomic state never is, so a parallel state with one never completes. A parallel state inside a region of another completes it in turn when the outer one's other regions are in a final state too. Once a region is in a final state it stays so until its parallel state is exited, since a final state has no transitions and every transition that could leave it exits the parallel state.
- A state's completion is queued once per entry, and removed from the queue when the state is exited (`exitState`), so a state in the queue is always active and still in a final state. A completion is therefore never taken for a state that was exited after it completed, even if it was entered again in the same macrostep: an `always` that leaves and re-enters it discards the completion. When a completion is taken from the queue, the state's `done` transitions are tried once: if none of their guards holds, it is discarded without a trace record and is not retried later.
- A run that takes more than 1000 microsteps while settling one input has a runtime error `limit`, so a guarded `always` loop cannot hang the host. (The compiler rejects unguarded ones.)

## Starting work

After settling, if the run is still running, the core starts the children spawned during this macrostep, in the order they were spawned, then the invokes and timers of every state that was **entered during this macrostep and is still active**, in document order (outermost first):

```
startWork():
  for p in the pending spawns, in order:
    id = snap.nextInvoke; snap.nextInvoke += 1
    append { id, children: p.children, key: p.key, args: p.args } to snap.spawned
    src = the machine of collection p.children
    emit trace { kind: "spawn.started", spawn: id, children: p.children, key: p.key, src, args: p.args }
    emit spawn.start { spawn: id, src, args: p.args }
  clear the pending spawns
  for s in active, in document order, if s was entered during this macrostep:
    if s.invoke != null:
      id = snap.nextInvoke; snap.nextInvoke += 1
      if s.invoke.kind == "call":                 # invoke inventory.reserve(…)
        to = eval({ kind: "ref", path: [s.invoke.to.name] })
        src = s.invoke.to.name + "." + s.invoke.src
        params = the parameters of the call s.invoke.src of machine s.invoke.to.machine
      else:
        to = null; src = s.invoke.src; params = the parameters of s.invoke.src
      args = the names of params zipped with eval of s.invoke.args, left to right
      append { id, state: s.id, src } to snap.invokes
      emit trace { kind: "invoke.started", invoke: id, state: s.id, src, args }
      emit invoke.start { invoke: id, state: s.id, src: s.invoke.src, srcKind: s.invoke.kind,
                          to, as: s.invoke.as, args }
    for each after block of s, in document order of its first transition (index i):
      delay = eval(s.transitions[i].trigger.delay)
      id = snap.nextTimer; snap.nextTimer += 1
      append { id, state: s.id, index: i, delay } to snap.timers
      emit trace { kind: "timer.started", timer: id, state: s.id, index: i, delay }
      emit timer.start { timer: id, delay }
```

- A call's peer is read before its arguments are evaluated, as a `send`'s address is, so a `ref` variable with no value is an `unassigned` defect. Its `src`, in the snapshot and the records, is `inventory.reserve`: the address's name, a dot, and the call's.
- Within one state the invoke starts before the timers. States are taken in document order, so the work of several regions of a parallel state starts region by region, in the order the regions are written, and gets its invoke and timer ids in that order.
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
  stopChildren()
  snap.result = status == "done" ? { output: value } : { error: value }
  snap.status = status
  clear the completion queue
  emit trace status == "done" ? { kind: "done", state: f.id, output: value }
                              : { kind: "failed", state: f.id, error: value }
```

The output or error is evaluated right after `f`'s entry effects, before any exit effect runs. Then every active state is exited, innermost first, with its exit effects, as on any exit (but no history is recorded: the run is over): this is when the root's `exit` effects run, and when timers on the root are cancelled. Then the spawned children still running are stopped, after every exit effect ([spawned children](#spawned-children)). A message an exit effect sends a child is queued before the stop, but the stop is a cancel request, which a run takes before anything queued, so whether the child takes the message first is up to the scheduler, as for any message to a run cancelled in the same step ([conformance](conformance/README.md#how-a-runner-works)); a chart cannot rely on it. Settling stops and no work is started. The run's result is the output (an Effect success, a Rust `Ok`) or the error (the Effect error channel, a Rust `Err` of the machine's error enum).

## Cancelling from outside

The input `{"kind": "cancel"}` exits every active state, innermost first (in reverse document order, so the later regions of a parallel state first), exactly as `exitState` does: each state's invoke and timers are cancelled and its exit effects run. No history is recorded, and `history` keeps what it had. Then the spawned children still running are stopped, as when the run finishes, the status becomes `cancelled` and a `cancelled` record is written. There is no `event` record for a cancel.

```
cancelRun():
  for s in active, innermost first: exitState(s)
  stopChildren()
  snap.status = "cancelled"
  emit trace { kind: "cancelled" }
```

This was decided on 27 November 2026 ([semantics](../docs/design/semantics.md#cancelling-from-outside)): exit effects are the only cleanup hook a chart has, so they must be quick. A parent leaving an invoking state cancels its child run this way, so the child's exit effects run too, and the parent waits for them ([child runs](#child-runs)). The cancel step is the run's last; a host defect in one of its exit actions turns it into a defect ([taking an input](#taking-an-input)).

## Messages between runs

A run talks to another only through queues. The core's part is small: a `send` or `emit` effect writes a record and gives the driver a `send` or `emit` command ([effects](#exiting-and-entering-a-state)), and a message or an emitted event comes back to the receiver's core as an input like any other ([taking an input](#taking-an-input), [selecting](#selecting-a-transition)). The driver does the delivering ([delivering messages](#delivering-messages)).

- **Sending is queuing.** The sender's driver pushes the input onto the receiver's queue and carries on with the next command. A sender never waits for the receiver and never runs the receiver's transitions; the receiver takes the input later, one at a time, like any other.
- **Who receives a `send`.** For an address of kind `child`, the child run of the invoke that is running in the address's `state`. For an address of kind `ref`, the run whose id the parameter or variable holds. For an address of kind `children`, the running child of the collection with the `send`'s key, or every running child of it for `send name.*.E` ([spawned children](#spawned-children)).
- **Who receives an `emit`.** The run's parent, if it invoked the run with `as` or spawned it; an invoker without `as` has no name to receive them under, and they are not delivered to it. And every run that holds a `ref` to the emitting run ([refs and handles](#refs-and-handles)). Of these, a run receives an emitted event `E` from a run of machine `M` only if its own machine **hears** it: has a transition, in any state, with trigger kind `emitted`, `event` `E` and an address whose `machine` is `M`, whatever the address's kind (`child`, `ref` or `children`) and name. The filter applies to a spawned child's parent as to an invoker: a parent whose machine has no `emitted` transition for `E` from the child's machine, `on sessions.E` or any other, is not sent it. Every other run is not sent it and writes nothing for it. The filter is decided from the chart alone, never from the states a run is in: a run that hears an event and has no enabled transition for it when it takes it, because its guard fails or it is in another state, writes `dropped`. A run never receives its own emitted events.
- **A run holds a `ref`** to each run whose handle it was given as an argument: the value of a parameter whose type is `ref M`, or contains one (`ref M?`, `ref M[]`, an alias of one). A machine gets a `ref` only as an argument (from host code, or from a parent that passes one of its own to an invoke), never from a result, an event, a call's arguments or a field, so the runs it holds are fixed when it starts. A `ref` variable holds one of them.
- **Messages are not supervised.** When a run ends, the runs that hold a `ref` to it are not told; they learn of it only through their own timeouts ([supervision](../docs/design/actors.md#supervision)). Messages sent to it afterwards are `undelivered`.
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

A machine declares keyed collections of children, `children sessions: Session by SessionId`, and starts, addresses and stops them by key with the effects `spawn sessions[k] = Session(args)`, `send sessions[k].E`, `send sessions.*.E` and `stop sessions[k]` ([language](../docs/design/language.md#many-children)). The rules:

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

## Defects

A defect ends the run at once. It is never routed to a `fail` branch, and no exit effect runs, since the machine can no longer be trusted to run its own code. A defect is an object:

| Defect | Cause |
| --- | --- |
| `{"kind": "host", "message": "…"}` | Host code failed: a function, action or task threw or panicked, or returned a value that is not of its declared type (a negative `duration`, say). `message` is set as described under [catching host defects](#catching-host-defects); in conformance cases it is given by the script. |
| `{"kind": "unassigned", "var": "receipt"}` | A variable was read before it had a value. The compiler rejects such a read (`RB0403`, definite assignment), so this is a runtime backstop, reachable only where that check is skipped, as for files listed in `allow-unassigned`. |
| `{"kind": "overflow", "expr": "attempts + 1"}` | Integer arithmetic left the `int` range, or float arithmetic produced a value that is not finite. `expr` is the `text` of the operator's IR node. |
| `{"kind": "undeclared", "error": "Oops"}` | A task or child machine failed with an error it does not declare. |
| `{"kind": "limit"}` | Settling took more than 1000 microsteps. |
| `{"kind": "unhandled", "event": {"kind": "event", "value": {"_tag": "Knock", …}}}` | A strict machine was given an external event, a message or a call that enabled no transition ([taking an input](#taking-an-input)). `event` is the input, an `event`, a `message` or a `call`, exactly as an `event` record would have it, so a message keeps its sender and message id. |
| `{"kind": "answered", "call": "reserve"}` | A `reply` was given a pending-call value whose call is not pending: it was answered already, or the run was restored ([calls between runs](#calls-between-runs)). |

**Inside the core.** Runtime errors (`unassigned`, `overflow`, `undeclared`, `limit`, `unhandled`, `answered`) and a `host` defect returned by `fns` while evaluating an expression happen inside `start` or `step`. The call is then **atomic**: every command it had produced is discarded, and it returns `failWithDefect(d)` applied to the snapshot it was given, so even an invoke the failed step had consumed is cancelled again (a driver ignores that). There is no `event` record for the input.

**A defect in `start`** works the same way, applied to the initial snapshot (`active` `[]`, `vars` `{}`, no invokes or timers): `start` still returns a snapshot, with status `defect`, and two commands, the `started` record and the `defect` record. The `started` record is kept so the trace still says what the run was. The driver then ends the run like any other that ends with a defect: a root run raises the defect to whoever called `run`, a child run reports it to its parent as `invoke.defect`. A host defect in an action of `start`'s commands (a root `entry` effect, say) is an ordinary action defect, below.

**A host defect in an action** is found by the driver while carrying out a step's commands (or `start`'s). It stops carrying them out, writes none of the remaining records, and gives the core `{"kind": "defect", "defect": {"kind": "host", "message": …}}`, also when the step had already ended the run ([taking an input](#taking-an-input)). **A host defect in a task** arrives as `invoke.defect`, and is ignored as stale if that invoke was already cancelled. **A child run that ends with a defect** is reported to its parent the same way, with the child's defect object unchanged, so the parent's run ends with a defect too and the defect travels up to whoever ran the root. **A defect in a child's cancel step**, while its parent carries out `invoke.cancel`, is met by the parent's driver like a defect in an action: it gives the parent `{"kind": "defect", "defect": d}` with the child's defect `d` unchanged ([child runs](#child-runs)).

```
failWithDefect(d):
  for inv in snap.invokes: emit invoke.cancel { invoke: inv.id }    # no trace records
  for ch in snap.spawned:  emit spawn.stop { spawn: ch.id }
  for tm in snap.timers:   emit timer.cancel { timer: tm.id }
  snap.invokes = []; snap.spawned = []; snap.timers = []
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
| `ref` | `path[0]` is a parameter (from `input`), a variable (from `vars`; reading one with no value is `unassigned`), or the implicit name `event`, `result`, `error`, `call` or `key`. Each further name reads that field of the value. A parameter or variable of a `ref` type reads as the run id it holds. |
| `field` | Evaluate `of`, then read field `name`. |
| `call` | Evaluate `args` left to right, then `fns(fn, args)`. |
| `record` | An object with the fields in order. Its `type` may be an instance of a generic record (`Page<Order>`); the fields are the same. If `type` is an error, it also has `"_tag": type`. If `type` is an event (the value of a `send` or `emit`: `Machine.Name`, or the name of one of the current machine's `emits`), it also has `"_tag"` with the event's name, the part after the dot: `send upload.Pause` sends `{"_tag": "Pause"}`. |
| `enum` | The string `variant`. |
| `op` | See below. |

Operators, by `op`. An operand's type is its IR `type` with aliases expanded, transitively, the arguments of a generic alias substituted: `<` on a `Money` after `type Money = int` compares integers, and so does `+` on a `First<int>` after `type First<T> = T`. The IR keeps the alias's name, so a runtime that dispatches on the type string must expand it first.

- `and`, `or`: evaluate the left side; if it decides the result (`false` for `and`, `true` for `or`), that is the result and the right side is **not evaluated**. Otherwise the result is the right side. `not` negates.
- `==`, `!=`: on `string`, `int`, `float`, `bool`, `duration` and enums. Strings are equal when their Unicode scalar values are. Floats compare as IEEE 754 doubles, so `0.0 == -0.0`.
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
- **Outcomes of a task.** Success gives `invoke.done` with the value. A failure with a tagged error gives `invoke.fail` with it (the core checks that it is one the task declares, and makes any other an `undeclared` defect). A defect gives `invoke.defect` ([catching host defects](#catching-host-defects)).
- **Run ids.** The driver names each top-level run; a conformance runner names them `r1`, `r2`, … in the order the case starts them. A child run's id is its parent's id, a dot, and the id of the invoke that started it: `r1.4` is the run started by invoke 4 of run `r1`. A `ref` is a run id, so the ids of top-level runs that can reach one another must differ: the default names (`r1`, `r2`, … in the order runs start in the process) do, and a driver given a fixed id (Rust `Config::run_id`, a `RootRunId` of one's own) must not be given the same one twice. A fixed id must be a run id as [`snapshot.schema.json`](snapshot.schema.json) writes one, `^[A-Za-z0-9_-]+(\.[1-9][0-9]*)*$`, or the run's saved snapshot would be refused as `invalid`: a runtime refuses any other when it is given (Rust's `Config::run_id` panics; in TypeScript the run dies with a host defect as it starts, before any record).

### One queue per run

Every source of input pushes onto one first-in, first-out queue per run, at the moment the input arrives. The queue is unbounded: nothing is ever refused because a queue is full ([full queues](../docs/design/actors.md#full-queues)).

| Source | Pushes |
| --- | --- |
| A caller's `send` | the external event |
| Another run's `send` | a `message` input ([delivery](#delivering-messages)) |
| Another run's `emit` | an `emitted` input, for its parent (its invoker or spawner) and the runs that hold a `ref` to it, of those that hear it |
| A task, or a machine invoke that is not a child run | its outcome, when its future or fiber ends |
| A child run | the outcome of the invoke that started it, when the child ends |
| A timer | its expiry, when its sleep completes |
| Another run's `invoke` of a call | a `call` input ([carrying out a call](#carrying-out-calls)) |
| A callee's `reply`, or its driver when it ends | the outcome of the call's invoke |

The run takes inputs from the front, one at a time, and takes the next only once the previous step's commands have all been carried out. A cancel request is the one exception: it is not queued, and is taken before anything that is.

- **Nothing reaches the core except through the queue.** An Effect driver can make the queue an Effect `Queue` that senders, invoke fibers and timer fibers offer to. A Rust driver's loop `select`s over the event channel, the set of invoke futures and the timers only to find out what has arrived: it pushes every input it finds onto the run's queue (a `VecDeque`, say) and gives the core the front of that queue, never an input straight from the source that produced it. So an outcome that completes while the run is busy, or in the same poll as an event, waits behind what arrived before it, and may be stale by the time it is taken. Inputs that one poll finds ready together are pushed in an order the driver chooses; nothing outside conformance depends on that order, and a conformance runner never lets it arise.
- **Several inputs at once.** A driver lets a caller append a list of inputs to a run's queue in one operation, so that the run takes no step between them: `Queue.offerAll` on the run's queue in Effect, and in Rust a method that pushes them all onto the queue while the run is between steps. A conformance runner delivers a `together` step this way ([conformance](conformance/README.md#how-a-runner-works)).
- **An invoke is forgotten once its outcome is taken.** When the core takes an invoke's outcome (whether it writes `event`, `dropped` or `stale`), the driver drops that invoke's future or interrupts its fiber if it is still there. Normally it has already ended; it has not when its outcome was appended by a caller, as a conformance runner does for a scripted invoke in a `together`, and dropping it then ensures it never queues a second outcome.
- **Cancelled work queues nothing.** Once the driver has carried out `invoke.cancel` or `timer.cancel`, the invoke or timer has been dropped or interrupted and pushes nothing more. An outcome or expiry it pushed before stays queued and reaches the core, which writes it as `stale`.
- **Cancellation takes effect between steps.** A cancel request does not interrupt the commands of a step in progress. When they are done the driver gives the core `{"kind": "cancel"}`, carries out the resulting commands, and discards anything still queued.

### Child runs

A machine invoke's implementation is host code: in TypeScript the value given for it in `machines`, in Rust a `Handlers` method whose body usually calls the child machine's `run`. When that code runs the chart, the driver has to know, so that the child writes to the parent's trace on the parent's clock and is cancelled gracefully. Both drivers do it with an **invoke context**.

- **The context.** When the driver carries out `invoke.start` with `srcKind` `machine`, or `spawn.start`, it runs the implementation with an ambient invoke context holding the trace sink, the clock and timer, the parent's run id, the invoke id, a fresh cancel token, the messages the parent has sent to the invoke so far (none yet), and, when the command is `spawn.start` or its `as` is not `null`, a way to push onto the parent's queue the emitted events the parent's machine hears from the invoked or spawned machine ([messages between runs](#messages-between-runs)). In TypeScript the context is a `Context.Reference` provided to the invoke's fiber. In Rust it is a task-local, or a thread-local that the driver sets around each `poll` of the invoke's future and restores afterwards, which works on any executor. All other host code the driver runs (functions, actions, tasks) runs with no invoke context; so does a child run's own work, except that each of its machine invokes gets a context of its own.
- **Claiming it.** A machine's `run` looks for an invoke context. If it finds one whose token is unclaimed, it claims the token and becomes the **child run**: its run id is `<parent run id>.<invoke id>`, its records carry `parent`, and it uses the context's sink, clock and timer. Its queue starts with the messages the context holds, in the order they were sent, and from then on the parent's sends go straight to its queue. Otherwise it is a root run. Host code that does not call `run` (a stub) never claims the token, which is how the parent's driver tells a stub from a child run. The parent's core cannot tell them apart and does not need to.
- **Its outcome.** A child run ends like any run: on `done` its `run` returns the output, on `failed` the error, and on `defect` it raises the defect ([catching host defects](#catching-host-defects)). The parent's driver turns that into `invoke.done`, `invoke.fail` or `invoke.defect` as for a task, with the output, error or defect object unchanged, and pushes it onto the parent's queue; for a spawned child, into `spawn.done`, `spawn.fail` or `spawn.defect` with the same members and `spawn` in place of `invoke`.
- **Spawned children** are child runs like any other: the host's implementation of the machine runs with an invoke context whose invoke id is the spawn's id, and `spawn.stop` is carried out exactly as `invoke.cancel`. A stubbed machine spawned is a stub like an invoked one. Below, "invoke" includes them.
- **Cancelling it.** On `invoke.cancel` the parent's driver signals the token. If the token was never claimed (a task or a stub), it drops the future or interrupts the fiber at once and is done. If it was claimed, the child takes the signal as a cancel request, between its steps like any other, runs its cancel step (exit effects and the `cancelled` record) and reports through the token that it has finished. The parent keeps driving the child until then (in Rust, it keeps polling the child's future), and only then drops it. Carrying out `invoke.cancel` waits for this, like an action, so nothing after it in the parent's step (the exit effects of the state being left, the rest of the transition) happens before the child's exit effects have run. A child that has already ended, however it ended, reports plain "finished" at once (never a defect), and its outcome is not queued after the signal; one already queued is stale.
- **A defect in the cancel step** is reported through the token too: the child writes its `defect` record and ends, and the parent's driver meets that defect while carrying out `invoke.cancel` and gives its own core `{"kind": "defect", "defect": d}`, with the child's `d` unchanged, as it would for a defect in an action. A cancelled task or stub has no cancel step, and whatever it does while being dropped or interrupted is ignored.
- **In Effect** the signal is interruption. The parent interrupts the invoke's fiber and waits for it to end (`FiberMap.remove`, or `Fiber.interrupt`, which returns the fiber's exit). A child's `run` runs each step uninterruptibly and its cancel step in an `Effect.onInterrupt` around the step loop, so interrupting it during a step waits for that step to finish, and the interruption completes only after the cancel step. A defect in the cancel step shows in the fiber's exit as a die with a `Defect`, which the parent uses only if the token was claimed.
- **In Rust** the token is a small shared value (a cancel flag with a waker, and a slot for "finished" or "finished with defect `d`"). The child's run loop checks it before each take, and the parent, after signalling it, polls the child's future until the slot is filled. The child's future may never complete after that; the parent drops it.

### Refs and handles

The core holds a `ref` as a run id ([values](#values)). Host code holds a **handle**: Rust's `Ref` (`inventory::Ref`, from `actor.handle()`), and its TypeScript counterpart, a value from a spawned run's actor. A handle can be cloned and passed on, and it is how host code reaches a run.

- **Encoding.** The binding layer turns a handle into its run's id when it encodes a value for the core (a machine's arguments), and a run id back into the handle when it decodes a value for host code (the arguments of an invoke, an action, a task or a function). A machine can only get a `ref` from its arguments, so every run id its core gives back, in a `send` or in arguments, is one of the handles the run was started with. The driver keeps those for as long as the run is open. It needs no registry of runs by id: a run reaches exactly the runs it was given. What a run needs to emit is the other way round, the runs subscribed to it ([delivery](#delivering-messages)), which its handle can keep.
- **Where handles come from.** A top-level run that host code `spawn`s has one; host code passes it in the arguments of the runs that are to talk to it. A child run has none: its parent reaches it through its invoke, and nothing else can. The tools' handles that a runtime gives for every run as it starts (Rust's `Config::on_run`, TypeScript's `RunObserver`) can queue inputs on a child run, but cannot cancel it or suspend it: a child run ends only by itself or when its parent leaves the invoking state, since its parent learns how it ended only through the invoke. The same holds for a top-level run of `run`, whose caller gets an output or an error, never a cancellation or a snapshot.
- **Host code sends** through a handle as through an actor: the run gets an ordinary `event` input, with no sender. An event that does not encode (a non-finite `float`) is the sender's host defect, never the receiver's: nothing is queued, and the host code that sent it fails (in TypeScript `send` dies with a `Defect`, in Rust it panics with a `roadbook::Defect` payload), so inside an action or a task it ends the run carrying that out with the defect, as any host defect there does. Host code receives what a run emits through its actor, not through a handle ([host listeners](#host-listeners)).
- **Equality.** The language defines no `==` on `ref`s. The core compares run ids only to tell which address an emitted event is from ([selecting](#selecting-a-transition)).

### Delivering messages

- **Open and closed.** A run is **open** from just before its driver calls `start` (or `restore`) until the core returns a snapshot whose status is not `running`, from `start`, `restore` or any `step`, or until it is [suspended](#suspending-and-resume); it is then **closed** for good. A run closes before the commands of its last step are carried out, so a message that another run sends it while it carries them out is undelivered, and an emitted event is not delivered to it. Checking that a receiver is open and pushing onto its queue should be one operation with respect to its closing; a driver where a push can slip in just before the receiver closes loses that message with the receiver's queue, without a record. No conformance case can tell these apart.
- **Subscriptions.** Before it calls `start`, the driver subscribes the run to the emitted events of every run it holds a `ref` to: each run whose handle is in its arguments, once however many times it appears there (a parameter of type `ref M`, `ref M?`, `ref M[]` or an alias of one). Each subscription keeps the events of `M` that the run's machine hears ([messages between runs](#messages-between-runs)), which a runtime works out from the IR when it loads it; it can be empty. When the run closes, it is unsubscribed.
- **Carrying out `send`.** The driver builds the input `{"kind": "message", "from": <this run's id>, "message": …, "value": …}`.
  - With an `invoke`, it is for the child run of that invoke. If a run has claimed the invoke's token and is open, the driver pushes the input onto its queue. If the token has not been claimed yet (the host code has not called `run` yet), the driver keeps the input in the invoke context for the run that claims it. If the run that claimed it has closed, the driver writes `{"kind": "undelivered", "message": …, "to": …, "value": …}` (adding `t`, `run` and `parent` as for any record), whose `value` is that of the `sent` record the core emitted right before the command: redacted ([redaction](#redaction)), unlike the command's. If the invoke ends or is cancelled without its token ever being claimed (a stub, a task-like implementation), the inputs kept for it are discarded without a record.
  - With `invoke` `null`, it is for the peer `to`, one of this run's handles. If that run is open, the driver pushes the input onto its queue; otherwise it writes the `undelivered` record.
- **Carrying out `emit`.** If this run is a child run whose `invoke.start` had an `as`, or one started by `spawn.start`, its parent's machine hears the event, and its parent is open, the driver pushes `{"kind": "emitted", "from": <this run's id>, "invoke": <its invoke id>, "message": …, "value": …}` onto the parent's queue. For every open run subscribed to this run whose subscription keeps the event, it pushes the same with `invoke` `null`. It writes nothing for receivers that are closed, and an emit with no receiver has only its `emitted` record. Pushing to the receivers is not an ordered operation the trace can show: they take their inputs independently.
- **Order per pair.** Everything one run pushes onto another's queue arrives in the order its driver carried out the commands, so two messages from A to B are taken in the order they were sent, and so are an emitted event and a message. A child run's emitted events reach its parent's queue before its outcome, which is pushed when the child's run ends, after the commands of its last step; a Rust driver that finds a child's emitted events and its end in one poll pushes the emitted events first. There is no order between what two different runs push.
- **Cancelled children.** "Cancelled work queues nothing" holds for a child run's emitted events as for its outcome: once the parent has carried out `invoke.cancel`, the child pushes nothing more. What it emitted before that, in its cancel step included, is in the parent's queue, where it is stale ([taking an input](#taking-an-input)), unless the parent has closed. A cancel request still goes before everything queued, messages included, and whatever a run has queued when it closes is discarded.
- **Stubs.** A stubbed machine never claims its token, so it receives nothing and emits nothing. Messages for it are kept until its invoke ends, then discarded.

### Host listeners

A **listener** is host code that receives what a run emits. It is not a run: the core never sees it, it has no queue the core takes from, and it writes no record. A run has any number of listeners, which subscribe and unsubscribe while it runs.

- **What a listener hears.** Every event the run emits, of its machine's `emits`, whatever any machine hears: the filter on emitted events ([messages between runs](#messages-between-runs)) is about runs, which write `dropped` for what they cannot take, and does not apply to listeners. A listener of a run hears that run's events only, never those of its child runs, invoked or spawned (a child's emitted events are for its parent, which hears them by the same filter as any run), and never the messages it sends. It hears the real values, `sensitive` fields included: a listener is host code, like an action given the same values, and only trace records are [redacted](#redaction), so a runner that compares what a listener heard with the `emitted` records redacts it first.
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

When a run ends, for whatever reason, the driver answers its pending and queued calls with `Unanswered` ([carrying out calls](#carrying-out-calls)), discards anything still in its queue, messages and emitted events included and without a record, unsubscribes it from the emitted events of its peers, and drops every invoke and timer of the run it still holds. Normally there are none, since the core cancelled them; there can be when an action's defect stopped a step before the `invoke.cancel` or `timer.cancel` of work the step had already removed from the snapshot, which `failWithDefect` therefore cannot see. A child run dropped this way gets a cancel signal first, as on `invoke.cancel`.

- **TypeScript.** Interrupting the fiber that runs a root `run`, or closing the scope of a `spawn`ed actor, is a graceful cancel. As for a child, each step runs uninterruptibly and the cancel step runs in `onInterrupt`, so the exit effects run and the trace ends with `cancelled` before the interruption completes; the run's Effect then ends interrupted.
- **Rust.** Dropping the future of a root `run` is a **hard stop**: nothing more runs, no exit effect and no final record (the trace simply stops, as a run's in progress does), and the invokes, child runs and timers it holds are dropped with it. Graceful cancellation from outside goes through the actor: `spawn` the run, then `actor.cancel().await` gives it a cancel request and returns once its cancel step has finished, after which `actor.result()` reports that it was cancelled. A child run is never dropped while it runs: its parent cancels it through the token.

### Time

- **Records.** `t` in a trace record is the driver's clock when it writes the record, in integer milliseconds: since the Unix epoch in production, and virtual milliseconds from 0 in conformance runs.
- **Timers.** A timer's expiry is due `delay` milliseconds after the driver carries out `timer.start`. The driver reads the time and waits only through the clock and timer it was given (in TypeScript a `Timer` service whose default sleeps on Effect's `Clock`, in Rust the `Timer` trait), telling the timer the run id, the timer id and the delay when it starts one and the ids when it cancels one. It never completes a timer itself, not even one with delay 0 (a `timer.fire` command queues an expiry without any timer). A test can therefore give it a virtual clock and fire timers itself, as a conformance runner does.
- **Due times.** The driver remembers, for each running timer, when it is **due**: the clock when it carried out the `timer.start`, plus the command's `delay`, capped at 2^53 − 1, the largest due time a saved snapshot holds (a delay near the longest `duration` would pass it). A timer restored by `timer.fire` keeps the due time it was saved with. A [saved snapshot](#the-saved-snapshot) records these.

How a conformance runner drives virtual time, scripted outcomes and quiescence, and what it may rely on the driver for, is specified in [conformance/README.md](conformance/README.md#how-a-runner-works).

## Saving and restoring

A run can be saved as plain data and started again from it later, in another process or in the other runtime: the request handler of [saving and restoring](../docs/design/semantics.md#saving-and-restoring) loads a snapshot, applies one event, and saves the result. Four operations do it:

- **save** gives a top-level run's [saved snapshot](#the-saved-snapshot), as its last step left it. The run carries on.
- **suspend** waits until a top-level run is idle, then stops it and gives its saved snapshot ([suspending](#suspending-and-resume)).
- **restore** checks a saved snapshot and starts a new top-level run from it ([restoring](#restoring-a-snapshot)).
- **resume** is restore, then one event, then suspend: the request handler's operation.

Only top-level runs are saved: host code holds no handle to a child run.

### The saved snapshot

A saved snapshot is the core's [snapshot](#snapshot), as the run's last step left it, with two additions: `snapshotVersion`, which is `3`, and a `due` member on each timer, from the driver ([time](#time)). Its `calls` member is written only when the run has pending calls, in any version, and a saved snapshot without one has none, so a snapshot saved before calls existed needs no new version. Its schema is [`snapshot.schema.json`](snapshot.schema.json). It holds the **real values**, those of `sensitive` parameters and variables and fields too: a restored run goes on with them, so nothing in a saved snapshot is [redacted](#redaction). Only the copy of it in a `restored` record is.

```jsonc
{
  "snapshotVersion": 3,
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

- **Versions 1 and 2.** Version 2 is the same without `spawned`, as written before keyed spawn, and version 1 is also without `history`, as written before history states: a runtime restores one as if it had `"spawned": []` (and `"history": {}`), and the `restored` record holds it as it was given. Save and suspend always write version 3. `calls` may be in any version: it is written only when not empty. A machine with no history state and no `children` keeps its hash from before both, so its older snapshots still restore.
- **`due`** is an absolute time on the driver's clock, in integer milliseconds: since the Unix epoch in production, virtual time in conformance. A deadline is kept rather than the time remaining because the run is in its state the whole time it is saved: an order that waits three days for payment expires three days after it began waiting, however long it spent in a store. A host can also find when to wake a saved run from its earliest `due`.
- **What is not in it.** Inputs queued but not yet taken (a request handler never has any, since it saves an idle run); the state of child runs, invoked or spawned, which start again from the beginning ([restoring](#restoring-a-snapshot)); messages kept for a child that has not claimed its invoke; subscriptions; and listeners.
- **The same in both runtimes.** Values are in their JSON form ([values](#values)), `active` is in document order, and `invokes`, `spawned` and `timers` are in the order they were started, so the two runtimes save the same run as the same document, and either restores what the other saved. Conformance compares saved snapshots as [canonical JSON](ir.md#canonical-json).
- **Numbers.** A JSON number's form does not matter. Wherever a saved snapshot has an integer (`snapshotVersion`, the ids, a timer's `index`, `delay` and `due`, an `int` or `duration` value), one written as a float with no fraction (`1.0`, `10000.0`, `-0.0`) is that integer, as JSON Schema's `integer` and JavaScript read it; a runtime always writes its integers in integer form (`2`, and `-0` as `0`).
- **A copy.** What save and suspend give host code, and what restore takes from it, is the runtime's no longer: changing a saved snapshot afterwards changes no run.

### Restoring a snapshot

Restoring checks the saved snapshot against the IR the runtime was given, refuses it with a reason if it does not fit, and otherwise starts a new top-level run from it. Nothing is written to the trace before the checks pass. They are made in this order:

| Reason | Refused when |
| --- | --- |
| `invalid` | It does not match `snapshot.schema.json`, or its `snapshotVersion` is not `1`, `2` or `3` (a version 1 snapshot has no `history` and no `spawned`, a version 2 one has `history`, a version 3 one both). |
| `machine` | The IR has no machine named `machine`, or it is not the machine restore was asked to restore (TypeScript `OrderLive.restore` restores only `Order`). |
| `ir` | Its `ir` is not the machine's hash in the IR and no migration was given, or the migration's result has another hash too. |
| `migration` | The migration failed: it threw (anything at all), panicked or returned an error. (Its result is checked from the top, as a snapshot of the machine being restored, the one the old snapshot names when restore was not asked for one: a result that is not a snapshot is `invalid`, one of another machine `machine`, and one that still has another hash `ir`.) |
| `ended` | Its `status` is not `running`. A run that ended with a defect is `ended` whatever its defect's `kind`: the schema requires only that it has one, so a kind this version does not name is not `invalid`. |
| `ref` | The machine has a parameter of type `ref M`, `ref M?` or `ref M[]`, or an alias of one ([not yet specified](#not-yet-specified)). |
| `calls` | Its `calls` is not empty. The callers that wait for those calls belong to the process that saved it, and are gone. A run with pending calls is not idle, so suspend never gives such a snapshot; save can. |
| `invalid` | It does not fit the machine: its `status` is `running` but its `result` is not `null` (a run has a result only once it has ended); `active` is not a configuration in document order (the root first, each other state's parent active, one active child for each active compound state, every child for each active parallel state, and no history state); an `invokes` entry's state is not active, has no invoke of `src`, or has two entries; a `timers` entry's state is not active, or its `index` is not the first transition of an `after` block of that state, or two entries have the same state and index; a `spawned` entry's `children` is not a collection of the machine, its `key` does not decode as the collection's key type, its `args` do not have exactly the parameters of the collection's machine or do not decode as their types, or two entries of one collection have the same key; an id is repeated (among `invokes` and `spawned` together, or among `timers`) or not below `nextInvoke` or `nextTimer`; `input` does not have exactly the machine's parameters, or `vars` has a name that is not one of its variables, or lacks an optional variable; a `history` key is not the id of one of its history states, or its value is not one that history state could have recorded (for a `history` state, one child of a compound parent or every child of a parallel parent; for a `deepHistory` state, the leaves of a configuration below its parent: atomic or final states which, with their ancestors up to and including the parent, have one child for each compound state and every child for each parallel state among them; in document order either way); or a value does not decode as its declared type, as a machine's arguments must. A record, an error or an event decodes only with exactly the members its type declares (and an error's or event's `_tag`): one with a member its type does not declare is `invalid`. |

**Migration.** When the hashes differ, the host may give a **migration**: a function from the old saved snapshot to a new one, both plain JSON (TypeScript `(saved: Json) => Json`, Rust `Fn(serde_json::Value) -> Result<serde_json::Value, String>`). It is called once, and only when the hashes differ. Its result is checked from the top as if it had been given instead, except that a second migration is never applied. It is how a deployment that changed a chart (renamed a variable, added a state) keeps its saved runs: the migration rewrites `ir`, the names, and whatever else must change. It needs no IR of the old chart; it sees only the data.

**The new run.** The restored run is a new top-level run with an id of its driver's choosing ([run ids](#the-driver)), not the `run` in the snapshot, which named it in the process that saved it. It has no peers. The driver opens it and calls the core:

```
restore(ir, saved, run, now, migrated, fns):
  m    = ir.machines[saved.machine]           # every check above has passed
  snap = saved without snapshotVersion, with run = run, no due on the timers, calls = [],
         history = {} if it has none (version 1), and spawned = [] if it has none (versions 1 and 2)
  emit trace { kind: "restored", machine: saved.machine, ir: m.hash, migrated, snapshot: saved }
  for ch in snap.spawned, in order:
    src = the machine of collection ch.children
    emit trace { kind: "spawn.started", spawn: ch.id, children: ch.children, key: ch.key, src, args: ch.args }
    emit spawn.start { spawn: ch.id, src, args: ch.args }
  for s in snap.active, in document order:
    if snap.invokes has an entry inv with state s.id:
      to, params and args as in startWork (never a call: a caller holds a ref, so is refused)
      emit trace { kind: "invoke.started", invoke: inv.id, state: s.id, src: inv.src, args }
      emit invoke.start { invoke: inv.id, state: s.id, src: s.invoke.src, srcKind: s.invoke.kind,
                          to, as: s.invoke.as, args }
    for each entry tm of saved.timers with state s.id, by ascending id, if tm.due > now:
      emit trace { kind: "timer.started", timer: tm.id, state: s.id, index: tm.index, delay: tm.due - now }
      emit timer.start { timer: tm.id, delay: tm.due - now }
  for each entry tm of saved.timers with tm.due <= now, by ascending due, then ascending id:
    emit timer.fire { timer: tm.id }
  return (snap, commands)
```

`now` is the driver's clock, `run` the new run's id, and `migrated` the hash the snapshot had before a migration, or `null` when there was none. Each timer of the core snapshot keeps the `delay` it was saved with, the delay its `after` evaluated to, though a restarted timer's `timer.started` record and command give the time that remains. `snapshot` in the `restored` record is the saved snapshot as restored, after any migration.

- **The spawned children restart.** Each child in `spawned` is started again, with its id and key and the arguments it was spawned with (they were evaluated from the event that spawned it, which is gone, so they are kept), as a new child run `<new run id>.<id>` that starts from the beginning, before the states' work, as in [starting work](#starting-work). A child that had ended when the run was saved, but whose outcome had not been taken yet, is still in `spawned` and runs again, as an invoke in that position does.
- **The work that was running restarts.** Each invoke in `invokes` is started again, with the same id and with its arguments evaluated now, from the snapshot's variables (a function in them is called again). A task runs again from the start, so a machine meant to be saved should wait in states with no `invoke`, or use tasks that are safe to repeat ([semantics](../docs/design/semantics.md#saving-and-restoring)). A child machine runs again from its `start`, as a new child run `<new run id>.<invoke id>`: its own progress, and the messages its parent had sent it, were not saved. An invoke whose outcome had already been taken is not in `invokes` and is not restarted, though its state is active.
- **Timers keep their deadlines.** A timer not yet due is started again, with the same id, for the time that remains, so it expires at its saved `due`. A timer already due is not started: `timer.fire` queues its expiry at once, before anything else can reach the run's queue, the earliest due first. Its deadline passed while the run was saved, so it is taken before any event given after restoring: an order restored after its payment deadline expires before it sees the payment. Since ids are kept, `nextInvoke` and `nextTimer` do not change; they cannot collide with anything, since the run id is new.
- **Nothing else happens.** No state is entered or exited, no effect runs and the run does not settle: the snapshot was saved between steps, already settled. The run then takes inputs like any run.
- **Defects.** A runtime error or a host defect while evaluating an invoke's arguments makes `restore` atomic, like `start`: it returns `failWithDefect(d)` applied to `snap`, with two commands, the `restored` record and the `defect` record. The `invoke.cancel`, `spawn.stop` and `timer.cancel` commands of `failWithDefect` are for work never started (the `spawn.start` commands before the defect are discarded with the rest), which the driver ignores. The run then ends with the defect like any run.
- **The driver** carries out the commands and drives the run as `run` does ([the driver](#the-driver)). The due times it keeps are `now + delay` for the timers it starts, which is their saved `due`, and the saved `due` for those whose expiry `timer.fire` queued.

### Suspending and resume

A run is **idle** when it is between steps, its queue is empty, no cancel request is waiting, it has no running invoke and no running spawned child (`snap.invokes` and `snap.spawned` are empty), and it has no pending call (`snap.calls` is empty): it waits only for events, messages and timers, nothing it started is in flight, and no caller waits for it. A run that has made a call has a running invoke, so it is not idle either, and a parent is suspended only once its spawned children have ended or been stopped.

**suspend** waits until the run is idle or has ended, taking inputs meanwhile as always (a cancel request among them: one that arrives while a step runs is taken after that step, so a run asked to cancel and then to suspend is cancelled, exit effects and all, and suspend gives its `cancelled` snapshot): the outcomes of its invokes and the expiries of its timers, which keep running, so a chart's own `after` bounds how long it waits for a task. The driver checks when suspend is called, and after each step's commands (or `start`'s, or `restore`'s) have been carried out. Then:

- If the run is idle, the driver **stops** it: it closes the run ([delivery](#delivering-messages)), deciding that it is idle and closing it as one operation with respect to what is pushed onto its queue, so that an input that arrived while it was open is taken (the run is then not idle), never discarded; cancels its timers through its timer (with no record: they are in the snapshot), discards its queue (empty), writes `{"kind": "suspended"}`, and gives the saved snapshot. Its status is still `running`: the run is not over, it goes on from the snapshot when that is restored. No exit effect runs.
- If the run has ended with `done`, `failed` or `cancelled`, the driver gives its saved snapshot, whose `status` and `result` say so, and writes nothing. A run that ended with a defect raises it, as `run` does, and gives no snapshot.

A timer that falls due while suspend waits is taken like any input; one still running when the run stops is in the snapshot. An event given to the run while suspend waits is taken if it arrives before the run stops.

**resume**`(saved, event?, migration?)` restores the snapshot, gives the run `event` if there is one, suspends it, and gives the saved snapshot that suspend gives. Expiries that `timer.fire` queued are taken before the event. Both inputs are checked before anything runs or is written: first the snapshot, as restore checks it (a refusal), then the event, which must be one of the events the machine accepts, with exactly its fields, each of its type, as an event given to a run must encode. An event that is not is the caller's error, as for an event host code gives a running run that does not encode: a host defect raised to the caller (TypeScript dies with it, Rust resumes unwinding with it); no run starts, nothing is traced and no run id is used. This is the request handler's operation: it awaits the tasks the event starts (so a snapshot resume gives has no running invokes) and saves the timers with their deadlines. It also waits for every spawned child to end, the restarted ones included, so it does not return for a machine whose children live on (a hub of sessions); such a machine is kept running, not resumed per request. A defect is raised; a refused snapshot is an error of its own, before any run starts ([TypeScript](../docs/design/typescript.md#saving-and-restoring), [Rust](../docs/design/rust.md#saving-and-restoring)).

## Not yet specified

The formats leave room for these; their rules will be added here.

- **Restoring a run that holds `ref`s.** Its `input` names peers by the run ids of the process that saved it. Restoring it needs host code to give a handle for each of those ids, and the binding layer to rewrite them in `input` and `vars` by their types; until then such a snapshot is refused (`ref`).
- **Saving a child run**, or a parent together with the state of its children, which is durable execution.
- **Host code making a call** through a handle, and a call to a child run (invoked with `as`, or spawned) rather than to a peer.
