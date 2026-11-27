/**
 * What the driver needs from `effect/workflow` to run durably (docs/design/typescript.md,
 * "Durable runs"): named records of an execution, read without waiting and written once
 * (`DurableDeferred`s through the engine), activities, parking on a `DurableClock`, and the
 * registry of the durable runs of one engine in this process, through which their handles,
 * `send`, `cancel` and other runs reach them whether they are live, parked, recovering or ended.
 */
import * as Core from "@dire-kiwi/roadbook-core"
import { Deferred, Effect, Exit, Option, Queue, Schema } from "effect"
import { Activity, DurableClock, DurableDeferred, type Workflow, WorkflowEngine } from "effect/workflow"
import { Defect, hostDefect } from "../Defect.ts"
import { type Item, type Port, registerHandle, type Wake } from "./port.ts"

// ---- one execution ------------------------------------------------------------------------------

/** A workflow of a durable machine, as the registry needs it: to wake, resume and poll a run. */
export type AnyWorkflow = Workflow.Workflow<string, Workflow.AnyStructSchema, Schema.Top, Schema.Top>

/** One workflow execution: the engine running it, its instance, and the run's options. */
export interface Execution {
  readonly engine: WorkflowEngine.WorkflowEngine["Service"]
  readonly instance: WorkflowEngine.WorkflowInstance["Service"]
  readonly workflow: AnyWorkflow
  readonly executionId: string
  /** Whether the steps taken again on replay write their trace records again. */
  readonly replayTrace: boolean
  /** Park an idle root run whose earliest timer is further than this (ms); never if undefined. */
  readonly park: number | undefined
  readonly registry: Registry
  /** The registry entry of the execution's root run, through which its runs' calls are answered. */
  readonly root: Entry
}

/** A run of an execution: the root run or one of its child runs. */
export interface DurableRun {
  readonly exec: Execution
  readonly runId: string
  /** The registry entry of a root run; child runs are reached through their parent. */
  readonly entry: Entry | undefined
}

const recordOf = (name: string) => DurableDeferred.make(name, { success: Schema.Json })

const inExecution = <A, E>(exec: Execution, effect: Effect.Effect<A, E, any>): Effect.Effect<A, E> =>
  effect.pipe(
    Effect.provideService(WorkflowEngine.WorkflowEngine, exec.engine),
    Effect.provideService(WorkflowEngine.WorkflowInstance, exec.instance)
  ) as Effect.Effect<A, E>

/** The value recorded under `name` in the execution, if there is one. */
export const peek = (exec: Execution, name: string): Effect.Effect<Core.Json | undefined> =>
  inExecution(
    exec,
    Effect.flatMap(exec.engine.deferredResult(recordOf(name)), (found) => {
      if (Option.isNone(found)) return Effect.succeed(undefined)
      const exit = found.value
      return Exit.isSuccess(exit) ? Effect.succeed(exit.value as Core.Json) : Effect.die(`record ${name} holds a failure`)
    })
  )

/** Record `value` under `name`, once: a name already recorded keeps its first value. */
export const write = (exec: Execution, name: string, value: Core.Json): Effect.Effect<void> =>
  inExecution(
    exec,
    exec.engine.deferredDone(recordOf(name), {
      workflowName: exec.workflow._tag,
      executionId: exec.executionId,
      deferredName: name,
      exit: Exit.succeed(value)
    })
  ).pipe(Effect.orDie)

/** Run `effect` as the activity `name`: once, its value stored, given again on replay. */
export const activity = (exec: Execution, name: string, effect: Effect.Effect<Core.Json>): Effect.Effect<Core.Json> =>
  inExecution(exec, Activity.make({ name, success: Schema.Json, execute: effect })) as Effect.Effect<
    Core.Json
  >

// ---- calls between durable runs ----------------------------------------------------------------

/**
 * The record of the answer to call invoke `invoke` of run `run`, in the run's execution: its
 * outcome input, written once, by the callee (its `reply`, or `Unanswered` as it ends) or by the
 * caller itself (`Unanswered`, for a callee that ended before it journalled the call). The first
 * answer written is the one the caller takes.
 */
export const answerRecord = (run: string, invoke: number) => `${run}/answer/${invoke}`

/** The record a caller writes once its callee has journalled call invoke `invoke`. */
export const callRecord = (run: string, invoke: number) => `${run}/call/${invoke}`

