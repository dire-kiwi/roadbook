/**
 * Durable runs: a machine run as an `effect/workflow` execution, so that it survives a crash
 * in the middle of a task without repeating completed work (docs/design/typescript.md,
 * "Durable runs"). Imported from `@dire-kiwi/roadbook-effect/durable`:
 *
 * ```ts
 * const ImportFlow = Durable.workflow(ImportJobLive, { idempotencyKey: ({ upload }) => upload.jobId })
 * const live = ImportFlow.layer.pipe(Layer.provide(engine))   // registers it with the engine
 * const summary = yield* ImportFlow.run({ upload })
 * ```
 *
 * Durable runs that hold a `ref` to one another call one another (`invoke peer.c(…)`), as
 * ordinary runs do: a call, its answer, and the `Unanswered` of a callee that ends survive a
 * crash of either side.
 *
 * A task, invoked or spawned, is one activity whether or not it emits. What it emits reaches the
 * run's queue as it runs, and the emits the run takes are journalled as its inputs and given
 * again on replay; none is ever dropped as taken already, since an emit has no identity. A task
 * still running at a crash starts again and emits again, as new inputs; emits it had queued and
 * the run had not taken are lost.
 */
import * as Core from "@dire-kiwi/roadbook-core"
import { Cause, Duration, Effect, Exit, Layer, Option, Queue, Schema, Stream } from "effect"
import { DurableClock, Workflow, WorkflowEngine } from "effect/workflow"
import { Defect, hostDefect } from "./Defect.ts"
import { type Begin, check, type Env, Program, runDurable } from "./internal/driver.ts"
import { type AnyWorkflow, entryOf, entryOfPort, type Execution, registryOf, type RunInfo } from "./internal/durable.ts"
import { portOf } from "./internal/port.ts"
import { ProgramOf } from "./make.ts"
import type { Handle, Implemented, MachineDef, RestoreOptions } from "./Machine.ts"
import { RunObserver, TraceSink, Timer } from "./References.ts"
import type { RestoreError, Saved } from "./Saved.ts"

/** How a machine runs durably. */
export interface Options<D extends MachineDef> {
  /** The workflow's name. Default: the machine's name. A new version of the chart needs a new one. */
  readonly name?: string
  /** The execution an input belongs to: starting another input with the same key joins that run. */
  readonly idempotencyKey: (input: D["input"]) => string
  /**
   * Whether an idle run parks: suspends on a `DurableClock` for its earliest timer, holding no
   * memory, when that timer is further than `after` (default: `DurableClock.InMemoryThreshold`,
   * 60 seconds). A run parks with its child runs, once its whole tree is idle, for the earliest
   * timer of any run in it. `false` keeps every run in memory until it ends.
   */
  readonly park?: false | { readonly after?: Duration.Input }
  /** Whether a replay writes the trace records of the steps it takes again. Default `false`. */
  readonly replayTrace?: boolean
  /** The run id of an execution's root run, in traces and snapshots. Default: the execution id. */
  readonly runId?: (executionId: string) => string
}

/** What a run's workflow execution ends with: its status and result, as the core holds them. */
export interface Outcome {
  readonly status: Core.Status
  readonly result: Core.RunResult | null
}

