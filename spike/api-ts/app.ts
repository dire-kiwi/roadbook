/**
 * What a developer (or an agent) writes: implementations of the functions the
 * charts declared, and the wiring into a web framework.
 */
import { Context, Duration, Effect, Layer, ManagedRuntime, Stream } from "effect"
import {
  type ApiResponse,
  CardDeclined,
  Checkout,
  GatewayUnavailable,
  type Order,
  PlaceOrder,
  type Receipt,
  Unauthenticated,
  type User,
  ValidationFailed
} from "./shop.way.js"

// ---- services the implementations use ---------------------------------------

export class Users extends Context.Service<Users, {
  byToken(token: string): Effect.Effect<User, Unauthenticated>
}>()("shop/Users") {}

export class Payments extends Context.Service<Payments, {
  charge(order: Order, user: User): Effect.Effect<Receipt, CardDeclined | GatewayUnavailable>
}>()("shop/Payments") {}

export class Mailer extends Context.Service<Mailer, {
  send(to: User, subject: string): Effect.Effect<void>
}>()("shop/Mailer") {}

// ---- Checkout ----------------------------------------------------------------

export const CheckoutLive = Checkout.implement({
  fns: {
    retryDelay: ({ attempt }) => Duration.millis(200 * 2 ** attempt)
  },
  tasks: {
    chargeCard: ({ order, user }) => Payments.use((payments) => payments.charge(order, user))
  }
})

// ---- PlaceOrder ----------------------------------------------------------------

const json = (status: number, body: ApiResponse["body"]): ApiResponse => ({ status, body })

export const PlaceOrderLive = PlaceOrder.implement({
  fns: {
    confirmation: ({ order, receipt }) => json(201, { orderId: order.id, chargeId: receipt.chargeId }),
    invalid: ({ issues }) => json(422, { code: "invalid", issues: [...issues] }),
    problem: ({ status, code }) => json(status, { code })
  },
  actions: {
    sendConfirmationEmail: ({ user, order }) =>
      Mailer.use((mailer) => mailer.send(user, `Order ${order.id} confirmed`)),
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

// The machine is an Effect. Its requirements are the union of what the
// implementations use, including the child machine's.
export const handle: (
  input: { token: string; draft: { items: ReadonlyArray<{ sku: string; qty: number }>; coupon: string | null } }
) => Effect.Effect<ApiResponse, never, Users | Payments | Mailer> = PlaceOrderLive.run

// ---- a test stubs the whole payment machine with one function ----------------

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
export const noRequirements: Effect.Effect<ApiResponse> = PlaceOrderWithFakePayments.run({
  token: "t",
  draft: { items: [], coupon: null }
})

// ---- long-lived use: send events, watch state ---------------------------------

export const watched = Effect.gen(function*() {
  const actor = yield* PlaceOrderLive.spawn({ token: "t", draft: { items: [], coupon: null } })
  yield* actor.changes.pipe(
    Stream.runForEach((snapshot) => Effect.log(snapshot.active)),
    Effect.forkScoped
  )
  yield* actor.send({ _tag: "Cancel" })
  const snapshot = yield* actor.snapshot
  if (snapshot.matches("PlaceOrder.Cancelled")) yield* Effect.log("cancelled")
  return yield* actor.result
}).pipe(Effect.scoped)

// ---- Express: one machine run per request -------------------------------------

declare const AppLayer: Layer.Layer<Users | Payments | Mailer>
const runtime = ManagedRuntime.make(AppLayer)

interface Req { readonly headers: Record<string, string | undefined>; readonly body: unknown }
interface Res { status(code: number): Res; json(body: unknown): void }
declare const app: { post(path: string, handler: (req: Req, res: Res) => void): void }
declare const decodeDraft: (body: unknown) => { items: ReadonlyArray<{ sku: string; qty: number }>; coupon: string | null }

app.post("/orders", (req, res) => {
  runtime
    .runPromise(PlaceOrderLive.run({ token: req.headers.authorization ?? "", draft: decodeDraft(req.body) }))
    .then((response) => res.status(response.status).json(response.body))
})

// ---- mistakes the types catch ---------------------------------------------------

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
    fns: { retryDelay: () => Duration.millis(1) },
    tasks: {
      chargeCard: () => Effect.succeed({ chargeId: "c", amountCents: 1 }),
      // @ts-expect-error  nothing the chart did not declare
      refund: () => Effect.void
    }
  })
}

// ---- the inferred types are exact, not `any` ------------------------------------

type Equal<A, B> = (<T>() => T extends A ? 1 : 2) extends (<T>() => T extends B ? 1 : 2) ? true : false
type RunOf<M> = M extends { readonly run: (input: any) => infer E } ? E : never
export const exact: [
  Equal<RunOf<typeof PlaceOrderLive>, Effect.Effect<ApiResponse, never, Users | Payments | Mailer>>,
  Equal<RunOf<typeof CheckoutLive>, Effect.Effect<Receipt, CardDeclined | GatewayUnavailable, Payments>>,
  Equal<RunOf<typeof PlaceOrderWithFakePayments>, Effect.Effect<ApiResponse, never, never>>
] = [true, true, true]
