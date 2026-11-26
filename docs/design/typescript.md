# TypeScript runtime (Effect v4)

The TypeScript side is two packages: `@roadbook/core`, the pure interpreter with no dependencies, and `@roadbook/effect`, the driver and public API built on Effect v4.

Both are implemented (milestone 3, messaging between machines in milestone 5, and parallel states and saving and restoring in milestone 6), against `effect@4.1.5`. `@roadbook/core` has no dependencies and runs in the browser too; `@roadbook/effect` passes every [conformance case](../../spec/conformance), the messaging, parallel and snapshot cases included, through its production driver. The [shop example](../../examples/shop) serves `POST /orders` from Express with them, and its [type tests](../../examples/shop/test/types.ts) check the guarantees below with `tsc`. The messaging examples, [`upload`](../../examples/upload) and [`orders`](../../examples/orders) with its [`inventory`](../../examples/inventory) peer, run end to end in [`packages/effect/test/examples.test.ts`](../../packages/effect/test/examples.test.ts).

## A machine is an Effect

A chart with the signature `PlaceOrder(token, draft) -> ApiResponse` becomes a value whose `run` has the type

```ts
(input: { token: string; draft: OrderDraft }) => Effect.Effect<ApiResponse, never, Users | Payments | Mailer>
```

The success type is the chart's output. The error type is the union of the chart's declared errors. The requirements are the union of the services its implementations use. It composes, retries, times out, traces and tests like any other Effect.

## What gets generated

`roadbook gen` writes one `.road.ts` file next to each `.road` file. It contains:

