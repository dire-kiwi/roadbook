/** The driver's behaviour beyond the conformance cases: the Effect-facing parts of the API. */
import { assert, describe, it } from "@effect/vitest"
import * as Core from "@roadbook/core"
import { Duration, Effect, Exit, Fiber, Stream } from "effect"
import { TestClock } from "effect/testing"
import * as fs from "node:fs"
import * as path from "node:path"
import * as Roadbook from "../src/index.ts"
import { Early, Go, Leftover, Overflow, Settle } from "./charts/runtime.road.ts"

const charts = path.resolve(import.meta.dirname, "../../../spec/conformance/charts")
const readIr = (file: string): unknown => JSON.parse(fs.readFileSync(path.join(charts, file), "utf8"))

/** `implement` without the static checks, for the hand-picked IR of these tests. */
const impl = (machine: Roadbook.Machine<any>, handlers: object): Roadbook.Implemented<any, never> =>
  machine.implement(handlers as never) as Roadbook.Implemented<any, never>

const collect = () => {
  const records: Array<Core.TraceRecord> = []
  return { records, sink: { write: (record: Core.TraceRecord) => void records.push(record) } }
}

describe("make", () => {
  it("refuses another IR version", () => {
    assert.throws(() => Roadbook.make({ irVersion: "9.9" }, "X"), /IR version/)
  })
  it("refuses a machine with a parallel state at implement", () => {
    const ir = readIr("nest.ir.json") as Core.Ir
    const nest = ir.machines["Nest"]!
    const parallel = { ...ir, machines: { Nest: { ...nest, root: { ...nest.root, kind: "parallel" } } } }
    const machine = Roadbook.make<any>(parallel, "Nest")
    assert.throws(() => impl(machine, {}), /parallel state/)
  })
})

describe("run", () => {
  const Parent = Roadbook.make<any>(readIr("cancel.ir.json"), "Parent")
  const Child = Roadbook.make<any>(readIr("cancel.ir.json"), "Child")
  const logged: Array<string> = []
  const log = ({ what }: { what: string }) => Effect.sync(() => void logged.push(what))
  const ChildLive = impl(Child, { actions: { log } })
  const ParentLive = impl(Parent, { actions: { log }, machines: { Child: ChildLive } })

  it.effect("interrupting a root run cancels it gracefully, child first", () =>
    Effect.gen(function*() {
      logged.length = 0
      const { records, sink } = collect()
      const fiber = yield* ParentLive.run({}).pipe(
        Effect.provideService(Roadbook.TraceSink, sink),
        Effect.provideService(Roadbook.RootRunId, () => "root"),
        Effect.forkChild
      )
      yield* TestClock.adjust(1000)
      const exit = yield* Fiber.interrupt(fiber).pipe(Effect.andThen(Fiber.await(fiber)))
      assert.isTrue(Exit.hasInterrupts(exit))
      assert.deepStrictEqual(logged, ["exit Inner", "exit Outer", "exit Child", "exit Running", "exit Parent"])
      const last = (run: string) => records.filter((r) => r.run === run).at(-1)
      assert.deepStrictEqual(last("root"), { kind: "cancelled", t: 1000, run: "root" })
      assert.deepStrictEqual(last("root.1"), { kind: "cancelled", t: 1000, run: "root.1", parent: "root" })
    }))

  it.effect("a child that finishes ends its parent through the parent's chart", () =>
    Effect.gen(function*() {
      const fiber = yield* ParentLive.run({}).pipe(Effect.forkChild)
      yield* TestClock.adjust(5000)
      const result = yield* Fiber.join(fiber)
      assert.strictEqual(result, undefined)
    }))

  it.effect("a host defect dies with a Defect holding the defect object", () =>
    Effect.gen(function*() {
      const Failing = impl(Parent, {
        actions: { log: ({ what }: { what: string }) => (what === "exit Running" ? Effect.die(new Error("disk full")) : Effect.void) },
        machines: { Child: ChildLive }
      })
      const fiber = yield* Failing.run({}).pipe(Effect.forkChild)
      yield* TestClock.adjust(5000)
      const exit = yield* Fiber.await(fiber)
      assert.isTrue(Exit.isFailure(exit))
      const defect = Exit.isFailure(exit) ? exit.cause.reasons.find((r) => r._tag === "Die") : undefined
      assert.instanceOf(defect && "defect" in defect ? defect.defect : undefined, Roadbook.Defect)
      assert.deepStrictEqual((defect as { defect: Roadbook.Defect }).defect.defect, { kind: "host", message: "disk full" })
    }))
})

