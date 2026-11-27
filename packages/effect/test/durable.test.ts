/**
 * The durable driver across crashes (docs/design/typescript.md, "Durable runs"). Each test runs
 * machines with `Durable.workflow` on `ClusterWorkflowEngine` over a `MessageStorage` that
 * outlives the "process": a crash closes the runner's scope (with no termination grace, so work
 * in flight is cut off) and a restart builds a new runner, engine and registry on the same
 * storage, which replays every execution that has not ended. Tasks wait on gates the test
 * opens, so it decides where each crash falls.
 */
import * as Core from "@dire-kiwi/roadbook-core"
import { Context, Deferred, Duration, Effect, Exit, Fiber, Layer, Option, Schema, Scope, Stream } from "effect"
import { MessageStorage, RunnerHealth, Runners, RunnerStorage, Sharding, ShardingConfig } from "effect/cluster"
import { ClusterWorkflowEngine } from "effect/cluster"
import { DurableDeferred, WorkflowEngine } from "effect/workflow"
import { describe, expect, it } from "vitest"
import { Durable } from "../src/entry-durable.ts"
import * as Roadbook from "../src/index.ts"
import { Asker, Desk, Host } from "./charts/durable-calls.road.ts"
import { Adder, Counter, Hub, Parent, Pipeline, Relay, Session, Watcher } from "./charts/durable.road.ts"
import { registryOf } from "../src/internal/durable.ts"

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
    actions: {
      log: ({ text }: { text: string }) => Effect.sync(() => void logged.push(text)),
      pause: ({ n }: { n: number }) => Deferred.await(gate(100 + n))
    },
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
    /** Let the step held by `pause(n)` go on. */
    unpause: (n: number) => Deferred.doneUnsafe(gate(100 + n), Exit.void),
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
  const AskerLive = Asker.implement({})
  const AdderLive = Adder.implement(world.handlers as never)
  const SessionLive = Session.implement(world.handlers as never)
  return {
    desk: Durable.workflow(Desk.implement(world.handlers as never), { idempotencyKey: ({ id }) => `desk-${id}`, ...options }),
    asker: Durable.workflow(AskerLive, { idempotencyKey: ({ id }) => `asker-${id}`, ...options }),
    host: Durable.workflow(Host.implement({ machines: { Asker: AskerLive } }), {
      idempotencyKey: ({ id }) => `host-${id}`,
      ...options
    }),
    pipeline: Durable.workflow(PipelineLive, { idempotencyKey: ({ id }) => id, ...options }),
    parent: Durable.workflow(ParentLive, { idempotencyKey: ({ id }) => id, ...options }),
    counter: Durable.workflow(Counter.implement({}), { idempotencyKey: () => "counter", ...options }),
    hub: Durable.workflow(Hub.implement({ ...world.handlers, machines: { Session: SessionLive } } as never), {
      idempotencyKey: ({ id }) => `hub-${id}`,
      ...options
    }),
    relay: Durable.workflow(Relay.implement({ machines: { Adder: AdderLive } }), {
      idempotencyKey: ({ goal }) => `relay-${goal}`,
      ...options
    }),
    adder: Durable.workflow(AdderLive, {
      idempotencyKey: ({ goal }) => `adder-${goal}`,
      ...options
    }),
    watcher: Durable.workflow(Watcher.implement(world.handlers as never), { idempotencyKey: () => "watcher", ...options })
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

/** The engine with every write of a record whose name `delayed` matches taking `ms`. */
const slowWrites = (delayed: RegExp, ms: number) =>
  Layer.effect(
    WorkflowEngine.WorkflowEngine,
    Effect.map(WorkflowEngine.WorkflowEngine, (engine): WorkflowEngine.WorkflowEngine["Service"] => ({
      ...engine,
      deferredDone: (deferred, options) =>
        delayed.test(options.deferredName)
          ? Effect.delay(engine.deferredDone(deferred, options), ms)
          : engine.deferredDone(deferred, options)
    }))
  ).pipe(Layer.provideMerge(engineLayer))

/** A process's engine: as it is, with slow reads (`slow` ms each), or another one. */
type EngineChoice = number | Layer.Layer<WorkflowEngine.WorkflowEngine, never, any> | undefined

/** One process, which registers the durable machines `only` names (default: all of them). */
const startProcess = (storage: Storage, world: World, flows: Machines, slow?: EngineChoice, only?: ReadonlyArray<keyof Machines>) =>
  Effect.gen(function*() {
    const scope = yield* Scope.make()
    const layers = (only ?? (Object.keys(flows) as Array<keyof Machines>)).map((name) => flows[name].layer)
    const layer = Layer.mergeAll(...(layers as [Layer.Layer<never, never, WorkflowEngine.WorkflowEngine>])).pipe(
      Layer.provideMerge(slow === undefined ? engineLayer : typeof slow === "number" ? slowReads(slow) : slow)
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
    start: (flows?: Machines, slow?: EngineChoice, only?: ReadonlyArray<keyof Machines>) => ReturnType<typeof startProcess>
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
      start: (given, slow, only) => startProcess(storage, world, given ?? flows, slow, only)
    })
  })))

