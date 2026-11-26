/**
 * Saving and restoring through the API (execution.md, "Saving and restoring"): `restore` and
 * its refusals, `actor.save`, `actor.suspend`, and the request handler of
 * docs/design/semantics.md built on `resume`, with a store, a migration and virtual time.
 */
import { assert, describe, it } from "@effect/vitest"
import type * as Core from "@roadbook/core"
import { Cause, Clock, Deferred, Effect, Exit, Fiber, Schema, Stream } from "effect"
import { TestClock } from "effect/testing"
import * as fs from "node:fs"
import * as path from "node:path"
import * as Roadbook from "../src/index.ts"

const charts = path.resolve(import.meta.dirname, "../../../spec/conformance/charts")
const ir = JSON.parse(fs.readFileSync(path.join(charts, "saved.ir.json"), "utf8")) as Core.Ir
const ordersIr = JSON.parse(fs.readFileSync(path.join(charts, "orders.ir.json"), "utf8")) as Core.Ir

/** `implement` without the static checks, for the IR of these tests. */
const impl = (machine: string, handlers: object, from: Core.Ir = ir): Roadbook.Implemented<any, never> =>
  Roadbook.make<any>(from, machine).implement(handlers as never) as Roadbook.Implemented<any, never>

const collect = () => {
  const records: Array<Core.TraceRecord> = []
  return { records, sink: { write: (record: Core.TraceRecord) => void records.push(record) } }
}

/** Let every fiber that can run do so. */
const settle = Effect.gen(function*() {
  for (let i = 0; i < 20; i++) yield* Effect.yieldNow
})

const CounterLive = impl("Counter", { actions: { count: () => Effect.void } })

/** The reason a restore was refused with, or `undefined`. */
const refusal = (exit: Exit.Exit<unknown, unknown>) => {
  if (Exit.isSuccess(exit)) return undefined
  const error = exit.cause.reasons.find(Cause.isFailReason)?.error
  return error instanceof Roadbook.RestoreError ? error.reason : `not a RestoreError: ${String(exit.cause)}`
}

