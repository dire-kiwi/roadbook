/**
 * The durable driver across crashes (docs/design/typescript.md, "Durable runs"). Each test runs
 * machines with `Durable.workflow` on `ClusterWorkflowEngine` over a `MessageStorage` that
 * outlives the "process": a crash closes the runner's scope (with no termination grace, so work
 * in flight is cut off) and a restart builds a new runner, engine and registry on the same
 * storage, which replays every execution that has not ended. Tasks wait on gates the test
 * opens, so it decides where each crash falls.
 */
import * as Core from "@dire-kiwi/roadbook-core"
import { Context, Deferred, Duration, Effect, Exit, Layer, Option, Schema, Scope } from "effect"
import { MessageStorage, RunnerHealth, Runners, RunnerStorage, Sharding, ShardingConfig } from "effect/cluster"
import { ClusterWorkflowEngine } from "effect/cluster"
import { DurableDeferred, WorkflowEngine } from "effect/workflow"
import { describe, expect, it } from "vitest"
import { Durable } from "../src/entry-durable.ts"
import * as Roadbook from "../src/index.ts"
import { Adder, Caller, Counter, Parent, Pipeline, Teller } from "./charts/durable.road.ts"

const config = ShardingConfig.layer({
  entityTerminationTimeout: 0,
  entityMessagePollInterval: 10,
  entityReplyPollInterval: 10,
  refreshAssignmentsInterval: 50
})

/** What the host code did, across every process of a test. */
const makeWorld = () => {
  const counts = new Map<number, number>()
  const gates = new Map<number, Deferred.Deferred<void>>()
  const gate = (n: number) => {
    let found = gates.get(n)
    if (found === undefined) gates.set(n, found = Deferred.makeUnsafe<void>())
    return found
  }
  const logged: Array<string> = []
  const records: Array<Core.TraceRecord> = []
  let attempts = 0
  let patience = 10_000
  let deadline = 60_000
  const handlers = {
    actions: { log: ({ text }: { text: string }) => Effect.sync(() => void logged.push(text)) },
    tasks: {
      step: ({ n }: { n: number }) =>
        Effect.gen(function*() {
          counts.set(n, (counts.get(n) ?? 0) + 1)
          yield* Deferred.await(gate(n))
          return n * 10
        })
    },
    fns: {
      patience: () => Duration.millis(patience),
      deadline: () => Duration.millis(deadline)
    }
  }
  return {
    counts,
    count: (n: number) => counts.get(n) ?? 0,
    open: (n: number) => Deferred.doneUnsafe(gate(n), Exit.void),
    logged,
    records,
    handlers,
    attempts: () => attempts,
    sink: { write: (record: Core.TraceRecord) => void records.push(record) },
    observer: { started: () => void (attempts += 1) },
    setPatience: (ms: number) => void (patience = ms),
    setDeadline: (ms: number) => void (deadline = ms)
  }
}
type World = ReturnType<typeof makeWorld>

const machines = (world: World, options: Partial<Durable.Options<any>> = {}) => {
  const PipelineLive = Pipeline.implement(world.handlers as never)
  const ParentLive = Parent.implement({ ...world.handlers, machines: { Pipeline: PipelineLive } } as never)
  return {
    pipeline: Durable.workflow(PipelineLive, { idempotencyKey: ({ id }) => id, ...options }),
    parent: Durable.workflow(ParentLive, { idempotencyKey: ({ id }) => id, ...options }),
    counter: Durable.workflow(Counter.implement({}), { idempotencyKey: () => "counter", ...options }),
    adder: Durable.workflow(Adder.implement(world.handlers as never), {
      idempotencyKey: ({ goal }) => `adder-${goal}`,
      ...options
    }),
    teller: Durable.workflow(Teller.implement(world.handlers as never), { idempotencyKey: () => "teller", ...options }),
    caller: Durable.workflow(Caller.implement({}), { idempotencyKey: ({ n }) => `caller-${n}`, ...options })
  }
}
type Machines = ReturnType<typeof machines>