/** The state of durable root run `run` as the process's registry knows it. */
const stateOf = (process: Effect.Success<ReturnType<typeof startProcess>>, run: string) =>
  process.run(Effect.map(WorkflowEngine.WorkflowEngine, (engine) => registryOf(engine).runs.get(run)?.state))

/** Wait until durable root run `run` is parked in `process`; fail after `ms`. */
const untilParked = (process: Effect.Success<ReturnType<typeof startProcess>>, run: string, ms = 5000) =>
  Effect.gen(function*() {
    const deadline = Date.now() + ms
    while ((yield* stateOf(process, run)) !== "parked") {
      if (Date.now() > deadline) return yield* Effect.die(new Error(`timed out waiting for ${run} to park`))
      yield* Effect.sleep(10)
    }
  })

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

describe("durable runs restored with their child runs", () => {
  it("a run restored from a version 4 snapshot recovers its child where the snapshot left it, across a crash mid-task", () =>
    scenario(({ world, flows, start }) =>
      Effect.gen(function*() {
        let process = yield* start()
        const id = yield* process.run(flows.parent.start({ id: "s" }))
        yield* until(() => world.count(1) === 1)
        world.open(1)
        // The Pipeline has done its first step and runs its second.
        yield* until(() => world.count(2) === 1)
        const saved = yield* process.run(flows.parent.snapshot(id))
        expect(saved.snapshotVersion).toBe(4)
        const child = saved.invokes[0]!.child!
        expect(child.active).toEqual(["Pipeline", "Pipeline.Two"])
        expect(child.vars).toEqual({ total: 10 })
        expect(child.invokes.map((inv) => inv.src)).toEqual(["step"])
        // Restored as another execution (its key is its input's id), with the child in it.
        const restored = yield* process.run(flows.parent.restore({ ...saved, input: { id: "t" } }))
        yield* until(() => world.count(2) === 2)
        expect(world.count(1)).toBe(1)
        yield* process.crash

        // Both runs replay, and each child runs its second step again, not its first.
        process = yield* start()
        yield* until(() => world.count(2) === 4)
        expect(world.count(1)).toBe(1)
        expect(world.logged).toEqual(["one"])
        world.open(2)
        yield* process.run(flows.parent.send(restored, { _tag: "Push", by: 5 }))
        yield* until(() => world.count(3) === 1)
        world.open(3)
        expect(yield* process.run(flows.parent.run({ id: "t" }))).toBe(10 + 20 + 5 + 30)
        expect(world.count(1)).toBe(1)
        // The restored child run began with its own `restored` record, once: the replay wrote none.
        const childRestored = world.records.filter((r) => r.kind === "restored" && r.parent !== undefined)
        expect(childRestored.map((r) => r.run)).toEqual([`${childRestored[0]!.parent}.1`])
        yield* process.crash
      })), 20_000)
})