describe("save and restore", () => {
  it.effect("save gives the run as its last step left it; the restored run goes on from it", () =>
    Effect.gen(function*() {
      const { records, sink } = collect()
      const actor = yield* CounterLive.spawn({ start: 1 })
      yield* actor.send({ _tag: "Add", by: 2 })
      yield* settle
      const saved = yield* actor.save
      assert.strictEqual(saved.snapshotVersion, 1)
      assert.deepStrictEqual(saved.vars, { n: 3 })
      assert.deepStrictEqual(saved.active, ["Counter", "Counter.Counting"])
      // The run carries on.
      yield* actor.send({ _tag: "Finish" })
      assert.strictEqual(yield* actor.result, 3)
      const restored = yield* CounterLive.restore(saved).pipe(Effect.provideService(Roadbook.TraceSink, sink))
      assert.notStrictEqual(restored.handle.id, saved.run)
      assert.deepStrictEqual(records.map((r) => r.kind), ["restored"])
      yield* restored.send({ _tag: "Add", by: 4 })
      yield* restored.send({ _tag: "Finish" })
      yield* settle
      assert.strictEqual(yield* restored.result, 7)
    }).pipe(Effect.scoped))

  it.effect("a saved snapshot survives JSON and decodes with Roadbook.Saved", () =>
    Effect.gen(function*() {
      const actor = yield* CounterLive.spawn({ start: 5 })
      const text = JSON.stringify(yield* actor.save)
      const decoded = Schema.decodeUnknownSync(Roadbook.Saved)(JSON.parse(text))
      assert.strictEqual(decoded.vars["n"], 5)
      assert.isTrue(Exit.isFailure(Schema.decodeUnknownExit(Roadbook.Saved)({ ...decoded, snapshotVersion: 2 })))
      const restored = yield* CounterLive.restore(decoded)
      yield* restored.send({ _tag: "Finish" })
      assert.strictEqual(yield* restored.result, 5)
    }).pipe(Effect.scoped))

  it.effect("refusals, each with its reason, before anything is written", () =>
    Effect.gen(function*() {
      const { records, sink } = collect()
      const actor = yield* CounterLive.spawn({ start: 1 })
      const saved = yield* actor.save
      const done = yield* Effect.scoped(Effect.gen(function*() {
        const other = yield* CounterLive.spawn({ start: 1 })
        yield* other.send({ _tag: "Finish" })
        return yield* other.suspend
      }))
      const OrderLive = impl("Order", { actions: { refund: () => Effect.void }, tasks: { ship: () => Effect.never } })
      const old = { ...saved, ir: `sha256:${"0".repeat(64)}` }
      const restore = (live: Roadbook.Implemented<any, never>, value: unknown, options?: Roadbook.RestoreOptions) =>
        Effect.exit(live.restore(value as Roadbook.Saved, options).pipe(Effect.provideService(Roadbook.TraceSink, sink)))
      assert.strictEqual(refusal(yield* restore(CounterLive, {})), "invalid")
      assert.strictEqual(refusal(yield* restore(CounterLive, { ...saved, snapshotVersion: 2 })), "invalid")
      assert.strictEqual(refusal(yield* restore(CounterLive, { ...saved, extra: 1 })), "invalid")
      assert.strictEqual(refusal(yield* restore(OrderLive, saved)), "machine")
      assert.strictEqual(refusal(yield* restore(CounterLive, old)), "ir")
      // A migration that throws, and one that does not give the current hash.
      assert.strictEqual(refusal(yield* restore(CounterLive, old, { migrate: () => { throw new Error("no") } })), "migration")
      assert.strictEqual(refusal(yield* restore(CounterLive, old, { migrate: (s) => s })), "ir")
      // Its result is checked from the top.
      assert.strictEqual(refusal(yield* restore(CounterLive, old, { migrate: () => ({}) as Roadbook.Saved })), "invalid")
      assert.strictEqual(refusal(yield* restore(CounterLive, done)), "ended")
      // A snapshot that does not fit the machine.
      assert.strictEqual(refusal(yield* restore(CounterLive, { ...saved, active: ["Counter"] })), "invalid")
      assert.strictEqual(refusal(yield* restore(CounterLive, { ...saved, active: ["Counter.Counting", "Counter"] })), "invalid")
      assert.strictEqual(refusal(yield* restore(CounterLive, { ...saved, vars: { n: "three" } })), "invalid")
      assert.strictEqual(refusal(yield* restore(CounterLive, { ...saved, vars: { m: 3 } })), "invalid")
      assert.strictEqual(refusal(yield* restore(CounterLive, { ...saved, input: {} })), "invalid")
      assert.strictEqual(refusal(yield* restore(CounterLive, { ...saved, input: { start: 1.5 } })), "invalid")
      const timer = { id: 1, state: "Counter.Counting", index: 0, delay: 5, due: 5 }
      assert.strictEqual(refusal(yield* restore(CounterLive, { ...saved, timers: [timer], nextTimer: 2 })), "invalid")
      const invoke = { id: 1, state: "Counter.Counting", src: "ship" }
      assert.strictEqual(refusal(yield* restore(CounterLive, { ...saved, invokes: [invoke], nextInvoke: 2 })), "invalid")
      // A machine with a `ref` parameter cannot be restored yet.
      const OrdersLive = impl("Order", {}, ordersIr)
      const order = ordersIr.machines["Order"]!
      const withRef = { ...saved, machine: "Order", ir: order.hash, active: ["Order"] }
      assert.strictEqual(refusal(yield* restore(OrdersLive, withRef)), "ref")
      assert.deepStrictEqual(records, [])
    }).pipe(Effect.scoped))

  it.effect("suspend waits until no invoke runs, stops the run without exit effects, and ends changes", () =>
    Effect.gen(function*() {
      const { records, sink } = collect()
      let shipped: (tracking: string) => void = () => {}
      const OrderLive = impl("Order", {
        actions: { refund: () => Effect.void },
        tasks: { ship: () => Effect.callback<string>((resume) => void (shipped = (t) => resume(Effect.succeed(t)))) }
      })
      const actor = yield* OrderLive.spawn({ id: "o1" }).pipe(Effect.provideService(Roadbook.TraceSink, sink))
      const changes = yield* Effect.forkChild(Stream.runCollect(actor.changes))
      yield* actor.send({ _tag: "PaymentReceived", amount: 42 })
      const suspending = yield* Effect.forkChild(actor.suspend)
      yield* settle
      // The shipping task is running, so the run is not idle.
      assert.isUndefined(suspending.pollUnsafe())
      shipped("t-1")
      const saved = yield* Fiber.join(suspending)
      assert.strictEqual(saved.status, "running")
      assert.deepStrictEqual(saved.active, ["Order", "Order.AwaitingDelivery"])
      assert.deepStrictEqual(saved.invokes, [])
      assert.deepStrictEqual(saved.vars, { paid: 42, tracking: "t-1" })
      assert.strictEqual(records.at(-1)?.kind, "suspended")
      assert.isFalse(records.some((r) => r.kind === "exited" && r.state === "Order.AwaitingDelivery"))
      // A suspended run takes no more events; its result is interrupted; save and suspend
      // give the same snapshot again.
      yield* actor.send({ _tag: "Delivered" })
      assert.strictEqual(records.at(-1)?.kind, "suspended")
      assert.isTrue(Exit.hasInterrupts(yield* Effect.exit(actor.result)))
      assert.deepStrictEqual(yield* actor.save, saved)
      assert.deepStrictEqual(yield* actor.suspend, saved)
      const seen = yield* Fiber.join(changes)
      assert.deepStrictEqual(seen.at(-1)?.active, ["Order", "Order.AwaitingDelivery"])
    }).pipe(Effect.scoped))

  it.effect("suspending a run that has ended gives its snapshot; one that ended with a defect dies", () =>
    Effect.gen(function*() {
      const actor = yield* CounterLive.spawn({ start: 2 })
      yield* actor.send({ _tag: "Finish" })
      const saved = yield* actor.suspend
      assert.strictEqual(saved.status, "done")
      assert.deepStrictEqual(saved.result, { output: 2 })
      const Broken = impl("Counter", { actions: { count: () => Effect.die(new Error("boom")) } })
      const broken = yield* Broken.spawn({ start: 0 })
      yield* broken.send({ _tag: "Add", by: 1 })
      const exit = yield* Effect.exit(broken.suspend)
      const defect = Exit.isFailure(exit) ? exit.cause.reasons.find(Cause.isDieReason)?.defect : undefined
      assert.deepStrictEqual((defect as Roadbook.Defect).defect, { kind: "host", message: "boom" })
    }).pipe(Effect.scoped))

  it.effect("a restored timer keeps its deadline", () =>
    Effect.gen(function*() {
      const rings: Array<number> = []
      const ReminderLive = impl("Reminder", {
        actions: { log: () => Effect.flatMap(Clock.currentTimeMillis, (t) => Effect.sync(() => void rings.push(t))) }
      })
      const actor = yield* ReminderLive.spawn({})
      yield* TestClock.adjust(400)
      const saved = yield* actor.save
      assert.deepStrictEqual(saved.timers.map((t) => [t.delay, t.due]), [[1000, 1000]])
      yield* actor.cancel
      yield* TestClock.adjust(300)
      const restored = yield* ReminderLive.restore(saved)
      assert.deepStrictEqual((yield* restored.save).timers.map((t) => t.due), [1000])
      yield* TestClock.adjust(299)
      assert.deepStrictEqual(rings, [])
      yield* TestClock.adjust(1)
      yield* settle
      assert.deepStrictEqual(rings, [1000])
    }).pipe(Effect.scoped))
})