describe("values at the boundary", () => {
  const Checkout = Roadbook.make<any>(readIr("shop/checkout.ir.json"), "Checkout")
  const input = { order: { id: "o1", items: [{ sku: "a", qty: 1 }], totalCents: 1 }, user: { id: "u1", role: "Customer" } }
  class GatewayUnavailable {
    readonly _tag = "GatewayUnavailable"
  }

  it.effect("a negative duration from a function is a host defect", () =>
    Effect.gen(function*() {
      const Live = impl(Checkout, {
        fns: { retryDelay: () => Duration.millis(-5) },
        tasks: { chargeCard: () => Effect.fail(new GatewayUnavailable()) }
      })
      const exit = yield* Effect.exit(Live.run(input))
      const die = Exit.isFailure(exit) ? exit.cause.reasons.find((r) => r._tag === "Die") : undefined
      const defect = (die as { defect: Roadbook.Defect } | undefined)?.defect.defect
      assert.strictEqual(defect?.kind, "host")
      assert.match((defect as { message: string }).message, /negative duration/)
    }))

  it.effect("errors reach the caller as instances of the error classes make was given", () =>
    Effect.gen(function*() {
      class CardDeclined {
        readonly _tag = "CardDeclined"
        readonly reason: string
        constructor(fields: { reason: string }) {
          this.reason = fields.reason
        }
      }
      const WithClasses = Roadbook.make<any>(readIr("shop/checkout.ir.json"), "Checkout", { CardDeclined })
      const Live = impl(WithClasses, {
        fns: { retryDelay: () => Duration.millis(1) },
        tasks: { chargeCard: () => Effect.fail({ _tag: "CardDeclined", reason: "expired" }) }
      })
      const error = yield* Effect.flip(Live.run(input))
      assert.instanceOf(error, CardDeclined)
      assert.strictEqual((error as CardDeclined).reason, "expired")
    }))

  it.effect("an int outside ±(2^53 - 1) in the input is a host defect", () =>
    Effect.gen(function*() {
      const Live = impl(Checkout, { fns: { retryDelay: () => Duration.millis(1) }, tasks: { chargeCard: () => Effect.never } })
      const exit = yield* Effect.exit(Live.run({ ...input, order: { ...input.order, totalCents: 2 ** 60 } }))
      assert.isTrue(Exit.hasDies(exit))
    }))
})

describe("spawn", () => {
  const Parent = Roadbook.make<any>(readIr("cancel.ir.json"), "Parent")
  const Child = Roadbook.make<any>(readIr("cancel.ir.json"), "Child")
  const log = () => Effect.void
  const ParentLive = impl(Parent, { actions: { log }, machines: { Child: impl(Child, { actions: { log } }) } })

  it.effect("an actor takes events and reports its snapshot", () =>
    Effect.gen(function*() {
      const actor = yield* ParentLive.spawn({})
      const before = yield* actor.snapshot
      assert.isTrue(before.matches("Parent.Running"))
      yield* actor.send({ _tag: "Stop" })
      const result = yield* actor.result
      assert.strictEqual(result, undefined)
      const after = yield* actor.snapshot
      assert.strictEqual(after.status, "done")
      assert.deepStrictEqual(after.active, [])
    }).pipe(Effect.scoped))

  it.effect("closing the scope cancels the actor", () =>
    Effect.gen(function*() {
      const { records, sink } = collect()
      yield* ParentLive.spawn({}).pipe(Effect.provideService(Roadbook.TraceSink, sink), Effect.scoped)
      assert.deepStrictEqual(records.filter((r) => r.kind === "cancelled").map((r) => r.run).sort(), [
        records[0]!.run,
        `${records[0]!.run}.1`
      ])
    }))
})

