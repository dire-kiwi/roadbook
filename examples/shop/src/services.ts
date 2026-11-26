/**
 * The fake services behind the shop's tasks and actions, behaving exactly as README.md says so
 * that the Express and axum applications answer every request alike.
 */
import { Context, Effect, Layer } from "effect"
import { CardDeclined, GatewayUnavailable, type Order, type Receipt, Unauthenticated, type User } from "../types.road.js"

export class Users extends Context.Service<Users, {
  byToken(token: string): Effect.Effect<User, Unauthenticated>
}>()("shop/Users") {
  static readonly fake = Layer.succeed(Users, {
    byToken: (token) => {
      switch (token) {
        case "Bearer good":
          return Effect.succeed({ id: "u1", role: "Customer" })
        case "Bearer admin":
          return Effect.succeed({ id: "u2", role: "Admin" })
        default:
          return Effect.fail(new Unauthenticated())
      }
    }
  })
}

export class Payments extends Context.Service<Payments, {
  charge(order: Order, user: User): Effect.Effect<Receipt, CardDeclined | GatewayUnavailable>
}>()("shop/Payments") {
  static readonly fake = Layer.succeed(Payments, {
    charge: (order) => {
      if (order.items.some((item) => item.sku === "declined")) {
        return Effect.fail(new CardDeclined({ reason: "insufficient_funds" }))
      }
      if (order.items.some((item) => item.sku === "flaky")) return Effect.fail(new GatewayUnavailable())
      return Effect.succeed({ chargeId: `ch_${order.id}`, amountCents: order.totalCents })
    }
  })
}

export class Mailer extends Context.Service<Mailer, {
  send(to: User, subject: string): Effect.Effect<void>
}>()("shop/Mailer") {
  static readonly fake = Layer.succeed(Mailer, {
    send: (to, subject) => Effect.logInfo(`mail to ${to.id}: ${subject}`)
  })
}

export const FakeServices = Layer.mergeAll(Users.fake, Payments.fake, Mailer.fake)