describe("saving and restoring, at the edges", () => {
  /** The `Defect` an exit died with, or `undefined`. */
  const defectOf = (exit: Exit.Exit<unknown, unknown>) =>
    Exit.isFailure(exit) ? exit.cause.reasons.find(Cause.isDieReason)?.defect : undefined

  it.effect("a cancel that arrives during a step is taken before a suspend that arrives after it", () =>
    Effect.gen(function*() {
      const { records, sink } = collect()
      const gate = yield* Deferred.make<void>()
      const Slow = impl("Counter", { actions: { count: () => Deferred.await(gate) } })
      const actor = yield* Slow.spawn({ start: 0 }).pipe(Effect.provideService(Roadbook.TraceSink, sink))
      yield* actor.send({ _tag: "Add", by: 1 })
      yield* settle
      // The step is waiting for the action; the cancel, then the suspend, arrive meanwhile.
      const cancelling = yield* Effect.forkChild(actor.cancel)
      yield* settle
      const suspending = yield* Effect.forkChild(actor.suspend)
      yield* settle
      yield* Deferred.succeed(gate, undefined)
      yield* Fiber.join(cancelling)
      const saved = yield* Fiber.join(suspending)
      assert.strictEqual(saved.status, "cancelled")
      assert.strictEqual(records.at(-1)?.kind, "cancelled")
      assert.isFalse(records.some((r) => r.kind === "suspended"))
    }).pipe(Effect.scoped))

  it.effect("what save, suspend and restore give and take is a copy", () =>
    Effect.gen(function*() {
      const actor = yield* CounterLive.spawn({ start: 1 })
      const saved = yield* actor.save
      ;(saved.vars as Record<string, Core.Json>)["n"] = 100
      assert.deepStrictEqual((yield* actor.save).vars, { n: 1 })
      // The restored run does not share the caller's snapshot either.
      const restored = yield* CounterLive.restore(yield* actor.save)
      const given = yield* actor.save
      const again = yield* CounterLive.restore(given)
      ;(given.vars as Record<string, Core.Json>)["n"] = 100
      yield* again.send({ _tag: "Finish" })
      assert.strictEqual(yield* again.result, 1)
      const suspended = yield* restored.suspend
      ;(suspended.vars as Record<string, Core.Json>)["n"] = 100
      assert.deepStrictEqual((yield* restored.save).vars, { n: 1 })
    }).pipe(Effect.scoped))

  it.effect("resume checks the event before anything runs: one that does not encode is the caller's defect", () =>
    Effect.gen(function*() {
      const { records, sink } = collect()
      const saved = yield* Effect.scoped(Effect.flatMap(CounterLive.spawn({ start: 1 }), (a) => a.suspend))
      const resume = (event: unknown) =>
        Effect.exit(CounterLive.resume(saved, event as never).pipe(Effect.provideService(Roadbook.TraceSink, sink)))
      for (const event of [{ _tag: "Add", by: "x" }, { _tag: "Nope" }, { _tag: "Add" }, 7]) {
        const defect = defectOf(yield* resume(event))
        assert.instanceOf(defect, Roadbook.Defect)
        assert.strictEqual((defect as Roadbook.Defect).defect.kind, "host")
      }
      assert.deepStrictEqual(records, [])
      // A refused snapshot is refused first, with its reason.
      const refused = yield* Effect.exit(CounterLive.resume({ ...saved, snapshotVersion: 2 } as never, { _tag: "Nope" } as never))
      assert.strictEqual(refusal(refused), "invalid")
      const fine = yield* resume({ _tag: "Add", by: 2 })
      assert.isTrue(Exit.isSuccess(fine) && fine.value.vars["n"] === 3)
    }))

  it.effect("refusals at the edges: a migration that throws anything, a running snapshot with a result, an unknown defect", () =>
    Effect.gen(function*() {
      const saved = yield* Effect.scoped(Effect.flatMap(CounterLive.spawn({ start: 1 }), (a) => a.save))
      const old = { ...saved, ir: `sha256:${"0".repeat(64)}` }
      const restore = (value: unknown, options?: Roadbook.RestoreOptions) =>
        Effect.exit(Effect.scoped(CounterLive.restore(value as Roadbook.Saved, options)))
      const throwing = (thrown: unknown) => ({
        migrate: () => {
          throw thrown
        }
      })
      assert.strictEqual(refusal(yield* restore(old, throwing(Object.create(null)))), "migration")
      assert.strictEqual(refusal(yield* restore(old, throwing(undefined))), "migration")
      // A migration to a snapshot of another machine.
      const reminder = yield* Effect.scoped(Effect.flatMap(impl("Reminder", { actions: { log: () => Effect.void } }).spawn({}), (a) => a.save))
      assert.strictEqual(refusal(yield* restore(old, { migrate: () => reminder })), "machine")
      assert.strictEqual(refusal(yield* restore({ ...saved, result: { output: 1 } })), "invalid")
      const defect = { ...saved, status: "defect", active: [], result: { defect: { kind: "cosmic", ray: 1 } } }
      assert.strictEqual(refusal(yield* restore(defect)), "ended")
    }))
})

