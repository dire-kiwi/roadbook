/**
 * The messaging examples end to end, under a virtual clock: examples/upload (a parent that
 * pauses and resumes the child it invokes, and shows the child's progress) and examples/orders
 * (orders calling a spawned Inventory peer they are given by handle). Every task sleeps on
 * Effect's `Clock`, so `TestClock` drives them.
 */
import { assert, describe, it } from "@effect/vitest"
import { Cause, Duration, Effect, Exit, Fiber } from "effect"
import { TestClock } from "effect/testing"
import { Inventory, type InventoryDef, OutOfStock } from "../../../examples/inventory/inventory.road.ts"
import { Order, PaymentDeclined } from "../../../examples/orders/orders.road.ts"
import { Publish, type PublishDef } from "../../../examples/upload/publish.road.ts"
import { Upload, UploadFailed } from "../../../examples/upload/upload.road.ts"
import * as Roadbook from "../src/index.ts"

/** Let every fiber that can run do so. */
const settle = Effect.gen(function*() {
  for (let i = 0; i < 20; i++) yield* Effect.yieldNow
})

const advance = (ms: number) => Effect.andThen(TestClock.adjust(ms), settle)

const ok = (value: unknown) => ({ value })
const failed = (error: unknown) => ({ error })

/** An exit as its value or its error, for comparing. */
const outcome = (exit: Exit.Exit<unknown, unknown>) =>
  Exit.isSuccess(exit) ? { value: exit.value } : { error: exit.cause.reasons.find(Cause.isFailReason)?.error }

const collect = () => {
  const records: Array<Roadbook.Core.TraceRecord> = []
  return { records, sink: { write: (record: Roadbook.Core.TraceRecord) => void records.push(record) } }
}

describe("examples/upload", () => {
  const CHUNK = 100
  const UploadLive = Upload.implement({
    fns: {
      percentOf: ({ file, offset }) => Math.round((100 * offset) / file.size),
      complete: ({ file, offset }) => offset >= file.size
    },
    tasks: {
      // Each chunk takes 10ms; the storage refuses every chunk of a file called "bad".
      sendChunk: ({ file, offset }) =>
        file.name === "bad"
          ? Effect.fail(new UploadFailed({ reason: "refused" }))
          : Effect.as(Effect.sleep(Duration.millis(10)), Math.min(offset + CHUNK, file.size)),
      finishUpload: ({ file }) => Effect.succeed(`https://files.example/${file.name}`)
    }
  })

  it.effect("pauses and resumes the child, and shows its progress", () =>
    Effect.gen(function*() {
      const shown: Array<number> = []
      const { records, sink } = collect()
      const PublishLive = Publish.implement({
        actions: { showProgress: ({ percent }) => Effect.sync(() => void shown.push(percent)) },
        tasks: { publishPost: ({ title, url }) => Effect.succeed({ title, url }) },
        machines: { Upload: UploadLive }
      })
      const actor = yield* PublishLive.spawn({ draft: { title: "Holiday", file: { name: "a.jpg", size: 300 } } }).pipe(
        Effect.provideService(Roadbook.TraceSink, sink),
        Effect.provideService(Roadbook.RootRunId, () => "publish")
      )
      yield* advance(10)
      assert.deepStrictEqual(shown, [33])

      // Paused: the chunk in flight is cancelled and nothing is sent until the user resumes.
      yield* actor.send({ _tag: "PauseClicked" })
      yield* settle
      yield* advance(1000)
      assert.deepStrictEqual(shown, [33])
      const child = records.filter((r) => r.run === "publish.1")
      assert.isTrue(child.some((r) => r.kind === "entered" && r.state === "Upload.Paused"))

      yield* actor.send({ _tag: "ResumeClicked" })
      yield* settle
      yield* advance(10)
      yield* advance(10)
      const post = yield* actor.result
      assert.deepStrictEqual(post, { title: "Holiday", url: "https://files.example/a.jpg" })
      assert.deepStrictEqual(shown, [33, 67, 100])

      // The parent's messages and the child's emitted events, as the trace has them.
      const sent = records.filter((r) => r.kind === "sent")
      assert.deepStrictEqual(sent.map((r) => [r.run, r.to, r.value._tag]), [
        ["publish", "publish.1", "Pause"],
        ["publish", "publish.1", "Resume"]
      ])
      const received = records.filter((r) => r.run === "publish" && r.kind === "event" && r.event.kind === "emitted")
      assert.strictEqual(received.length, 3)
    }).pipe(Effect.scoped))

  it.effect("a child's failure fails the parent through its chart", () =>
    Effect.gen(function*() {
      const PublishLive = Publish.implement({
        actions: { showProgress: () => Effect.void },
        tasks: { publishPost: ({ title, url }) => Effect.succeed({ title, url }) },
        machines: { Upload: UploadLive }
      })
      const exit = yield* Effect.exit(PublishLive.run({ draft: { title: "Bad", file: { name: "bad", size: 300 } } }))
      assert.deepStrictEqual(outcome(exit), failed(new UploadFailed({ reason: "refused" })))
    }))
})

