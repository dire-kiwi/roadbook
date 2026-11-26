/** `POST /orders` on Express: one PlaceOrder run per request, on a `ManagedRuntime`. */
import { Option, Schema } from "effect"
import type { ManagedRuntime } from "effect"
import express from "express"
import { OrderDraft } from "../types.road.js"
import { PlaceOrderLive } from "./machines.js"
import type { Mailer, Payments, Users } from "./services.js"

const decodeDraft = Schema.decodeUnknownOption(OrderDraft)

/** The body as an `OrderDraft`; `coupon` may be omitted, meaning `null`. */
const draftOf = (body: unknown): Option.Option<OrderDraft> =>
  typeof body === "object" && body !== null && !Array.isArray(body)
    ? decodeDraft({ coupon: null, ...body })
    : Option.none()

export const makeApp = (runtime: ManagedRuntime.ManagedRuntime<Users | Payments | Mailer, never>) => {
  const app = express()
  app.use(express.json())

  app.post("/orders", async (req, res) => {
    const draft = draftOf(req.body)
    if (Option.isNone(draft)) {
      res.status(400).json({ code: "bad_request" })
      return
    }
    const response = await runtime.runPromise(
      PlaceOrderLive.run({ token: req.headers.authorization ?? "", draft: draft.value })
    )
    res.status(response.status).json(response.body)
  })

  // A body that is not JSON at all.
  app.use((_error: unknown, _req: express.Request, res: express.Response, _next: express.NextFunction) => {
    res.status(400).json({ code: "bad_request" })
  })
  return app
}
