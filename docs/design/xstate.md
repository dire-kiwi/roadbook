# Roadbook compared with XState

This page compares Roadbook's state machines with XState's, feature by feature. It covers the machine itself: states, transitions, events, data, actors, timers, errors, persistence, and the tools that read machines. It leaves out code generation and the host-language bindings.

It compares against **XState v5**, the stable release, and the **XState v6 beta**, announced on 5 November 2026 ([announcement](https://stately.ai/blog/2026-11-05-xstate-v6-beta), [migration guide](https://stately.ai/docs/xstate/v6/xstate-v5-to-v6)). On the Roadbook side it covers what is on `main` on 27 November 2026, plus work that is decided or in progress but not merged:

| Task | What | State on 27 November 2026 |
| --- | --- | --- |
| SM-8 | Decisions on the open questions: unbounded queues, no supervision yet, `==` stays scalar, `call` and `strict` decided | PR #2, in review |
| SM-18 | Emitted events go only to machines that have an `on` for them | PR #5, in review |
| SM-19 | `strict machine` and `on Event ignore` | PR #6, in review |
| SM-20 | `call` declarations for asking a peer, with `reply` and the built-in `Unanswered` error | Being built, plan approved |

Below, "Roadbook" means `main`, and a feature from one of these tasks is marked with the task's key.

## In short

- **The core is the same model.** Both are SCXML statecharts: nested, parallel, final and history states; `always`, `after` and `done` transitions; run to completion; innermost transition first; invoked children tied to a state; spawned children tied to the machine. Anyone who knows XState can read a Roadbook chart.
- **Roadbook is stricter about work and data.** Every task is tied to a state, every declared error must be handled, host functions see only the arguments the chart gives them, variables are typed and proved assigned before they are read, and machines reach only the machines they are given. XState lets a guard or action read the whole context, keeps errors untyped, and has a registry for looking up any actor.
- **XState has more ways to write things.** It has delayed `sendTo` to another actor, partial wildcards (`mouse.*`), tags and `meta`, `stateIn` checks, and, in v6, choice states, route states, per-state context and state input, and state-level `onError`. Roadbook has none of these.
- **Roadbook checks and records more.** It has a compiler with static checks (exhaustive errors, definite assignment, unguarded `always` cycles, unbounded recursion, unreachable states, states with no way out), a trace format with a schema that two runtimes write byte for byte, redaction of sensitive values, snapshots that refuse a changed chart unless given a migration, typed messaging between machines, keyed child collections, retry sugar, and (SM-20) request and reply between peers. XState v6 has caught up on some of this: timers in snapshots, snapshot versioning, a durable module, unhandled-event hooks and a CLI that validates and checks properties.

[The code examples](xstate-examples.md) show the main differences in XState v5, v6 and Roadbook. [What to add](xstate-plan.md) recommends what to close.

## Side by side

✅ is the same thing, ≈ is the same idea done differently, ➕ is something only that side has, and — is missing.

### States

| Feature | XState v5 | XState v6 beta | Roadbook | |
| --- | --- | --- | --- | --- |
| Atomic, compound, final states | Yes | Yes | Yes | ✅ |
| Parallel states, `done` when every region is final | Yes | Yes | Yes | ✅ |
| Shallow and deep history | Yes, with an optional default `target` | Yes | Yes. With no record it enters the parent's default entry; there is no separate default target | ≈ |
| Which states a transition to a history state leaves | SCXML's rule, from the states it restores | Same | Decided as if the history state were an ordinary child, never by what was recorded | ≈ |
| Choice (decision) pseudo-state | Write a transient state with `always` | `type: 'choice'` with a `choice` function | A transient state with an `always` block (`if … -> A else -> B`) | ≈ |
| Route states, jump to a state by an event | — | `route` and `xstate.route` | — | — |
| Tags (`hasTag`) and `meta` on states | Yes | Yes | No. Doc comments only, kept in the IR | — |
| Descriptions | `description` | Same | `///` doc comments, on states, transitions, declarations and fields, shown by the viewer | ✅ |
| State ids | Implicit paths and `#id` | `#id` resolved exactly | Absolute ids in the IR (`Checkout.Charging`); targets resolved by name in the chart, like block scoping | ≈ |

### Transitions

| Feature | XState v5 | XState v6 beta | Roadbook | |
| --- | --- | --- | --- | --- |
| Guards | Named or inline functions over context and event; `and`, `or`, `not`, `stateIn` | Plain JavaScript inside a transition function | An expression in the chart (`attempts < 3 and isVip(user)`), calling pure host `fn`s with explicit arguments | ≈ |
| Several guarded branches | An array, tried in order | `if` in the function | A block, tried in order; only the last may be unguarded, and the same trigger twice in a state is an error | ≈ |
| Default when a transition targets its own state or a child of it | Not re-entered (`reenter: false`); `reenter: true` re-enters | Same, and `reenter` re-enters even a target that resolves to the current state | Re-entered (SCXML's external transition), unless the target has a leading dot: `-> .Child` keeps the source (SCXML's `type="internal"`) | ✅ |
| Targetless (internal) transitions | Yes | Yes | Yes | ✅ |
| Several targets in one transition | `target: ['.a.x', '.b.y']` | Yes | `-> P.A.X, P.B.Y`, checked to be in different regions | ✅ |
| A transition into one region of a parallel state | Resets sibling regions in some cases | Exits and re-enters only that region | SCXML's rule for an external transition, which leaves and re-enters the parallel state (and warns, `RB0913`); an internal one written in the parallel state exits and enters only the regions it targets, as v6 does | ✅ |
| Wildcard event | `*`, lowest priority | Same | `on *`, tried only in a state with no `on` for the event; never matches outcomes, timers or emitted events | ✅ |
| Partial wildcards (`mouse.*`) | Yes | In `internalEvents` schemas and `enq.listen` patterns | — | — |
| `always` | Yes | Yes | Yes, and an unguarded `always` cycle is a compile error | ➕ |
| `after` with a computed delay | Named `delays` | Same, plus duration strings and ISO 8601 | A `duration` expression, evaluated after entry effects | ✅ |
| A timeout on a state | `after` | `timeout` and `onTimeout`, on a state or an invoke | `after` on the state; an invoke is tied to its state, so the state's `after` is its timeout | ≈ |
| Forbidden transitions: stop the search at this state | A transition with no target or actions | Same | `on E ignore` (SM-19), which is taken, so it also stops the search | ≈ |
| Order of effects in a transition | In order, `assign` included | The returned context patch, then the queued effects | Every `set` first, in order, then the other effects in order; the formatter writes them in that order | ≈ |
| Runaway eventless loops | Runs until memory runs out | Fails after 1000 microsteps | Compile error for an unguarded cycle; a `limit` defect after 1000 microsteps for a guarded one | ➕ |

### Events and the queue

| Feature | XState v5 | XState v6 beta | Roadbook | |
| --- | --- | --- | --- | --- |
| Run to completion, one queue | Yes | Yes | Yes. One FIFO queue per run holds external events, messages, emitted events, task outcomes, timer expiries and child outcomes; the internal queue of completions and raised events is worked through before the next of them (SM-28) | ✅ |
| Typed events | With `setup({ types })` | Schemas (Zod or any Standard Schema), with typo checks | Declared `event` records, checked by the compiler | ✅ |
| `raise`: an event the machine sends itself | Yes | `enq.raise`, plus internal events that cannot come from outside | `raise E` on SCXML's internal queue, taken before the step ends, and `internal event E`, which host code and other machines cannot send (SM-28) | ✅ |
| Delayed events and `cancel` | `raise`/`sendTo` with `delay`, `cancel(id)` | `enq.raise({ delay, id })`, `enq.cancel` | `raise E after 24h as name` and `cancel name` (SM-28): a timer of the run, not of a state, replaced by a raise of the same name and saved in snapshots with its deadline. No delayed `send` to another machine: a delayed raise whose transition sends does the same | ≈ |
| Unhandled events | Ignored | Ignored, a warning in development, an `onUnhandledEvent` hook | Dropped and written as `dropped` in the trace; a `strict machine` turns its own unhandled events into a defect (SM-19) | ➕ |
| Event namespaces | One string space, dots by convention | Same | A machine's own events (`on Cancel`) apart from events received from others (`on upload.Cancel`) | ➕ |

### Data

| Feature | XState v5 | XState v6 beta | Roadbook | |
| --- | --- | --- | --- | --- |
| Machine data | `context`, any type | `context`, typed by schema | Typed `var`s and read-only parameters | ✅ |
| Values that exist only in some states | Optional fields every reader asserts | Per-state context (typestates) and state `input` | Definite assignment: the compiler proves every read happens after a `set` | ≈ |
| Data local to a state | — | State `input`, gone when the state is left | State parameters, `state Confirming(token: ResetToken)` entered with `-> Confirming(result)`, and `var`s inside a state, set on each entry and gone when it is left (SM-29). The compiler checks that every transition into the state passes its arguments | ✅ |
| What host code sees | The whole context and event | The same | Only the arguments the chart passes | ➕ |
| Expressions in the chart | None; JavaScript in functions | JavaScript in functions | Small and total: literals, fields, calls, comparison, `+ - *`, `and or not`; no loops, no string or list operations | ➕ |
| Equality | JavaScript's | JavaScript's | `==` on scalars and enums only; structural equality is designed but not built (SM-8) | — |
| Checking in state from a guard (`stateIn`, `matches`) | `stateIn` | `self.getSnapshot().matches()` | — A guard cannot read the configuration | — |
| Sensitive values kept out of traces | — | — | `sensitive` fields, parameters and variables, redacted in every trace record, checked so a value cannot leak to an unmarked place | ➕ |

### Actions and work

| Feature | XState v5 | XState v6 beta | Roadbook | |
| --- | --- | --- | --- | --- |
| Entry and exit actions | Yes | Yes | `entry` and `exit` effects | ✅ |
| Side-effect actions | Synchronous, fire and forget | `enq(fn)` | `do action(args)`: awaited, in order, and cannot fail | ≈ |
| Long-running work | `invoke` a promise, callback, observable or machine | `createAsyncLogic` (timeouts, `AbortSignal`, emitting events), callback, observable, step logic | `invoke` a task (async, cancellable, with declared errors) or a machine | ≈ |
| Invokes per state | Any number | Any number | Any number, each named with `as` when there are several (SM-37); each has its own outcomes, and a parallel state is the join | ✅ |
| Invoked work tied to its state | Yes | Yes | Yes, and nothing a state starts can outlive it; an outcome that arrives after the state is left is stale | ✅ |
| Callback and observable actors (event sources) | Yes | Yes | A stream task (SM-26): a task that returns nothing and emits until its state is left, `invoke ticks(1s) as clock` with `on clock.Tick`. It cannot be sent events | ≈ |
| Reducer actors (`fromTransition`) | Yes | `createStepLogic` | — | — |
| Retrying | Write the states | Write the states | `invoke … retry N [on E] [after delay]`, written out by the compiler into ordinary states | ➕ |
| A task that reports progress | A callback actor can `sendBack` | `createAsyncLogic` can emit | `task upload(…) -> Url emits Progress { … }`, received as `on up.Progress` through `invoke upload(…) as up` (SM-26) | ✅ |

### Errors

| Feature | XState v5 | XState v6 beta | Roadbook | |
| --- | --- | --- | --- | --- |
| Errors from invoked work | `onError`, untyped | Same, plus `onError` on any state | `fail E` per declared error, typed; the compiler rejects an invoke that leaves one unhandled | ➕ |
| Bugs versus expected failures | One error channel | One error channel | Two: declared errors are flow; defects (a thrown exception, a panic, overflow, an undeclared error) end the run and travel up | ➕ |
| A machine's own failure | Error status | Error status | `final state … { fail E }`: a machine has a task's signature, `-> T ! E1 \| E2` | ➕ |
| Recovering from a child's error | `onError` on the invoke | Nearest active `onError`, then the actor errors | A `fail` branch on the invoke; a defect cannot be caught | ≈ |

### Actors and messaging

| Feature | XState v5 | XState v6 beta | Roadbook | |
| --- | --- | --- | --- | --- |
| Invoked child machine | Yes | Yes | Yes. From the parent's chart a child machine and a task look the same, so a test can stub a whole sub-machine | ✅ |
| Spawned children | `spawnChild`/`spawn`, refs kept in context | `enq.spawn`, owned by the actor | `children name: M by Key`, `spawn name[key]`, `stop name[key]`: a keyed collection declared on the machine, not a value | ≈ |
| Sending to another actor | `sendTo` any ref; `sendParent`; `forwardTo` | `enq.sendTo` | `send name.E` to an `as` child, a `ref` or a collection (`name[key]`, `name.*`), checked against the receiver's declared events | ≈ |
| Emitting | `emit` to `actor.on` listeners | `enq.emit`, and `enq.listen` maps a child's events to the parent's | `emit E`, received with `on name.E` by the invoker or spawner and by holders of a `ref`; since SM-18, only by those that have an `on` for it. Host code: `actor.emitted` (Effect `Stream`), `actor.subscribe()` (Rust), since SM-23 | ✅ |
| Reacting to a child's snapshot | `onSnapshot` | `enq.subscribeTo` | — | — |
| Request and reply | Write it with `sendTo` and `sendBack` | Same | `call` declarations (SM-20): `invoke inventory.reserve(items)`, `on reserve`, `reply pending done …`, `Unanswered` when the callee ends | ➕ |
| How a machine gets a reference | Context, events, the `system` registry | The registry, by `registryKey` | Only its parameters, `as` and collections; no registry, by design | ≈ |
| Learning that a peer ended | Subscribe to it | `enq.subscribeTo` lifecycle events | `on peer.ended`, with `event.status` (SM-25); a pending `call` fails with `Unanswered` (SM-20) | ≈ |
| Sending to an actor that has stopped | Dropped | A `missingTarget` dead letter | `undelivered` in the trace | ✅ |
| Queue bounds | Unbounded | Unbounded | Unbounded; a bound would be a driver setting that drops, not chart syntax (SM-8) | ✅ |

### Persistence and durability

| Feature | XState v5 | XState v6 beta | Roadbook | |
| --- | --- | --- | --- | --- |
| Persist and restore | `getPersistedSnapshot`, `createActor(m, { snapshot })` | `restoreActor` | `save`, `restore`, `suspend`, `resume` | ✅ |
| Pure step function | `transition`, `initialTransition` | Same | `start` and `step` in a pure core, specified in `spec/execution.md` | ✅ |
| Child machines in a snapshot | Persisted recursively | Restored | Saved inside the parent's snapshot, each checked by its own hash, and restored where it was (SM-27); running tasks restart | ✅ |
| Timers in a snapshot | — | Saved, resumed towards their deadline | Saved with their deadlines; one that passed fires as soon as the run is restored | ✅ |
| A snapshot from a different chart | Restored as is | `version` and `machineVersions()` migrations | Refused unless a migration is given, by the IR hash, which changes with any change in behaviour | ≈ |
| One snapshot format for two languages | — | — | The same JSON in TypeScript and Rust | ➕ |
| Durable execution | — | `xstate/durable`, experimental: journals effects through an adapter (Restate, Temporal, Inngest…) | A durable driver on `effect/workflow`: journals inputs, replays them through the core, timers keep their deadlines | ≈ |

### Tools that read machines

| Feature | XState v5 | XState v6 beta | Roadbook | |
| --- | --- | --- | --- | --- |
| Static checks | TypeScript types | Types, `xstate validate` | A compiler: unknown names, types, exhaustive errors, definite assignment, `always` cycles, unbounded recursion, unreachable states, no way out, unused declarations, emitted events nobody receives, optional invoke-timeout warning | ➕ |
| Checking properties ("never reach `refunded`") | — | `xstate test`, `xstate replay` | — `roadbook paths` lists paths; nothing checks a property | — |
| Machine as data | The config object | `machineToJSON`, `machineFromJSON` | The IR, with a JSON Schema and a content hash | ✅ |
| Diagrams | Stately Studio, Inspector | A read-only viz, and Editor Sync that edits code from the diagram | `roadbook viz`: explore, simulate, replay, coverage and diff modes, drawn from the IR; read-only | ≈ |
| Diff between versions | — | `xstate diff` (structure) | `roadbook viz --diff`: a drawn diff, with `summary.md` and a CI recipe | ➕ |
| Run records | The inspection protocol (`@xstate.actor`, `@xstate.transition`) | Same | The trace: JSONL with a schema, each run's records in order, child runs nested; drives replay and coverage | ≈ |
| Test path generation | `@xstate/graph`, `@xstate/test` | `@xstate/test`, coverage gating | `roadbook paths`, coverage from traces | ≈ |
| Two implementations held to one behaviour | — (`@xstate/scxml` runs SCXML) | — | A conformance suite both runtimes pass byte for byte | ➕ |

## What Roadbook is missing

Ordered from most to least likely to matter for program flow, which is what Roadbook is for.

1. **Reacting to a peer's end.** XState can subscribe to another actor's lifecycle. Roadbook's holders of a `ref` found out only through their own timeouts. Closed by SM-25: `on peer.ended` tells a holder how its peer ended, though not its output or error.
2. **Checking properties.** XState v6's `xstate test` checks that a state can or cannot be reached. Roadbook has the IR and `roadbook paths`, which is most of what such a check needs, but nothing that takes a property and fails.
3. **Transitions that target a child without re-entering the source.** In XState, `on: { RESET: { target: '.idle' } }` on a parent keeps the parent's work running. In Roadbook the same transition exited and re-entered the parent, restarting its invoke and timers. Closed by SM-24: `-> .Idle` is SCXML's `type="internal"`.
4. **Several invokes per state.** XState allows an array. Roadbook needed a parallel state with one region per invoke. Closed by SM-37: a state may have several invokes, each named with `as`, whose outcomes are each their own invoke's; a join is still a parallel state.
5. **Delayed and self-sent events.** XState has `raise`, delayed `sendTo`, and `cancel`. Roadbook had only `after`, tied to a state, so a debounce, or a deadline that should survive moving between states, had to be modelled with a parent state that holds the `after`. Closed by SM-28: `raise E` puts an event on the internal queue, `internal event E` declares one only the machine raises, and `raise E after d as name` is a timer of the run, which a raise of the same name replaces and `cancel name` cancels. A delayed `send` to another machine is still not in the language; a delayed raise whose transition sends does the same.
6. **Event sources.** XState's callback and observable actors push events into their parent. Roadbook left this to host code calling `send` on the run's handle, which does not show in the chart, and a task could not report progress. Closed by SM-26: a task declares what it `emits`, an invoke names it with `as`, and the state takes its events with `on up.E`; a stream task is one that returns nothing and emits until its state is left. Tasks can also be spawned into a `children` collection.
7. **A guard on the configuration.** XState's `stateIn` and `matches()` let a guard in one region ask what another region is doing. Roadbook has no form for it, and a chart keeps a variable in step instead.
8. **Several targets in one transition**, to set more than one region of a parallel state at once. Closed by SM-24: `-> P.A.X, P.B.Y`.
9. **Data local to a state.** XState v6's state `input` is passed on the transition into a state and is gone when it is left. In Roadbook every variable belonged to the machine; definite assignment made this safe, but the variable outlived the state that needed it. Closed by SM-29 (IR 0.13): a state takes parameters, `state Confirming(token: ResetToken)`, passed as `-> Confirming(result)`, and declares its own `var`s, which get their values each time it is entered, are readable in it and the states inside it, and are gone once it is left. Default entry never enters a state with parameters, so the compiler sees every way in, and a history state restores a state's locals with it.
10. **Child machines in snapshots.** XState persists a child machine's snapshot with its parent's. Roadbook restarted a child from the beginning when it restored a snapshot saved while the child ran, and `suspend` waited until no child was running, so a parent with long-lived children could not be suspended between events. Closed by SM-27: snapshot version 4 nests each running child run's snapshot in its parent's, restore puts it back where it was, and `suspend` waits for the whole tree to be idle.
11. **Smaller things:** partial wildcards (`mouse.*`), tags and `meta`, a default target for a history state, route states (for UI navigation, not much use here), reducer actors, and equality on records, lists and maps.

## What Roadbook does differently

- **The chart is a separate file, not code.** XState machines are TypeScript objects (v5) or objects with transition functions (v6). Roadbook's are a small language compiled to an IR, so a diagram, a check or a diff never runs host code. XState v6 moving to transition functions makes its diagrams depend on reading conditions out of code; Roadbook's guards and effects are in the IR as expressions.
- **Host code sees only its arguments.** XState passes `{ context, event }` to every guard and action. Roadbook's `fn`, `action` and `task` declarations take typed parameters, and the chart passes them explicitly, so the chart is the wiring diagram for data as well as control.
- **Targeted transitions re-enter.** Roadbook follows SCXML's external transitions; XState defaults to not re-entering. A Roadbook transition from a state to its own child restarts the state's work.
- **`set` runs first.** XState v5 runs actions in the order written, `assign` included. Roadbook runs every `set` of a transition, entry or exit before its other effects, so an action always sees the new values. XState v6's returned context patch is close to this.
- **Actions are awaited and cannot fail.** XState's actions are synchronous and fire and forget. Roadbook's `do` actions are awaited in order, and anything that can fail must be a task, so its failure has a branch.
- **Two kinds of failure.** Declared errors are flow, checked to be handled. Defects (host exceptions, overflow, an undeclared error) are bugs: they end the run without exit effects and propagate to the parent and then the caller. XState has one error channel.
- **Children are not values.** XState refs go in context, in events, to other actors. Roadbook names a child through `as` or a keyed collection, and a peer only through a `ref` parameter, so who can talk to whom is readable from the charts. There is deliberately no registry.
- **Emitted events go to declared listeners in the chart**, received as `on name.E`, and to host code through the actor (`actor.emitted`, `actor.subscribe()`, since SM-23), as XState's `actor.on`. A host listener hears every emitted event and writes nothing to the trace; it subscribes after `spawn` has run the start step, where XState lets `actor.on` come before `actor.start()`.
- **Unhandled events are visible.** They are written to the trace as `dropped`, and SM-19's `strict` can make them fatal. XState ignores them, with a warning and a hook in v6.
- **Snapshots are refused across chart changes.** Any change in behaviour changes the hash, so a restore needs a migration. XState v6 adds versioning that the author bumps.
- **Durable runs journal inputs, not effects.** Roadbook's durable driver records every input a run takes and replays them through the pure core. XState v6's `xstate/durable` journals effects through an adapter. Both rely on transitions being pure.

## What Roadbook has that XState does not

- **Typed, exhaustive errors.** `invoke chargeCard(…) { done … fail CardDeclined … fail GatewayUnavailable … }`, with a compile error for a missing branch, and machines whose signature (`-> Receipt ! CardDeclined | GatewayUnavailable`) is the same as a task's, so a machine can replace a task and a test can stub a machine.
- **Static analysis.** Definite assignment, unguarded `always` cycles, unbounded recursion through invokes and spawns, unreachable states, states with no way out, unused events and declarations, emitted events nobody receives, and the optional warning for an invoke with no timeout above it. Diagnostics come as JSON with fixes.
- **A specified execution model held by two runtimes.** `spec/execution.md` plus a conformance suite that TypeScript and Rust pass with byte-identical traces and snapshots.
- **A trace format.** One schema for debugging, replay, coverage and conformance, with nested child runs and every send, emit and drop.
- **Redaction.** `sensitive` values are redacted in traces, and the compiler stops them leaking into unmarked places.
- **Retry sugar** that compiles to ordinary states, so the diagram, trace and snapshot show what runs.
- **Keyed child collections**, `send name.*.E` broadcasts, and `on name.done` / `on name.fail E` with `key`.
- **Request and reply** between peers (SM-20), with directed replies and `Unanswered` when the callee ends.
- **Emitted events filtered by the chart** (SM-18): a holder that has no `on` for an event is not sent it.
- **`strict` machines and `ignore`** (SM-19), so a chart can say which events it ignores on purpose and fail on the rest.
- **Review tooling:** a drawn diff between revisions with a pull-request recipe, and coverage of transitions from traces.

## What to add

[What to add from XState](xstate-plan.md) gives a recommendation for every gap on this page, and the tasks that would close them.