/** One process: a runner, its engine and the durable machines, on the shared storage. */
type Storage = Context.Context<MessageStorage.MessageStorage | MessageStorage.MemoryDriver>

const engineLayer = ClusterWorkflowEngine.layer.pipe(
  Layer.provideMerge(Sharding.layer),
  Layer.provide(Runners.layerNoop),
  Layer.provide([RunnerStorage.layerMemory, RunnerHealth.layerNoop]),
  Layer.provide(config)
)

/**
 * The engine with every record read taking `ms`, as reads from SQL storage take time: the
 * memory storage answers at once, which would hide what happens while a run waits for a read.
 */
const slowReads = (ms: number) =>
  Layer.effect(
    WorkflowEngine.WorkflowEngine,
    Effect.map(WorkflowEngine.WorkflowEngine, (engine): WorkflowEngine.WorkflowEngine["Service"] => ({
      ...engine,
      deferredResult: (deferred) => Effect.delay(engine.deferredResult(deferred), ms)
    }))
  ).pipe(Layer.provideMerge(engineLayer))

const startProcess = (storage: Storage, world: World, flows: Machines, slow?: number) =>
  Effect.gen(function*() {
    const scope = yield* Scope.make()
    const layer = Layer.mergeAll(
      flows.pipeline.layer,
      flows.parent.layer,
      flows.counter.layer,
      flows.adder.layer,
      flows.teller.layer,
      flows.caller.layer
    ).pipe(
      Layer.provideMerge(slow === undefined ? engineLayer : slowReads(slow))
    )
    const context = yield* Layer.buildWithScope(layer, scope).pipe(
      Effect.provideContext(storage),
      Effect.provideService(Roadbook.TraceSink, world.sink),
      Effect.provideService(Roadbook.RunObserver, world.observer)
    )
    return {
      run: <A, E>(effect: Effect.Effect<A, E, WorkflowEngine.WorkflowEngine>) =>
        Effect.provideContext(effect, context as Context.Context<WorkflowEngine.WorkflowEngine>),
      crash: Scope.close(scope, Exit.void)
    }
  })

/** A test: fresh storage and world, and a way to start processes on them. */
const scenario = <A>(
  body: (t: {
    world: World
    start: (flows?: Machines, slow?: number) => ReturnType<typeof startProcess>
    flows: Machines
    storage: Storage
  }) => Effect.Effect<A, unknown, Scope.Scope>,
  options: Partial<Durable.Options<any>> = {}
) =>
  Effect.runPromise(Effect.scoped(Effect.gen(function*() {
    const storage = yield* Layer.build(MessageStorage.layerMemory.pipe(Layer.provide(config)))
    const world = makeWorld()
    const flows = machines(world, options)
    return yield* body({
      world,
      flows,
      storage,
      start: (given, slow) => startProcess(storage, world, given ?? flows, slow)
    })
  })))

/** Wait until `condition` holds, polling; fail after `ms`. */
const until = (condition: () => boolean, ms = 5000) =>
  Effect.gen(function*() {
    const deadline = Date.now() + ms
    while (!condition()) {
      if (Date.now() > deadline) return yield* Effect.die(new Error("timed out waiting"))
      yield* Effect.sleep(10)
    }
  })

