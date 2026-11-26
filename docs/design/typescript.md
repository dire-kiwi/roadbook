# TypeScript runtime (Effect v4)

The TypeScript side is two packages: `@roadbook/core`, the pure interpreter with no dependencies, and `@roadbook/effect`, the driver and public API built on Effect v4.

The API on this page is not implemented yet, but its types are real. [`spike/api-ts`](../../spike/api-ts) declares the whole surface and a full example, and typechecks against `effect@4.1.5`, including the negative cases.

## A machine is an Effect

A chart with the signature `PlaceOrder(token, draft) -> ApiResponse` becomes a value whose `run` has the type

```ts
(input: { token: string; draft: OrderDraft }) => Effect.Effect<ApiResponse, never, Users | Payments | Mailer>
```

The success type is the chart's output. The error type is the union of the chart's declared errors. The requirements are the union of the services its implementations use. It composes, retries, times out, traces and tests like any other Effect.

## What gets generated

`roadbook gen` writes one `.road.ts` file next to each `.road` file. It contains:

- An Effect `Schema` for every `type`, `enum` and `event`, and a `Schema.TaggedError` class for every `error`. These are what make snapshots and traces serialisable and inputs decodable.
- For each machine, a definition type listing its input, output, errors, events, state ids, and the signature of every function, action, task and child machine it uses.
- For each machine, a typed handle with the IR embedded: `export const PlaceOrder = Roadbook.make<PlaceOrderDef>(ir, "PlaceOrder")`.

The generated file is committed. Reviewers see type changes in the diff, consumers need no build step, and `roadbook gen --check` in CI fails if it is stale. A Vite plugin that generates on the fly is a later convenience.

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

app.post("/orders", (req, res) => {
  runtime
    .runPromise(PlaceOrderLive.run({ token: req.headers.authorization ?? "", draft: decodeDraft(req.body) }))
    .then((response) => res.status(response.status).json(response.body))
})
```

With Effect's own `HttpApi`, the machine run is the body of the endpoint's handler.

**Long-lived.** A machine that accepts events is spawned into a scope and driven through an actor:

```ts
const actor = yield* PlaceOrderLive.spawn(input)      // Effect<Actor, never, R | Scope>
yield* actor.send({ _tag: "Cancel" })
const snapshot = yield* actor.snapshot                // active states, status
actor.changes                                         // Stream<Snapshot>
const response = yield* actor.result                  // Effect<Output, Errors>
```

Closing the scope, or interrupting the fiber of a `run`, cancels the machine gracefully: the step in progress finishes, then every active state's exit effects run, innermost first, and the trace ends with `cancelled`. Everything the machine started is cancelled with it.

## How the driver maps onto Effect

| Statechart concept | Effect v4 |
| --- | --- |
| A machine instance | A fiber running the event loop inside a `Scope` |
| The event queue | One `Queue` per run, which events, invoke fibers and timer fibers all offer to. The run takes one input at a time. |
| The current snapshot | `SubscriptionRef`, whose `changes` is the stream above |
| A state's `invoke` | A fiber in a `FiberMap` keyed by invoke id. Removing the key interrupts it and waits for it to end. |
| `after` timers | Fibers that sleep through the driver's `Timer` service, in a map keyed by timer id. The default `Timer` sleeps on Effect's `Clock`. |
| A child machine run from its chart | The invoke's fiber, given an invoke context (a `Context.Reference`) that the child's `run` claims |
| Cancelling a run | Interruption. Each step runs uninterruptibly and the cancel step (exit effects, the `cancelled` record) in `Effect.onInterrupt`. |
| A task's declared errors | The `E` channel. The driver matches `_tag` to the `fail` branch. |
| A defect in host code | A defect (`Effect.die`, or a thrown exception), which fails the run and is never routed to a branch. A run that ends with one dies with a `Defect` value holding the defect object. |
| Services used by implementations | The `R` channel, provided once with a `Layer` |
| Trace output | A `Context.Reference` holding a sink, with a no-op default |
| Tracing | `Effect.withSpan` per machine and per state, transitions as span events |

Effect's structured concurrency is the reason this fits so well. "Leaving a state cancels its work" is a scope closing.

### Child runs and cancellation

The rules are in [execution.md](../../spec/execution.md#child-runs); in Effect they come out like this.

- **The invoke context.** The driver runs a machine invoke's implementation in a fiber that has an invoke context: a `Context.Reference` holding the trace sink, the clock and timer, the parent's run id, the invoke id and a cancel token. `CheckoutLive.run(…)` finds it, claims the token and runs as child run `r1.4`, writing to the parent's sink. A stub such as `() => Effect.fail(…)` never claims it and is treated as a task. Functions, actions and tasks run with no context.
- **Cancelling a child.** Leaving the state removes the invoke from the `FiberMap`, which interrupts the fiber and waits for it. The child's `run` finishes its current step, runs its cancel step in `onInterrupt`, and only then lets the interruption complete, so leaving the state waits for the child's exit effects, like an action.
- **Defects.** A thrown `Error` becomes a host defect with its `message`, any other value one with `String(value)`. A run that ends with a defect dies with a `Defect`, which the parent's driver passes on unchanged, so a child's defect reaches the root as it was. A defect in a child's cancel step shows in the interrupted fiber's exit, and ends the parent with that defect.
- **Cancelling from outside.** Interrupting the fiber of a root `run`, or closing the scope of a `spawn`ed actor, cancels it the same way: exit effects run and the trace ends with `cancelled` before the interruption completes.

## Testing

- **A whole sub-machine can be a stub.** `machines: { Checkout: () => Effect.fail(new CardDeclined({ reason: "test" })) }` replaces the payment chart with one function of the same signature.
- **Time is virtual.** `after` and backoff delays sleep through the driver's `Timer` service on Effect's `Clock`, so they run under `TestClock`. The conformance runner, which has to fire timers one at a time and let the machines settle in between, gives the driver its own virtual `Timer` ([what a runner relies on](../../spec/conformance/README.md#how-a-runner-works)).
- **The core is pure.** `@roadbook/core`'s `step` can be called directly to assert on transitions without running any effects.
- **Paths can be generated.** Because the chart is data, `roadbook paths` can list every simple path through a machine, and a test can assert each one is exercised. The viewer's coverage mode shows the result.

## Why not build on an existing Effect state machine library

There are several, and an official bridge between XState and Effect is in alpha. They are compared in [alternatives](alternatives.md#existing-libraries). All of them define the machine in TypeScript code, which gives the best type inference with no code generation, and none of them can give Rust the same definition or produce a diagram from a file without executing the program.