/** A machine that runs durably. Everything needs the `WorkflowEngine` it is registered with. */
export interface DurableMachine<D extends MachineDef, R> {
  /** The `effect/workflow` workflow, for tools that work on workflows. */
  readonly workflow: AnyWorkflow
  /** Registers the machine with the engine. Provide the services its implementations need. */
  readonly layer: Layer.Layer<never, never, WorkflowEngine.WorkflowEngine | R>
  /** The execution id of the run an input belongs to. */
  readonly executionId: (input: D["input"]) => Effect.Effect<string>
  /** Start the run for `input` (or join it, if its key has one) and give its execution id. */
  readonly start: (input: D["input"]) => Effect.Effect<string, never, WorkflowEngine.WorkflowEngine>
  /**
   * Start or join the run for `input` and wait for it to end: its output, its error, a `Defect`
   * die, or an interrupt if it was cancelled. Interrupting this stops waiting; the run carries on.
   */
  readonly run: (input: D["input"]) => Effect.Effect<D["output"], D["error"], WorkflowEngine.WorkflowEngine>
  /**
   * Start a durable run from a saved snapshot (checked, and migrated with `options.migrate`, as
   * `restore` does), keyed by `idempotencyKey` of the snapshot's input. Gives its execution id.
   * Each child run nested in it is restored where it was, and a replay restores it again from
   * the same snapshot before it replays the child's own journal.
   */
  readonly restore: (
    saved: Saved,
    options?: RestoreOptions
  ) => Effect.Effect<string, RestoreError, WorkflowEngine.WorkflowEngine>
  /**
   * Give the run one of the machine's events, and wait until it is in the run's journal: once
   * this succeeds the event survives a crash. Does nothing once the run has ended. An event that
   * does not encode dies with the caller's host `Defect`.
   */
  readonly send: (executionId: string, event: D["event"]) => Effect.Effect<void, never, WorkflowEngine.WorkflowEngine>
  /** Cancel the run (its exit effects run) and wait until it has ended. */
  readonly cancel: (executionId: string) => Effect.Effect<void, never, WorkflowEngine.WorkflowEngine>
  /** How the run ended, as `run` gives it, or none while it runs (or parks). */
  readonly poll: (
    executionId: string
  ) => Effect.Effect<Option.Option<Exit.Exit<D["output"], D["error"]>>, never, WorkflowEngine.WorkflowEngine>
  /**
   * The run's saved snapshot as its last step in this process left it, with the snapshots of its
   * running child runs nested in it, each as its own last step left it. Dies if it has none here.
   */
  readonly snapshot: (executionId: string) => Effect.Effect<Saved, never, WorkflowEngine.WorkflowEngine>
  /** The run's handle: what a `ref` argument of another durable run takes. */
  readonly handle: (executionId: string) => Effect.Effect<Handle<D>, never, WorkflowEngine.WorkflowEngine>
  /**
   * The events the run emits, every one of them whatever any machine hears, as `actor.emitted`
   * gives them: from when the stream starts (once the run has replayed in this process, waking
   * it if it must, so that what it emitted before is known) until the run ends. Each is given
   * once, also when a replay after a park carries out its step again. Listening lasts as long as
   * this process; a stream started in a later one hears from then on.
   */
  readonly emitted: (executionId: string) => Stream.Stream<D["emits"], never, WorkflowEngine.WorkflowEngine>
}

/** The payload of a run's workflow: its key, and how it begins, in JSON. */
type BeginJson =
  | {
    readonly kind: "start"
    readonly input: { readonly [param: string]: Core.Json }
    /** The durable runs its `ref` arguments refer to. */
    readonly peers: ReadonlyArray<RunInfo>
  }
  | { readonly kind: "restore"; readonly saved: Core.Saved; readonly migrated: string | null }