describe("durable runs across a crash", () => {
  it("do not repeat finished tasks and actions, and run the one in flight again", () =>
    scenario(({ world, flows, start }) =>
      Effect.gen(function*() {
        let process = yield* start()
        const id = yield* process.run(flows.parent.start({ id: "a" }))
        yield* until(() => world.count(1) === 1)
        world.open(1)
        yield* until(() => world.count(2) === 1)
        yield* process.crash

        process = yield* start()
        // Replayed: step 1 and the action are not run again, step 2 is.
        yield* until(() => world.count(2) === 2)
        expect(world.count(1)).toBe(1)
        expect(world.logged).toEqual(["one"])
        world.open(2)
        // The Pipeline waits for a nudge, which its Parent passes on.
        yield* process.run(flows.parent.send(id, { _tag: "Push", by: 5 }))
        yield* until(() => world.count(3) === 1)
        world.open(3)
        const total = yield* process.run(flows.parent.run({ id: "a" }))
        expect(total).toBe(10 + 20 + 5 + 30)
        expect([world.count(1), world.count(2), world.count(3)]).toEqual([1, 2, 1])
        expect(world.logged).toEqual(["one", "pipeline exited"])
        yield* process.crash
      })), 20_000)

  it("do not start again a task cancelled before the crash", () =>
    scenario(({ world, flows, start }) =>
      Effect.gen(function*() {
        world.setDeadline(200)
        let process = yield* start()
        yield* process.run(flows.parent.start({ id: "b" }))
        // Past the deadline the Parent cancels its Pipeline, whose step 1 is running.
        yield* until(() => world.count(4) === 1)
        expect(world.logged).toEqual(["one", "pipeline exited"])
        yield* process.crash

        process = yield* start()
        yield* until(() => world.count(4) === 2)
        world.open(4)
        expect(yield* process.run(flows.parent.run({ id: "b" }))).toBe(-1)
        expect(world.count(1)).toBe(1)
        expect(world.logged).toEqual(["one", "pipeline exited"])
        yield* process.crash
      })), 20_000)

  it("keep an event once send has returned", () =>
    scenario(({ world, flows, start }) =>
      Effect.gen(function*() {
        world.open(1)
        world.open(2)
        let process = yield* start()
        const id = yield* process.run(flows.pipeline.start({ id: "c" }))
        yield* until(() => world.count(2) === 1)
        yield* Effect.sleep(50)
        yield* process.run(flows.pipeline.send(id, { _tag: "Nudge", by: 7 }))
        yield* process.crash

        process = yield* start()
        world.open(3)
        expect(yield* process.run(flows.pipeline.run({ id: "c" }))).toBe(10 + 20 + 7 + 30)
        yield* process.crash
      })), 20_000)

  it("keep a timer's deadline: one that fell due while the process was down fires at once", () =>
    scenario(({ world, flows, start }) =>
      Effect.gen(function*() {
        world.setPatience(300)
        world.open(1)
        world.open(2)
        let process = yield* start()
        yield* process.run(flows.pipeline.start({ id: "d" }))
        yield* until(() => world.records.some((r) => r.kind === "timer.started"))
        yield* process.crash
        yield* Effect.sleep(500)

        const restarted = Date.now()
        process = yield* start()
        yield* until(() => world.count(3) === 1)
        expect(Date.now() - restarted).toBeLessThan(250)
        world.open(3)
        expect(yield* process.run(flows.pipeline.run({ id: "d" }))).toBe(60)
        yield* process.crash
      })), 20_000)

  it("keep a cancel: exit effects run once, and the run stays cancelled", () =>
    scenario(({ world, flows, start }) =>
      Effect.gen(function*() {
        let process = yield* start()
        const id = yield* process.run(flows.pipeline.start({ id: "e" }))
        yield* until(() => world.count(1) === 1)
        yield* process.run(flows.pipeline.cancel(id))
        expect(world.logged).toEqual(["one", "pipeline exited"])
        yield* process.crash

        process = yield* start()
        const polled = yield* process.run(flows.pipeline.poll(id))
        expect(Option.isSome(polled) && Exit.hasInterrupts(polled.value)).toBe(true)
        expect(world.count(1)).toBe(1)
        expect(world.logged).toEqual(["one", "pipeline exited"])
        yield* process.crash
      })), 20_000)

  it("refuse to replay a run whose chart changed: it ends with a defect", () =>
    scenario(({ world, flows, start }) =>
      Effect.gen(function*() {
        let process = yield* start()
        const id = yield* process.run(flows.pipeline.start({ id: "f" }))
        yield* until(() => world.count(1) === 1)
        yield* process.crash

        // The same workflow name, with another version of the chart.
        const ir = structuredClone(Pipeline.ir) as Core.Ir
        ;(ir.machines.Pipeline as { hash: string }).hash = "sha256:changed"
        const changed = Roadbook.make<any>(ir, "Pipeline").implement(world.handlers as never)
        process = yield* start({ ...flows, pipeline: Durable.workflow(changed, { idempotencyKey: ({ id }) => id }) })
        const exit = yield* process.run(Effect.exit(flows.pipeline.run({ id: "f" })))
        expect(Exit.isFailure(exit)).toBe(true)
        const defect = Exit.isFailure(exit) ? exit.cause.reasons.find((r) => r._tag === "Die") : undefined
        expect(String((defect as { defect: Roadbook.Defect } | undefined)?.defect.message)).toMatch(/changed while run/)
        expect(Option.isSome(yield* process.run(flows.pipeline.poll(id)))).toBe(true)
        // The run has ended, so sends and cancels do nothing.
        yield* process.run(flows.pipeline.send(id, { _tag: "Nudge", by: 1 }).pipe(Effect.timeout(3000)))
        yield* process.run(flows.pipeline.cancel(id).pipe(Effect.timeout(3000)))
        yield* process.crash
      })), 20_000)

  it("keep refs: a peer reached and heard from across the crash", () =>
    scenario(({ world, flows, start }) =>
      Effect.gen(function*() {
        let process = yield* start()
        const counterId = yield* process.run(flows.counter.start({}))
        const counter = yield* process.run(flows.counter.handle(counterId))
        yield* process.run(flows.adder.start({ counter, goal: 10 }))
        yield* until(() => world.count(1) === 1)
        yield* process.crash

        process = yield* start()
        yield* until(() => world.count(1) === 2)
        world.open(1)
        expect(yield* process.run(flows.adder.run({ counter, goal: 10 }))).toBe(10)
        // `send` returns once the Close is journalled; the run ends just after.
        yield* process.run(flows.counter.send(counterId, { _tag: "Close" }))
        let polled = yield* process.run(flows.counter.poll(counterId))
        for (let i = 0; i < 100 && Option.isNone(polled); i++) {
          yield* Effect.sleep(20)
          polled = yield* process.run(flows.counter.poll(counterId))
        }
        expect(Option.isSome(polled) && Exit.isSuccess(polled.value)).toBe(true)
        yield* process.crash
      })), 20_000)
})