/** The outcome input that answers a call `Unanswered`. */
export const unanswered = (invoke: number): Core.Input => ({
  kind: "invoke.fail",
  invoke,
  error: { _tag: Core.UNANSWERED }
})

/**
 * Answer the call that run `run`, of the execution whose root run is `root`, made with its
 * invoke: record `outcome` there (a name already recorded keeps its first answer), then tell
 * the run, if it is live in this process. Nothing is written for a caller that has ended here.
 */
export const answer = (exec: Execution, root: RunInfo, run: string, outcome: Core.Input): Effect.Effect<void> =>
  Effect.suspend(() => {
    const caller = entryOf(exec.registry, exec.engine, root)
    if (caller.state === "ended") return Effect.void
    const invoke = (outcome as { readonly invoke: number }).invoke
    const name = answerRecord(run, invoke)
    return inExecution(
      exec,
      exec.engine.deferredDone(recordOf(name), {
        workflowName: root.workflow,
        executionId: root.executionId,
        deferredName: name,
        exit: Exit.succeed(outcome as unknown as Core.Json)
      })
    ).pipe(Effect.orDie, Effect.andThen(Effect.sync(() => caller.answered(run, invoke))))
  })

/** The root run of the caller's execution, for each call input a durable caller gave a callee. */
const vias = new WeakMap<Core.Input, RunInfo>()

/** The root run of the execution of the durable run that made call input `input`, if one did. */
export const viaOf = (input: Core.Input): RunInfo | undefined => vias.get(input)

/** What an idle run waits for when it parks: its earliest timer's clock, or a wake. */
export type ParkResult = "clock" | "wake"

const wakeRecord = (name: string) => DurableDeferred.make(`${name}/wake`, { success: Schema.Json })

/** The name of the race a park at `name` records its result under. */
export const parkRecord = (name: string) => `raceAll/${name}`

/**
 * Park: suspend the execution until the clock (`delay` ms, if there is a timer), a wake, or the
 * answer to one of the run's calls (the records `answers`), which wakes it. The engine interrupts
 * the body and replays it when one of them completes; if one already has (a replay reaching the
 * same park), its result is given at once.
 */
export const park = (
  exec: Execution,
  name: string,
  delay: number | undefined,
  answers: ReadonlyArray<string>
): Effect.Effect<ParkResult> => {
  const wake = DurableDeferred.await(wakeRecord(name)).pipe(Effect.as("wake" as const))
  const answered = answers.map((answer) => DurableDeferred.await(recordOf(answer)).pipe(Effect.as("wake" as const)))
  const effects = delay === undefined
    ? [wake, ...answered] as const
    : [
      DurableClock.sleep({ name: `${name}/clock`, duration: Math.max(1, delay), inMemoryThreshold: 0 }).pipe(
        Effect.as("clock" as const)
      ),
      wake,
      ...answered
    ] as const
  return inExecution(
    exec,
    DurableDeferred.raceAll({ name, success: Schema.Literals(["clock", "wake"]), error: Schema.Never, effects })
  ) as Effect.Effect<ParkResult>
}

// ---- the registry -----------------------------------------------------------------------------

/** The durable runs of one engine in this process, by run id, and the workflows registered. */
export interface Registry {
  readonly runs: Map<string, Entry>
  /** The durable machines registered with the engine, by workflow name. */
  readonly flows: Map<string, Flow>
}

/** A durable machine as the registry needs it: its workflow, and how its events encode. */
export interface Flow {
  readonly workflow: AnyWorkflow
  /** The JSON form of one of the machine's events. Throws if it is not one, or does not encode. */
  readonly encodeEvent: (event: unknown) => Core.Tagged
}

const registries = new WeakMap<object, Registry>()

/**
 * The registry of an engine's durable runs in this process. It is keyed by the engine's
 * `register`, not the engine object, so that an engine wrapped by spreading it (`{ ...engine,
 * … }`, to trace or instrument it) shares one registry with the engine its workflows run on.
 */
export const registryOf = (engine: WorkflowEngine.WorkflowEngine["Service"]): Registry => {
  let registry = registries.get(engine.register)
  if (registry === undefined) {
    registry = { runs: new Map(), flows: new Map() }
    registries.set(engine.register, registry)
  }
  return registry
}

