/** The implementations of the shop's charts: the code a developer writes. */
import { Duration, Effect } from "effect"
import { Checkout } from "../checkout.road.js"
import { PlaceOrder } from "../place_order.road.js"
import { type ApiResponse, type LineItem, ValidationFailed } from "../types.road.js"
import { Mailer, Payments, Users } from "./services.js"

export const CheckoutLive = Checkout.implement({
  fns: {
    retryDelay: ({ attempt }) => Duration.millis(10 * 2 ** attempt)
  },
  tasks: {
    chargeCard: ({ order, user }) => Payments.use((payments) => payments.charge(order, user))
  }
})

const json = (status: number, body: ApiResponse["body"]): ApiResponse => ({ status, body })

const issuesOf = (items: ReadonlyArray<LineItem>): Array<string> =>
  items.length === 0
    ? ["order has no items"]
    : items.filter((item) => item.qty < 1).map((item) => `${item.sku}: quantity must be at least 1`)

export const PlaceOrderLive = PlaceOrder.implement({
  fns: {
    confirmation: ({ order, receipt }) =>
      json(201, { orderId: order.id, chargeId: receipt.chargeId, totalCents: order.totalCents }),
    invalid: ({ issues }) => json(422, { code: "invalid", issues: [...issues] }),
    problem: ({ status, code }) => json(status, { code })
  },
  actions: {
    sendConfirmationEmail: ({ order, user }) =>
      Mailer.use((mailer) => mailer.send(user, `Order ${order.id} confirmed`)),
    audit: ({ order, what }) => Effect.logInfo(`audit ${what} ${order.id}`)
  },
  tasks: {
    authenticate: ({ token }) => Users.use((users) => users.byToken(token)),
    validateOrder: ({ draft }) => {
      const issues = issuesOf(draft.items)
      return issues.length > 0
        ? Effect.fail(new ValidationFailed({ issues }))
        : Effect.succeed({ id: "ord_1", items: draft.items, totalCents: 0 })
    },
    priceOrder: ({ order, user }) => {
      const total = order.items.reduce((sum, item) => sum + item.qty * 1250, 0)
      const totalCents = user.role === "Admin" ? total - Math.floor(total / 10) : total
      return Effect.succeed({ ...order, totalCents })
    }
  },
  machines: {
    Checkout: CheckoutLive
  }
})
