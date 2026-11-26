# Conformance cases

These cases are the definition of correct behaviour. Each runtime runs every case with scripted host behaviour and virtual time and must write the expected trace byte for byte. The rules they follow are in [execution.md](../execution.md) and the record format in [trace.md](../trace.md); every expected trace here was derived by hand from those pages. A semantic change is a change to these files, reviewed like any other.

## Layout

```
spec/conformance/
  case.schema.json        the schema of case.json
  regen-ir.sh             regenerates every chart's IR with the compiler
  charts/                 the charts, shared between cases
    shop/                 a copy of examples/shop, frozen for the suite
    nest.road  nest.ir.json  …
  cases/
    checkout-retry/
      case.json           the machine, its input and the script
      trace.jsonl         the expected trace
    …
```

A chart's IR sits next to it, named after it with `.road` replaced by `.ir.json`, and is committed: runtimes read the IR, never the chart. Run `spec/conformance/regen-ir.sh` after changing a chart or the compiler. (Until the compiler exists the IR files are missing; the loader in [`crates/roadbook-conformance`](../../crates/roadbook-conformance) tolerates that.) The shop charts are a copy so that edits to the documentation's example do not silently change the suite.

The `ir` member of each `started` record is the machine's hash from the IR. When the IR changes, `regen-ir.sh` also rewrites those members (`ROADBOOK_BLESS=1 cargo test -p roadbook-conformance`), and nothing else in the traces. Until then the traces of the shop charts carry the hashes of the [hand-written IR fixture](../../crates/roadbook-ir/tests/data/place_order.ir.json), and the others `sha256:` followed by 64 zeros.

## case.json