/** What identifies a durable run from outside, as a `ref` argument records it. */
export interface RunInfo {
  readonly run: string
  readonly workflow: string
  readonly executionId: string
  readonly machine: string
  readonly hash: string
}

/** A live attempt of a root run: the queue of the body running it now. */
export interface Attempt {
  readonly queue: Queue.Queue<Item>
  readonly save: () => Core.Saved
  readonly snapshot: () => Core.Snapshot
  /** Whether it has replayed its journal: its snapshot is then the run's, and no older one. */
  readonly live: () => boolean
}

const WAKE: Wake = { kind: "wake" }

/**
 * A durable root run as this process knows it. `pending`: no attempt here yet (the engine is
 * about to replay it, or it runs elsewhere); `live`: an attempt is running here; `parked`: idle
 * and suspended on a durable clock and a wake; `waking`: woken, not live again yet; `ended`.
 * Inputs for a run that is not live wait in `mailbox` and are queued when it is.
 */
export class Entry {
  state: "pending" | "live" | "parked" | "waking" | "ended" = "pending"
  attempt: Attempt | undefined = undefined
  mailbox: Array<Core.Input> = []
  /**
   * Inputs whose sender waits until the run has journalled them (true), or until it has ended
   * (false): an event from host code, or a call from a durable caller.
   */
  readonly acks = new Map<Core.Input, Deferred.Deferred<boolean>>()
  /**
   * The runs of this entry's execution (its root run and its child runs) that are live in this
   * process, each with what it does when the answer to one of its call invokes is recorded.
   */
  readonly callers = new Map<string, (invoke: number) => void>()
  cancelRequested = false
  /** The park the run is suspended in, while `parked`. */
  parkedAt: string | undefined = undefined
  /** The saved snapshot as the last step in this process left it. */
  saved: Core.Saved | undefined = undefined
  /** Completed when the run ends. */
  readonly ended = Deferred.makeUnsafe<void>()
  /**
   * Completed once an attempt of the run in this process has replayed its journal: from then on
   * `emitted` is the run's, not only what this process has seen of it.
   */
  readonly known = Deferred.makeUnsafe<void>()
  /** The largest message id this run has emitted, and what it emitted, for later subscribers. */
  emitted = 0
  readonly log: Array<{ readonly message: number; readonly value: Core.Tagged }> = []
  /**
   * The durable runs holding a `ref` to this one, from which message id each receives, and
   * which of the events this run emits it receives: those its machine has an `on` for.
   */
  readonly subscribers = new Map<
    string,
    { readonly entry: Entry; readonly from: number; readonly heard: ReadonlySet<string> }
  >()
  /**
   * Host listeners of the run's emitted events (`DurableMachine.emitted`), each with the message
   * id it hears from: an emit a replay carries out again is not heard again.
   */
  private readonly listeners = new Set<{ next: number; readonly emit: (value: Core.Tagged) => void; readonly end: () => void }>()
  readonly port: Port
  readonly handle: { readonly id: string; readonly machine: string; send: (event: unknown) => Effect.Effect<void> }
  private resumed = false
  readonly info: RunInfo
  readonly engine: WorkflowEngine.WorkflowEngine["Service"]
  readonly registry: Registry

  constructor(info: RunInfo, engine: WorkflowEngine.WorkflowEngine["Service"], registry: Registry) {
    this.info = info
    this.engine = engine
    this.registry = registry
    const entry = this
    this.port = {
      id: info.run,
      machine: info.machine,
      hash: info.hash,
      get queue() {
        return entry.attempt?.queue as Queue.Queue<Item>
      },
      get open() {
        return entry.state !== "ended"
      },
      subscribers: new Map(),
      deliver: (input) => entry.deliver(input)
    }
    // As `send` from host code always does, it queues and does not wait for the run.
    this.handle = {
      id: info.run,
      machine: info.machine,
      send: (event) =>
        Effect.suspend(() => {
          const flow = registry.flows.get(info.workflow)
          let value: Core.Tagged
          try {
            if (flow === undefined) throw new Error(`no durable machine ${info.workflow} is registered with the engine`)
            value = flow.encodeEvent(event)
          } catch (error) {
            return Effect.die(new Defect(hostDefect(error)))
          }
          this.deliver({ kind: "event", value })
          return Effect.void
        })
    }
  }

  private fork(effect: Effect.Effect<void, unknown, WorkflowEngine.WorkflowEngine>) {
    Effect.runFork(Effect.ignore(Effect.provideService(effect, WorkflowEngine.WorkflowEngine, this.engine)))
  }

