# Execution

This page is the algorithm both runtimes implement: what the pure core does with a snapshot and an input, what it asks the driver to do, and how the driver carries that out. It is precise enough that two independent implementations produce the same [trace](trace.md) byte for byte, and the [conformance cases](conformance/) hold them to it. The design rationale is in [semantics](../docs/design/semantics.md); this page only says what happens.

It covers what the first version supports: atomic, compound and final states, events, guards, `invoke` of tasks and machines, `after`, `always` and `done`. Parallel states and restoring snapshots (milestone 6) and messaging (milestone 5) are not specified yet; [the end of this page](#not-yet-specified) lists where they will fit.

## Terms

- A **run** is one instance of a machine, from `start` until it finishes. A run's **status** is `running`, `done`, `failed`, `cancelled` or `defect`; only `running` is not final.
- The **active states** are the states the run is in. Without parallel states they form a chain from the root (the machine itself, a compound state whose id is the machine's name) down to one atomic or final state. The root is active for the whole run.
- **Document order** is the order of a pre-order walk of the IR's state tree: a state comes before its children, and children are in `states` order. **Innermost first** means reverse document order; for a chain it is leaf to root.
- An **input** is what the driver gives the core: an external event, an invoke's outcome, a timer's expiry, or a request to cancel or to fail with a defect.
- A **step** is what the core does with one input (or with `start`). It is a **macrostep** in SCXML's terms: one **microstep** for the input's transition, if any, then the microsteps of [settling](#settling), then [starting work](#starting-work).
- `parent(s)` is the state whose `states` contains `s`. A **proper ancestor** of `s` is `parent(s)`, its parent, and so on up to the root. A **proper descendant** is the converse.

## The core

The core is two pure, synchronous functions. They do no I/O and read no clock.

```
start(ir, machine, input, fns)  -> (snapshot, commands)
step(ir, snapshot, input, fns)  -> (snapshot, commands)
```

`ir` is the whole IR document and `machine` the name of the machine to run. `input` (for `start`) is the machine's arguments as an object keyed by parameter name. `fns(name, args)` calls the host's pure function `name` with positional argument values and returns its value, or a `host` [defect](#defects) if the host code threw, panicked or returned a value that is not of the declared type. The binding layer turns the positional list into whatever the host implementation takes (an object of named parameters in TypeScript, arguments in Rust), decodes the result, and catches what the host code throws (TypeScript `try`/`catch`, Rust `std::panic::catch_unwind`), so nothing ever unwinds through the core. `snapshot` is never modified in place: `step` returns a new one.

`commands` is a list the driver carries out in order. It is the core's only way to affect the world. Trace records are commands too, so the trace interleaves with the work exactly as the core emitted it.

### Snapshot

A snapshot is plain JSON, so both runtimes have the same shape and a snapshot can be stored and compared.

```jsonc
{
  "machine": "Checkout",
  "ir": "sha256:fa1a…",                         // the machine's hash from the IR
  "status": "running",                           // running | done | failed | cancelled | defect
  "active": ["Checkout", "Checkout.Backoff"],    // active state ids, in document order
  "input": { "order": { /* … */ }, "user": { /* … */ } },
  "vars": { "attempts": 1 },                     // assigned variables only
  "invokes": [],                                 // running invokes
  "timers": [{ "id": 1, "state": "Checkout.Backoff", "index": 0, "delay": 200 }],
  "nextInvoke": 2,
  "nextTimer": 2,
  "result": null
}
```

| Member | Meaning |
| --- | --- |
| `machine`, `ir` | The machine's name and its `hash`. |
| `status` | The run's status. |
| `active` | The active states' ids in document order. `[]` once the run is `done`, `failed` or `cancelled`. When the run ends with a `defect`, left as it was in the snapshot the defect was applied to, to show roughly where it stopped (`[]` if that snapshot had already finished). |
| `input` | The arguments, keyed by parameter name. Parameters are read from here. |
| `vars` | Every variable that has a value, keyed by name. A variable with no value yet has no member, which is different from an optional variable holding `null`. An optional variable always has a member, since it starts as `null` when it has no initial value. |
| `invokes` | `[{"id", "state", "src"}]`: the invokes that are running, in the order they were started. `state` is the id of the state that owns it, `src` the task's or machine's name. |
| `timers` | `[{"id", "state", "index", "delay"}]`: the `after` timers that are running, in the order they were started. `index` is the position in the state's `transitions` of the first transition of the timer's `after` block, and `delay` the evaluated delay in milliseconds. |
| `nextInvoke`, `nextTimer` | The id the next invoke or timer will get. Both start at 1. Ids are never reused within a run, which is what lets the core recognise a stale outcome. |
| `result` | `null` while running and when cancelled. `{"output": value}` when done (`value` is `null` for a machine that returns nothing), `{"error": value}` when failed, `{"defect": defect}` when the run ended with a [defect](#defects). |

Within a macrostep the core also keeps two transient values that are not part of the snapshot: the set of states **entered during this macrostep**, and the **completion queue** of compound states that reached a final child and have not been exited since.

### Inputs

| Input | Meaning |
| --- | --- |
| `{"kind": "event", "value": {"_tag": "Cancel", …}}` | An external event: one of the machine's `events`, encoded as a tagged record (see [values](#values)). |
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
| `{"kind": "invoke.start", "invoke": 1, "state": "…", "src": "chargeCard", "srcKind": "task", "args": {…}}` | starts the task (`srcKind` `task`) or machine (`srcKind` `machine`) and, when it ends, queues its outcome as an `invoke.*` input with the same id. It does not wait for it. |
| `{"kind": "invoke.cancel", "invoke": 1}` | cancels invoke 1 and waits until it has stopped: at once for a task, after its cancel step for a [child run](#child-runs). Its outcome is not queued after this; one already queued stays queued. |
| `{"kind": "timer.start", "timer": 1, "delay": 200}` | starts a timer that queues `{"kind": "after", "timer": 1}` after `delay` milliseconds. |
| `{"kind": "timer.cancel", "timer": 1}` | cancels timer 1. Its expiry is not queued after this; one already queued stays queued. |

A trace record is always emitted immediately before the command it describes. Records are listed in [trace.md](trace.md).

## Starting a machine

```
start(ir, name, input, fns):
  m    = ir.machines[name]
  snap = { machine: name, ir: m.hash, status: "running", active: [], input, vars: {},
           invokes: [], timers: [], nextInvoke: 1, nextTimer: 1, result: null }
  emit trace { kind: "started", machine: name, ir: m.hash, input }
  for v in m.vars, in order:
    if v.init != null:          snap.vars[v.name] = eval(v.init)
    elif v.type ends with "?":  snap.vars[v.name] = null
  enterStates([m.root] + defaultEntry(m.root))
  settle()
  if snap.status == "running": startWork()
  return (snap, commands)
```

A variable's initial value may read the parameters and the variables declared before it; reading a later one is a runtime error (`unassigned`). A variable with no initial value starts unassigned, unless its type is optional (`string?`, whose IR type string ends in `?`): an optional variable with no initial value starts as `null`, as the [language](../docs/design/language.md#definite-assignment) says, and reading it is never `unassigned`. Initial values are not traced.

`defaultEntry(s)` is `[]` for a state that is not compound, and `[c] + defaultEntry(c)` where `c` is `s.initial` for a compound one. So starting enters the root, then its initial child, and so on down to an atomic or final state, outermost first: the root's `entry` effects run before any other state's. Starting takes no transition, so it writes no `transition` record.

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
      t = selectOutcome(inv, input)
    "after":
      tm = the entry of snap.timers with id input.timer
      if none: emit trace { kind: "stale", event: input }; return
      remove tm from snap.timers
      t = selectAfter(tm)
    "event":
      t = selectEvent(input.value)
  if t == null:
    emit trace { kind: "dropped", event: input }
    return
  emit trace { kind: "event", event: input }
  microstep(t)
  settle()
  if snap.status == "running": startWork()
```

- An invoke's outcome or a timer's expiry **consumes** it: it is removed from the snapshot before a transition is chosen, whether or not one is. Leaving the state afterwards therefore does not cancel it again.
- An outcome or expiry whose id is not in the snapshot is **stale**: its state was left (and the invoke or timer cancelled) after the driver had already queued it. It is traced as `stale` and changes nothing.
- An input that enables no transition is **dropped**: traced as `dropped`, and nothing else happens. Settling is skipped, since nothing changed. A consumed invoke or timer stays consumed.
- A `dropped` or `stale` record replaces the `event` record; an input is traced by exactly one of the three.
- **A defect in the step that ended the run.** The core finishes the run (or cancels it) inside the step, so the snapshot a step returns can already be `done`, `failed` or `cancelled` while the driver is still carrying out that step's commands, every action of the step among them: say the effects of the transition into a top-level final state, the exit effects `finish` runs (the root's among them), or the exit effects of `cancelRun`. If any of those actions has a host defect, the driver stops at it as for any action ([defects](#defects)), so the `done`, `failed` or `cancelled` record after it is never written, and gives the core the `defect` input. That is why a `defect` input is taken in any status but `defect`: the run's status becomes `defect`, its result the defect, and the `defect` record is its last. `failWithDefect` finds no invokes or timers left to cancel, and `active` stays `[]`. A second `defect` input, for a run that already ended with one, changes nothing and writes nothing.

## Selecting a transition

A transition is **enabled** if its trigger matches the input and its guard holds. A `null` guard holds. Guards are evaluated in the order given here, and evaluation stops at the first enabled transition, so a guard after it is never evaluated (which matters because a guard can call host functions).

**An external event** `{"_tag": N, …}`: for each active state `s`, innermost first, let the candidates be `s`'s transitions with trigger `{"kind": "event", "event": N}`. If `s` has none, the candidates are its `any` (`on *`) transitions instead. Try the candidates' guards in document order; the first that holds is taken. If none holds, go on to the next state outward. An `on *` is not tried in a state that has an `on N` whose guards all failed. `on *` never matches an invoke outcome or a timer.

**An invoke outcome** for invoke `inv`: only the state `inv.state` is searched. For `invoke.done` the candidates are its `invoke.done` transitions. For `invoke.fail` with error tag `E`, they are its `invoke.fail` transitions with `error` `E`, or, if it has none, those with `error` `"*"`. Guards are tried in document order.

**A timer expiry** for timer `tm`: the candidates are the transitions of `tm.state` with trigger `after` and the same `group` as the transition at `tm.index`, in document order.

**`always`**: for each active state, innermost first, its `always` transitions in document order.

**`done`** of compound state `p`: `p`'s `done` transitions in document order.

`event` is in scope in the guard and effects of an `on` transition (including `on *`), `result` in an `invoke.done` transition and `error` in an `invoke.fail` transition, bound to the input's `value` or `error`. Whether a chart may read `event` in an `on *` is the compiler's business. The implicit names are reserved and parameter and variable names are unique, so a name in a `ref` means exactly one thing.

## A microstep

```
microstep(t):                         # t belongs to state `source`
  emit trace { kind: "transition", from: source.id, index: t's index in source.transitions, to: t.target }
  if t.target == null:                # internal: no state is left or entered
    runEffects(t.effects); return
  target = the state with id t.target
  if source is the root: domain = the root
  else: domain = the nearest proper ancestor of source that is also a proper ancestor of target
  for s in active states that are proper descendants of domain, innermost first:
    exitState(s)
  runEffects(t.effects)
  enterStates(path(domain, target) + defaultEntry(target))
```

`path(domain, target)` is the states from just below `domain` down to `target`, outermost first.

**Every transition with a target is external**, in SCXML's sense. Unless the source is the root, its domain is the least common compound ancestor of its source and target, which is a proper ancestor of both, so the source itself is always exited and the target always entered:

| Transition | Domain | Exited | Entered |
| --- | --- | --- | --- |
| `A -> B`, siblings | their parent | `A` and its active descendants | `B` and its default entry |
| `A -> A` (self) | `A`'s parent | `A` and its active descendants | `A` and its default entry |
| `A -> A.C` (into its own child) | `A`'s parent | `A` and its active descendants | `A`, `C` and `C`'s default entry |
| `A.C -> A` (to its own ancestor) | `A`'s parent | `A` and its active descendants | `A` and its default entry |

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
      append parent(s) to the completion queue

runEffects(effects):
  for e in effects with kind "set", in order:
    v = eval(e.value); snap.vars[e.var] = v
    emit trace { kind: "set", var: e.var, value: v }
  for e in effects with kind "do", in order:
    args = the action's parameter names zipped with eval of e.args, left to right
    emit trace { kind: "action", action: e.action, args }
    emit action { action: e.action, args }
```

So a state's work stops before its own exit effects run, and the `exited` record comes last. All `set` effects of a list run before its `do` effects, whatever order the IR lists them in, and each `set` sees the assignments before it. Actions cannot change variables, so the arguments of every `do` in a list see the same values.

In a microstep, then, the order is: the `transition` record; for each state exited, innermost first, its invoke cancelled, its timers cancelled, its exit effects, `exited`; the transition's effects; for each state entered, outermost first, `entered` and its entry effects. No work is started during a microstep.

## Settling

After the input's microstep (and after the initial entry in `start`), the core takes eventless transitions until none is enabled:

```
settle():
  count = 0
  loop:
    if snap.status != "running": return
    t = the first enabled always transition (innermost first, document order)
    if t == null:
      while t == null and the completion queue is not empty:
        p = take the first state from the completion queue
        t = the first enabled done transition of p
    if t == null: return
    count = count + 1
    if count > 1000: runtime error { kind: "limit" }
    microstep(t)
```

- `always` transitions take priority over `done` transitions. A `done` is considered only when no `always` is enabled, and completions are considered in the order the final children were entered.
- A compound state is complete when one of its final children is entered. Its completion is queued once per entry, and removed from the queue when the state is exited (`exitState`), so a state in the queue is always active and still in the final child that completed it. A completion is therefore never taken for a state that was exited after it completed, even if it was entered again in the same macrostep: an `always` that leaves and re-enters it discards the completion. When a completion is taken from the queue, the state's `done` transitions are tried once: if none of their guards holds, it is discarded without a trace record and is not retried later.
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
      emit invoke.start { invoke: id, state: s.id, src: s.invoke.src, srcKind: s.invoke.kind, args }
    for each after block of s, in document order of its first transition (index i):
      delay = eval(s.transitions[i].trigger.delay)
      id = snap.nextTimer; snap.nextTimer += 1
      append { id, state: s.id, index: i, delay } to snap.timers
      emit trace { kind: "timer.started", timer: id, state: s.id, index: i, delay }
      emit timer.start { timer: id, delay }
```

- Within one state the invoke starts before the timers.
- A delay is a `duration`, so it is never negative: a host function declared to return `duration` that gives a negative one has returned a value that is not of its type, a `host` defect ([values](#values)). A delay of 0 is a timer like any other: its expiry is queued by the driver's timer, never by the core, and it comes after every input already queued.
- Work starts after every entry effect of the macrostep, including those of states entered while settling. So an `after` delay or an invoke argument sees the variables as the step left them: `Checkout`'s `Backoff` sets `attempts` on entry and its `after retryDelay(attempts)` reads the new value.
- A state that is entered and left again within one macrostep (`Confirming` in `PlaceOrder`, which an `always` leaves at once) never starts its work. This is SCXML's rule for `invoke`, applied to timers as well.
- A state that was already active before the input and is not exited keeps its work running.

Time spent is the driver's business: the core records only the delay. A timer's expiry is due `delay` milliseconds after the driver carries out `timer.start`.

## Finishing

Entering a final state whose parent is the root finishes the run, in the middle of the microstep that entered it (it is always the last state that microstep enters):

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

The output or error is evaluated right after `f`'s entry effects, before any exit effect runs. Then every active state is exited, innermost first, with its exit effects, as on any exit: this is when the root's `exit` effects run, and when timers on the root are cancelled. Settling stops and no work is started. The run's result is the output (an Effect success, a Rust `Ok`) or the error (the Effect error channel, a Rust `Err` of the machine's error enum).

## Cancelling from outside

The input `{"kind": "cancel"}` exits every active state, innermost first, exactly as `exitState` does: each state's invoke and timers are cancelled and its exit effects run. Then the status becomes `cancelled` and a `cancelled` record is written. There is no `event` record for a cancel.

```
cancelRun():
  for s in active, innermost first: exitState(s)
  snap.status = "cancelled"
  emit trace { kind: "cancelled" }
```

This is the default in [open question 4](../docs/design/roadmap.md#open-questions). A parent leaving an invoking state cancels its child run this way, so the child's exit effects run too, and the parent waits for them ([child runs](#child-runs)). The cancel step is the run's last; a host defect in one of its exit actions turns it into a defect ([taking an input](#taking-an-input)).

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

Cancelling a child run here is an ordinary [cancellation](#cancelling-from-outside) of the child, which runs the child's exit effects, and the driver waits for it as for any `invoke.cancel`. A defect in that cancel step is written to the child's trace and changes nothing in the parent, which has already ended. A driver ignores an `invoke.cancel` or `timer.cancel` for an id that has already ended, or whose start it never carried out (because a step was cut short by an action's defect). After writing the record the driver raises the defect to whoever ran the run, as the host language does: `Effect.die` with a `Defect` value holding the defect object in TypeScript, and `std::panic::resume_unwind` with a `roadbook::Defect` payload in Rust.

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
| `ref` | `path[0]` is a parameter (from `input`), a variable (from `vars`; reading one with no value is `unassigned`), or the implicit name `event`, `result` or `error`. Each further name reads that field of the value. |
| `field` | Evaluate `of`, then read field `name`. |
| `call` | Evaluate `args` left to right, then `fns(fn, args)`. |
| `record` | An object with the fields in order. If `type` is an error, it also has `"_tag": type`. |
| `enum` | The string `variant`. |
| `op` | See below. |

Operators, by `op`:

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
| `int` | An integer number. |
| `float` | A number, in canonical form when written: `2.0` is written `2`, `-0.0` is written `0`. Always finite. |
| `bool` | `true` or `false`. |
| `duration` | A non-negative integer number of milliseconds. A host duration finer than a millisecond is truncated to whole milliseconds when it enters the core. A negative one (which TypeScript can produce and Rust's `Duration` cannot) is not a `duration`: the binding layer rejects it as a `host` defect. |
| `json` | The value itself. |
| A record (`type R { … }`) | An object with every field, by name. An optional field with no value is `null`, never omitted. |
| An error (`error E { … }`) | An object with `"_tag": "E"` and every field: `{"_tag": "CardDeclined", "reason": "expired"}`, or `{"_tag": "GatewayUnavailable"}` for an error with no fields. |
| An event (`event N { … }`) | The same as an error: `{"_tag": "Check", "amount": 5}`. |
| An enum | The variant's name: `"Customer"`. |
| `T?` | `null`, or the `T`. (`T??` cannot be told apart from `T?` and should be rejected by the compiler.) |
| `T[]` | An array. |
| An alias | The aliased type's form. |
| An `extern` type | Whatever the language binding's encoder produces. The core never looks inside it. Conformance cases do not use extern types. |

Arguments, inputs and the `args` of actions and invokes are objects keyed by parameter name. Values are decoded against their declared type by the binding layer before they reach the core, so the core can tell an `int` from a `float` without looking at the JSON.

**Integers.** The language's `int` is 64-bit. The Rust runtime holds it as `i64`, and its overflow bound is `i64`'s. The TypeScript runtime holds it as a `number`, and its overflow bound is ±(2^53 − 1), the range in which a double is exact: an arithmetic result outside it is `overflow`, and an input integer outside it is rejected by the decoder as a host defect. Between those bounds the two runtimes differ, and traces of values there are not comparable (canonical JSON rounds them to the nearest double). Conformance cases keep every integer within ±(2^53 − 1) and do not test overflow.

## The driver

The driver gives each run a queue and a clock, carries out the core's commands with the host's concurrency, and ends the run.

```
run(machine, input):
  (snap, commands) = core.start(ir, machine, input, fns)
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
      (snap, rest) = core.step(ir, snap, { kind: "defect", defect: d }, fns)
      carry out rest                # cancels and the defect record; a defect met here is ignored
      return snap
  return snap
```

- **Commands are carried out in order, one at a time.** An action is awaited before the next command, and so is an `invoke.cancel` ([child runs](#child-runs)). Starting an invoke or a timer only registers it; its outcome comes back later through the queue.
- **A defect stops the commands.** The commands after the one that met a defect are not carried out and their records are not written; the core's answer to the `defect` input is carried out instead. That answer is the same whether or not the step had already ended the run.
- **Outcomes of a task.** Success gives `invoke.done` with the value. A failure with a tagged error gives `invoke.fail` with it (the core checks that it is one the task declares, and makes any other an `undeclared` defect). A defect gives `invoke.defect` ([catching host defects](#catching-host-defects)).
- **Run ids.** The driver names each root run; a conformance runner names it `r1`. A child run's id is its parent's id, a dot, and the id of the invoke that started it: `r1.4` is the run started by invoke 4 of run `r1`.

### One queue per run

Every source of input pushes onto one first-in, first-out queue per run, at the moment the input arrives:

| Source | Pushes |
| --- | --- |
| A caller's `send` | the external event |
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

- **The context.** When the driver carries out `invoke.start` with `srcKind` `machine`, it runs the implementation with an ambient invoke context holding the trace sink, the clock and timer, the parent's run id, the invoke id and a fresh cancel token. In TypeScript the context is a `Context.Reference` provided to the invoke's fiber. In Rust it is a task-local, or a thread-local that the driver sets around each `poll` of the invoke's future and restores afterwards, which works on any executor. All other host code the driver runs (functions, actions, tasks) runs with no invoke context; so does a child run's own work, except that each of its machine invokes gets a context of its own.
- **Claiming it.** A machine's `run` looks for an invoke context. If it finds one whose token is unclaimed, it claims the token and becomes the **child run**: its run id is `<parent run id>.<invoke id>`, its records carry `parent`, and it uses the context's sink, clock and timer. Otherwise it is a root run. Host code that does not call `run` (a stub) never claims the token, which is how the parent's driver tells a stub from a child run. The parent's core cannot tell them apart and does not need to.
- **Its outcome.** A child run ends like any run: on `done` its `run` returns the output, on `failed` the error, and on `defect` it raises the defect ([catching host defects](#catching-host-defects)). The parent's driver turns that into `invoke.done`, `invoke.fail` or `invoke.defect` as for a task, with the output, error or defect object unchanged, and pushes it onto the parent's queue.
- **Cancelling it.** On `invoke.cancel` the parent's driver signals the token. If the token was never claimed (a task or a stub), it drops the future or interrupts the fiber at once and is done. If it was claimed, the child takes the signal as a cancel request, between its steps like any other, runs its cancel step (exit effects and the `cancelled` record) and reports through the token that it has finished. The parent keeps driving the child until then (in Rust, it keeps polling the child's future), and only then drops it. Carrying out `invoke.cancel` waits for this, like an action, so nothing after it in the parent's step (the exit effects of the state being left, the rest of the transition) happens before the child's exit effects have run. A child that has already ended, however it ended, reports plain "finished" at once (never a defect), and its outcome is not queued after the signal; one already queued is stale.
- **A defect in the cancel step** is reported through the token too: the child writes its `defect` record and ends, and the parent's driver meets that defect while carrying out `invoke.cancel` and gives its own core `{"kind": "defect", "defect": d}`, with the child's `d` unchanged, as it would for a defect in an action. A cancelled task or stub has no cancel step, and whatever it does while being dropped or interrupted is ignored.
- **In Effect** the signal is interruption. The parent interrupts the invoke's fiber and waits for it to end (`FiberMap.remove`, or `Fiber.interrupt`, which returns the fiber's exit). A child's `run` runs each step uninterruptibly and its cancel step in an `Effect.onInterrupt` around the step loop, so interrupting it during a step waits for that step to finish, and the interruption completes only after the cancel step. A defect in the cancel step shows in the fiber's exit as a die with a `Defect`, which the parent uses only if the token was claimed.
- **In Rust** the token is a small shared value (a cancel flag with a waker, and a slot for "finished" or "finished with defect `d`"). The child's run loop checks it before each take, and the parent, after signalling it, polls the child's future until the slot is filled. The child's future may never complete after that; the parent drops it.

### Ending a run

A run ends when its status is no longer `running`. On `done` the result is the output (an Effect success, a Rust `Ok`), on `failed` the error (the Effect error channel, a Rust `Err` of the machine's error enum), and on `defect` the defect is raised ([defects](#defects)). A cancelled run has no result: a child run reports through its token, a root run as follows.

When a run ends, for whatever reason, the driver discards anything still in its queue and drops every invoke and timer of the run it still holds. Normally there are none, since the core cancelled them; there can be when an action's defect stopped a step before the `invoke.cancel` or `timer.cancel` of work the step had already removed from the snapshot, which `failWithDefect` therefore cannot see. A child run dropped this way gets a cancel signal first, as on `invoke.cancel`.

- **TypeScript.** Interrupting the fiber that runs a root `run`, or closing the scope of a `spawn`ed actor, is a graceful cancel. As for a child, each step runs uninterruptibly and the cancel step runs in `onInterrupt`, so the exit effects run and the trace ends with `cancelled` before the interruption completes; the run's Effect then ends interrupted.
- **Rust.** Dropping the future of a root `run` is a **hard stop**: nothing more runs, no exit effect and no final record (the trace simply stops, as a run's in progress does), and the invokes, child runs and timers it holds are dropped with it. Graceful cancellation from outside goes through the actor: `spawn` the run, then `actor.cancel().await` gives it a cancel request and returns once its cancel step has finished, after which `actor.result()` reports that it was cancelled. A child run is never dropped while it runs: its parent cancels it through the token.

### Time

- **Records.** `t` in a trace record is the driver's clock when it writes the record, in integer milliseconds: since the Unix epoch in production, and virtual milliseconds from 0 in conformance runs.
- **Timers.** A timer's expiry is due `delay` milliseconds after the driver carries out `timer.start`. The driver reads the time and waits only through the clock and timer it was given (in TypeScript a `Timer` service whose default sleeps on Effect's `Clock`, in Rust the `Timer` trait), telling the timer the run id, the timer id and the delay when it starts one and the ids when it cancels one. It never completes a timer itself, not even one with delay 0. A test can therefore give it a virtual clock and fire timers itself, as a conformance runner does.

How a conformance runner drives virtual time, scripted outcomes and quiescence, and what it may rely on the driver for, is specified in [conformance/README.md](conformance/README.md#how-a-runner-works).

## Not yet specified

The formats leave room for these; their rules will be added here.

- **Parallel states** (milestone 6). `active` already lists states in document order and every "innermost first" is reverse document order, as in SCXML, so regions fit without changing the snapshot or trace. Transition selection, conflict resolution between regions and completion of a parallel state are to be specified.
- **Restoring a snapshot** (milestone 6). A snapshot holds everything the core needs except timer due times, which the driver will have to save alongside it. A restored run restarts its running invokes.
- **Messaging** (milestone 5). `send` and `emit` will be new commands and effects, an event from a child or peer a new input with its source, and `sent` and `received` new trace records carrying both run ids.
- **`strict`**, which would turn a dropped event into a failure, is not in the grammar yet.
