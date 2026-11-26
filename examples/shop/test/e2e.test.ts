/** Every request/response row of README.md, against the Express app on an ephemeral port. */
import { ManagedRuntime } from "effect"
import type { AddressInfo } from "node:net"
import type { Server } from "node:http"
import { afterAll, beforeAll, describe, expect, it } from "vitest"
import { makeApp } from "../src/app.js"
import { FakeServices } from "../src/services.js"

const runtime = ManagedRuntime.make(FakeServices)
let server: Server
let base: string

beforeAll(async () => {
  server = await new Promise<Server>((resolve) => {
    const s = makeApp(runtime).listen(0, () => resolve(s))
  })
  base = `http://127.0.0.1:${(server.address() as AddressInfo).port}`
})

afterAll(async () => {
  await new Promise((resolve) => server.close(resolve))
  await runtime.dispose()
})

const apple = { items: [{ sku: "apple", qty: 2 }] }

const rows: ReadonlyArray<{
  readonly name: string
  readonly authorization: string | null
  readonly body: unknown
  readonly status: number
  readonly response: unknown
}> = [
  {
    name: "a customer's order",
    authorization: "Bearer good",
    body: apple,
    status: 201,
    response: { orderId: "ord_1", chargeId: "ch_ord_1", totalCents: 2500 }
  },
  {
    name: "an admin's order, with the discount",
    authorization: "Bearer admin",
    body: apple,
    status: 201,
    response: { orderId: "ord_1", chargeId: "ch_ord_1", totalCents: 2250 }
  },
  { name: "an unknown token", authorization: "Bearer nope", body: apple, status: 401, response: { code: "unauthenticated" } },
  { name: "no Authorization header", authorization: null, body: apple, status: 401, response: { code: "unauthenticated" } },
  {
    name: "no items",
    authorization: "Bearer good",
    body: { items: [] },
    status: 422,
    response: { code: "invalid", issues: ["order has no items"] }
  },
  {
    name: "bad quantities",
    authorization: "Bearer good",
    body: { items: [{ sku: "a", qty: 0 }, { sku: "b", qty: -1 }] },
    status: 422,
    response: { code: "invalid", issues: ["a: quantity must be at least 1", "b: quantity must be at least 1"] }
  },
  {
    name: "a declined card",
    authorization: "Bearer good",
    body: { items: [{ sku: "declined", qty: 1 }] },
    status: 402,
    response: { code: "payment_failed" }
  },
  {
    name: "a gateway that stays down, after three retries",
    authorization: "Bearer good",
    body: { items: [{ sku: "flaky", qty: 1 }] },
    status: 402,
    response: { code: "payment_failed" }
  },
  {
    name: "a body that is not an OrderDraft",
    authorization: "Bearer good",
    body: { items: "nope" },
    status: 400,
    response: { code: "bad_request" }
  }
]

describe("POST /orders", () => {
  it.each(rows)("$name: $status", async (row) => {
    const headers: Record<string, string> = { "content-type": "application/json" }
    if (row.authorization !== null) headers["authorization"] = row.authorization
    const response = await fetch(`${base}/orders`, { method: "POST", headers, body: JSON.stringify(row.body) })
    expect(response.status).toBe(row.status)
    expect(await response.json()).toEqual(row.response)
  })

  it("answers a body that is not JSON with 400", async () => {
    const response = await fetch(`${base}/orders`, {
      method: "POST",
      headers: { "content-type": "application/json", authorization: "Bearer good" },
      body: "{"
    })
    expect(response.status).toBe(400)
    expect(await response.json()).toEqual({ code: "bad_request" })
  })
})