describe("the request handler", () => {
  // docs/design/semantics.md, "Saving and restoring": each request loads the order's snapshot,
  // resumes it with one event, and stores what resume gives. Nothing stays in memory between
  // requests but the store.
  const HOUR = 3_600_000
  const store = new Map<string, string>()
  const load = (id: string) => Schema.decodeUnknownSync(Roadbook.Saved)(JSON.parse(store.get(id)!))
  const keep = (id: string, saved: Roadbook.Saved) => void store.set(id, JSON.stringify(saved))
  const refunds: Array<string> = []

  const OrderLive = impl("Order", {
    actions: { refund: ({ order, amount }: { order: string; amount: number }) => Effect.sync(() => void refunds.push(`${order} ${amount}`)) },
    // Shipping takes a moment: resume waits for it.
    tasks: { ship: ({ order }: { order: string }) => Effect.as(Effect.yieldNow, `track-${order}`) }
  })

  /** The first request: spawn the order and suspend it at once. */
  const create = (id: string) =>
    Effect.scoped(Effect.flatMap(OrderLive.spawn({ id }), (actor) => actor.suspend)).pipe(Effect.map((s) => keep(id, s)))

  /** Every later request. */
  const request = (id: string, event: unknown, options?: Roadbook.RestoreOptions) =>
    Effect.map(OrderLive.resume(load(id), event, options), (saved) => {
      keep(id, saved)
      return saved
    })

  it.effect("create, pay and deliver, one request each, through a chart change and a migration", () =>
    Effect.gen(function*() {
      const { records, sink } = collect()
      yield* create("o1").pipe(Effect.provideService(Roadbook.TraceSink, sink))
      assert.deepStrictEqual(load("o1").timers.map((t) => t.due), [72 * HOUR])

      yield* TestClock.adjust(HOUR)
      const paid = yield* request("o1", { _tag: "PaymentReceived", amount: 42 }).pipe(
        Effect.provideService(Roadbook.TraceSink, sink)
      )
      assert.deepStrictEqual(paid.active, ["Order", "Order.AwaitingDelivery"])
      assert.deepStrictEqual(paid.vars, { paid: 42, tracking: "track-o1" })
      assert.deepStrictEqual(paid.invokes, [])

      // A deployment changed the chart: the stored snapshot is now of an older version, in
      // which `paid` was called `amount`. Without a migration it is refused, and the store
      // keeps it; with one it goes on.
      const current = ir.machines["Order"]!.hash
      const older = `sha256:${"a".repeat(64)}`
      const { paid: amount, ...rest } = load("o1").vars
      keep("o1", { ...load("o1"), ir: older, vars: { ...rest, amount: amount! } })
      const refused = yield* Effect.exit(request("o1", { _tag: "Delivered" }))
      assert.strictEqual(refusal(refused), "ir")
      const migrate = (old: Roadbook.Saved): Roadbook.Saved => {
        const { amount, ...vars } = old.vars
        return { ...old, ir: current, vars: { ...vars, paid: amount ?? 0 } }
      }
      yield* TestClock.adjust(24 * HOUR)
      const delivered = yield* request("o1", { _tag: "Delivered" }, { migrate }).pipe(
        Effect.provideService(Roadbook.TraceSink, sink)
      )
      assert.strictEqual(delivered.status, "done")
      assert.deepStrictEqual(delivered.result, { output: null })
      const restored = records.filter((r) => r.kind === "restored")
      assert.deepStrictEqual(restored.map((r) => r.migrated), [null, older])
      assert.deepStrictEqual(records.filter((r) => r.kind === "suspended").map((r) => r.t), [0, HOUR])
      // A run that has ended cannot be resumed again.
      assert.strictEqual(refusal(yield* Effect.exit(request("o1", { _tag: "Delivered" }))), "ended")
    }))

  it.effect("a payment after the deadline: the expiry is taken first, and the payment refunded", () =>
    Effect.gen(function*() {
      yield* create("o2")
      yield* TestClock.adjust(73 * HOUR)
      const expired = yield* request("o2", { _tag: "PaymentReceived", amount: 7 })
      assert.deepStrictEqual(expired.active, ["Order", "Order.Expired"])
      assert.deepStrictEqual(expired.timers, [])
      assert.deepStrictEqual(refunds, ["o2 7"])
      // No event: only what fell due.
      const again = yield* request("o2", undefined)
      assert.deepStrictEqual(again.active, ["Order", "Order.Expired"])
    }))
})
