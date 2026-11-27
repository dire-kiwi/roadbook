/**
 * Saving and restoring through the API (execution.md, "Saving and restoring"): `restore` and
 * its refusals, `actor.save`, `actor.suspend`, and the request handler of
 * docs/design/semantics.md built on `resume`, with a store, a migration and virtual time.
 */
import { assert, describe, it } from "@effect/vitest"
import type * as Core from "@dire-kiwi/roadbook-core"
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
      assert.strictEqual(saved.snapshotVersion, 4)
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
      assert.isTrue(Exit.isFailure(Schema.decodeUnknownExit(Roadbook.Saved)({ ...decoded, snapshotVersion: 5 })))
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
      assert.strictEqual(refusal(yield* restore(CounterLive, { ...saved, snapshotVersion: 5 })), "invalid")
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
      const refused = yield* Effect.exit(CounterLive.resume({ ...saved, snapshotVersion: 5 } as never, { _tag: "Nope" } as never))
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

  it.effect("history: saved as version 2, a version 1 snapshot restored with no records, no history state reported", () =>
    Effect.gen(function*() {
      const historyIr = JSON.parse(fs.readFileSync(path.join(charts, "history.ir.json"), "utf8")) as Core.Ir
      const EditorLive = impl("Editor", { actions: { log: () => Effect.void }, tasks: { fetch: () => Effect.never } }, historyIr)
      const actor = yield* EditorLive.spawn({})
      yield* actor.send({ _tag: "Next" })
      yield* actor.send({ _tag: "Help" })
      yield* actor.send({ _tag: "BackDeep" })
      yield* settle
      const saved = yield* actor.save
      assert.strictEqual(saved.snapshotVersion, 4)
      assert.deepStrictEqual(saved.history, {
        "Editor.Work.Deep": ["Editor.Work.Review.Read"],
        "Editor.Work.Shallow": ["Editor.Work.Review"]
      })
      assert.deepStrictEqual((yield* actor.snapshot).active, ["Editor", "Editor.Work", "Editor.Work.Review", "Editor.Work.Review.Read"])
      // Version 1, in Help: it has no records, so going back enters Work by default.
      const { history: _, spawned: __, ...rest } = yield* Effect.scoped(Effect.gen(function*() {
        const other = yield* EditorLive.spawn({})
        yield* other.send({ _tag: "Help" })
        yield* settle
        return yield* other.save
      }))
      const v1 = { ...rest, snapshotVersion: 1 } as Roadbook.Saved
      const restored = yield* EditorLive.restore(v1)
      const resaved = yield* restored.save
      assert.strictEqual(resaved.snapshotVersion, 4)
      assert.deepStrictEqual(resaved.history, {})
      yield* restored.send({ _tag: "BackDeep" })
      yield* settle
      const snapshot = yield* restored.snapshot
      assert.deepStrictEqual(snapshot.active, ["Editor", "Editor.Work", "Editor.Work.Draft"])
      assert.isFalse(snapshot.matches("Editor.Work.Deep"))
    }).pipe(Effect.scoped))
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

