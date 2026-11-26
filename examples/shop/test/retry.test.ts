/**
 * The retry path under a virtual clock: `after` timers sleep on Effect's `Clock` through the
 * default `Timer`, so `TestClock` moves them without real waiting.
 */
import { assert, describe, it } from "@effect/vitest"
import * as Roadbook from "@roadbook/effect"
import { Effect, Fiber } from "effect"
import { TestClock } from "effect/testing"
import { PlaceOrderLive } from "../src/machines.js"
import { FakeServices } from "../src/services.js"

describe("the retry path", () => {
  it.effect("retries a gateway that stays down three times, 20, 40 and 80 ms apart, then gives up", () =>
    Effect.gen(function*() {
      const records: Array<Roadbook.Core.TraceRecord> = []
      const fiber = yield* PlaceOrderLive.run({
        token: "Bearer good",
        draft: { items: [{ sku: "flaky", qty: 1 }], coupon: null }
      }).pipe(
        Effect.provide(FakeServices),
        Effect.provideService(Roadbook.TraceSink, { write: (record) => records.push(record) }),
        Effect.forkChild
      )
      for (const ms of [20, 40, 80]) {
        yield* TestClock.adjust(ms)
      }
      const response = yield* Fiber.join(fiber)
      assert.deepStrictEqual(response, { status: 402, body: { code: "payment_failed" } })

      const child = records.filter((r) => r.run.split(".").length === 2)
      const delays = child.flatMap((r) => (r.kind === "timer.started" ? [r.delay] : []))
      assert.deepStrictEqual(delays, [20, 40, 80])
      const charges = child.filter((r) => r.kind === "invoke.started")
      assert.strictEqual(charges.length, 4)
      const failed = child.at(-1)
      assert.deepStrictEqual(failed?.kind, "failed")
      // Virtual time: the child gave up 140 ms after it started, by the test clock.
      assert.strictEqual(failed!.t - child[0]!.t, 140)
    }))
})