describe("calls between durable runs", () => {
  it("a call is answered once, and a crash while the callee works on it loses nothing", () =>
    scenario(({ world, flows, start }) =>
      Effect.gen(function*() {
        world.open(1)
        let process = yield* start()
        const tellerId = yield* process.run(flows.teller.start({}))
        const teller = yield* process.run(flows.teller.handle(tellerId))
        expect(yield* process.run(flows.caller.run({ teller, n: 1 }))).toBe(10)

        // The teller takes the second call and works on it; the process crashes meanwhile.
        yield* process.run(flows.caller.start({ teller, n: 2 }))
        yield* until(() => world.count(2) === 1)
        yield* process.crash

        // Both replay: the caller makes its call again at its live edge, and the teller, which
        // took it before the crash, answers it once its step has run again.
        process = yield* start()
        yield* until(() => world.count(2) === 2)
        world.open(2)
        expect(yield* process.run(flows.caller.run({ teller, n: 2 }))).toBe(20)
        // The teller took each call once, and replied to each once.
        const taken = world.records.filter((r) => r.kind === "event" && r.event.kind === "call")
        expect(taken.length).toBe(2)
        expect(world.records.filter((r) => r.kind === "replied").length).toBe(2)
        yield* process.run(flows.teller.send(tellerId, { _tag: "Close" }))
        yield* process.crash
      })), 20_000)
})