describe("child runs in a saved snapshot", () => {
  // kept.road: a Desk keeps a Tally, which counts what it is sent and naps after five seconds.
  const keptIr = JSON.parse(fs.readFileSync(path.join(charts, "kept.ir.json"), "utf8")) as Core.Ir
  const log = { actions: { log: () => Effect.void } }
  const TallyLive = impl("Tally", log, keptIr)
  const DeskLive = impl("Desk", { ...log, machines: { Tally: TallyLive } }, keptIr)

  it.effect("save nests the child's snapshot; resume restores it where it was and suspends the idle tree", () =>
    Effect.gen(function*() {
      const { records, sink } = collect()
      const saved = yield* Effect.scoped(Effect.gen(function*() {
        const desk = yield* DeskLive.spawn({ start: 1 })
        yield* desk.send({ _tag: "Bump", by: 2 })
        yield* settle
        return yield* desk.save
      }))
      const child = saved.invokes[0]!.child!
      assert.strictEqual(child.run, `${saved.run}.1`)
      assert.deepStrictEqual(child.vars, { n: 3 })
      assert.deepStrictEqual(child.timers.map((t) => t.due), [5000])
      // A hub of one: resume returns once the tree is idle, the child's snapshot in the parent's.
      const resumed = yield* DeskLive.resume(saved, { _tag: "Bump", by: 4 }).pipe(Effect.provideService(Roadbook.TraceSink, sink))
      assert.deepStrictEqual(resumed.vars, { heard: 7 })
      assert.deepStrictEqual(resumed.invokes[0]!.child!.vars, { n: 7 })
      const kinds = (run: string) => records.filter((r) => r.run === run).map((r) => r.kind)
      const [root] = records
      assert.strictEqual(kinds(root!.run)[0], "restored")
      assert.strictEqual(kinds(`${root!.run}.1`)[0], "restored")
      assert.strictEqual(kinds(`${root!.run}.1`).at(-1), "suspended")
      assert.strictEqual(kinds(root!.run).at(-1), "suspended")
    }))

  it.effect("suspend stops a tree two levels deep, children first, and resume goes on from it", () =>
    Effect.gen(function*() {
      const OfficeLive = impl("Office", { ...log, machines: { Desk: DeskLive } }, keptIr)
      const { records, sink } = collect()
      const first = yield* Effect.scoped(Effect.gen(function*() {
        const office = yield* OfficeLive.spawn({})
        yield* office.send({ _tag: "Bump", by: 2 })
        return yield* office.suspend
      })).pipe(Effect.provideService(Roadbook.TraceSink, sink))
      const desk = first.invokes[0]!.child!
      const tally = desk.invokes[0]!.child!
      assert.deepStrictEqual([desk.run, tally.run], [`${first.run}.1`, `${first.run}.1.1`])
      assert.deepStrictEqual(desk.vars, { heard: 3 })
      assert.deepStrictEqual(tally.vars, { n: 3 })
      const suspended = records.filter((r) => r.kind === "suspended").map((r) => r.run)
      assert.deepStrictEqual(suspended, [tally.run, desk.run, first.run])
      // One request: the event reaches the grandchild, and the tree suspends again.
      const second = yield* OfficeLive.resume(first, { _tag: "Bump", by: 4 })
      assert.deepStrictEqual(second.invokes[0]!.child!.invokes[0]!.child!.vars, { n: 7 })
      assert.deepStrictEqual(second.invokes[0]!.child!.vars, { heard: 7 })
      // Closing it ends the whole tree.
      const done = yield* OfficeLive.resume(second, { _tag: "Close" })
      assert.strictEqual(done.status, "done")
    }))

  it.effect("a restored child takes its overdue timer before what its parent sent it before it claimed the invoke", () =>
    Effect.gen(function*() {
      const saved = yield* Effect.scoped(Effect.gen(function*() {
        const desk = yield* DeskLive.spawn({ start: 1 })
        yield* desk.send({ _tag: "Bump", by: 2 })
        yield* settle
        return yield* desk.save
      }))
      // The host code yields before it calls `run`, so the Bump the restored Desk passes on is
      // held for the child; the child's timer (due at 5000) is overdue at 10000.
      const SlowTally = { run: (input: unknown) => Effect.andThen(Effect.yieldNow, TallyLive.run(input as never)) }
      const SlowDesk = impl("Desk", { ...log, machines: { Tally: SlowTally } }, keptIr)
      const { records, sink } = collect()
      const timer: Roadbook.Timer = { now: Effect.succeed(10_000), sleep: () => Effect.never }
      const after = yield* Effect.gen(function*() {
        const desk = yield* SlowDesk.restore(saved)
        yield* desk.send({ _tag: "Bump", by: 4 })
        yield* settle
        return yield* desk.save
      }).pipe(Effect.scoped, Effect.provideService(Roadbook.Timer, timer), Effect.provideService(Roadbook.TraceSink, sink))
      const taken = records
        .filter((r) => r.kind === "event" && r.run.endsWith(".1"))
        .map((r) => (r as { event: { kind: string } }).event.kind)
      assert.deepStrictEqual(taken, ["after", "message"])
      assert.deepStrictEqual(after.invokes[0]!.child!.active, ["Tally", "Tally.Counting"])
      assert.deepStrictEqual(after.invokes[0]!.child!.vars, { n: 7 })
    }))

  it.effect("a child run bound to another machine than its snapshot's ends with a host defect", () =>
    Effect.gen(function*() {
      const saved = yield* Effect.scoped(Effect.gen(function*() {
        const desk = yield* DeskLive.spawn({ start: 1 })
        yield* settle
        return yield* desk.save
      }))
      // The Tally invoke runs a Desk, which claims the token with the Tally's snapshot.
      const OtherDesk = impl("Desk", { ...log, machines: { Tally: { run: (input: unknown) => Effect.suspend(() => DeskLive.run(input as never)) } } }, keptIr)
      const { records, sink } = collect()
      const restored = yield* OtherDesk.restore(saved).pipe(Effect.provideService(Roadbook.TraceSink, sink))
      const exit = yield* Effect.exit(restored.result)
      assert.isTrue(Exit.isFailure(exit))
      const event = records.find((r) => r.kind === "event")
      assert.strictEqual((event as { event: { kind: string } }).event.kind, "invoke.defect")
      // The child run wrote nothing.
      assert.isFalse(records.some((r) => r.run.includes(".")))
    }).pipe(Effect.scoped))
})