describe("cancelling a task or stub", () => {
  // A cancelled task or stub is interrupted at once; whatever it does while being interrupted
  // is ignored, and nothing waits for it (execution.md, "Child runs").
  const Race = Roadbook.make<any>(readIr("race.ir.json"), "Race")
  const Parent = Roadbook.make<any>(readIr("cancel.ir.json"), "Parent")
  const stuck = () => Effect.uninterruptible(Effect.never)
  const slowFinalizer = () => Effect.never.pipe(Effect.onInterrupt(() => Effect.sleep("10 seconds")))

  it.effect("leaving the state does not wait for an uninterruptible task", () =>
    Effect.gen(function*() {
      const Live = impl(Race, { tasks: { slow: stuck, quick: () => Effect.succeed(7) } })
      const actor = yield* Live.spawn({})
      yield* actor.send({ _tag: "Skip" })
      assert.strictEqual(yield* actor.result, 7)
    }).pipe(Effect.scoped))

  it.effect("a slow finalizer does not delay what comes after invoke.cancelled", () =>
    Effect.gen(function*() {
      const { records, sink } = collect()
      const Live = impl(Race, { tasks: { slow: slowFinalizer, quick: () => Effect.as(Effect.sleep("1 second"), 7) } })
      const actor = yield* Live.spawn({}).pipe(Effect.provideService(Roadbook.TraceSink, sink))
      yield* actor.send({ _tag: "Skip" })
      yield* TestClock.adjust("1 second")
      assert.strictEqual((yield* actor.snapshot).status, "done")
      assert.strictEqual(yield* actor.result, 7)
      const from = records.findIndex((r) => r.kind === "invoke.cancelled")
      assert.deepStrictEqual(records.slice(from).map((r) => `${r.kind} ${r.t}`), [
        "invoke.cancelled 0",
        "exited 0",
        "entered 0",
        "invoke.started 0",
        "event 1000",
        "transition 1000",
        "exited 1000",
        "set 1000",
        "entered 1000",
        "exited 1000",
        "exited 1000",
        "done 1000"
      ])
    }).pipe(Effect.scoped))

  it.effect("closing the actor's scope does not wait for an uninterruptible stub", () =>
    Effect.gen(function*() {
      const { records, sink } = collect()
      const Live = impl(Parent, { actions: { log: () => Effect.void }, machines: { Child: stuck } })
      yield* Live.spawn({}).pipe(Effect.provideService(Roadbook.TraceSink, sink), Effect.scoped)
      assert.strictEqual(records.at(-1)?.kind, "cancelled")
    }))

  it.effect("cancel does not wait for a task with a slow finalizer", () =>
    Effect.gen(function*() {
      const Live = impl(Race, { tasks: { slow: slowFinalizer, quick: () => Effect.succeed(7) } })
      const actor = yield* Live.spawn({})
      yield* actor.cancel
      assert.strictEqual((yield* actor.snapshot).status, "cancelled")
    }).pipe(Effect.scoped))
})

