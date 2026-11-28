/**
 * Tasks that emit, with real host code rather than the conformance runner's scripted tasks
 * (docs/design/typescript.md, "Tasks that emit"): an upload that says how far it has got, and a
 * stream task that ticks on the clock until its state is left, written once as an Effect that
 * runs forever and once as a Stream drained into the emitter.
 */
import { assert, describe, it } from "@effect/vitest"
import type * as Core from "@dire-kiwi/roadbook-core"
import { Duration, Effect, Stream } from "effect"
import { TestClock } from "effect/testing"
import * as Roadbook from "../src/index.ts"
import { Progress, Stage, Tick, Uploader } from "./charts/emits.road.ts"

const collect = () => {
  const records: Array<Core.TraceRecord> = []
  return { records, sink: { write: (record: Core.TraceRecord) => void records.push(record) } }
}

/** The values of the `task.emitted` inputs the run took, as its `event` records show them. */
const taken = (records: ReadonlyArray<Core.TraceRecord>) =>
  records.flatMap((r) => (r.kind === "event" && r.event.kind === "task.emitted" ? [r.event.value] : []))

/** Every record that holds a `task.emitted` input, taken, dropped or stale. */
const anyEmitted = (records: ReadonlyArray<Core.TraceRecord>) =>
  records.filter((r) => (r.kind === "event" || r.kind === "dropped" || r.kind === "stale") && r.event.kind === "task.emitted")

/** Uploads in `chunks` steps of a second each, saying where it is before each and how far after. */
const upload = ({ chunks, size }: { size: number; chunks: number }, emit: Roadbook.Emit<Progress | Stage>) =>
  Effect.gen(function*() {
    for (let i = 1; i <= chunks; i++) {
      yield* emit(Stage.make({ name: `chunk ${i}` }))
      yield* Effect.sleep("1 second")
      yield* emit(Progress.make({ percent: (100 * i) / chunks }))
    }
    return size
  })

describe("tasks that emit, with real host code", () => {
  for (
    const [how, ticks] of [
      [
        "an Effect that runs forever",
        (stopped: { count: number }) => ({ every }: { every: Duration.Duration }, emit: Roadbook.Emit<Tick>) =>
          Effect.forever(Effect.sleep(every).pipe(Effect.andThen(emit(Tick.make({}))))).pipe(
            Effect.onInterrupt(() => Effect.sync(() => void (stopped.count += 1)))
          )
      ],
      [
        "a Stream drained into the emitter",
        (stopped: { count: number }) => ({ every }: { every: Duration.Duration }, emit: Roadbook.Emit<Tick>) =>
          Stream.tick(every).pipe(
            // `Stream.tick` gives its first element at once: the first tick is one `every` later.
            Stream.drop(1),
            Stream.map(() => Tick.make({})),
            Stream.runForEach(emit),
            Effect.onInterrupt(() => Effect.sync(() => void (stopped.count += 1)))
          )
      ]
    ] as const
  ) {
    it.effect(`the machine hears the events it has an on for, and leaving the state stops ${how}`, () =>
      Effect.gen(function*() {
        const { records, sink } = collect()
        const stopped = { count: 0 }
        const Live = Uploader.implement({ tasks: { upload, ticks: ticks(stopped) } })
        const actor = yield* Live.spawn({ size: 4096 }).pipe(Effect.provideService(Roadbook.TraceSink, sink))

        // Four chunks, a second each: each Progress reaches the machine, no Stage does (it
        // has no `on up.Stage`), and nothing records the unheard ones.
        yield* TestClock.adjust("4 seconds")
        assert.deepStrictEqual(taken(records), [25, 50, 75, 100].map((percent) => ({ _tag: "Progress", percent })))
        let snapshot = yield* actor.snapshot
        assert.deepStrictEqual(snapshot.active, ["Uploader", "Uploader.Waiting"])
        assert.deepStrictEqual(snapshot.core.vars, { percent: 100, stored: 4096, seconds: 0 })

        // The stream task ticks every second while its state is active.
        yield* TestClock.adjust("3 seconds")
        snapshot = yield* actor.snapshot
        assert.strictEqual(snapshot.core.vars["seconds"], 3)
        assert.strictEqual(stopped.count, 0)

        // Leaving the state cancels its invoke, which interrupts the stream (the driver does
        // not wait for it): no more ticks.
        yield* actor.send({ _tag: "Stop" })
        for (let i = 0; i < 100 && stopped.count === 0; i++) yield* Effect.yieldNow
        assert.deepStrictEqual((yield* actor.snapshot).active, ["Uploader", "Uploader.Stopped"])
        assert.strictEqual(stopped.count, 1)
        const before = anyEmitted(records).length
        yield* TestClock.adjust("5 seconds")
        assert.strictEqual(anyEmitted(records).length, before)
        assert.deepStrictEqual(
          records.filter((r) => r.kind === "invoke.cancelled").map((r) => r.kind === "invoke.cancelled" && r.src),
          ["ticks"]
        )

        yield* actor.send({ _tag: "Stop" })
        assert.strictEqual(yield* actor.result, 3)
      }).pipe(Effect.scoped))
  }

  it.effect("an event that does not encode is the task's defect", () =>
    Effect.gen(function*() {
      const { records, sink } = collect()
      const Live = Uploader.implement({
        tasks: {
          // Past the schema, as code that builds the event by hand can: 1.5 is not an int.
          upload: (_, emit) => Effect.as(emit({ _tag: "Progress", percent: 1.5 }), 1),
          ticks: () => Effect.never
        }
      })
      const exit = yield* Effect.exit(Live.run({ size: 1 }).pipe(Effect.provideService(Roadbook.TraceSink, sink)))
      assert.isTrue(exit._tag === "Failure")
      const input = records.find((r) => r.kind === "event")
      assert.deepStrictEqual(input?.kind === "event" && input.event, {
        kind: "invoke.defect",
        invoke: 1,
        defect: { kind: "host", message: "upload.Progress.percent is not an int" }
      })
      assert.strictEqual(records.at(-1)?.kind, "defect")
    }))
})