  private workflow(): AnyWorkflow | undefined {
    return this.registry.flows.get(this.info.workflow)?.workflow
  }

  /**
   * Have the engine replay the run, once: it is not live here and something waits for it. If
   * its execution has already ended (in an earlier process, or before the run could begin), the
   * entry ends instead, so that nothing waits for it for ever.
   */
  private resume() {
    const workflow = this.workflow()
    if (this.resumed || workflow === undefined) return
    this.resumed = true
    const executionId = this.info.executionId
    this.fork(Effect.flatMap(workflow.poll(executionId), (found) =>
      Option.isSome(found) && found.value._tag === "Complete"
        ? Effect.sync(() => this.finish())
        : workflow.resume(executionId)) as Effect.Effect<void, never, WorkflowEngine.WorkflowEngine>)
  }

  /** Wait until `emitted` is the run's: an attempt here has replayed it, or the run has ended. */
  whenKnown(): Effect.Effect<void> {
    return Effect.suspend(() => {
      if (this.state === "pending") this.resume()
      return Effect.raceFirst(Deferred.await(this.known), Deferred.await(this.ended))
    })
  }

  /** An attempt here has replayed the run's journal. */
  reachedEdge() {
    Deferred.doneUnsafe(this.known, Exit.void)
  }

  private wake() {
    const workflow = this.workflow()
    const at = this.parkedAt
    if (workflow === undefined || at === undefined) return
    this.state = "waking"
    const record = wakeRecord(at)
    const token = DurableDeferred.tokenFromExecutionId(record, { workflow, executionId: this.info.executionId })
    this.fork(DurableDeferred.succeed(record, { token, value: { kind: "wake" } }))
  }

  /** Give the run `input`. Returns false once it has ended. */
  deliver(input: Core.Input): boolean {
    switch (this.state) {
      case "ended":
        return false
      case "live":
        return Queue.offerUnsafe(this.attempt!.queue, input)
      case "parked":
        this.mailbox.push(input)
        this.wake()
        return true
      default:
        this.mailbox.push(input)
        if (this.state === "pending") this.resume()
        return true
    }
  }

  /** Give the run an input from host code, and wait until it is journalled or the run ends. */
  deliverAcked(input: Core.Input): Effect.Effect<void> {
    return Effect.suspend(() => {
      if (this.state === "ended") return Effect.void
      const ack = Deferred.makeUnsafe<boolean>()
      this.acks.set(input, ack)
      this.deliver(input)
      return Effect.asVoid(Deferred.await(ack))
    })
  }

  /**
   * Give the run call input `input` from a durable caller, whose execution's root run is `via`
   * (where the answer goes), and wait until the run has journalled it (true) or has ended
   * (false). A run that ended is waited for until its driver has answered the calls it had
   * journalled, so that a caller answering itself `Unanswered` never answers before it.
   */
  deliverCall(input: Core.Input, via: RunInfo): Effect.Effect<boolean> {
    return Effect.suspend(() => {
      vias.set(input, via)
      if (this.state === "ended") return Effect.as(Deferred.await(this.ended), false)
      const ack = Deferred.makeUnsafe<boolean>()
      this.acks.set(input, ack)
      this.deliver(input)
      return Deferred.await(ack)
    })
  }

  /** The answer to call invoke `invoke` of run `run` (of this execution) has been recorded. */
  answered(run: string, invoke: number) {
    this.callers.get(run)?.(invoke)
  }

  /** Cancel the run (a cancel input, taken before what is queued) and wait for it to end. */
  cancel(): Effect.Effect<void> {
    return Effect.suspend(() => {
      if (this.state === "ended") return Effect.void
      this.cancelRequested = true
      if (this.state === "live") Queue.offerUnsafe(this.attempt!.queue, WAKE)
      else if (this.state === "parked") this.wake()
      else if (this.state === "pending") this.resume()
      return Deferred.await(this.ended)
    })
  }

  /** An attempt of the run starts in this process. */
  attach(attempt: Attempt) {
    if (this.state === "ended") return
    this.attempt = attempt
    this.state = "live"
    this.resumed = false
    this.drain()
  }

  private drain() {
    const waiting = this.mailbox
    this.mailbox = []
    for (const input of waiting) Queue.offerUnsafe(this.attempt!.queue, input)
  }