```jsonc
{
  "description": "Two gateway failures with growing backoff, then a successful charge.",
  "chart": "charts/shop/checkout.road",       // relative to spec/conformance
  "machine": "Checkout",
  "input": { "order": { /* … */ }, "user": { /* … */ } },
  "stub": [],                                 // machines scripted like tasks
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

Every member is required. Values are in their JSON form ([values](../execution.md#values)).

| Member | Meaning |
| --- | --- |
| `description` | What the case checks. |
| `chart` | The entry chart, relative to this directory. |
| `machine` | The machine run as the root run, `r1`. |
| `input` | Its arguments, keyed by parameter name. |
| `stub` | Machines that are scripted like tasks when invoked. Every other invoked machine runs from the chart as a child run. |
| `fns` | For each function, a list of `{"args": [...], "returns": value}`: the result for those positional arguments. A call whose arguments match no entry (compared as canonical JSON) fails the case. Entries that are never called are allowed. |
| `script` | The steps, in order. |
| `status` | The root run's status after the last step: `running`, `done`, `failed`, `cancelled` or `defect`. |

Script steps:

| Step | Does |
| --- | --- |
| `{"advance": ms}` | Advances virtual time by `ms`, firing the timers that fall due on the way, one at a time. |
| `{"event": {"_tag": "Cancel", …}}` | Gives the root run the external event. |
| `{"task": "chargeCard", "done": value}` | Ends the running scripted invoke of `chargeCard` with success. |
| `{"task": "chargeCard", "fail": {"_tag": …}}` | Ends it with that declared error. |
| `{"task": "chargeCard", "defect": "message"}` | Ends it with the host defect `{"kind": "host", "message": "message"}`. |
| `{"cancel": true}` | Cancels the root run from outside. |
| `{"together": [step, …]}` | Two or more `event` and `task` steps whose inputs are all queued before any run takes the first: the way to make an outcome arrive after its state was left. |

`task` names a task or a stubbed machine. It refers to the one invoke of that name that is running (started, not cancelled, no outcome given yet) in any run; if several runs have one, `"run": "r1.4"` picks one. No match, or an ambiguous one, fails the case.

## How a runner works

A runner gives the driver scripted host code and a virtual clock, then drives it like this:

1. Read `case.json` and the chart's IR. Check the IR's version and that it has the machine.
2. Bind the host: every task, and every machine in `stub`, to a scripted invoke that does nothing until the script ends it; every action to one that does nothing and returns at once; every function to the `fns` table. Every other machine runs from the chart.
3. Set the virtual clock to 0, start the root run `r1` with `input`, and wait until the runs are **quiescent**.
4. For each step of `script`, in order: fail the case if the root run has already ended; carry out the step; wait until quiescent.
5. Check that the root run's status is `status`. A run still running is left as it is: the runner does not cancel it and writes nothing more.
6. Arrange the records [grouped by run](../trace.md#order), write each in canonical form followed by a line feed, and compare the bytes with `trace.jsonl`.

**Quiescent** means that every run's queue is empty, no run is carrying out commands, and every child run that has ended has had its outcome taken by its parent. Only scripted invokes and timers are then pending, and nothing happens until the runner acts.

**Inputs.** Ending a scripted invoke gives its run the `invoke.done`, `invoke.fail` or `invoke.defect` input for that invoke's id; an event step gives the root run an `event` input; a cancel step a `cancel` input. Within a `together` step, every item is resolved first, against the invokes running before any is delivered (an invoke resolved by an earlier item of the same step no longer counts as running), and its input queued, in order, before any run takes one.

**Virtual time.** A timer is due at the virtual time at which the driver carried out its `timer.start`, plus its `delay`. `{"advance": d}` from time `now` repeats: of the running timers in all runs that are due at or before `now + d`, take the one with the earliest due time (ties go to the run that comes first in [run order](../trace.md#order), then to the lower timer id); set the clock to its due time; give its run the `after` input; wait until quiescent. When no timer is left in the window, it sets the clock to `now + d`. A timer started while an `advance` is firing timers is in the same window if it falls due at or before `now + d`, where `now` is the clock when the step began. Otherwise timers fire only during `advance`: a timer with delay 0 started by any other step fires at the next `advance`, even `{"advance": 0}`. Records carry the clock's value when written, so everything a step causes has the same `t`.

**A case must not depend on scheduling.** The one order these rules leave open is that of outcomes from two child runs of one parent that both end while the runs settle after a single step. No case does that.

## The cases

| Case | Checks |
| --- | --- |
| `checkout-charged` | An invoke's `done` leads to a final state whose `output` is the result. |
| `checkout-declined` | `fail CardDeclined` is routed by tag, and a final state's `fail` fails the run. |
| `checkout-retry` | The retry loop: an `after` whose delay comes from a function and reads a variable set by the state's entry, under virtual time; invokes restarted with new ids. |
| `checkout-gave-up` | A guarded block tried in order, falling through to `else` once the guard fails; a bare error as the failure. |
| `place-order-checkout` | `PlaceOrder` running `Checkout` from its chart as child run `r1.4`; an `always` that leaves a state at once, so its work never starts; actions with arguments. |
| `place-order-checkout-stubbed` | The same with `Checkout` stubbed: `r1`'s records are identical, and there is no child run. |
| `place-order-cancel` | `Cancel` on the compound `Processing` cancels its timer and the child invoke of `Paying`; the child run is cancelled from outside and exits its states. |
| `place-order-timeout` | `Processing`'s `after 30s` fires while the child waits in `Backoff`; the child's own timer is cancelled with it. |
| `place-order-defect` | A host defect in the child's task ends the child with a defect, then the parent, which cancels its other work without exit effects. |
| `fetch-fallbacks` | `on *` for an event the state has no `on` for, `fail *` for an unlisted error, and a dropped event. |
| `wizard-done` | A nested final state completes its compound parent, whose `done` transition and then an `always` run in the same step. |
| `nest-effects` | The order of entry, exit and transition effects across three levels; `set` before `do`; an external self-transition restarting work; a transition into the source's own child; an internal transition; exit effects on cancellation from outside. |
| `gate-guards` | Guards tried innermost state first and in document order, falling back to the parent when no branch holds; `and` and `or` short-circuit, which the function table makes observable. |
| `race-stale` | An outcome queued before its state was left is traced as `stale` and ignored. |
