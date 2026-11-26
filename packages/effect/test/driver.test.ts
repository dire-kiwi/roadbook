/** The driver's behaviour beyond the conformance cases: the Effect-facing parts of the API. */
import { assert, describe, it } from "@effect/vitest"
import * as Core from "@roadbook/core"
import { Duration, Effect, Exit, Fiber } from "effect"
import { TestClock } from "effect/testing"
import * as fs from "node:fs"
import * as path from "node:path"
import * as Roadbook from "../src/index.ts"

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