describe("host listeners of durable runs", () => {
  it("hear each emitted event once, though a run that parks replays the steps that emitted it", () =>
    scenario(({ flows, start, world }) =>
      Effect.gen(function*() {
        const process = yield* start()
        const id = yield* process.run(flows.counter.start({}))
        const heard = yield* process.run(flows.counter.emitted(id).pipe(Stream.runCollect, Effect.forkChild))
        // The stream subscribes once the run has replayed in this process.
        yield* Effect.sleep(100)
        for (const by of [1, 2, 3]) {
          yield* process.run(flows.counter.send(id, { _tag: "Add", by }))
          // Idle, it parks; the next event wakes it and it replays every step before.
          yield* Effect.sleep(250)
        }
        yield* process.run(flows.counter.send(id, { _tag: "Close" }))
        const values = yield* Fiber.join(heard)
        expect([...values]).toEqual([{ _tag: "Total", n: 1 }, { _tag: "Total", n: 3 }, { _tag: "Total", n: 6 }])
        expect(world.attempts()).toBeGreaterThan(2)
        yield* process.crash
      }), { park: { after: 100 } }), 20_000)
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

describe("parking a tree whose child runs are idle", () => {
  it("a hub whose sessions wait for events parks, and an event wakes it: the sessions replay their journals", () =>
    scenario(({ world, flows, start }) =>
      Effect.gen(function*() {
        const process = yield* start()
        const id = yield* process.run(flows.hub.start({ id: "a" }))
        yield* process.run(flows.hub.send(id, { _tag: "Join", key: "x" }))
        yield* process.run(flows.hub.send(id, { _tag: "Join", key: "y" }))
        yield* process.run(flows.hub.send(id, { _tag: "Tell", key: "x", by: 2 }))
        yield* process.run(flows.hub.send(id, { _tag: "Tell", key: "y", by: 3 }))
        yield* untilParked(process, id)
        // Parked: no attempt starts while the tree waits.
        const before = world.attempts()
        yield* Effect.sleep(300)
        expect(world.attempts()).toBe(before)
        expect(yield* stateOf(process, id)).toBe("parked")

        // An event to the hub, passed on to a session: the hub and both sessions replay.
        yield* process.run(flows.hub.send(id, { _tag: "Tell", key: "x", by: 5 }))
        expect(world.attempts()).toBeGreaterThanOrEqual(before + 3)
        yield* process.run(flows.hub.send(id, { _tag: "Part", key: "x" }))
        yield* process.run(flows.hub.send(id, { _tag: "Part", key: "y" }))
        yield* until(() => world.logged.includes("x") && world.logged.includes("y"))
        yield* process.run(flows.hub.send(id, { _tag: "Finish" }))
        expect(yield* process.run(flows.hub.run({ id: "a" }))).toBe(10)
        yield* process.crash
      }), { park: { after: 100 } }), 20_000)

  it("a session running a task keeps its tree live; a crash once it has parked loses nothing", () =>
    scenario(({ world, flows, start }) =>
      Effect.gen(function*() {
        let process = yield* start()
        const id = yield* process.run(flows.hub.start({ id: "b" }))
        yield* process.run(flows.hub.send(id, { _tag: "Join", key: "x" }))
        yield* process.run(flows.hub.send(id, { _tag: "Tell", key: "x", by: 2 }))
        yield* process.run(flows.hub.send(id, { _tag: "Assign", key: "x", n: 1 }))
        yield* until(() => world.count(1) === 1)
        // The session's task is in flight: the tree does not park.
        yield* Effect.sleep(400)
        expect(yield* stateOf(process, id)).toBe("live")
        world.open(1)
        yield* untilParked(process, id)
        yield* process.crash

        // The hub and its session replay, park again, and wake for the next event.
        process = yield* start()
        yield* process.run(flows.hub.send(id, { _tag: "Tell", key: "x", by: 3 }))
        yield* process.run(flows.hub.send(id, { _tag: "Part", key: "x" }))
        yield* until(() => world.logged.includes("x"))
        yield* process.run(flows.hub.send(id, { _tag: "Finish" }))
        expect(yield* process.run(flows.hub.run({ id: "b" }))).toBe(15)
        expect(world.count(1)).toBe(1)
        yield* process.crash
      }), { park: { after: 100 } }), 20_000)

  it("a session's timer wakes its parked tree when it is due, and the session takes its expiry", () =>
    scenario(({ world, flows, start }) =>
      Effect.gen(function*() {
        world.setPatience(1500)
        const process = yield* start()
        const id = yield* process.run(flows.hub.start({ id: "c" }))
        yield* process.run(flows.hub.send(id, { _tag: "Join", key: "x" }))
        yield* process.run(flows.hub.send(id, { _tag: "Tell", key: "x", by: 4 }))
        yield* untilParked(process, id)
        const before = world.attempts()
        // The hub has no timer of its own: its park waits for the session's.
        yield* until(() => world.logged.includes("x"), 8000)
        expect(world.attempts()).toBeGreaterThan(before)
        yield* process.run(flows.hub.send(id, { _tag: "Finish" }))
        expect(yield* process.run(flows.hub.run({ id: "c" }))).toBe(-1)
        yield* process.crash
      }), { park: { after: 100 } }), 20_000)

  it("a parked hub's snapshot nests its sessions' snapshots", () =>
    scenario(({ flows, start }) =>
      Effect.gen(function*() {
        const process = yield* start()
        const id = yield* process.run(flows.hub.start({ id: "d" }))
        yield* process.run(flows.hub.send(id, { _tag: "Join", key: "x" }))
        yield* process.run(flows.hub.send(id, { _tag: "Tell", key: "x", by: 6 }))
        yield* untilParked(process, id)
        const saved = yield* process.run(flows.hub.snapshot(id))
        const child = saved.spawned?.[0]?.child
        expect(child?.machine).toBe("Session")
        expect(child?.vars).toEqual({ total: 6, work: 0 })
        yield* process.crash
      }), { park: { after: 100 } }), 20_000)

  it("a child run's call wakes its parked tree when it is answered, also across a crash", () =>
    scenario(({ world, flows, start }) =>
      Effect.gen(function*() {
        let process = yield* start()
        const deskId = yield* process.run(flows.desk.start({ id: "p" }))
        const desk = yield* process.run(flows.desk.handle(deskId))
        const host = yield* process.run(flows.host.start({ desk, id: "p1", n: 4 }))
        yield* until(() => world.logged.includes("held"))
        // The Host's child waits for its answer: the tree parks.
        yield* untilParked(process, host)
        yield* process.run(flows.desk.send(deskId, { _tag: "Answer", by: 1 }))
        expect(yield* process.run(flows.host.run({ desk, id: "p1", n: 4 }))).toBe(5)

        const host2 = yield* process.run(flows.host.start({ desk, id: "p2", n: 3 }))
        yield* until(() => world.logged.length === 2)
        yield* untilParked(process, host2)
        yield* process.crash

        process = yield* start()
        yield* process.run(flows.desk.send(deskId, { _tag: "Answer", by: 1 }))
        expect(yield* process.run(flows.host.run({ desk, id: "p2", n: 3 }))).toBe(4)
        expect(world.logged).toEqual(["held", "held"])
        yield* process.crash
      }), { park: { after: 100 } }), 20_000)

  it("a child run that hears a peer keeps its tree live, so that it misses nothing the peer emits", () =>
    scenario(({ world, flows, start }) =>
      Effect.gen(function*() {
        const process = yield* start()
        const counterId = yield* process.run(flows.counter.start({}))
        const counter = yield* process.run(flows.counter.handle(counterId))
        const relay = yield* process.run(flows.relay.start({ counter, goal: 15 }))
        world.open(1)
        // The Adder has sent its 10 and waits for the Counter's total to reach 15.
        yield* until(() => world.count(1) === 1)
        yield* Effect.sleep(400)
        expect(yield* stateOf(process, relay)).toBe("live")
        yield* process.run(flows.counter.send(counterId, { _tag: "Add", by: 5 }))
        expect(yield* process.run(flows.relay.run({ counter, goal: 15 }))).toBe(15)
        yield* process.crash
      }), { park: { after: 100 } }), 20_000)
})

describe("durable runs, with slow storage reads", () => {
  it("take an answer to a child run's call that arrives while its tree decides to park", () =>
    scenario(({ world, flows, start }) =>
      Effect.gen(function*() {
        const process = yield* start(undefined, 200)
        const deskId = yield* process.run(flows.desk.start({ id: "q" }))
        const desk = yield* process.run(flows.desk.handle(deskId))
        yield* process.run(flows.host.start({ desk, id: "q", n: 7 }))
        yield* until(() => world.logged.includes("held"), 10_000)
        // While the Host reads its park records and its child's answer record before parking.
        yield* Effect.sleep(250)
        yield* process.run(flows.desk.send(deskId, { _tag: "Answer", by: 1 }))
        expect(yield* process.run(flows.host.run({ desk, id: "q", n: 7 }).pipe(Effect.timeout(10_000)))).toBe(8)
        yield* process.crash
      }), { park: { after: 100 } }), 30_000)

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

describe("durable holders of a peer that ends", () => {
  /** The `ended` inputs the Watcher (run `watcher`) took, as its records show them. */
  const ends = (world: World, watcher: string) =>
    world.records.filter((r) =>
      r.run === watcher && (r.kind === "event" || r.kind === "dropped") && r.event.kind === "ended"
    )

  it("a holder that was down while its peer ended hears it once it recovers", () =>
    scenario(({ world, flows, start }) =>
      Effect.gen(function*() {
        let process = yield* start()
        const counterId = yield* process.run(flows.counter.start({}))
        const counter = yield* process.run(flows.counter.handle(counterId))
        const watcher = yield* process.run(flows.watcher.start({ counter }))
        yield* until(() => world.count(1) === 1)
        yield* process.crash

        // Only the Counter runs in this process: it ends while the Watcher is down.
        process = yield* start(undefined, undefined, ["counter"])
        yield* process.run(flows.counter.send(counterId, { _tag: "Close" }))
        let polled = yield* process.run(flows.counter.poll(counterId))
        for (let i = 0; i < 100 && Option.isNone(polled); i++) {
          yield* Effect.sleep(20)
          polled = yield* process.run(flows.counter.poll(counterId))
        }
        expect(Option.isSome(polled)).toBe(true)
        yield* process.crash

        // The Watcher recovers, subscribes again, and is told by the rule for a peer that had
        // ended before it subscribed: from the engine's result, how it ended.
        process = yield* start()
        yield* until(() => world.count(1) === 2)
        world.open(1)
        expect(yield* process.run(flows.watcher.run({ counter }))).toBe(1)
        expect(ends(world, watcher).map((r) => r.kind === "event" && r.event)).toEqual([
          { kind: "ended", from: counterId, status: "Done" }
        ])
        yield* process.crash
      })), 20_000)

  it("a holder that took its peer's end before a crash does not take it again", () =>
    scenario(({ world, flows, start }) =>
      Effect.gen(function*() {
        let process = yield* start()
        const counterId = yield* process.run(flows.counter.start({}))
        const counter = yield* process.run(flows.counter.handle(counterId))
        const watcher = yield* process.run(flows.watcher.start({ counter }))
        yield* until(() => world.count(1) === 1)
        yield* process.run(flows.counter.send(counterId, { _tag: "Close" }))
        yield* until(() => ends(world, watcher).length === 1)
        yield* process.crash

        // Replayed from its journal, which has the end, and told again on subscribing: once.
        process = yield* start()
        yield* until(() => world.count(1) === 2)
        world.open(1)
        expect(yield* process.run(flows.watcher.run({ counter }))).toBe(1)
        expect(ends(world, watcher)).toHaveLength(1)
        yield* process.crash
      })), 20_000)
})

describe("calls between durable runs", () => {
  /** Wait until the durable run `id` of `flow` has ended. */
  const ended = (
    process: { run: <A, E>(effect: Effect.Effect<A, E, WorkflowEngine.WorkflowEngine>) => Effect.Effect<A, E> },
    flow: { readonly poll: (id: string) => Effect.Effect<Option.Option<unknown>, never, WorkflowEngine.WorkflowEngine> },
    id: string
  ) =>
    Effect.gen(function*() {
      for (let i = 0; i < 250; i++) {
        if (Option.isSome(yield* process.run(flow.poll(id)))) return
        yield* Effect.sleep(20)
      }
      return yield* Effect.die(new Error(`run ${id} did not end`))
    })

  it("a callee that crashes with a call pending answers it once it has recovered", () =>
    scenario(({ world, flows, start }) =>
      Effect.gen(function*() {
        let process = yield* start()
        const deskId = yield* process.run(flows.desk.start({ id: "a" }))
        const desk = yield* process.run(flows.desk.handle(deskId))
        yield* process.run(flows.asker.start({ desk, id: "a", n: 2 }))
        yield* until(() => world.logged.includes("held"))
        yield* process.crash

        world.records.length = 0
        process = yield* start()
        // The Desk replays the step that took the call: it knows whom to answer.
        yield* process.run(flows.desk.send(deskId, { _tag: "Answer", by: 5 }))
        expect(yield* process.run(flows.asker.run({ desk, id: "a", n: 2 }))).toBe(7)
        // Neither run took the call, nor journalled its answer, twice.
        expect(world.logged).toEqual(["held"])
        const taken = world.records.filter((r) => r.kind === "event")
        expect(taken.map((r) => (r as { event: { kind: string } }).event.kind)).toEqual(["event", "invoke.done"])
        yield* process.crash
      })), 20_000)

  it("a caller that parks waiting for an answer wakes for it, also across a crash", () =>
    scenario(({ world, flows, start }) =>
      Effect.gen(function*() {
        let process = yield* start()
        const deskId = yield* process.run(flows.desk.start({ id: "b" }))
        const desk = yield* process.run(flows.desk.handle(deskId))
        yield* process.run(flows.asker.start({ desk, id: "b1", n: 1 }))
        yield* until(() => world.logged.includes("held"))
        // Both park: the Desk waits for an event, the Asker for its answer.
        yield* Effect.sleep(300)
        const before = world.attempts()
        yield* process.run(flows.desk.send(deskId, { _tag: "Answer", by: 1 }))
        expect(yield* process.run(flows.asker.run({ desk, id: "b1", n: 1 }))).toBe(2)
        expect(world.attempts()).toBeGreaterThan(before)

        // Again, with a crash while both are parked.
        yield* process.run(flows.asker.start({ desk, id: "b2", n: 3 }))
        yield* until(() => world.logged.length === 2)
        yield* Effect.sleep(300)
        yield* process.crash

        process = yield* start()
        yield* process.run(flows.desk.send(deskId, { _tag: "Answer", by: 1 }))
        expect(yield* process.run(flows.asker.run({ desk, id: "b2", n: 3 }))).toBe(4)
        expect(world.logged).toEqual(["held", "held"])
        yield* process.crash
      }), { park: { after: 100 } }), 20_000)

  it("a caller that crashes before it knows its call was journalled hands it over again, and it is taken once", () =>
    scenario(({ world, flows, start }) =>
      Effect.gen(function*() {
        // The Asker's record that its call was made is never written in this process.
        let process = yield* start(undefined, slowWrites(/\/call\//, 60_000))
        const deskId = yield* process.run(flows.desk.start({ id: "c" }))
        const desk = yield* process.run(flows.desk.handle(deskId))
        yield* process.run(flows.asker.start({ desk, id: "c", n: 2 }))
        yield* until(() => world.logged.includes("held"))
        yield* Effect.sleep(100)
        yield* process.crash

        process = yield* start()
        yield* process.run(flows.desk.send(deskId, { _tag: "Answer", by: 1 }))
        expect(yield* process.run(flows.asker.run({ desk, id: "c", n: 2 }))).toBe(3)
        // The Desk dropped the call it was given again: it holds no second one.
        expect(world.logged).toEqual(["held"])
        yield* process.crash
      })), 20_000)

  it("a callee that ends answers Unanswered the calls it holds and those queued, durably", () =>
    scenario(({ world, flows, start }) =>
      Effect.gen(function*() {
        let process = yield* start()
        const deskId = yield* process.run(flows.desk.start({ id: "d" }))
        const desk = yield* process.run(flows.desk.handle(deskId))
        yield* process.run(flows.asker.start({ desk, id: "d1", n: 1 }))
        yield* until(() => world.logged.includes("held"))
        yield* process.run(flows.asker.start({ desk, id: "d2", n: 2 }))
        yield* until(() => world.logged.includes("extra"))
        // Closing, the Desk pauses; meanwhile a Shut and then a third call queue up.
        yield* process.run(flows.desk.send(deskId, { _tag: "Close" }))
        const shut = yield* Effect.forkChild(process.run(flows.desk.send(deskId, { _tag: "Shut" })))
        const third = yield* process.run(flows.asker.start({ desk, id: "d3", n: 3 }))
        yield* until(() => world.records.some((r) => r.run === third && r.kind === "invoke.started"))
        yield* Effect.sleep(200)
        world.unpause(1)
        yield* Fiber.join(shut)
        for (const [id, n] of [["d1", 1], ["d2", 2], ["d3", 3]] as const) {
          expect(yield* process.run(flows.asker.run({ desk, id, n }))).toBe(-1)
        }
        expect(world.logged).toEqual(["held", "extra"])
        yield* process.crash

        process = yield* start()
        expect(yield* process.run(flows.asker.run({ desk, id: "d1", n: 1 }))).toBe(-1)
        yield* process.crash
      })), 20_000)

  it("a callee that ends while its caller is parked, after a crash, answers it Unanswered", () =>
    scenario(({ world, flows, start }) =>
      Effect.gen(function*() {
        let process = yield* start()
        const deskId = yield* process.run(flows.desk.start({ id: "e" }))
        const desk = yield* process.run(flows.desk.handle(deskId))
        yield* process.run(flows.asker.start({ desk, id: "e", n: 1 }))
        yield* until(() => world.logged.includes("held"))
        yield* Effect.sleep(300)
        yield* process.crash

        // In the new process the Asker stays parked until the Desk's answer wakes it.
        process = yield* start()
        yield* process.run(flows.desk.cancel(deskId))
        expect(yield* process.run(flows.asker.run({ desk, id: "e", n: 1 }))).toBe(-1)
        yield* process.crash
      }), { park: { after: 100 } }), 20_000)

  it("a call to a peer whose execution ended in an earlier process is answered Unanswered at once", () =>
    scenario(({ flows, start }) =>
      Effect.gen(function*() {
        let process = yield* start()
        const deskId = yield* process.run(flows.desk.start({ id: "f" }))
        const desk = yield* process.run(flows.desk.handle(deskId))
        yield* process.run(flows.desk.send(deskId, { _tag: "Close" }))
        yield* ended(process, flows.desk, deskId)
        yield* process.crash

        // A new registry, which learns from the engine that the Desk has ended.
        process = yield* start()
        expect(yield* process.run(flows.asker.run({ desk, id: "f", n: 1 }))).toBe(-1)
        yield* process.crash
      })), 20_000)

  it("a child run's call is answered through its root's execution, across a crash", () =>
    scenario(({ world, flows, start }) =>
      Effect.gen(function*() {
        let process = yield* start()
        const deskId = yield* process.run(flows.desk.start({ id: "g" }))
        const desk = yield* process.run(flows.desk.handle(deskId))
        yield* process.run(flows.host.start({ desk, id: "g", n: 4 }))
        yield* until(() => world.logged.includes("held"))
        yield* process.crash

        process = yield* start()
        yield* process.run(flows.desk.send(deskId, { _tag: "Answer", by: 1 }))
        expect(yield* process.run(flows.host.run({ desk, id: "g", n: 4 }))).toBe(5)
        yield* process.crash
      })), 20_000)

  it("without replayTrace, a callee's replay writes no replied record", () =>
    scenario(({ world, flows, start }) =>
      Effect.gen(function*() {
        let process = yield* start()
        const deskId = yield* process.run(flows.desk.start({ id: "h" }))
        const desk = yield* process.run(flows.desk.handle(deskId))
        expect(yield* Effect.gen(function*() {
          yield* process.run(flows.asker.start({ desk, id: "h", n: 1 }))
          yield* until(() => world.logged.includes("held"))
          yield* process.run(flows.desk.send(deskId, { _tag: "Answer", by: 1 }))
          return yield* process.run(flows.asker.run({ desk, id: "h", n: 1 }))
        })).toBe(2)
        expect(world.records.filter((r) => r.kind === "replied")).toHaveLength(1)
        yield* process.crash

        world.records.length = 0
        process = yield* start()
        // The Desk replays the step that replied, then takes the Close.
        yield* process.run(flows.desk.send(deskId, { _tag: "Close" }))
        yield* ended(process, flows.desk, deskId)
        expect(world.records.filter((r) => r.kind === "replied")).toEqual([])
        expect(world.records.some((r) => r.run === deskId && r.kind === "event")).toBe(true)
        yield* process.crash
      })), 20_000)

  it("with replayTrace, a callee and a caller that recover write the trace an uninterrupted run does", async () => {
    const strip = (records: ReadonlyArray<Core.TraceRecord>) =>
      [...records].sort((a, b) => (a.run < b.run ? -1 : a.run > b.run ? 1 : 0)).map(({ t: _t, ...rest }) =>
        Core.canonicalJson(rest as never)
      )
    const drive = (crash: boolean) =>
      scenario(({ world, flows, start }) =>
        Effect.gen(function*() {
          let process = yield* start()
          const deskId = yield* process.run(flows.desk.start({ id: "i" }))
          const desk = yield* process.run(flows.desk.handle(deskId))
          yield* process.run(flows.asker.start({ desk, id: "i", n: 1 }))
          yield* until(() => world.logged.includes("held"))
          if (crash) {
            yield* process.crash
            world.records.length = 0
            process = yield* start()
          }
          yield* process.run(flows.desk.send(deskId, { _tag: "Answer", by: 1 }))
          yield* process.run(flows.asker.run({ desk, id: "i", n: 1 }))
          yield* process.run(flows.desk.send(deskId, { _tag: "Close" }))
          yield* ended(process, flows.desk, deskId)
          yield* process.crash
          return strip(world.records)
        }), {
          replayTrace: true,
          park: false,
          runId: (id) => `x${id.replace(/[^A-Za-z0-9]/g, "").slice(0, 16)}`
        })
    const uninterrupted = await drive(false)
    const recovered = await drive(true)
    expect(recovered).toEqual(uninterrupted)
    expect(uninterrupted.filter((line) => line.includes('"kind":"replied"'))).toHaveLength(1)
  }, 30_000)
})