  /** The run journalled `input` (or had already): its sender stops waiting. */
  journalled(input: Core.Input) {
    const ack = this.acks.get(input)
    if (ack === undefined) return
    this.acks.delete(input)
    Deferred.doneUnsafe(ack, Exit.succeed(true))
  }

  /**
   * The live run parks at `name`, unless an input reached its queue meanwhile (it then takes
   * it instead). Called in the same synchronous block as the run's own check that it is idle.
   */
  parking(name: string): boolean {
    if (this.cancelRequested || (this.attempt !== undefined && Queue.sizeUnsafe(this.attempt.queue) > 0)) return false
    this.state = "parked"
    this.parkedAt = name
    if (this.mailbox.length > 0) this.wake()
    return true
  }

  /** The run carries on after a park in the same attempt. */
  unparked() {
    this.state = "live"
    this.parkedAt = undefined
    this.drain()
  }

  /** The run's last step ended it: nothing more reaches it. */
  close() {
    this.state = "ended"
    this.mailbox = []
    this.subscribers.clear()
    this.log.length = 0
  }

  /**
   * Listen to what the run emits from now on: from the message after the largest it has emitted
   * (call it once `whenKnown` has completed). Ended when the run ends; at once if it has.
   */
  listen(emit: (value: Core.Tagged) => void, end: () => void): () => void {
    if (Deferred.isDoneUnsafe(this.ended)) {
      end()
      return () => {}
    }
    const listener = { next: this.emitted + 1, emit, end }
    this.listeners.add(listener)
    return () => {
      this.listeners.delete(listener)
    }
  }

  /** The run has ended: whoever waits for it stops waiting. */
  finish() {
    this.close()
    for (const listener of this.listeners) listener.end()
    this.listeners.clear()
    for (const ack of this.acks.values()) Deferred.doneUnsafe(ack, Exit.succeed(false))
    this.acks.clear()
    Deferred.doneUnsafe(this.ended, Exit.void)
  }

  /** The attempt stopped: the run ended, parked, or the process is stopping. */
  detach(attempt: Attempt) {
    if (this.attempt !== attempt) return
    // A snapshot from the middle of a replay is older than the run: keep the last whole one.
    if (attempt.live()) this.saved = attempt.save()
    this.attempt = undefined
    if (attempt.snapshot().status !== "running") this.finish()
    else if (this.state === "live") this.state = "pending"
  }

  /** Emit to the durable runs holding a `ref` to this one (replays emit again; they deduplicate). */
  emit(message: number, value: Core.Tagged) {
    if (message > this.emitted) {
      this.emitted = message
      this.log.push({ message, value })
    }
    for (const listener of this.listeners) {
      if (message < listener.next) continue
      listener.next = message + 1
      listener.emit(value)
    }
    for (const { entry, from, heard } of this.subscribers.values()) {
      if (message >= from && heard.has(value._tag)) {
        entry.deliver({ kind: "emitted", from: this.info.run, invoke: null, message, value })
      }
    }
  }

  /**
   * `holder` receives what this run emits from message `from` on, what it already emitted too:
   * the events in `heard`.
   */
  subscribe(holder: Entry, from: number, heard: ReadonlySet<string>) {
    if (this.state === "ended") return
    this.subscribers.set(holder.info.run, { entry: holder, from, heard })
    for (const { message, value } of this.log) {
      if (message >= from && heard.has(value._tag)) {
        holder.deliver({ kind: "emitted", from: this.info.run, invoke: null, message, value })
      }
    }
  }

  unsubscribe(holder: string) {
    this.subscribers.delete(holder)
  }
}

/** The entry of a durable run, made the first time this process hears of it. */
export const entryOf = (
  registry: Registry,
  engine: WorkflowEngine.WorkflowEngine["Service"],
  info: RunInfo
): Entry => {
  let entry = registry.runs.get(info.run)
  if (entry === undefined) {
    entry = new Entry(info, engine, registry)
    registry.runs.set(info.run, entry)
    ports.set(entry.port, entry)
    registerHandle(entry.handle, entry.port)
  }
  return entry
}

/** The entry behind a durable run's handle's port, or `undefined` for any other port. */
export const entryOfPort = (port: Port | undefined): Entry | undefined =>
  port === undefined ? undefined : ports.get(port)

const ports = new WeakMap<Port, Entry>()