describe("an actor's lifetime", () => {
  const Parent = Roadbook.make<any>(readIr("cancel.ir.json"), "Parent")
  const Child = Roadbook.make<any>(readIr("cancel.ir.json"), "Child")
  const logged: Array<string> = []
  const log = ({ what }: { what: string }) => Effect.sync(() => void logged.push(what))
  const ParentLive = impl(Parent, { actions: { log }, machines: { Child: impl(Child, { actions: { log } }) } })

  it.effect("send, result and cancel may be called again after the run is done", () =>
    Effect.gen(function*() {
      const actor = yield* ParentLive.spawn({})
      yield* actor.send({ _tag: "Stop" })
      assert.strictEqual(yield* actor.result, undefined)
      assert.strictEqual(yield* actor.result, undefined)
      yield* actor.send({ _tag: "Stop" })
      yield* actor.send({ _tag: "Stop" })
      yield* actor.cancel
      yield* actor.cancel
      assert.strictEqual((yield* actor.snapshot).status, "done")
      assert.strictEqual(yield* actor.result, undefined)
    }).pipe(Effect.scoped))

  it.effect("send, result and cancel may be called again after the run is cancelled", () =>
    Effect.gen(function*() {
      const { records, sink } = collect()
      const actor = yield* ParentLive.spawn({}).pipe(Effect.provideService(Roadbook.TraceSink, sink))
      yield* actor.cancel
      yield* actor.cancel
      yield* actor.send({ _tag: "Stop" })
      yield* actor.send({ _tag: "Stop" })
      assert.isTrue(Exit.hasInterrupts(yield* Effect.exit(actor.result)))
      assert.isTrue(Exit.hasInterrupts(yield* Effect.exit(actor.result)))
      assert.strictEqual((yield* actor.snapshot).status, "cancelled")
      assert.strictEqual(records.filter((r) => r.kind === "cancelled" && r.parent === undefined).length, 1)
    }).pipe(Effect.scoped))

  it.effect("changes ends after the final snapshot", () =>
    Effect.gen(function*() {
      const actor = yield* ParentLive.spawn({})
      const seen = yield* Effect.forkChild(Stream.runCollect(actor.changes))
      yield* Effect.yieldNow
      yield* actor.send({ _tag: "Stop" })
      const all = yield* Fiber.join(seen)
      assert.deepStrictEqual(all.map((s) => s.status), ["running", "done"])
      // Subscribed after the end: the final snapshot, then the end.
      const late = yield* Stream.runCollect(actor.changes)
      assert.deepStrictEqual(late.map((s) => s.status), ["done"])
    }).pipe(Effect.scoped))

  it.effect("changes ends after a cancelled run's final snapshot", () =>
    Effect.gen(function*() {
      const actor = yield* ParentLive.spawn({})
      const seen = yield* Effect.forkChild(Stream.runCollect(actor.changes))
      yield* Effect.yieldNow
      yield* actor.cancel
      const all = yield* Fiber.join(seen)
      assert.strictEqual(all.at(-1)?.status, "cancelled")
    }).pipe(Effect.scoped))

  it.effect("cancel is taken before events already queued", () =>
    Effect.gen(function*() {
      logged.length = 0
      const { records, sink } = collect()
      const actor = yield* ParentLive.spawn({}).pipe(Effect.provideService(Roadbook.TraceSink, sink))
      yield* actor.send({ _tag: "Stop" })
      yield* actor.send({ _tag: "Stop" })
      yield* actor.cancel
      assert.notInclude(logged, "stop")
      assert.strictEqual(records.filter((r) => r.kind === "event" && r.parent === undefined).length, 0)
      assert.strictEqual((yield* actor.snapshot).status, "cancelled")
    }).pipe(Effect.scoped))

  it.effect("cancel is taken before events queued while a step runs", () =>
    Effect.gen(function*() {
      const Nest = Roadbook.make<any>(readIr("nest.ir.json"), "Nest")
      const seen: Array<string> = []
      const Live = impl(Nest, {
        actions: {
          log: ({ what }: { what: string }) =>
            Effect.andThen(Effect.sync(() => void seen.push(what)), what === "bump" ? Effect.sleep("1 second") : Effect.void)
        },
        tasks: { work: () => Effect.never }
      })
      const actor = yield* Live.spawn({})
      yield* actor.send({ _tag: "Bump" })
      yield* Effect.yieldNow
      assert.strictEqual(seen.at(-1), "bump") // the step is in progress
      yield* actor.send({ _tag: "Bump" })
      yield* actor.send({ _tag: "Leave" })
      const cancelling = yield* Effect.forkChild(actor.cancel)
      yield* Effect.yieldNow
      yield* TestClock.adjust("1 second")
      yield* Fiber.join(cancelling)
      assert.deepStrictEqual(seen.slice(seen.indexOf("bump")), ["bump", "exit Inner", "exit Middle", "exit Outer", "exit Nest"])
      assert.strictEqual((yield* actor.snapshot).status, "cancelled")
    }).pipe(Effect.scoped))
})

