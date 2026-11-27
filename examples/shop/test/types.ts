/**
 * Type-level tests, checked by `tsc` (`pnpm typecheck`), not run: the guarantees of
 * docs/design/typescript.md's table, against the generated shop modules and the real
 * `@dire-kiwi/roadbook-effect`. Every `@ts-expect-error` must be needed, so each mistake must be an error.
 */
import { Duration, Effect, type Scope, Stream } from "effect"
import type * as Roadbook from "@dire-kiwi/roadbook-effect"
import { Checkout, type CheckoutDef } from "../checkout.road.js"
import { PlaceOrder, type PlaceOrderDef } from "../place_order.road.js"
import {
  type ApiResponse,
  CardDeclined,
  GatewayUnavailable,
  type Receipt,
  Unauthenticated,
  ValidationFailed
} from "../types.road.js"
import { CheckoutLive, PlaceOrderLive } from "../src/machines.js"
import type { Mailer, Payments, Users } from "../src/services.js"

const receipt = { chargeId: "c", amountCents: 1 }

export const mistakes = () => {
  Checkout.implement({
    fns: { retryDelay: () => Duration.millis(1) },
    tasks: {
      // @ts-expect-error  a task may only fail with the errors the chart declared
      chargeCard: () => Effect.fail(new Unauthenticated())
    }
  })

  // @ts-expect-error  every declared task must be implemented
  Checkout.implement({ fns: { retryDelay: () => Duration.millis(1) } })

  Checkout.implement({
    // @ts-expect-error  every declared function must be implemented
    fns: {},
    tasks: { chargeCard: () => Effect.succeed(receipt) }
  })

  Checkout.implement({
    fns: { retryDelay: () => Duration.millis(1) },
    tasks: {
      chargeCard: () => Effect.succeed(receipt),
      // @ts-expect-error  nothing the chart did not declare
      refund: () => Effect.void
    }
  })

  Checkout.implement({
    fns: { retryDelay: () => Duration.millis(1) },
    tasks: { chargeCard: () => Effect.succeed(receipt) },
    // @ts-expect-error  Checkout runs no actions, so it takes none
    actions: { audit: () => Effect.void }
  })

  Checkout.implement({
    fns: {
      // @ts-expect-error  a function must return the declared type: retryDelay returns a duration
      retryDelay: ({ attempt }) => attempt * 10
    },
    tasks: { chargeCard: () => Effect.succeed(receipt) }
  })

  Checkout.implement({
    fns: { retryDelay: () => Duration.millis(1) },
    tasks: {
      // @ts-expect-error  a task must succeed with the declared type
      chargeCard: () => Effect.succeed({ chargeId: 1 })
    }
  })

  PlaceOrder.implement({
    fns: { confirmation: () => json(201, null), invalid: () => json(422, null), problem: () => json(500, null) },
    actions: {
      sendConfirmationEmail: () => Effect.void,
      // @ts-expect-error  actions cannot fail: they are Effect<void, never, R>
      audit: () => Effect.fail(new ValidationFailed({ issues: [] }))
    },
    tasks: {
      authenticate: () => Effect.succeed({ id: "u1", role: "Customer" }),
      validateOrder: ({ draft }) => Effect.succeed({ id: "o1", items: draft.items, totalCents: 1 }),
      priceOrder: ({ order }) => Effect.succeed(order)
    },
    machines: { Checkout: CheckoutLive }
  })

  PlaceOrder.implement({
    fns: { confirmation: () => json(201, null), invalid: () => json(422, null), problem: () => json(500, null) },
    actions: { sendConfirmationEmail: () => Effect.void, audit: () => Effect.void },
    tasks: {
      authenticate: () => Effect.succeed({ id: "u1", role: "Customer" }),
      validateOrder: ({ draft }) => Effect.succeed({ id: "o1", items: draft.items, totalCents: 1 }),
      priceOrder: ({ order }) => Effect.succeed(order)
    },
    machines: {
      // @ts-expect-error  a stub for a child machine must have its signature: Checkout cannot fail with Unauthenticated
      Checkout: () => Effect.fail(new Unauthenticated())
    }
  })

  // @ts-expect-error  an event the machine does not declare
  Effect.flatMap(PlaceOrderLive.spawn({ token: "t", draft: { items: [], coupon: null } }), (a) => a.send({ _tag: "Nope" }))

  // @ts-expect-error  the input is checked: draft is required
  PlaceOrderLive.run({ token: "t" })
}

const json = (status: number, body: ApiResponse["body"]): ApiResponse => ({ status, body })

// ---- a test stubs the whole payment machine with one function -----------------------------------

export const PlaceOrderWithFakePayments = PlaceOrder.implement({
  fns: { confirmation: () => json(201, null), invalid: () => json(422, null), problem: ({ status }) => json(status, null) },
  actions: { sendConfirmationEmail: () => Effect.void, audit: () => Effect.void },
  tasks: {
    authenticate: () => Effect.succeed({ id: "u1", role: "Customer" as const }),
    validateOrder: ({ draft }) => Effect.succeed({ id: "o1", items: draft.items, totalCents: 1 }),
    priceOrder: ({ order }) => Effect.succeed(order)
  },
  machines: {
    Checkout: () => Effect.fail(new CardDeclined({ reason: "test" }))
  }
})

// ---- the inferred types are exact, not `any` ----------------------------------------------------

type Equal<A, B> = (<T>() => T extends A ? 1 : 2) extends (<T>() => T extends B ? 1 : 2) ? true : false
type RunOf<M> = M extends { readonly run: (input: any) => infer E } ? E : never
type SpawnOf<M> = M extends { readonly spawn: (input: any) => infer E } ? E : never

export const exact: [
  Equal<RunOf<typeof PlaceOrderLive>, Effect.Effect<ApiResponse, never, Users | Payments | Mailer>>,
  Equal<RunOf<typeof CheckoutLive>, Effect.Effect<Receipt, CardDeclined | GatewayUnavailable, Payments>>,
  Equal<RunOf<typeof PlaceOrderWithFakePayments>, Effect.Effect<ApiResponse, never, never>>,
  Equal<
    SpawnOf<typeof PlaceOrderLive>,
    Effect.Effect<Roadbook.Actor<PlaceOrderDef>, never, Users | Payments | Mailer | Scope.Scope>
  >,
  Equal<Roadbook.Actor<CheckoutDef>["result"], Effect.Effect<Receipt, CardDeclined | GatewayUnavailable>>
] = [true, true, true, true, true]

// ---- long-lived use: send events, watch state ---------------------------------------------------

export const watched = Effect.gen(function*() {
  const actor = yield* PlaceOrderWithFakePayments.spawn({ token: "t", draft: { items: [], coupon: null } })
  yield* actor.changes.pipe(
    Stream.runForEach((snapshot) => Effect.log(snapshot.active)),
    Effect.forkScoped
  )
  yield* actor.send({ _tag: "Cancel" })
  const snapshot = yield* actor.snapshot
  if (snapshot.matches("PlaceOrder.Cancelled")) yield* Effect.log("cancelled")
  // @ts-expect-error  not a state of PlaceOrder
  snapshot.matches("PlaceOrder.Nowhere")
  return yield* actor.result
}).pipe(Effect.scoped)

export const watchedType: Equal<typeof watched, Effect.Effect<ApiResponse>> = true
export const errorClass: GatewayUnavailable = new GatewayUnavailable()
