/** `POST /orders` on Express: one PlaceOrder run per request, on a `ManagedRuntime`. */
import { Option, Schema } from "effect"
import type { ManagedRuntime } from "effect"
import express from "express"
import { type ApiResponse, OrderDraft } from "../types.road.js"
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
    let response: ApiResponse
    try {
      response = await runtime.runPromise(
        PlaceOrderLive.run({ token: req.headers.authorization ?? "", draft: draft.value })
      )
    } catch (error) {
      // PlaceOrder declares no errors, so a run that does not succeed ended with a defect.
      console.error("PlaceOrder ended with a defect:", error)
      res.status(500).json({ code: "internal" })
      return
    }
    res.status(response.status).json(response.body)
  })

  // A body that is not JSON at all is the client's mistake; anything else is ours.
  app.use((error: unknown, _req: express.Request, res: express.Response, _next: express.NextFunction) => {
    const notJson = typeof error === "object" && error !== null && (error as { type?: unknown }).type === "entity.parse.failed"
    if (notJson) res.status(400).json({ code: "bad_request" })
    else res.status(500).json({ code: "internal" })
  })
  return app
}