- An Effect `Schema` for every `type`, `enum`, `event` and `emits`, and a `Schema.TaggedError` class for every `error`. These are what make snapshots and traces serialisable and inputs decodable.
- For each machine, a definition type (`PlaceOrderDef`) listing its input, output, errors, events (`event`), emitted events (`emits`), state ids (the root's included), and the signature of every function, action, task and child machine it uses.
- For each machine, a typed handle with the IR embedded: `export const PlaceOrder = Roadbook.make<PlaceOrderDef>(ir, "PlaceOrder", errors)`. `errors` holds the error class of every error declared in the file or in a file its declarations refer to, directly or not (through a type, a `ref`, or the signature of a function, action, task or machine they use), imported from the modules that declare them, so that the runtime can hand host code and callers instances of them rather than plain objects. An imported file nothing uses contributes none, so its module is not imported.

An error's class extends JavaScript's `Error`, so a few field names are taken. A field named `name`, `message` or `stack` must be a `string` or an enum, as on `Error`; any other type would give the field the wrong type (a `bool` makes the class `never`, which `tsc` rejects). A field named `toString`, `toJSON` or `pipe` would replace a method every error instance has. `roadbook gen` rejects both, naming the file, line and field; rename the field. Records and events have no such restriction.

Declarations of an imported file are not repeated: the module imports them from that file's generated module (`import { Order, User } from "./types.road.js"`). A file of another package is imported from the generated module next to it, `<chart>.road.ts`, so the package must ship its generated modules beside its charts:

- For a package of the `[packages]` table of `roadbook.toml`, by a relative path to where the chart was found, after the package's `exports` (`orders.road.ts` has `import { type InventoryDef, Item, OutOfStock } from "../inventory/inventory.road.js"`). The directory may be outside the project, or absolute.
- For a package found in `node_modules`, by a specifier of the package that Node and TypeScript resolve to the module: the one the package's `exports` maps to it, matched with the conditions `types`, `import`, `node` and `default` and the precedence the loader uses ([projects](language.md#projects)). A package that exports `"./*.road": "./charts/*.road"` should also export `"./*.road.js": "./charts/*.road.js"`, and `"@shop/inventory/inventory.road"` is then imported as `"@shop/inventory/inventory.road.js"`; a pattern such as `"./*": "./charts/*"` covers both. Without `exports` the specifier is the chart's path in the package with `.js` added. If `exports` maps no subpath to the module, `roadbook gen` fails for the importing chart, naming the entry to add, since the import could not be resolved. A `ref Inventory` is the type `Roadbook.Handle<InventoryDef>`, with `InventoryDef` imported as a type when another file declares `Inventory`; it has no schema, since a `ref` is never in a record, error or event, and an alias of a type with a `ref` in it is a type alias only. A name that exists only as a type, a machine's `<Name>Def` or such an alias, is imported with `type` (`import { type InventoryDef, Item }`), as `verbatimModuleSyntax` requires and as Node needs, the module having no value of that name; if a module imports nothing else from another it does so with `import type`, so that the other module is not loaded for it. An `extern type` is `unknown` (and `Schema.Unknown`); binding one to a host type in `roadbook.toml` is not implemented yet. The generator is the library crate [`roadbook-gen-ts`](../../crates/roadbook-gen-ts), so other tools can reuse it.

Charts may import each other (`a.road` takes `error Bad` from `b.road`, which takes `type T` from `a.road`), and so may their modules. In an import cycle the module that is loaded first runs last: the modules it imports run before it, while its bindings are not yet initialised. So a module reads what it takes from a module that may import it back, directly or not, only when it is first needed: a schema as `Schema.suspend(() => T)` (`Schema.Struct({ t: Schema.suspend(() => T) })`, with the same type and encoded type as `T`), an error class through a getter of the errors map (`const errors = { get Bad() { return Bad } }`), which `Roadbook.make` keeps and reads only when it decodes an error. The generator tells which modules may import which from the IR, which holds every file the chart imports, directly or not: a module imports only from the files its declarations refer to, directly or not, so a file may import the chart's module back exactly when its declarations reach the chart's. Everything else is used directly. Types whose definitions are recursive across files (`type X { y: Y? }` in one file and `type Y { x: X? }` in another) are the one shape this cannot express, since their schemas would need interfaces written across modules: `roadbook gen` rejects them, naming the types, and they must be declared in one file.

The generated file is committed. Reviewers see type changes in the diff, consumers need no build step, and `roadbook gen --check` in CI fails if one is stale or missing, or left behind by a deleted chart (the project's `gen-exclude` names charts that need none; see [projects](language.md#projects)). A Vite plugin that generates on the fly is a later convenience.

## Implementing a machine

```ts
import { Duration, Effect } from "effect"
import { Checkout, PlaceOrder, ValidationFailed } from "./shop.road.js"

export const CheckoutLive = Checkout.implement({
  fns: {
    retryDelay: ({ attempt }) => Duration.millis(200 * 2 ** attempt)
  },
  tasks: {
    chargeCard: ({ order, user }) => Payments.use((payments) => payments.charge(order, user))
  }
})

export const PlaceOrderLive = PlaceOrder.implement({
  fns: {
    confirmation: ({ order, receipt }) => json(201, { orderId: order.id, chargeId: receipt.chargeId }),
    invalid: ({ issues }) => json(422, { code: "invalid", issues: [...issues] }),
    problem: ({ status, code }) => json(status, { code })
  },
  actions: {
    sendConfirmationEmail: ({ user, order }) => Mailer.use((mailer) => mailer.send(user, `Order ${order.id} confirmed`)),
    audit: ({ what, order }) => Effect.log("audit", what, order.id)
  },
  tasks: {
    authenticate: ({ token }) => Users.use((users) => users.byToken(token)),
    validateOrder: ({ draft }) =>
      draft.items.length === 0
        ? Effect.fail(new ValidationFailed({ issues: ["order has no items"] }))
        : Effect.succeed({ id: "ord_1", items: draft.items, totalCents: 0 }),
    priceOrder: ({ order }) => Effect.succeed({ ...order, totalCents: 4200 })
  },
  machines: {
    Checkout: CheckoutLive
  }
})
```

Each implementation receives the arguments the chart passes, as one object with the declared parameter names. It does not receive the machine's variables or the current event.

Values cross the boundary in their host form: records as plain objects, errors as instances of their tagged error classes, enums as string literals, `duration` as an Effect `Duration`, an optional value as `null`, `int` and `float` as `number`, a machine or task that returns nothing as `void`. The driver converts them to and from the core's JSON form by the IR's types, and a value that is not of its declared type (a negative, infinite or longer than 2^53 − 1 ms `Duration`, an `int` outside ±(2^53 − 1), a missing field) is a `host` defect. A `json` or `extern type` value is copied and must be JSON: `null`, a boolean, a finite number, a string, an array or a plain object of JSON values; `NaN`, `Infinity`, `undefined`, a function, a class instance such as a `Date`, or a cycle is a `host` defect.

The types enforce the contract in both directions:

| Mistake | Result |
| --- | --- |
| A declared task, function, action or child machine is missing. | Type error at `implement`. |
| A task fails with an error the chart did not declare. | Type error on that task. |
| An implementation is supplied for a name the chart does not declare (for example after the chart stopped using it). | Type error on that name. |
| An action can fail. | Type error: actions are `Effect<void, never, R>`. |
| A function returns the wrong type. | Type error on that function. |

## Running

**One run per request.** Most backend use is this: build the input, run the machine, use the result.

```ts
const runtime = ManagedRuntime.make(AppLayer)

app.post("/orders", async (req, res) => {
  const draft = draftOf(req.body)                       // Schema.decodeUnknownOption(OrderDraft)
  if (Option.isNone(draft)) return res.status(400).json({ code: "bad_request" })
  const response = await runtime.runPromise(
    PlaceOrderLive.run({ token: req.headers.authorization ?? "", draft: draft.value })
  )
  res.status(response.status).json(response.body)
})
```

The generated modules import each other as `./types.road.js`, the usual form for TypeScript's `NodeNext` resolution, so they run under a bundler, `tsx` or vitest, or once compiled; Node's own type stripping does not map `.js` to `.ts`.

With Effect's own `HttpApi`, the machine run is the body of the endpoint's handler.

**Long-lived.** A machine that accepts events is spawned into a scope and driven through an actor:

```ts
const actor = yield* PlaceOrderLive.spawn(input)      // Effect<Actor, never, R | Scope>, once the run has started
yield* actor.send({ _tag: "Cancel" })
const snapshot = yield* actor.snapshot                // status, active states, matches(state), core
actor.changes                                         // Stream<Snapshot>, ends after the final one
const response = yield* actor.result                  // Effect<Output, Errors>
yield* actor.cancel                                   // cancel gracefully and wait
```

**Handles.** `actor.handle` is a `Handle<D>`, the TypeScript counterpart of Rust's `actor.handle()`: the run's `id`, its `machine`, and `send`. It is what a `ref` parameter takes, so a spawned machine is passed to the machines that talk to it as an ordinary argument:

```ts
const stock = yield* InventoryLive.spawn({ warehouse: "w1" })
const receipt = yield* OrderLive.run({ id: "o1", draft, inventory: stock.handle })   // inventory: ref Inventory
yield* stock.handle.send({ _tag: "Restock", sku: "apple", qty: 10 })               // host code sends too
```

The input type of `Order` has `inventory: Roadbook.Handle<InventoryDef>`, so only a handle to an `Inventory` fits, and `send` takes only the events the target declares with `event`: `stock.handle.send({ _tag: "Reserved", … })` does not compile, since `Reserved` is one `Inventory` emits. The core holds a `ref` as the run's id (`"r2"`), as snapshots and traces show it; the driver turns a handle into its id when it encodes a machine's arguments, and the id back into the same handle object for host code (the arguments of an invoke, an action, a task or a function). A value that is not a handle to a running machine of the right kind is a `host` defect. `handle.send` gives the run an ordinary external event, with no sender, and does nothing once the run has ended, like `actor.send`. An event that does not encode is the sender's `host` defect: `send` queues nothing and dies with the `Defect`, so inside an action or a task it ends the run carrying it out ([refs and handles](../../spec/execution.md#refs-and-handles)). A `run` that is not spawned has no handle. Host code cannot subscribe to what a run emits through its handle: that is [not specified yet](../../spec/execution.md#not-yet-specified).

Closing the scope, `actor.cancel`, or interrupting the fiber of a `run`, cancels the machine gracefully: the step in progress finishes, then every active state's exit effects run, innermost first, and the trace ends with `cancelled`. Everything the machine started is cancelled with it. `snapshot.status` is the run's status (`running`, `done`, `failed`, `cancelled` or `defect`), and `snapshot.core` the snapshot as the core holds it.

`make` throws for an IR of another version, or one without the machine.

## Saving and restoring

Specified in [execution.md](../../spec/execution.md#saving-and-restoring) (milestone 6). A saved snapshot is a `Roadbook.Saved`: plain JSON matching [`snapshot.schema.json`](../../spec/snapshot.schema.json), the same document the Rust runtime saves. `Roadbook.Saved` is also an Effect `Schema` of that shape (a `Schema.declare` that checks the members and `snapshotVersion`), for a store to decode with; the values inside (`input`, `vars`) are checked against the IR's types when the snapshot is restored, so the generator adds nothing per machine. The [request handler](semantics.md#saving-and-restoring) runs end to end, through a migration, in [`packages/effect/test/saved.test.ts`](../../packages/effect/test/saved.test.ts).

```ts
// The request handler: restore, one event, run until idle, save.
const next = yield* OrderLive.resume(saved, { _tag: "PaymentReceived", amount: 42 })   // Effect<Saved, RestoreError, R>
yield* OrderLive.resume(saved)                       // no event: only the timers whose deadline passed

// The first request: spawn, then suspend once idle.
const created = yield* Effect.scoped(Effect.flatMap(OrderLive.spawn({ id }), (actor) => actor.suspend))

// Long-lived: restore as an actor, and save one without stopping it.
const actor = yield* OrderLive.restore(saved)        // Effect<Actor, RestoreError, R | Scope>
const now = yield* actor.save                        // Effect<Saved>: the run carries on
const last = yield* actor.suspend                    // Effect<Saved>: waits until idle, then stops the run

// A chart that changed: the migration is called only when the hashes differ.
yield* OrderLive.resume(saved, event, { migrate: (old) => ({ ...old, ir: OrderLive.def.ir.machines.Order!.hash }) })
```

- **`resume(saved, event?, options?)`** is `restore`, `send` and `suspend` in a scope of its own. It succeeds with the saved snapshot whatever the run's status, `done`, `failed` and `cancelled` included (the output or error is its `result`), and dies with a `Defect` if the run ended with one, as `run` does. Interrupting it cancels the run as interrupting `run` does. To migrate without an event, pass `undefined` for it. Nothing runs and nothing is written until both inputs pass: the snapshot (else it fails with a `RestoreError`), then the event, which must be one of the machine's events and encode (else it dies with the caller's host `Defect`, as `send` does).
- **`restore(saved, options?)`** spawns the restored run into the scope like `spawn`, with a new run id from `RootRunId`, and fails with a `RestoreError` before starting anything (no record, no run id used) when the snapshot is refused. `RestoreError` is a `Schema.TaggedError` whose `reason` is `invalid`, `machine`, `ir`, `migration`, `ended` or `ref`, with a `message` saying what did not fit. A value in `input` or `vars` must be exactly the JSON form of its declared type: a record with a member that is not one of its fields is `invalid`.
- **`actor.save`** gives the saved snapshot as of the last step, a copy of its own each time (and `restore` copies what it is given), so host code that changes a snapshot it holds changes no run; **`actor.suspend`** waits until the run is idle (or has ended), stops it without exit effects, writes `suspended` to the trace and gives the snapshot. A suspended actor takes no more events, its `result` is interrupted, `changes` ends with the snapshot it was suspended with, `save` and `suspend` give that snapshot again, and closing its scope does nothing more. A cancel that arrives while a step runs is taken after that step, before a suspend can stop the run: `actor.cancel` then `actor.suspend` during a step gives the `cancelled` snapshot, with the exit effects run.
- **`options.migrate`** is `(saved: Saved) => Saved`, plain JSON both ways: the old snapshot has already passed the checks of its shape and version, which come before the hash's, and the result is checked again from the top. A migration that throws, whatever it throws, refuses the snapshot with `migration`; one that returns a snapshot of another machine is refused with `machine`.

## How the driver maps onto Effect

| Statechart concept | Effect v4 |
| --- | --- |
| A machine instance | A fiber running the event loop inside a `Scope` |
| The event queue | One `Queue` per run, which events, invoke fibers and timer fibers all offer to. The run takes one input at a time. |
| The current snapshot | `SubscriptionRef`, set after each step's commands are carried out, whose `changes` is the stream above |
| A state's `invoke` | A detached fiber in a map keyed by invoke id. Cancelling a task or stub interrupts its fiber without waiting for it to end; cancelling a child run interrupts its fiber and waits for its cancel step ([below](#child-runs-and-cancellation)). |
| `after` timers | Fibers that sleep through the driver's `Timer` service, in a map keyed by timer id. The default `Timer` sleeps on Effect's `Clock`. |
| A child machine run from its chart | The invoke's fiber, given an invoke context (a `Context.Reference`) that the child's `run` claims |
| Cancelling a run | Interruption. Each step runs uninterruptibly and the cancel step (exit effects, the `cancelled` record) in `Effect.onInterrupt`. |
| A task's declared errors | The `E` channel. The driver matches `_tag` to the `fail` branch. |
| A defect in host code | A defect (`Effect.die`, or a thrown exception), which fails the run and is never routed to a branch. A run that ends with one dies with a `Defect` value holding the defect object. |
| Services used by implementations | The `R` channel, provided once with a `Layer` |
| Trace output | `TraceSink`, a `Context.Reference` holding a synchronous `write(record)`, with a no-op default |
| Time | `Timer`, a `Context.Reference` with `now` and `sleep({ run, timer, delay })`. The default, `ClockTimer`, uses Effect's `Clock`. |
| Root run ids | `RootRunId`, a `Context.Reference` holding a function that names each root run (`r1`, `r2`, … by default). Each id must be a run id as the snapshot schema writes one, `RUN_ID_PATTERN` of `@roadbook/core` (letters, digits, `_` and `-`, optionally followed by `.` and an invoke id): a root run given any other dies with a host `Defect` as it starts, before any record, since a run named `order:42` would save a snapshot that `restore` refuses as `invalid`. `isRunId` of `@roadbook/core` checks an id from outside first. |
| Tracing | `Effect.withSpan` per run, and log annotations `roadbook.machine` and `roadbook.run` on everything a run does, plus `roadbook.invoke` in an invoke's fiber. Spans per state and transitions as span events are not done yet. |
| `send` and `emit` | A run's queue sits behind a port: the queue, an open flag, and the ports of the runs subscribed to what it emits. Pushing a message or an emitted event checks the flag and calls `Queue.offerUnsafe`, synchronously, so it is one operation with respect to the receiver closing, which also happens synchronously, as soon as its core returns a snapshot that is not `running` ([delivery](#messages)) |
| A `ref` | A `Handle`, which a run's arguments encode to the run's id; the run decodes the ids its core gives back to the same handles |
| Saving | The driver keeps each running timer's due time (the clock when it carried out `timer.start`, plus the delay, or the saved due of a restored one) and, after each step's commands, the saved snapshot `actor.save` gives |
| Suspending | A flag the run checks after each step's commands. A run waiting in `Queue.take` is woken by a marker on its queue that is never an input, so that it checks at once |
| Tools that drive runs | `RunObserver`, a `Context.Reference` told about every run as it starts, with a handle that can append inputs to its queue in one operation (`Queue.offerAll`). The conformance runner uses it for `together` steps. |

Effect's structured concurrency is the reason this fits so well. "Leaving a state cancels its work" is a scope closing.

### Child runs and cancellation

The rules are in [execution.md](../../spec/execution.md#child-runs); in Effect they come out like this.

- **The invoke context.** The driver runs a machine invoke's implementation in a fiber that has an invoke context (`CurrentInvoke`): a `Context.Reference` holding the trace sink, the timer, the run observer, the parent's run id, the invoke id and a cancel token. `CheckoutLive.run(…)` finds it, claims the token and runs as child run `r1.4`, writing to the parent's sink. A stub such as `() => Effect.fail(…)` never claims it and is treated as a task. Functions, actions and tasks run with no context, and so does everything a child run does, except its own machine invokes.
- **Cancelling a task or stub.** Leaving the state marks the token cancelled and interrupts the invoke's fiber, without waiting for it to end: whatever it does while being interrupted (its finalizers, an uninterruptible region) runs on detached and is ignored. The same holds for work a run still holds when it ends.
- **Cancelling a child.** If the token was claimed, leaving the state marks it cancelled, interrupts the fiber and waits for it. The child's `run` finishes its current step, runs its cancel step in `onInterrupt`, and only then lets the interruption complete, so leaving the state waits for the child's exit effects, like an action. A fiber whose token is cancelled queues nothing, whatever its exit.
- **Defects.** A thrown `Error` becomes a host defect with its `message`, any other value one with `String(value)`; so does a failure in an action, or a task's failure with a value that has no `_tag`. A run that ends with a defect dies with a `Defect` (an `Error` whose `defect` member is the defect object), which the parent's driver passes on unchanged, so a child's defect reaches the root as it was. A defect in a child's cancel step is written into the token by the child, and the parent reads it once the interrupted fiber has ended and ends with that defect, unless it has already ended with a defect of its own (the child was cancelled because of that one), when it ignores it and carries on with its other cancels; the child's fiber also dies with it.
- **Cancelling from outside.** Interrupting the fiber of a root `run`, or closing the scope of a `spawn`ed actor, cancels it the same way: exit effects run and the trace ends with `cancelled` before the interruption completes.

### Messages

The rules are in [execution.md](../../spec/execution.md#delivering-messages).

- **Open and closed.** A run's port is open from before its `start` until its core returns a snapshot that is not `running`, from `start`, a step, the cancel step or the answer to a defect; the driver closes it then, before carrying out that step's commands, and unsubscribes it from its peers. A message pushed onto a closed port is not pushed: for a `send` the driver writes `undelivered` after the `sent` record, for an `emit` it writes nothing.
- **Handles of another version.** Encoding a handle for a `ref M` argument checks that it is a handle to a running `M` whose IR hash is `M`'s hash in the IR of the machine being started; a handle to another version of `M` is a `host` defect before the run starts, with no record, as in the Rust runtime.
- **Subscriptions.** Encoding a machine's arguments collects the handles in them. Just before `start` the run's port is added to the subscribers of each of those runs, once each, and removed when it closes. Carrying out `emit` pushes `{"kind": "emitted", "invoke": null, …}` onto every subscriber's queue.
- **Children.** The invoke context also holds, for an invoke with `as`, the parent's port, and its token holds the messages the parent sent before a child run took them. A `send` to a child pushes onto the child's port once a run has claimed the token and made its queue; before that it is kept in the token, and the child's queue starts with what was kept. A stub never claims it, so what is kept for it is dropped with the token when the invoke ends. A child's `emit` pushes onto the parent's port with its invoke id; it does so in the child's own step, and its outcome is offered only when its `run` ends, after the commands of its last step, so its emitted events always come first. A child cancelled by its parent emits from its cancel step while the parent waits in `invoke.cancel`; if the parent is still open those events are queued and later taken as stale, and once the parent's `invoke.cancel` is over the child has ended and pushes nothing more.

## Testing

- **A whole sub-machine can be a stub.** `machines: { Checkout: () => Effect.fail(new CardDeclined({ reason: "test" })) }` replaces the payment chart with one function of the same signature.
- **Time is virtual.** `after` and backoff delays sleep through the driver's `Timer` service on Effect's `Clock`, so they run under `TestClock` ([the shop's retry test](../../examples/shop/test/retry.test.ts)). The conformance runner, which has to fire timers one at a time and let the machines settle in between, gives the driver its own virtual `Timer` ([what a runner relies on](../../spec/conformance/README.md#how-a-runner-works)).
- **The conformance runner** ([`packages/effect/test/conformance`](../../packages/effect/test/conformance)) drives each case through `make`, `implement` and `spawn` (`restore`, `actor.save` and `actor.suspend` for the snapshot steps, comparing what they give as canonical JSON with the case's snapshot files), replacing only the timer and the host code. A `start` step spawns another top-level run with the same timer, sink and observer, named by `RootRunId`, and decodes its input with the handles of the runs started before it. It runs every fiber on a manual `Scheduler` that queues tasks rather than using `setImmediate`; since nothing in a case waits on real time or I/O, the runs are quiescent exactly when that scheduler has no task left, which the runner checks by draining it. A scripted invoke learns its run and invoke id from the log annotations above. `pnpm --filter @roadbook/effect conformance` prints a line per case; with `ROADBOOK_CONFORMANCE_OUT=dir` each case's actual trace is written to `dir/<case>.jsonl`.
- **The core is pure.** `@roadbook/core`'s `step` can be called directly to assert on transitions without running any effects.
- **Paths can be generated.** Because the chart is data, `roadbook paths` can list every simple path through a machine, and a test can assert each one is exercised. The viewer's coverage mode shows the result.

## Why not build on an existing Effect state machine library

There are several, and an official bridge between XState and Effect is in alpha. They are compared in [alternatives](alternatives.md#existing-libraries). All of them define the machine in TypeScript code, which gives the best type inference with no code generation, and none of them can give Rust the same definition or produce a diagram from a file without executing the program.
