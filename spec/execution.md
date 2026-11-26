# Execution

This page is the algorithm both runtimes implement: what the pure core does with a snapshot and an input, what it asks the driver to do, and how the driver carries that out. It is precise enough that two independent implementations produce the same [trace](trace.md) byte for byte, and the [conformance cases](conformance/) hold them to it. The design rationale is in [semantics](../docs/design/semantics.md); this page only says what happens.

It covers atomic, compound, parallel and final states, events, guards, `invoke` of tasks and machines, `after`, `always` and `done`, [messages between runs](#messages-between-runs) (`send`, `emit`, `on name.Event` and `ref`, milestone 5), [saving and restoring](#saving-and-restoring) a run (milestone 6), and [history states](#history-states). Parallel states and history states change nothing for a machine that has none: every rule below reduces to the one it replaced, and the traces of such machines are unchanged.

## Terms

- A **run** is one instance of a machine, from `start` until it finishes. A run's **status** is `running`, `done`, `failed`, `cancelled` or `defect`; only `running` is not final.
- The **active states** are the states the run is in. They form a tree from the root (the machine itself, a compound state whose id is the machine's name): an active compound state has exactly one active child, and an active parallel state has all its children active. Without parallel states the tree is a chain from the root down to one atomic or final state. The root is active for the whole run.
- A **history state** is a state of kind `history` (shallow) or `deepHistory` (deep): a pseudo-state in the `states` of a compound or parallel state other than the root, its **parent**. It is never active and has no children, effects, work or transitions; it is only ever a transition's target ([history states](#history-states)). On this page the **children** of a state are the states in its `states` that are not history states, so a history state is never a region, an initial state, an active state or an active leaf.
- A **region** is a child of a parallel state. A region may be a state of any kind: compound, parallel, atomic or final.
- An **active leaf** is an active state with no active child: an atomic or final state. Without parallel states there is exactly one.
- **Document order** is the order of a pre-order walk of the IR's state tree: a state comes before its children, and children are in `states` order. **Innermost first** means reverse document order; for a chain it is leaf to root.
- An **input** is what the driver gives the core: an external event, a message or an emitted event from another run, an invoke's outcome, a timer's expiry, or a request to cancel or to fail with a defect.
- A **top-level run** is one that host code started (`run`, `spawn`), as opposed to a **child run**, which an `invoke` of its parent started ([child runs](#child-runs)). A **peer** of a run is a top-level run it holds a `ref` to.
- A **message** is an event one run sends to another with `send`. An **emitted event** is one a run emits with `emit`, for its invoker and the runs that hold a `ref` to it. Both are queued for the receiver like any input ([messages between runs](#messages-between-runs)).
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
| `invokes` | `[{"id", "state", "src"}]`: the invokes that are running, in the order they were started. `state` is the id of the state that owns it, `src` the task's or machine's name. |
| `timers` | `[{"id", "state", "index", "delay"}]`: the `after` timers that are running, in the order they were started. `index` is the position in the state's `transitions` of the first transition of the timer's `after` block, and `delay` the evaluated delay in milliseconds. |
| `nextInvoke`, `nextTimer` | The id the next invoke or timer will get. Both start at 1. Ids are never reused within a run, which is what lets the core recognise a stale outcome. |
| `nextMessage` | The id the next `send` or `emit` effect will get, from 1. A message is named in traces by its sender's run id and this id. |
| `history` | What each history state has recorded ([history states](#history-states)), keyed by the history state's id: a list of state ids, in document order. A history state whose parent no microstep has exited yet has no member. Starts as `{}`. |
| `result` | `null` while running and when cancelled. `{"output": value}` when done (`value` is `null` for a machine that returns nothing), `{"error": value}` when failed, `{"defect": defect}` when the run ended with a [defect](#defects). |

A **saved snapshot**, the form host code stores, is this snapshot with a format version and each timer's due time added ([saving and restoring](#saving-and-restoring)).

Within a macrostep the core also keeps two transient values that are not part of the snapshot: the set of states **entered during this macrostep**, and the **completion queue** of compound states that reached a final child and have not been exited since.

### Inputs

| Input | Meaning |
| --- | --- |
| `{"kind": "event", "value": {"_tag": "Cancel", …}}` | An external event: one of the machine's `events`, encoded as a tagged record (see [values](#values)), given by host code. |
| `{"kind": "message", "from": "r2", "message": 1, "value": {"_tag": "Reserve", …}}` | A message: run `r2` sent one of this machine's `events` with `send`, as its message 1. |
| `{"kind": "emitted", "from": "r1.3", "invoke": 3, "message": 2, "value": {"_tag": "Progress", …}}` | An emitted event: run `r1.3` emitted one of its machine's `emits` as its message 2. `invoke` is the id of this run's invoke that started `r1.3` when this run is its invoker, and `null` when this run holds a `ref` to it. |
| `{"kind": "invoke.done", "invoke": 1, "value": v}` | Invoke 1 succeeded with `v` (`null` for a task that returns nothing). |
| `{"kind": "invoke.fail", "invoke": 1, "error": e}` | Invoke 1 failed with the declared error `e`, a tagged record. |
| `{"kind": "invoke.defect", "invoke": 1, "defect": d}` | Invoke 1 ended with a [defect](#defects): a host defect in a task, or a child run that ended with a defect. |
| `{"kind": "after", "timer": 1}` | Timer 1 expired. |
| `{"kind": "cancel"}` | Cancel the run from outside. |
| `{"kind": "defect", "defect": d}` | The driver met a defect while carrying out this run's commands: a host defect in an action, or a defect in the cancel step of a child run it was cancelling ([child runs](#child-runs)). Unlike every other input, it is taken even when the run has already ended, unless it ended with a defect. |

The driver only ever passes events the machine declares, with values of the declared types. The core may assume its inputs are well formed.

### Commands

| Command | The driver… |
| --- | --- |
| `{"kind": "trace", "record": r}` | writes trace record `r`, after adding `t`, `run` and, for a child run, `parent` ([trace](trace.md)). |
| `{"kind": "action", "action": "audit", "args": {…}}` | runs the action and waits for it to finish. `args` is keyed by parameter name. |
| `{"kind": "invoke.start", "invoke": 1, "state": "…", "src": "chargeCard", "srcKind": "task", "as": null, "args": {…}}` | starts the task (`srcKind` `task`) or machine (`srcKind` `machine`) and, when it ends, queues its outcome as an `invoke.*` input with the same id. It does not wait for it. `as` is the invoke's `as` name or `null`: a child run invoked with one delivers what it emits to this run ([delivery](#delivering-messages)). |
| `{"kind": "invoke.cancel", "invoke": 1}` | cancels invoke 1 and waits until it has stopped: at once for a task, after its cancel step for a [child run](#child-runs). Its outcome is not queued after this; one already queued stays queued. |
| `{"kind": "timer.start", "timer": 1, "delay": 200}` | starts a timer that queues `{"kind": "after", "timer": 1}` after `delay` milliseconds. |
| `{"kind": "timer.cancel", "timer": 1}` | cancels timer 1. Its expiry is not queued after this; one already queued stays queued. |
| `{"kind": "timer.fire", "timer": 1}` | queues `{"kind": "after", "timer": 1}` on this run's queue at once, without starting a timer. Only `restore` emits it, for a restored timer that was already due ([restoring](#restoring-a-snapshot)). |
| `{"kind": "send", "to": "r2", "invoke": null, "message": 1, "value": v}` | queues the message for run `to` and does not wait for it: for the child run of this run's invoke `invoke`, or, when `invoke` is `null`, for the peer `to` ([delivery](#delivering-messages)). |
| `{"kind": "emit", "message": 2, "value": v}` | queues the emitted event for this run's invoker, if it invoked this run with `as`, and for every run that holds a `ref` to this run. It does not wait for them. |

A trace record is always emitted immediately before the command it describes. Records are listed in [trace.md](trace.md). Two records are written by the driver instead of the core: `undelivered`, when it finds that the receiver of a `send` has ended ([delivery](#delivering-messages)), and `suspended`, when it stops a run it has saved ([suspending](#suspending-and-resume)).

## Starting a machine

```
start(ir, name, run, input, fns):
  m    = ir.machines[name]
  snap = { machine: name, run, ir: m.hash, status: "running", active: [], input, vars: {},
           invokes: [], timers: [], nextInvoke: 1, nextTimer: 1, nextMessage: 1, history: {},
           result: null }
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
    "after":
      tm = the entry of snap.timers with id input.timer
      if none: emit trace { kind: "stale", event: input }; return
      remove tm from snap.timers
      T = selectAfter(tm)
    "event", "message":
      T = selectEvent(input.value)
    "emitted":
      if input.invoke != null and snap.invokes has no entry with id input.invoke:
        emit trace { kind: "stale", event: input }; return
      T = selectEmitted(input)
  if T is empty:
    emit trace { kind: "dropped", event: input }
    return
  emit trace { kind: "event", event: input }
  microstep(T)
  settle()
  if snap.status == "running": startWork()
```

- An invoke's outcome or a timer's expiry **consumes** it: it is removed from the snapshot before a transition is chosen, whether or not one is. Leaving the state afterwards therefore does not cancel it again.
- An outcome or expiry whose id is not in the snapshot is **stale**: its state was left (and the invoke or timer cancelled) after the driver had already queued it. It is traced as `stale` and changes nothing.
- An emitted event from a child run whose invoke is no longer in the snapshot is **stale** too: its invoke was cancelled after the child had emitted it, as a child cancelled by its parent does when its exit effects `emit`. Taking an emitted event does not consume the invoke. An emitted event from a peer (`invoke` `null`) and a message are never stale.
- An input that enables no transition is **dropped**: traced as `dropped`, and nothing else happens. Settling is skipped, since nothing changed. A consumed invoke or timer stays consumed.
- A `dropped` or `stale` record replaces the `event` record; an input is traced by exactly one of the three.
- **A defect in the step that ended the run.** The core finishes the run (or cancels it) inside the step, so the snapshot a step returns can already be `done`, `failed` or `cancelled` while the driver is still carrying out that step's commands, every action of the step among them: say the effects of the transition into a top-level final state, the exit effects `finish` runs (the root's among them), or the exit effects of `cancelRun`. If any of those actions has a host defect, the driver stops at it as for any action ([defects](#defects)), so the `done`, `failed` or `cancelled` record after it is never written, and gives the core the `defect` input. That is why a `defect` input is taken in any status but `defect`: the run's status becomes `defect`, its result the defect, and the `defect` record is its last. `failWithDefect` finds no invokes or timers left to cancel, and `active` stays `[]`. A second `defect` input, for a run that already ended with one, changes nothing and writes nothing.

## Selecting a transition

A transition is **enabled** if its trigger matches the input and its guard holds. A `null` guard holds. Selection gives the list `T` of transitions the input takes, in order: empty when it enables none, one transition, or, with parallel states, one per region that takes the input. Guards are evaluated in the order given here, and evaluation stops at the first enabled transition of each search, so a guard after it is never evaluated (which matters because a guard can call host functions).

### Searching from the active leaves

An external event, a message, an emitted event and `always` are selected by **walks**. Each state has **candidates** for the input, given below, tried in document order; the state **offers** the first candidate whose guard holds, or nothing.

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

`T = selectByWalks(those candidates)`. There is no fallback to `on *`. So `on upload.Progress` is taken only for a `Progress` from the child that `upload` names, and from the run that started it, not from an earlier child of the same state: a child restarted by re-entering its state has a new invoke id.

**`always`**: the candidates of `s` are its `always` transitions. `T = selectByWalks(those candidates)`, used by [settling](#settling).

The other inputs search one state and take at most one transition, so `T` is empty or holds the first enabled candidate:

**An invoke outcome** for invoke `inv`: only the state `inv.state` is searched. For `invoke.done` the candidates are its `invoke.done` transitions. For `invoke.fail` with error tag `E`, they are its `invoke.fail` transitions with `error` `E`, or, if it has none, those with `error` `"*"`. Guards are tried in document order.

**A timer expiry** for timer `tm`: the candidates are the transitions of `tm.state` with trigger `after` and the same `group` as the transition at `tm.index`, in document order.

**`done`** of state `p` (compound or parallel): `p`'s `done` transitions in document order.

`event` is in scope in the guard and effects of an `on` transition (including `on *` and `on name.Event`), `result` in an `invoke.done` transition and `error` in an `invoke.fail` transition, bound to the input's `value` or `error`. For an emitted event `event` has the type `Machine.Name` of the sender's event ([type references](ir.md#type-references)), and its value is the tagged record the sender emitted. Whether a chart may read `event` in an `on *` is the compiler's business. The implicit names are reserved and parameter and variable names are unique, so a name in a `ref` means exactly one thing.

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
  for e in effects with kind "do", "send" or "emit", in order:
    switch e.kind:
      "do":
        args = the action's parameter names zipped with eval of e.args, left to right
        emit trace { kind: "action", action: e.action, args }
        emit action { action: e.action, args }
      "send":
        id = snap.nextMessage; snap.nextMessage += 1
        if e.to.kind == "child":
          inv = the entry of snap.invokes with state e.to.state
          v = eval(e.value)
          if inv == null:
            emit trace { kind: "undelivered", message: id, to: null, value: v }
            continue
          to = snap.run + "." + inv.id; invoke = inv.id
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
```

So a state's work stops before its own exit effects run, and the `exited` record comes last. All `set` effects of a list run before its other effects, whatever order the IR lists them in, and each `set` sees the assignments before it. Then the `do`, `send` and `emit` effects run in the order the IR lists them. Actions, sends and emits cannot change variables, so every one of them in a list sees the same values, and a message carries what its transition assigned.

- **Message ids.** Every `send` and `emit` effect that runs takes the next message id, also one that is undelivered. With the run id it names the message in every record about it: the sender's `sent`, `emitted` or `undelivered`, and the receiver's `event`, `dropped` or `stale`.
- **A send to a child** goes to the run started by the invoke of the address's `state` that is running now. If that state has no running invoke, the message is **undelivered**: traced with `to` `null`, and nothing is sent. This happens when the send runs in the step that enters the invoking state, before its work starts (the `entry` of a state inside it, say: the compiler rejects only the invoking state's own `entry`), and when the invoke's outcome has already been taken while the state stays active (an outcome transition with no target, or an outcome that enabled no transition).
- **A send to a peer** reads the `ref` parameter or variable the address names, exactly as an expression `ref` would, so a variable with no value is an `unassigned` defect. The address is read before the value is evaluated.
- **Delivery is the driver's.** The core does not know whether the receiver is still running. If it is not, the driver writes `undelivered` after the `sent` record ([delivery](#delivering-messages)).

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

After settling, if the run is still running, the core starts the invokes and timers of every state that was **entered during this macrostep and is still active**, in document order (outermost first):

```
startWork():
  for s in active, in document order, if s was entered during this macrostep:
    if s.invoke != null:
      id = snap.nextInvoke; snap.nextInvoke += 1
      args = the parameter names of s.invoke.src zipped with eval of s.invoke.args, left to right
      append { id, state: s.id, src: s.invoke.src } to snap.invokes
      emit trace { kind: "invoke.started", invoke: id, state: s.id, src: s.invoke.src, args }
      emit invoke.start { invoke: id, state: s.id, src: s.invoke.src, srcKind: s.invoke.kind,
                          as: s.invoke.as, args }
    for each after block of s, in document order of its first transition (index i):
      delay = eval(s.transitions[i].trigger.delay)
      id = snap.nextTimer; snap.nextTimer += 1
      append { id, state: s.id, index: i, delay } to snap.timers
      emit trace { kind: "timer.started", timer: id, state: s.id, index: i, delay }
      emit timer.start { timer: id, delay }
```

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
  snap.result = status == "done" ? { output: value } : { error: value }
  snap.status = status
  clear the completion queue
  emit trace status == "done" ? { kind: "done", state: f.id, output: value }
                              : { kind: "failed", state: f.id, error: value }
```

The output or error is evaluated right after `f`'s entry effects, before any exit effect runs. Then every active state is exited, innermost first, with its exit effects, as on any exit (but no history is recorded: the run is over): this is when the root's `exit` effects run, and when timers on the root are cancelled. Settling stops and no work is started. The run's result is the output (an Effect success, a Rust `Ok`) or the error (the Effect error channel, a Rust `Err` of the machine's error enum).

## Cancelling from outside

The input `{"kind": "cancel"}` exits every active state, innermost first (in reverse document order, so the later regions of a parallel state first), exactly as `exitState` does: each state's invoke and timers are cancelled and its exit effects run. No history is recorded, and `history` keeps what it had. Then the status becomes `cancelled` and a `cancelled` record is written. There is no `event` record for a cancel.

```
cancelRun():
  for s in active, innermost first: exitState(s)
  snap.status = "cancelled"
  emit trace { kind: "cancelled" }
```

This is the default in [open question 4](../docs/design/roadmap.md#open-questions). A parent leaving an invoking state cancels its child run this way, so the child's exit effects run too, and the parent waits for them ([child runs](#child-runs)). The cancel step is the run's last; a host defect in one of its exit actions turns it into a defect ([taking an input](#taking-an-input)).

## Messages between runs

A run talks to another only through queues. The core's part is small: a `send` or `emit` effect writes a record and gives the driver a `send` or `emit` command ([effects](#exiting-and-entering-a-state)), and a message or an emitted event comes back to the receiver's core as an input like any other ([taking an input](#taking-an-input), [selecting](#selecting-a-transition)). The driver does the delivering ([delivering messages](#delivering-messages)).

- **Sending is queuing.** The sender's driver pushes the input onto the receiver's queue and carries on with the next command. A sender never waits for the receiver and never runs the receiver's transitions; the receiver takes the input later, one at a time, like any other.
- **Who receives a `send`.** For an address of kind `child`, the child run of the invoke that is running in the address's `state`. For an address of kind `ref`, the run whose id the parameter or variable holds.
- **Who receives an `emit`.** The run's invoker, if it invoked the run with `as`; an invoker without `as` has no name to receive them under, and they are not delivered to it. And every run that holds a `ref` to the emitting run ([refs and handles](#refs-and-handles)), whether or not it has an `on` for the event: one with no enabled transition writes `dropped`. A run never receives its own emitted events.
- **A run holds a `ref`** to each run whose handle it was given as an argument: the value of a parameter whose type is `ref M`, or contains one (`ref M?`, `ref M[]`, an alias of one). A machine gets a `ref` only as an argument (from host code, or from a parent that passes one of its own to an invoke), never from a result, an event or a field, so the runs it holds are fixed when it starts. A `ref` variable holds one of them.
- **Messages are not supervised.** When a run ends, the runs that hold a `ref` to it are not told; they learn of it only through their own timeouts ([open question 4](../docs/design/actors.md#open-questions)). Messages sent to it afterwards are `undelivered`.
- **Ending a run.** The exit effects that run when a run finishes or is cancelled are effects like any others, and can send and emit. Whatever is still queued for a run when it ends, messages and emitted events included, is discarded without a record ([ending a run](#ending-a-run)).
- **Defects.** A runtime error inside the core discards every command of the step, so nothing the step sent or emitted is delivered and no record of it is written; the snapshot the defect is applied to keeps its `nextMessage`. A host defect in an action stops the step's commands, so the sends and emits after it are neither delivered nor traced. `failWithDefect` runs no exit effects, so it sends and emits nothing itself; a child run it cancels runs its own cancel step, whose emitted events find the parent already ended.

## Defects

A defect ends the run at once. It is never routed to a `fail` branch, and no exit effect runs, since the machine can no longer be trusted to run its own code. A defect is an object:

| Defect | Cause |
| --- | --- |
| `{"kind": "host", "message": "…"}` | Host code failed: a function, action or task threw or panicked, or returned a value that is not of its declared type (a negative `duration`, say). `message` is set as described under [catching host defects](#catching-host-defects); in conformance cases it is given by the script. |
| `{"kind": "unassigned", "var": "receipt"}` | A variable was read before it had a value. Definite assignment analysis (milestone 6) will make this a compile error. |
| `{"kind": "overflow", "expr": "attempts + 1"}` | Integer arithmetic left the `int` range, or float arithmetic produced a value that is not finite. `expr` is the `text` of the operator's IR node. |
| `{"kind": "undeclared", "error": "Oops"}` | A task or child machine failed with an error it does not declare. |
| `{"kind": "limit"}` | Settling took more than 1000 microsteps. |

**Inside the core.** Runtime errors (`unassigned`, `overflow`, `undeclared`, `limit`) and a `host` defect returned by `fns` while evaluating an expression happen inside `start` or `step`. The call is then **atomic**: every command it had produced is discarded, and it returns `failWithDefect(d)` applied to the snapshot it was given, so even an invoke the failed step had consumed is cancelled again (a driver ignores that). There is no `event` record for the input.

**A defect in `start`** works the same way, applied to the initial snapshot (`active` `[]`, `vars` `{}`, no invokes or timers): `start` still returns a snapshot, with status `defect`, and two commands, the `started` record and the `defect` record. The `started` record is kept so the trace still says what the run was. The driver then ends the run like any other that ends with a defect: a root run raises the defect to whoever called `run`, a child run reports it to its parent as `invoke.defect`. A host defect in an action of `start`'s commands (a root `entry` effect, say) is an ordinary action defect, below.

**A host defect in an action** is found by the driver while carrying out a step's commands (or `start`'s). It stops carrying them out, writes none of the remaining records, and gives the core `{"kind": "defect", "defect": {"kind": "host", "message": …}}`, also when the step had already ended the run ([taking an input](#taking-an-input)). **A host defect in a task** arrives as `invoke.defect`, and is ignored as stale if that invoke was already cancelled. **A child run that ends with a defect** is reported to its parent the same way, with the child's defect object unchanged, so the parent's run ends with a defect too and the defect travels up to whoever ran the root. **A defect in a child's cancel step**, while its parent carries out `invoke.cancel`, is met by the parent's driver like a defect in an action: it gives the parent `{"kind": "defect", "defect": d}` with the child's defect `d` unchanged ([child runs](#child-runs)).

```
failWithDefect(d):
  for inv in snap.invokes: emit invoke.cancel { invoke: inv.id }    # no trace records
  for tm in snap.timers:   emit timer.cancel { timer: tm.id }
  snap.invokes = []; snap.timers = []
  snap.status = "defect"; snap.result = { defect: d }
  emit trace { kind: "defect", defect: d }
```

Cancelling a child run here is an ordinary [cancellation](#cancelling-from-outside) of the child, which runs the child's exit effects, and the driver waits for it as for any `invoke.cancel`. A defect in that cancel step is written to the child's trace and changes nothing in the parent, which has already ended: the parent's driver ignores it and goes on with the remaining cancels and the `defect` record ([the driver](#the-driver)). A driver ignores an `invoke.cancel` or `timer.cancel` for an id that has already ended, or whose start it never carried out (because a step was cut short by an action's defect). After writing the record the driver raises the defect to whoever ran the run, as the host language does: `Effect.die` with a `Defect` value holding the defect object in TypeScript, and `std::panic::resume_unwind` with a `roadbook::Defect` payload in Rust.

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
| `ref` | `path[0]` is a parameter (from `input`), a variable (from `vars`; reading one with no value is `unassigned`), or the implicit name `event`, `result` or `error`. Each further name reads that field of the value. A parameter or variable of a `ref` type reads as the run id it holds. |
| `field` | Evaluate `of`, then read field `name`. |
| `call` | Evaluate `args` left to right, then `fns(fn, args)`. |
| `record` | An object with the fields in order. If `type` is an error, it also has `"_tag": type`. If `type` is an event (the value of a `send` or `emit`: `Machine.Name`, or the name of one of the current machine's `emits`), it also has `"_tag"` with the event's name, the part after the dot: `send upload.Pause` sends `{"_tag": "Pause"}`. |
| `enum` | The string `variant`. |
| `op` | See below. |

Operators, by `op`. An operand's type is its IR `type` with aliases expanded, transitively: `<` on a `Money` after `type Money = int` compares integers. The IR keeps the alias's name, so a runtime that dispatches on the type string must expand it first.

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
| An alias | The aliased type's form. |
| An `extern` type | Whatever the language binding's encoder produces. The core never looks inside it. Conformance cases do not use extern types. |
| `ref M` | The id of the run it refers to, a string: `"r2"`. The binding layer gives host code a handle to that run instead ([refs](#refs-and-handles)). |

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

Every source of input pushes onto one first-in, first-out queue per run, at the moment the input arrives:

| Source | Pushes |
| --- | --- |
| A caller's `send` | the external event |
| Another run's `send` | a `message` input ([delivery](#delivering-messages)) |
| Another run's `emit` | an `emitted` input, for its invoker and the runs that hold a `ref` to it |
| A task, or a machine invoke that is not a child run | its outcome, when its future or fiber ends |
| A child run | the outcome of the invoke that started it, when the child ends |
| A timer | its expiry, when its sleep completes |

The run takes inputs from the front, one at a time, and takes the next only once the previous step's commands have all been carried out. A cancel request is the one exception: it is not queued, and is taken before anything that is.

- **Nothing reaches the core except through the queue.** An Effect driver can make the queue an Effect `Queue` that senders, invoke fibers and timer fibers offer to. A Rust driver's loop `select`s over the event channel, the set of invoke futures and the timers only to find out what has arrived: it pushes every input it finds onto the run's queue (a `VecDeque`, say) and gives the core the front of that queue, never an input straight from the source that produced it. So an outcome that completes while the run is busy, or in the same poll as an event, waits behind what arrived before it, and may be stale by the time it is taken. Inputs that one poll finds ready together are pushed in an order the driver chooses; nothing outside conformance depends on that order, and a conformance runner never lets it arise.
- **Several inputs at once.** A driver lets a caller append a list of inputs to a run's queue in one operation, so that the run takes no step between them: `Queue.offerAll` on the run's queue in Effect, and in Rust a method that pushes them all onto the queue while the run is between steps. A conformance runner delivers a `together` step this way ([conformance](conformance/README.md#how-a-runner-works)).
- **An invoke is forgotten once its outcome is taken.** When the core takes an invoke's outcome (whether it writes `event`, `dropped` or `stale`), the driver drops that invoke's future or interrupts its fiber if it is still there. Normally it has already ended; it has not when its outcome was appended by a caller, as a conformance runner does for a scripted invoke in a `together`, and dropping it then ensures it never queues a second outcome.
- **Cancelled work queues nothing.** Once the driver has carried out `invoke.cancel` or `timer.cancel`, the invoke or timer has been dropped or interrupted and pushes nothing more. An outcome or expiry it pushed before stays queued and reaches the core, which writes it as `stale`.
- **Cancellation takes effect between steps.** A cancel request does not interrupt the commands of a step in progress. When they are done the driver gives the core `{"kind": "cancel"}`, carries out the resulting commands, and discards anything still queued.

### Child runs

A machine invoke's implementation is host code: in TypeScript the value given for it in `machines`, in Rust a `Handlers` method whose body usually calls the child machine's `run`. When that code runs the chart, the driver has to know, so that the child writes to the parent's trace on the parent's clock and is cancelled gracefully. Both drivers do it with an **invoke context**.

- **The context.** When the driver carries out `invoke.start` with `srcKind` `machine`, it runs the implementation with an ambient invoke context holding the trace sink, the clock and timer, the parent's run id, the invoke id, a fresh cancel token, the messages the parent has sent to the invoke so far (none yet), and, when the command's `as` is not `null`, a way to push emitted events onto the parent's queue. In TypeScript the context is a `Context.Reference` provided to the invoke's fiber. In Rust it is a task-local, or a thread-local that the driver sets around each `poll` of the invoke's future and restores afterwards, which works on any executor. All other host code the driver runs (functions, actions, tasks) runs with no invoke context; so does a child run's own work, except that each of its machine invokes gets a context of its own.
- **Claiming it.** A machine's `run` looks for an invoke context. If it finds one whose token is unclaimed, it claims the token and becomes the **child run**: its run id is `<parent run id>.<invoke id>`, its records carry `parent`, and it uses the context's sink, clock and timer. Its queue starts with the messages the context holds, in the order they were sent, and from then on the parent's sends go straight to its queue. Otherwise it is a root run. Host code that does not call `run` (a stub) never claims the token, which is how the parent's driver tells a stub from a child run. The parent's core cannot tell them apart and does not need to.
- **Its outcome.** A child run ends like any run: on `done` its `run` returns the output, on `failed` the error, and on `defect` it raises the defect ([catching host defects](#catching-host-defects)). The parent's driver turns that into `invoke.done`, `invoke.fail` or `invoke.defect` as for a task, with the output, error or defect object unchanged, and pushes it onto the parent's queue.
- **Cancelling it.** On `invoke.cancel` the parent's driver signals the token. If the token was never claimed (a task or a stub), it drops the future or interrupts the fiber at once and is done. If it was claimed, the child takes the signal as a cancel request, between its steps like any other, runs its cancel step (exit effects and the `cancelled` record) and reports through the token that it has finished. The parent keeps driving the child until then (in Rust, it keeps polling the child's future), and only then drops it. Carrying out `invoke.cancel` waits for this, like an action, so nothing after it in the parent's step (the exit effects of the state being left, the rest of the transition) happens before the child's exit effects have run. A child that has already ended, however it ended, reports plain "finished" at once (never a defect), and its outcome is not queued after the signal; one already queued is stale.
- **A defect in the cancel step** is reported through the token too: the child writes its `defect` record and ends, and the parent's driver meets that defect while carrying out `invoke.cancel` and gives its own core `{"kind": "defect", "defect": d}`, with the child's `d` unchanged, as it would for a defect in an action. A cancelled task or stub has no cancel step, and whatever it does while being dropped or interrupted is ignored.
- **In Effect** the signal is interruption. The parent interrupts the invoke's fiber and waits for it to end (`FiberMap.remove`, or `Fiber.interrupt`, which returns the fiber's exit). A child's `run` runs each step uninterruptibly and its cancel step in an `Effect.onInterrupt` around the step loop, so interrupting it during a step waits for that step to finish, and the interruption completes only after the cancel step. A defect in the cancel step shows in the fiber's exit as a die with a `Defect`, which the parent uses only if the token was claimed.
- **In Rust** the token is a small shared value (a cancel flag with a waker, and a slot for "finished" or "finished with defect `d`"). The child's run loop checks it before each take, and the parent, after signalling it, polls the child's future until the slot is filled. The child's future may never complete after that; the parent drops it.

### Refs and handles

The core holds a `ref` as a run id ([values](#values)). Host code holds a **handle**: Rust's `Ref` (`inventory::Ref`, from `actor.handle()`), and its TypeScript counterpart, a value from a spawned run's actor. A handle can be cloned and passed on, and it is how host code reaches a run.

- **Encoding.** The binding layer turns a handle into its run's id when it encodes a value for the core (a machine's arguments), and a run id back into the handle when it decodes a value for host code (the arguments of an invoke, an action, a task or a function). A machine can only get a `ref` from its arguments, so every run id its core gives back, in a `send` or in arguments, is one of the handles the run was started with. The driver keeps those for as long as the run is open. It needs no registry of runs by id: a run reaches exactly the runs it was given. What a run needs to emit is the other way round, the runs subscribed to it ([delivery](#delivering-messages)), which its handle can keep.
- **Where handles come from.** A top-level run that host code `spawn`s has one; host code passes it in the arguments of the runs that are to talk to it. A child run has none: its parent reaches it through its invoke, and nothing else can. The tools' handles that a runtime gives for every run as it starts (Rust's `Config::on_run`, TypeScript's `RunObserver`) can queue inputs on a child run, but cannot cancel it or suspend it: a child run ends only by itself or when its parent leaves the invoking state, since its parent learns how it ended only through the invoke. The same holds for a top-level run of `run`, whose caller gets an output or an error, never a cancellation or a snapshot.
- **Host code sends** through a handle as through an actor: the run gets an ordinary `event` input, with no sender. An event that does not encode (a non-finite `float`) is the sender's host defect, never the receiver's: nothing is queued, and the host code that sent it fails (in TypeScript `send` dies with a `Defect`, in Rust it panics with a `roadbook::Defect` payload), so inside an action or a task it ends the run carrying that out with the defect, as any host defect there does. Host code does not receive emitted events through a handle; that is not specified yet.
- **Equality.** The language defines no `==` on `ref`s. The core compares run ids only to tell which address an emitted event is from ([selecting](#selecting-a-transition)).

### Delivering messages

- **Open and closed.** A run is **open** from just before its driver calls `start` (or `restore`) until the core returns a snapshot whose status is not `running`, from `start`, `restore` or any `step`, or until it is [suspended](#suspending-and-resume); it is then **closed** for good. A run closes before the commands of its last step are carried out, so a message that another run sends it while it carries them out is undelivered, and an emitted event is not delivered to it. Checking that a receiver is open and pushing onto its queue should be one operation with respect to its closing; a driver where a push can slip in just before the receiver closes loses that message with the receiver's queue, without a record. No conformance case can tell these apart.
- **Subscriptions.** Before it calls `start`, the driver subscribes the run to the emitted events of every run it holds a `ref` to: each run whose handle is in its arguments, once however many times it appears there (a parameter of type `ref M`, `ref M?`, `ref M[]` or an alias of one). When the run closes, it is unsubscribed.
- **Carrying out `send`.** The driver builds the input `{"kind": "message", "from": <this run's id>, "message": …, "value": …}`.
  - With an `invoke`, it is for the child run of that invoke. If a run has claimed the invoke's token and is open, the driver pushes the input onto its queue. If the token has not been claimed yet (the host code has not called `run` yet), the driver keeps the input in the invoke context for the run that claims it. If the run that claimed it has closed, the driver writes `{"kind": "undelivered", "message": …, "to": …, "value": …}` (adding `t`, `run` and `parent` as for any record). If the invoke ends or is cancelled without its token ever being claimed (a stub, a task-like implementation), the inputs kept for it are discarded without a record.
  - With `invoke` `null`, it is for the peer `to`, one of this run's handles. If that run is open, the driver pushes the input onto its queue; otherwise it writes the `undelivered` record.
- **Carrying out `emit`.** If this run is a child run whose `invoke.start` had an `as`, and its parent is open, the driver pushes `{"kind": "emitted", "from": <this run's id>, "invoke": <its invoke id>, "message": …, "value": …}` onto the parent's queue. For every open run subscribed to this run, it pushes the same with `invoke` `null`. It writes nothing for receivers that are closed, and an emit with no receiver has only its `emitted` record. Pushing to the receivers is not an ordered operation the trace can show: they take their inputs independently.
- **Order per pair.** Everything one run pushes onto another's queue arrives in the order its driver carried out the commands, so two messages from A to B are taken in the order they were sent, and so are an emitted event and a message. A child run's emitted events reach its parent's queue before its outcome, which is pushed when the child's run ends, after the commands of its last step; a Rust driver that finds a child's emitted events and its end in one poll pushes the emitted events first. There is no order between what two different runs push.
- **Cancelled children.** "Cancelled work queues nothing" holds for a child run's emitted events as for its outcome: once the parent has carried out `invoke.cancel`, the child pushes nothing more. What it emitted before that, in its cancel step included, is in the parent's queue, where it is stale ([taking an input](#taking-an-input)), unless the parent has closed. A cancel request still goes before everything queued, messages included, and whatever a run has queued when it closes is discarded.
- **Stubs.** A stubbed machine never claims its token, so it receives nothing and emits nothing. Messages for it are kept until its invoke ends, then discarded.

### Ending a run

A run ends when its status is no longer `running`. On `done` the result is the output (an Effect success, a Rust `Ok`), on `failed` the error (the Effect error channel, a Rust `Err` of the machine's error enum), and on `defect` the defect is raised ([defects](#defects)). A cancelled run has no result: a child run reports through its token, a root run as follows.

When a run ends, for whatever reason, the driver discards anything still in its queue, messages and emitted events included and without a record, unsubscribes it from the emitted events of its peers, and drops every invoke and timer of the run it still holds. Normally there are none, since the core cancelled them; there can be when an action's defect stopped a step before the `invoke.cancel` or `timer.cancel` of work the step had already removed from the snapshot, which `failWithDefect` therefore cannot see. A child run dropped this way gets a cancel signal first, as on `invoke.cancel`.

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

A saved snapshot is the core's [snapshot](#snapshot), as the run's last step left it, with two additions: `snapshotVersion`, which is `2`, and a `due` member on each timer, from the driver ([time](#time)). Its schema is [`snapshot.schema.json`](snapshot.schema.json).

```jsonc
{
  "snapshotVersion": 2,
  "machine": "Order",
  "run": "r1",                                  // the id the run had when it was saved
  "ir": "sha256:6ae1…",
  "status": "running",
  "active": ["Order", "Order.AwaitingPayment"],
  "input": { "id": "o1" },
  "vars": { "paid": 0, "tracking": "" },
  "invokes": [],
  "timers": [{ "id": 1, "state": "Order.AwaitingPayment", "index": 0, "delay": 259200000, "due": 259200000 }],
  "nextInvoke": 1,
  "nextTimer": 2,
  "nextMessage": 1,
  "history": {},
  "result": null
}
```

- **Version 1** is the same without `history`, as written before history states: a runtime restores one as if it had `"history": {}`, and the `restored` record holds it as it was given. Save and suspend always write version 2. A machine with no history state keeps its hash from before history states, so its version 1 snapshots still restore.
- **`due`** is an absolute time on the driver's clock, in integer milliseconds: since the Unix epoch in production, virtual time in conformance. A deadline is kept rather than the time remaining because the run is in its state the whole time it is saved: an order that waits three days for payment expires three days after it began waiting, however long it spent in a store. A host can also find when to wake a saved run from its earliest `due`.
- **What is not in it.** Inputs queued but not yet taken (a request handler never has any, since it saves an idle run); the state of child runs, which start again from the beginning ([restoring](#restoring-a-snapshot)); messages kept for a child that has not claimed its invoke; and subscriptions.
- **The same in both runtimes.** Values are in their JSON form ([values](#values)), `active` is in document order, and `invokes` and `timers` are in the order they were started, so the two runtimes save the same run as the same document, and either restores what the other saved. Conformance compares saved snapshots as [canonical JSON](ir.md#canonical-json).
- **Numbers.** A JSON number's form does not matter. Wherever a saved snapshot has an integer (`snapshotVersion`, the ids, a timer's `index`, `delay` and `due`, an `int` or `duration` value), one written as a float with no fraction (`1.0`, `10000.0`, `-0.0`) is that integer, as JSON Schema's `integer` and JavaScript read it; a runtime always writes its integers in integer form (`2`, and `-0` as `0`).
- **A copy.** What save and suspend give host code, and what restore takes from it, is the runtime's no longer: changing a saved snapshot afterwards changes no run.

### Restoring a snapshot

Restoring checks the saved snapshot against the IR the runtime was given, refuses it with a reason if it does not fit, and otherwise starts a new top-level run from it. Nothing is written to the trace before the checks pass. They are made in this order:

| Reason | Refused when |
| --- | --- |
| `invalid` | It does not match `snapshot.schema.json`, or its `snapshotVersion` is not `1` or `2` (a version 1 snapshot has no `history`, a version 2 one has it). |
| `machine` | The IR has no machine named `machine`, or it is not the machine restore was asked to restore (TypeScript `OrderLive.restore` restores only `Order`). |
| `ir` | Its `ir` is not the machine's hash in the IR and no migration was given, or the migration's result has another hash too. |
| `migration` | The migration failed: it threw (anything at all), panicked or returned an error. (Its result is checked from the top, as a snapshot of the machine being restored, the one the old snapshot names when restore was not asked for one: a result that is not a snapshot is `invalid`, one of another machine `machine`, and one that still has another hash `ir`.) |
| `ended` | Its `status` is not `running`. A run that ended with a defect is `ended` whatever its defect's `kind`: the schema requires only that it has one, so a kind this version does not name is not `invalid`. |
| `ref` | The machine has a parameter of type `ref M`, `ref M?` or `ref M[]`, or an alias of one ([not yet specified](#not-yet-specified)). |
| `invalid` | It does not fit the machine: its `status` is `running` but its `result` is not `null` (a run has a result only once it has ended); `active` is not a configuration in document order (the root first, each other state's parent active, one active child for each active compound state, every child for each active parallel state); an `invokes` entry's state is not active, has no invoke of `src`, or has two entries; a `timers` entry's state is not active, or its `index` is not the first transition of an `after` block of that state, or two entries have the same state and index; an id is repeated or not below `nextInvoke` or `nextTimer`; `input` does not have exactly the machine's parameters, or `vars` has a name that is not one of its variables, or lacks an optional variable; a `history` key is not the id of one of its history states, or its value is not one that history state could have recorded (for a `history` state, one child of a compound parent or every child of a parallel parent; for a `deepHistory` state, the leaves of a configuration below its parent: atomic or final states which, with their ancestors up to and including the parent, have one child for each compound state and every child for each parallel state among them; in document order either way); or a value does not decode as its declared type, as a machine's arguments must. A record, an error or an event decodes only with exactly the members its type declares (and an error's or event's `_tag`): one with a member its type does not declare is `invalid`. |

**Migration.** When the hashes differ, the host may give a **migration**: a function from the old saved snapshot to a new one, both plain JSON (TypeScript `(saved: Json) => Json`, Rust `Fn(serde_json::Value) -> Result<serde_json::Value, String>`). It is called once, and only when the hashes differ. Its result is checked from the top as if it had been given instead, except that a second migration is never applied. It is how a deployment that changed a chart (renamed a variable, added a state) keeps its saved runs: the migration rewrites `ir`, the names, and whatever else must change. It needs no IR of the old chart; it sees only the data.

**The new run.** The restored run is a new top-level run with an id of its driver's choosing ([run ids](#the-driver)), not the `run` in the snapshot, which named it in the process that saved it. It has no peers. The driver opens it and calls the core:

```
restore(ir, saved, run, now, migrated, fns):
  m    = ir.machines[saved.machine]           # every check above has passed
  snap = saved without snapshotVersion, with run = run, no due on the timers, and history = {}
         if it has none (version 1)
  emit trace { kind: "restored", machine: saved.machine, ir: m.hash, migrated, snapshot: saved }
  for s in snap.active, in document order:
    if snap.invokes has an entry inv with state s.id:
      args = the parameter names of s.invoke.src zipped with eval of s.invoke.args, left to right
      emit trace { kind: "invoke.started", invoke: inv.id, state: s.id, src: inv.src, args }
      emit invoke.start { invoke: inv.id, state: s.id, src: inv.src, srcKind: s.invoke.kind,
                          as: s.invoke.as, args }
    for each entry tm of saved.timers with state s.id, by ascending id, if tm.due > now:
      emit trace { kind: "timer.started", timer: tm.id, state: s.id, index: tm.index, delay: tm.due - now }
      emit timer.start { timer: tm.id, delay: tm.due - now }
  for each entry tm of saved.timers with tm.due <= now, by ascending due, then ascending id:
    emit timer.fire { timer: tm.id }
  return (snap, commands)
```

`now` is the driver's clock, `run` the new run's id, and `migrated` the hash the snapshot had before a migration, or `null` when there was none. Each timer of the core snapshot keeps the `delay` it was saved with, the delay its `after` evaluated to, though a restarted timer's `timer.started` record and command give the time that remains. `snapshot` in the `restored` record is the saved snapshot as restored, after any migration.

- **The work that was running restarts.** Each invoke in `invokes` is started again, with the same id and with its arguments evaluated now, from the snapshot's variables (a function in them is called again). A task runs again from the start, so a machine meant to be saved should wait in states with no `invoke`, or use tasks that are safe to repeat ([semantics](../docs/design/semantics.md#saving-and-restoring)). A child machine runs again from its `start`, as a new child run `<new run id>.<invoke id>`: its own progress, and the messages its parent had sent it, were not saved. An invoke whose outcome had already been taken is not in `invokes` and is not restarted, though its state is active.
- **Timers keep their deadlines.** A timer not yet due is started again, with the same id, for the time that remains, so it expires at its saved `due`. A timer already due is not started: `timer.fire` queues its expiry at once, before anything else can reach the run's queue, the earliest due first. Its deadline passed while the run was saved, so it is taken before any event given after restoring: an order restored after its payment deadline expires before it sees the payment. Since ids are kept, `nextInvoke` and `nextTimer` do not change; they cannot collide with anything, since the run id is new.
- **Nothing else happens.** No state is entered or exited, no effect runs and the run does not settle: the snapshot was saved between steps, already settled. The run then takes inputs like any run.
- **Defects.** A runtime error or a host defect while evaluating an invoke's arguments makes `restore` atomic, like `start`: it returns `failWithDefect(d)` applied to `snap`, with two commands, the `restored` record and the `defect` record. The `invoke.cancel` and `timer.cancel` commands of `failWithDefect` are for work never started, which the driver ignores. The run then ends with the defect like any run.
- **The driver** carries out the commands and drives the run as `run` does ([the driver](#the-driver)). The due times it keeps are `now + delay` for the timers it starts, which is their saved `due`, and the saved `due` for those whose expiry `timer.fire` queued.

### Suspending and resume

A run is **idle** when it is between steps, its queue is empty, no cancel request is waiting, and it has no running invoke (`snap.invokes` is empty): it waits only for events, messages and timers, and nothing it started is in flight.

**suspend** waits until the run is idle or has ended, taking inputs meanwhile as always (a cancel request among them: one that arrives while a step runs is taken after that step, so a run asked to cancel and then to suspend is cancelled, exit effects and all, and suspend gives its `cancelled` snapshot): the outcomes of its invokes and the expiries of its timers, which keep running, so a chart's own `after` bounds how long it waits for a task. The driver checks when suspend is called, and after each step's commands (or `start`'s, or `restore`'s) have been carried out. Then:

- If the run is idle, the driver **stops** it: it closes the run ([delivery](#delivering-messages)), deciding that it is idle and closing it as one operation with respect to what is pushed onto its queue, so that an input that arrived while it was open is taken (the run is then not idle), never discarded; cancels its timers through its timer (with no record: they are in the snapshot), discards its queue (empty), writes `{"kind": "suspended"}`, and gives the saved snapshot. Its status is still `running`: the run is not over, it goes on from the snapshot when that is restored. No exit effect runs.
- If the run has ended with `done`, `failed` or `cancelled`, the driver gives its saved snapshot, whose `status` and `result` say so, and writes nothing. A run that ended with a defect raises it, as `run` does, and gives no snapshot.

A timer that falls due while suspend waits is taken like any input; one still running when the run stops is in the snapshot. An event given to the run while suspend waits is taken if it arrives before the run stops.

**resume**`(saved, event?, migration?)` restores the snapshot, gives the run `event` if there is one, suspends it, and gives the saved snapshot that suspend gives. Expiries that `timer.fire` queued are taken before the event. Both inputs are checked before anything runs or is written: first the snapshot, as restore checks it (a refusal), then the event, which must be one of the events the machine accepts, with exactly its fields, each of its type, as an event given to a run must encode. An event that is not is the caller's error, as for an event host code gives a running run that does not encode: a host defect raised to the caller (TypeScript dies with it, Rust resumes unwinding with it); no run starts, nothing is traced and no run id is used. This is the request handler's operation: it awaits the tasks the event starts (so a snapshot resume gives has no running invokes) and saves the timers with their deadlines. A defect is raised; a refused snapshot is an error of its own, before any run starts ([TypeScript](../docs/design/typescript.md#saving-and-restoring), [Rust](../docs/design/rust.md#saving-and-restoring)).

## Not yet specified

The formats leave room for these; their rules will be added here.

- **Restoring a run that holds `ref`s.** Its `input` names peers by the run ids of the process that saved it. Restoring it needs host code to give a handle for each of those ids, and the binding layer to rewrite them in `input` and `vars` by their types; until then such a snapshot is refused (`ref`).
- **Saving a child run**, or a parent together with the state of its children, which is durable execution.
- **Keyed `spawn`** ([actors and messaging](../docs/design/actors.md#many-children)), and host code receiving a run's emitted events through a handle.
- **`strict`**, which would turn a dropped event into a failure, is not in the grammar yet.