describe("defects the conformance cases leave out", () => {
  const defectOf = (exit: Exit.Exit<unknown, unknown>) => {
    const die = Exit.isFailure(exit) ? exit.cause.reasons.find((r) => r._tag === "Die") : undefined
    assert.instanceOf((die as { defect: unknown } | undefined)?.defect, Roadbook.Defect)
    return (die as { defect: Roadbook.Defect }).defect.defect
  }
  const lastRecord = (records: ReadonlyArray<Core.TraceRecord>) => {
    const { t: _t, run: _run, ...rest } = records.at(-1)!
    return rest
  }

  it.effect("an int result outside ±(2^53 - 1) is overflow", () =>
    Effect.gen(function*() {
      const { records, sink } = collect()
      const exit = yield* Effect.exit(Overflow.implement({}).run({}).pipe(Effect.provideService(Roadbook.TraceSink, sink)))
      const defect = defectOf(exit)
      assert.strictEqual(defect.kind, "overflow")
      assert.deepStrictEqual(lastRecord(records), { kind: "defect", defect })
    }))

  it.effect("settling past 1000 microsteps is limit", () =>
    Effect.gen(function*() {
      const { records, sink } = collect()
      const exit = yield* Effect.exit(Settle.implement({}).run({}).pipe(Effect.provideService(Roadbook.TraceSink, sink)))
      assert.deepStrictEqual(defectOf(exit), { kind: "limit" })
      assert.deepStrictEqual(lastRecord(records), { kind: "defect", defect: { kind: "limit" } })
    }))

  it.effect("reading a variable in another's initial value before it has one is unassigned", () =>
    Effect.gen(function*() {
      const exit = yield* Effect.exit(Early.implement({}).run({}))
      assert.deepStrictEqual(defectOf(exit), { kind: "unassigned", var: "a" })
    }))

  it.effect("a run that ends with work left over interrupts it and does not wait for it", () =>
    Effect.gen(function*() {
      // Inner's exit effect dies before the step's invoke.cancel of Outer's work.
      let interrupted = false
      const Live = Leftover.implement({
        actions: { log: () => Effect.die(new Error("boom")) },
        tasks: {
          work: () =>
            Effect.never.pipe(Effect.onInterrupt(() => Effect.andThen(Effect.sync(() => (interrupted = true)), Effect.sleep("10 seconds"))))
        }
      })
      const actor = yield* Live.spawn({})
      yield* actor.send(Go.make({}))
      assert.deepStrictEqual(defectOf(yield* Effect.exit(actor.result)), { kind: "host", message: "boom" })
      yield* Effect.yieldNow
      assert.isTrue(interrupted)
    }).pipe(Effect.scoped))

  it.effect("a run that ends with an uninterruptible task left over ends at once", () =>
    Effect.gen(function*() {
      const Live = Leftover.implement({
        actions: { log: () => Effect.die(new Error("boom")) },
        tasks: { work: () => Effect.uninterruptible(Effect.never) }
      })
      const actor = yield* Live.spawn({})
      yield* actor.send(Go.make({}))
      assert.isTrue(Exit.hasDies(yield* Effect.exit(actor.result)))
    }).pipe(Effect.scoped))
})