describe("durable traces", () => {
  it("with replayTrace, a recovered run writes the trace an uninterrupted one does", async () => {
    const strip = (records: ReadonlyArray<Core.TraceRecord>) =>
      records.filter((r) => r.run.startsWith("p")).map(({ t: _t, ...rest }) => Core.canonicalJson(rest as never))
    const drive = (crash: boolean) =>
      scenario(({ world, flows, start }) =>
        Effect.gen(function*() {
          let process = yield* start()
          const id = yield* process.run(flows.pipeline.start({ id: "g" }))
          yield* until(() => world.count(1) === 1)
          world.open(1)
          yield* until(() => world.count(2) === 1)
          if (crash) {
            yield* process.crash
            world.records.length = 0
            process = yield* start()
            yield* until(() => world.count(2) === 2)
          }
          world.open(2)
          yield* until(() => world.records.some((r) => r.kind === "timer.started"))
          yield* process.run(flows.pipeline.send(id, { _tag: "Nudge", by: 1 }))
          world.open(3)
          yield* process.run(flows.pipeline.run({ id: "g" }))
          yield* process.crash
          return strip(world.records)
        }), { replayTrace: true, runId: () => "p1" })
    const uninterrupted = await drive(false)
    const recovered = await drive(true)
    expect(recovered).toEqual(uninterrupted)
    expect(uninterrupted.length).toBeGreaterThan(10)
  }, 30_000)

  it("without replayTrace, a recovered run writes only the steps after the crash", () =>
    scenario(({ world, flows, start }) =>
      Effect.gen(function*() {
        let process = yield* start()
        yield* process.run(flows.pipeline.start({ id: "h" }))
        yield* until(() => world.count(1) === 1)
        yield* process.crash
        world.records.length = 0
        process = yield* start()
        yield* until(() => world.count(1) === 2)
        expect(world.records).toEqual([])
        world.open(1)
        yield* until(() => world.count(2) === 1)
        expect(world.records.map((r) => r.kind)).toContain("invoke.started")
        expect(world.records.map((r) => r.kind)).not.toContain("started")
        yield* process.crash
      })), 20_000)
})

describe("parking", () => {
  it("an idle run parks on a durable clock and carries on when its timer is due", () =>
    scenario(({ world, flows, start }) =>
      Effect.gen(function*() {
        world.setPatience(1500)
        world.open(1)
        world.open(2)
        const process = yield* start()
        yield* process.run(flows.pipeline.start({ id: "i" }))
        yield* until(() => world.records.some((r) => r.kind === "timer.started"))
        const before = world.attempts()
        // Parked, then replayed when the clock fires: a new attempt, and step 3.
        yield* until(() => world.count(3) === 1, 8000)
        expect(world.attempts()).toBeGreaterThan(before)
        world.open(3)
        expect(yield* process.run(flows.pipeline.run({ id: "i" }))).toBe(60)
        yield* process.crash
      }), { park: { after: 100 } }), 20_000)

  it("a parked run wakes for an event, and a crash while parked loses nothing", () =>
    scenario(({ world, flows, start }) =>
      Effect.gen(function*() {
        world.open(1)
        world.open(2)
        let process = yield* start()
        const id = yield* process.run(flows.pipeline.start({ id: "j" }))
        yield* until(() => world.records.some((r) => r.kind === "timer.started"))
        yield* Effect.sleep(300)
        yield* process.crash

        process = yield* start()
        yield* process.run(flows.pipeline.send(id, { _tag: "Nudge", by: 2 }))
        yield* until(() => world.count(3) === 1)
        world.open(3)
        expect(yield* process.run(flows.pipeline.run({ id: "j" }))).toBe(62)
        expect(world.count(1)).toBe(1)
        yield* process.crash
      }), { park: { after: 100 } }), 20_000)
})