/** Run `machine` durably on `effect/workflow`. */
export const workflow = <D extends MachineDef, R>(
  machine: Implemented<D, R>,
  options: Options<D>
): DurableMachine<D, R> => {
  const program = (machine as unknown as { readonly [ProgramOf]: Program })[ProgramOf]
  const name = options.name ?? program.name
  const hash = program.machine.hash
  const runIdOf = options.runId ?? ((executionId: string) => executionId)
  const replayTrace = options.replayTrace ?? false
  const parkAfter = options.park === false
    ? Effect.succeed(undefined)
    : options.park?.after !== undefined
    ? Effect.succeed(Duration.toMillis(Duration.fromInputUnsafe(options.park.after)))
    : Effect.gen(function*() {
      return Duration.toMillis(yield* DurableClock.InMemoryThreshold)
    })

  const flow = Workflow.make(name, {
    payload: { key: Schema.String, begin: Schema.Json },
    success: Schema.Json,
    idempotencyKey: ({ key }) => key
  }) as unknown as AnyWorkflow

  const infoOf = (executionId: string): RunInfo => ({
    run: runIdOf(executionId),
    workflow: name,
    executionId,
    machine: program.name,
    hash
  })
  const entryFor = (executionId: string) =>
    Effect.map(WorkflowEngine.WorkflowEngine, (engine) =>
      entryOf(registryOf(engine), engine, infoOf(executionId)))

  // ---- the workflow's body --------------------------------------------------------------------

  const body = (payload: { readonly key: string; readonly begin: Core.Json }, executionId: string) =>
    Effect.gen(function*() {
      const engine = yield* WorkflowEngine.WorkflowEngine
      const instance = yield* WorkflowEngine.WorkflowInstance
      const registry = registryOf(engine)
      const info = infoOf(executionId)
      const entry = entryOf(registry, engine, info)
      const exec: Execution = {
        engine,
        instance,
        workflow: flow,
        executionId,
        replayTrace,
        park: yield* parkAfter,
        registry,
        root: entry
      }
      if (!Core.isRunId(info.run)) {
        entry.finish("Defect")
        const message = `runId gave ${JSON.stringify(info.run)}, which is not a run id: it must match ${Core.RUN_ID_PATTERN}`
        return { status: "defect", result: { defect: { kind: "host", message } } } as Core.Json
      }
      const json = payload.begin as unknown as BeginJson
      let begin: Begin
      if (json.kind === "restore") begin = { kind: "restore", saved: json.saved, migrated: json.migrated }
      else {
        const handles = new Map<string, object>()
        for (const peer of json.peers) handles.set(peer.run, entryOf(registry, engine, peer).handle)
        begin = { kind: "encoded", input: json.input, handles }
      }
      const env: Env = {
        runId: info.run,
        parent: undefined,
        sink: yield* TraceSink,
        timer: yield* Timer,
        observer: yield* RunObserver,
        token: undefined,
        invoker: undefined,
        durable: { exec, runId: info.run, entry },
        restoreFrom: undefined,
        tree: undefined
      }
      const { exit, snapshot } = yield* runDurable(program, begin, env)
      if (snapshot.status === "running") {
        // Not ended: the process is stopping, or the run parked (the engine suspends it). A
        // defect before the run began (a changed chart) ends the execution with it.
        const defect = Exit.isFailure(exit) ? Cause.squash(exit.cause) : undefined
        if (defect instanceof Defect) {
          entry.finish("Defect")
          return { status: "defect", result: { defect: defect.defect } } as Core.Json
        }
        return yield* Effect.failCause(Exit.isFailure(exit) ? exit.cause : Cause.interrupt())
      }
      return { status: snapshot.status, result: snapshot.result } as unknown as Core.Json
    })

  // ---- outside the run --------------------------------------------------------------------------

  const outcome = (json: Core.Json): Effect.Effect<D["output"], D["error"]> => {
    const { status, result } = json as unknown as Outcome
    if (status === "cancelled" || result === null) return Effect.interrupt
    return program.outcome({ result } as Core.Snapshot) as Effect.Effect<D["output"], D["error"]>
  }

  const payloadOf = (input: D["input"]) =>
    Effect.suspend(() => {
      try {
        const key = options.idempotencyKey(input)
        const refs = new Map<string, object>()
        const encoded = program.codec.withRefs(refs).encodeArgs(program.machine.params, input, `the input of ${program.name}`)
        const peers: Array<RunInfo> = []
        for (const handle of refs.values()) {
          const peer = entryOfPort(portOf(handle))
          if (peer === undefined) throw new Error(`a ref argument of a durable run must be the handle of a durable run`)
          peers.push(peer.info)
        }
        const begin: BeginJson = { kind: "start", input: encoded, peers }
        return Effect.succeed({ key, begin: begin as unknown as Core.Json })
      } catch (error) {
        return Effect.die(new Defect(hostDefect(error)))
      }
    })

  const executionId = (input: D["input"]) =>
    Effect.flatMap(payloadOf(input), (payload) => flow.executionId(payload))

  const start = (input: D["input"]) =>
    Effect.flatMap(payloadOf(input), (payload) => flow.execute(payload, { discard: true }))

  const run = (input: D["input"]) =>
    Effect.flatMap(payloadOf(input), (payload) => Effect.flatMap(flow.execute(payload), (json) => outcome(json as Core.Json)))

  const restore = (saved: Saved, restoreOptions?: RestoreOptions) =>
    Effect.flatMap(check(program, saved, restoreOptions), (checked) =>
      Effect.suspend(() => {
        const begun = checked as Extract<Begin, { kind: "restore" }>
        let key: string
        try {
          key = options.idempotencyKey(program.codec.decodeArgs(program.machine.params, begun.saved.input) as D["input"])
        } catch (error) {
          return Effect.die(new Defect(hostDefect(error)))
        }
        const begin: BeginJson = { kind: "restore", saved: begun.saved, migrated: begun.migrated }
        return flow.execute({ key, begin: begin as unknown as Core.Json }, { discard: true })
      }))

  const send = (id: string, event: D["event"]) =>
    Effect.flatMap(entryFor(id), (entry) => {
      let value: Core.Tagged
      try {
        value = program.encodeEvent(event)
      } catch (error) {
        return Effect.die(new Defect(hostDefect(error)))
      }
      return entry.deliverAcked({ kind: "event", value })
    })

  const poll = (id: string) =>
    Effect.flatMap(flow.poll(id), (found) => {
      if (Option.isNone(found) || found.value._tag === "Suspended") return Effect.succeedNone
      const exit = found.value.exit as Exit.Exit<Core.Json, never>
      if (Exit.isFailure(exit)) return Effect.succeed(Option.some(exit as Exit.Exit<D["output"], D["error"]>))
      return Effect.map(Effect.exit(outcome(exit.value)), Option.some)
    })

  const snapshot = (id: string) =>
    Effect.flatMap(entryFor(id), (entry) => {
      // During a replay the attempt's snapshot is older than the run: give the last whole one.
      const saved = entry.attempt !== undefined && entry.attempt.live() ? entry.attempt.save() : entry.saved
      if (saved === undefined) return Effect.die(new Error(`no snapshot of durable run ${entry.info.run} in this process`))
      return Effect.succeed(Core.copyJson(saved) as Saved)
    })

  const layer = Layer.mergeAll(
    flow.toLayer(body as never),
    Layer.effectDiscard(Effect.map(WorkflowEngine.WorkflowEngine, (engine) => {
      registryOf(engine).flows.set(name, { workflow: flow, encodeEvent: (event) => program.encodeEvent(event) })
    }))
  ) as unknown as Layer.Layer<never, never, WorkflowEngine.WorkflowEngine | R>

  return {
    workflow: flow,
    layer,
    executionId,
    start: start as DurableMachine<D, R>["start"],
    run: run as DurableMachine<D, R>["run"],
    restore: restore as DurableMachine<D, R>["restore"],
    send,
    cancel: (id) => Effect.flatMap(entryFor(id), (entry) => entry.cancel()),
    poll: poll as DurableMachine<D, R>["poll"],
    snapshot,
    handle: (id) => Effect.map(entryFor(id), (entry) => entry.handle as unknown as Handle<D>),
    emitted: (id) =>
      Stream.unwrap(Effect.map(entryFor(id), (entry) =>
        Stream.callback<Core.Tagged>((queue) =>
          Effect.acquireRelease(
            Effect.andThen(
              entry.whenKnown(),
              Effect.sync(() => entry.listen((value) => Queue.offerUnsafe(queue, value), () => Queue.endUnsafe(queue)))
            ),
            (unlisten) => Effect.sync(unlisten)
          )
        ))).pipe(Stream.map((value) => program.decodeEmitted(value) as D["emits"]))
  }
}