describe("examples/orders", () => {
  /** The warehouse: what it holds, and what the actions did. */
  const warehouse = () => {
    const stock = new Map<string, number>([["apple", 5]])
    const log: Array<string> = []
    const InventoryLive = Inventory.implement({
      actions: {
        releaseStock: ({ orderId }) => Effect.sync(() => void log.push(`release ${orderId}`)),
        addStock: ({ sku, qty }) =>
          Effect.sync(() => {
            stock.set(sku, (stock.get(sku) ?? 0) + qty)
            log.push(`restock ${sku} ${qty}`)
          })
      },
      tasks: {
        // Takes 5ms; a sku called "slow" never answers.
        reserveStock: ({ items }) =>
          items.some((item) => item.sku === "slow")
            ? Effect.never
            : Effect.andThen(Effect.sleep(Duration.millis(5)), () => {
              const short = items.find((item) => (stock.get(item.sku) ?? 0) < item.qty)
              if (short !== undefined) return Effect.fail(new OutOfStock({ reason: `no ${short.sku}` }))
              for (const item of items) stock.set(item.sku, stock.get(item.sku)! - item.qty)
              return Effect.void
            })
      }
    })
    return { stock, log, InventoryLive }
  }

  const OrderLive = Order.implement({
    tasks: {
      charge: ({ orderId, draft }) =>
        draft.card === "declined"
          ? Effect.fail(new PaymentDeclined())
          : Effect.as(Effect.sleep(Duration.millis(5)), { orderId, total: 4200 })
    }
  })

  const apples = (qty: number, card = "4242") => ({ items: [{ sku: "apple", qty }], card })

  it.effect("orders reserve with a spawned Inventory passed by handle, and host code restocks it", () =>
    Effect.gen(function*() {
      const { InventoryLive, log, stock } = warehouse()
      const inventory = yield* InventoryLive.spawn({ warehouse: "w1" })
      assert.strictEqual(inventory.handle.machine, "Inventory")
      const order = (id: string, draft: ReturnType<typeof apples>) =>
        Effect.forkChild(Effect.exit(OrderLive.run({ id, draft, inventory: inventory.handle })))

      // Paid: reserved, then charged.
      const paid = yield* order("o1", apples(2))
      yield* advance(5)
      yield* advance(5)
      assert.deepStrictEqual(outcome(yield* Fiber.join(paid)), ok({ orderId: "o1", total: 4200 }))
      assert.strictEqual(stock.get("apple"), 3)

      // Declined: the order sends Release back as it fails.
      const declined = yield* order("o2", apples(1, "declined"))
      yield* advance(5)
      assert.deepStrictEqual(outcome(yield* Fiber.join(declined)), failed(new PaymentDeclined()))
      yield* settle
      assert.deepStrictEqual(log, ["release o2"])

      // Out of stock, until host code restocks through the handle.
      const short = yield* order("o3", apples(10))
      yield* advance(5)
      assert.deepStrictEqual(outcome(yield* Fiber.join(short)), failed(new OutOfStock({ reason: "no apple" })))
      yield* inventory.handle.send({ _tag: "Restock", sku: "apple", qty: 10 })
      yield* settle
      assert.strictEqual(stock.get("apple"), 12)
      const restocked = yield* order("o4", apples(10))
      yield* advance(5)
      yield* advance(5)
      assert.deepStrictEqual(outcome(yield* Fiber.join(restocked)), ok({ orderId: "o4", total: 4200 }))

      // A warehouse that does not answer: the order gives up after 5s.
      const stuck = yield* order("o5", { items: [{ sku: "slow", qty: 1 }], card: "4242" })
      yield* advance(5000)
      const gaveUp = failed(new OutOfStock({ reason: "the warehouse did not answer" }))
      assert.deepStrictEqual(outcome(yield* Fiber.join(stuck)), gaveUp)
      const snapshot = yield* inventory.snapshot
      assert.deepStrictEqual(snapshot.active, ["Inventory", "Inventory.Reserving"])
    }).pipe(Effect.scoped))

  it.effect("each order gets its own answer, and a closed inventory answers Unanswered at once", () =>
    Effect.gen(function*() {
      const { records, sink } = collect()
      const { InventoryLive } = warehouse()
      const traced = <A, E, R>(effect: Effect.Effect<A, E, R>) =>
        Effect.provideService(effect, Roadbook.TraceSink, sink)
      const inventory = yield* traced(InventoryLive.spawn({ warehouse: "w1" }))
      const order = (id: string) =>
        Effect.forkChild(Effect.exit(traced(OrderLive.run({ id, draft: apples(1), inventory: inventory.handle }))))
      // The second order's call arrives while the first is being reserved: turned away.
      const first = yield* order("a")
      const second = yield* order("b")
      yield* settle
      yield* advance(5)
      yield* advance(5)
      assert.deepStrictEqual(outcome(yield* Fiber.join(first)), ok({ orderId: "a", total: 4200 }))
      assert.deepStrictEqual(outcome(yield* Fiber.join(second)), failed(new OutOfStock({ reason: "busy" })))
      // Each reply goes to its caller alone: "b" is answered at once, "a" when its stock is held,
      // and no order receives, or drops, the other's.
      const callers = () =>
        records.flatMap((r) => r.kind === "invoke.started" && r.src === "inventory.reserve" ? [r.run] : [])
      const [a, b] = callers()
      const replied = records.flatMap((r) => r.kind === "replied" ? [[r.to, r.outcome, r.value]] : [])
      assert.deepStrictEqual(replied, [[b!, "fail", { _tag: "OutOfStock", reason: "busy" }], [a!, "done", null]])
      assert.deepStrictEqual(records.filter((r) => r.kind === "dropped"), [])

      // A closed warehouse cannot answer: the order fails without waiting for its timeout.
      yield* inventory.handle.send({ _tag: "Close" })
      yield* settle
      assert.strictEqual((yield* inventory.snapshot).status, "done")
      const late = yield* order("c")
      yield* settle
      const closed = failed(new OutOfStock({ reason: "the warehouse is closed" }))
      assert.deepStrictEqual(outcome(yield* Fiber.join(late)), closed)
      // Its invoke failed with the built-in Unanswered, which the chart maps to OutOfStock.
      const unanswered = records.filter((r) =>
        r.kind === "event" && r.event.kind === "invoke.fail" && r.event.error._tag === "Unanswered"
      )
      assert.deepStrictEqual(unanswered.map((r) => r.run), [callers()[2]])
    }).pipe(Effect.scoped))

  it.effect("a value that is not a handle is a host defect", () =>
    Effect.gen(function*() {
      const exit = yield* Effect.exit(OrderLive.run({ id: "x", draft: apples(1), inventory: {} as never }))
      assert.isTrue(Exit.isFailure(exit) && Roadbook.isDefect(exit.cause.reasons.find((r) => r._tag === "Die")?.defect))
    }))

  it.effect("a handle to another version of the machine is a host defect, before the run starts", () =>
    Effect.gen(function*() {
      // The same Inventory from a chart whose Inventory has another hash.
      const machines = Inventory.ir.machines
      const changed = { ...machines["Inventory"]!, hash: `sha256:${"1".repeat(64)}` }
      const Other = Roadbook.make<InventoryDef>({ ...Inventory.ir, machines: { ...machines, Inventory: changed } }, "Inventory")
      const other = yield* Other.implement({
        actions: { releaseStock: () => Effect.void, addStock: () => Effect.void },
        tasks: { reserveStock: () => Effect.void }
      }).spawn({ warehouse: "w1" })
      const { records, sink } = collect()
      const exit = yield* Effect.exit(
        OrderLive.run({ id: "x", draft: apples(1), inventory: other.handle }).pipe(
          Effect.provideService(Roadbook.TraceSink, sink)
        )
      )
      const defect = Exit.isFailure(exit) ? exit.cause.reasons.find(Cause.isDieReason)?.defect : undefined
      assert.isTrue(Roadbook.isDefect(defect))
      const found = (defect as Roadbook.Defect).defect
      assert.match(found.kind === "host" ? found.message : "", /another version of Inventory/)
      assert.deepStrictEqual(records, [])
    }).pipe(Effect.scoped))
})

/** Type-level checks, by `tsc`. Never run. */
export const types = (inventory: Roadbook.Handle<InventoryDef>, publish: Roadbook.Handle<PublishDef>) => {
  // @ts-expect-error  `reserve` is a call, not an event: a handle cannot send it
  inventory.send({ _tag: "Reserve", orderId: "o1", items: [] })
  // @ts-expect-error  a Restock needs its fields
  inventory.send({ _tag: "Restock" })
  const draft = { items: [], card: "4242" }
  const OrderLive = Order.implement({ tasks: { charge: () => Effect.die("unused") } })
  // @ts-expect-error  `inventory` must be a handle, not a run id
  OrderLive.run({ id: "x", draft, inventory: "r1" })
  // @ts-expect-error  `inventory` must be a handle to an Inventory
  OrderLive.run({ id: "x", draft, inventory: publish })
  return Order.implement({ tasks: { charge: () => Effect.die("unused") } }).run({ id: "x", draft, inventory })
}