describe("durable runs, with slow storage reads", () => {
  it("take an event that arrives while they decide to park", () =>
    scenario(({ world, flows, start }) =>
      Effect.gen(function*() {
        world.setPatience(60_000)
        world.open(1)
        world.open(2)
        const process = yield* start(undefined, 300)
        const id = yield* process.run(flows.pipeline.start({ id: "k" }))
        yield* until(() => world.records.some((r) => r.kind === "timer.started"), 10_000)
        // While the run reads its park records before parking.
        yield* Effect.sleep(350)
        yield* process.run(flows.pipeline.send(id, { _tag: "Nudge", by: 1 }).pipe(Effect.timeout(8000)))
        yield* until(() => world.count(3) === 1)
        yield* process.crash
      }), { park: { after: 100 } }), 30_000)

  it("run the exit effects of a child cancelled by the last journalled step", () =>
    scenario(({ world, flows, start, storage }) =>
      Effect.gen(function*() {
        let process = yield* start()
        const id = yield* process.run(flows.parent.start({ id: "l" }))
        yield* until(() => world.count(1) === 1)
        yield* process.crash
        // The crash fell just after the Parent journalled its deadline's expiry (timer 1), before
        // the step that cancels its Pipeline.
        const scope = yield* Scope.make()
        const engine = Context.get(
          yield* Layer.buildWithScope(engineLayer, scope).pipe(Effect.provideContext(storage)),
          WorkflowEngine.WorkflowEngine
        )
        const name = `${id}/in/1`
        yield* engine.deferredDone(DurableDeferred.make(name, { success: Schema.Json }), {
          workflowName: "Parent",
          executionId: id,
          deferredName: name,
          exit: Exit.succeed({ kind: "after", timer: 1 })
        })
        yield* Scope.close(scope, Exit.void)

        process = yield* start(undefined, 50)
        yield* until(() => world.count(4) === 1, 10_000)
        expect(world.logged).toEqual(["one", "pipeline exited"])
        expect(world.count(1)).toBe(1)
        yield* process.crash
      })), 30_000)
})

describe("durable runs that have ended", () => {
  it("take no more events or cancels, in a later process too", () =>
    scenario(({ world, flows, start }) =>
      Effect.gen(function*() {
        world.open(1)
        world.open(2)
        world.open(3)
        let process = yield* start()
        const id = yield* process.run(flows.pipeline.start({ id: "m" }))
        yield* until(() => world.records.some((r) => r.kind === "timer.started"))
        yield* process.run(flows.pipeline.send(id, { _tag: "Nudge", by: 1 }))
        expect(yield* process.run(flows.pipeline.run({ id: "m" }))).toBe(61)
        yield* process.crash

        process = yield* start()
        yield* process.run(flows.pipeline.send(id, { _tag: "Nudge", by: 1 }).pipe(Effect.timeout(3000)))
        yield* process.run(flows.pipeline.cancel(id).pipe(Effect.timeout(3000)))
        yield* process.crash
      })), 20_000)

  it("a cancel reaches a parked run", () =>
    scenario(({ world, flows, start }) =>
      Effect.gen(function*() {
        world.open(1)
        world.open(2)
        const process = yield* start()
        const id = yield* process.run(flows.pipeline.start({ id: "n" }))
        yield* until(() => world.records.some((r) => r.kind === "timer.started"))
        yield* Effect.sleep(300)
        yield* process.run(flows.pipeline.cancel(id).pipe(Effect.timeout(5000)))
        expect(world.logged).toEqual(["one", "pipeline exited"])
        yield* process.crash
      }), { park: { after: 100 } }), 20_000)

  it("a holder started after a restart hears only what its parked peer emits from then on", () =>
    scenario(({ world, flows, start }) =>
      Effect.gen(function*() {
        world.open(1)
        let process = yield* start()
        const counterId = yield* process.run(flows.counter.start({}))
        const counter = yield* process.run(flows.counter.handle(counterId))
        // The Counter reaches 100, then parks.
        yield* process.run(flows.counter.send(counterId, { _tag: "Add", by: 100 }))
        yield* Effect.sleep(300)
        yield* process.crash

        process = yield* start()
        // An Adder with goal 5 would end on the old Total { n: 100 } if it heard it.
        world.records.length = 0
        yield* process.run(flows.adder.start({ counter, goal: 5 }))
        expect(yield* process.run(flows.adder.run({ counter, goal: 5 }))).toBe(5)
        const heard = world.records.filter((r) => r.kind === "event" && r.event.kind === "emitted")
        expect(heard.map((r) => (r as { event: { message: number } }).event.message)).toEqual([2])
        yield* process.crash
      }), { park: { after: 100 } }), 20_000)
})
