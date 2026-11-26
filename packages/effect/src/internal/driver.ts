/**
 * The driver: execution.md's "The driver", on Effect. One run is one fiber running the loop
 * below in its own `Scope`, with one `Queue` that events, invoke fibers and timer fibers all
 * offer to, behind a `Port` that other runs and handles push messages and emitted events onto.
 * Invokes are detached fibers in a map keyed by invoke id, timers fibers in a `FiberMap` keyed
 * by timer id. Each step runs uninterruptibly; interrupting the run is a graceful cancel, whose
 * step runs in `Effect.onInterrupt`. A run starts from its input (`start`) or from a saved
 * snapshot (`restore`), and can be saved at any time and suspended once idle.
 */
import * as Core from "@roadbook/core"
import { Cause, Context, Deferred, Effect, Exit, Fiber, FiberMap, Predicate, Queue, Stream, SubscriptionRef } from "effect"
import { Defect, hostDefect } from "../Defect.ts"
import type { Actor, Handle, MachineDef, RestoreOptions, Snapshot } from "../Machine.ts"
import { RootRunId, RunObserver, TraceSink, Timer } from "../References.ts"
import { RestoreError } from "../Saved.ts"
import type { Codec } from "./codec.ts"
import { activity, type Attempt, type DurableRun, entryOfPort, type Execution, park, parkRecord, peek, write as writeRecord } from "./durable.ts"
import { type Port, portOf, push, registerHandle, type Wake } from "./port.ts"

export type { Port } from "./port.ts"

// ---- the invoke context ----------------------------------------------------------------------

/**
 * The token a machine invoke's fiber carries. A machine's `run` that finds it unclaimed claims
 * it and becomes the child run; the parent sets `cancelled` before it interrupts the fiber
 * (after which the fiber queues nothing). It waits for the fiber to end only if the token was
 * claimed, and then reads `cancelDefect`.
 */
export interface CancelToken {
  claimed: boolean
  cancelled: boolean
  cancelDefect: Core.Defect | undefined
  /** The messages the parent sent to the invoke before a child run took them, in order. */
  held: Array<Core.Input>
  /** The child run's port, once the run that claimed the token has its queue. */
  child: Port | undefined
  /** A durable child is cancelled by an input: the parent asked for it. */
  cancelRequested: boolean
  /**
   * A durable machine invoke whose implementation has not finished yet and may still claim the
   * token: cancelling it asks and waits, so that a child run cut short by a crash gets to run
   * its cancel step, even if it has not claimed the token when its parent cancels it.
   */
  starting: boolean
}

/** What a machine invoke's implementation runs with: what a child run needs from its parent. */
export interface InvokeContext {
  readonly sink: TraceSink
  readonly timer: Timer
  readonly observer: RunObserver
  readonly parentRun: string
  readonly invoke: number
  readonly token: CancelToken
  /** The parent's port, when it invoked the child with `as`: where the child's emitted events go. */
  readonly invoker: Port | undefined
  /** The workflow execution the parent runs in, if it runs durably: the child runs in it too. */
  readonly durable: Execution | undefined
}

export const CurrentInvoke: Context.Reference<InvokeContext | undefined> = Context.Reference<
  InvokeContext | undefined
>("@roadbook/effect/InvokeContext", { defaultValue: () => undefined })

// ---- a machine bound to its implementations ----------------------------------------------------

type AnyFn = (args: any) => unknown
// The services implementations need are provided by whoever runs the machine, so inside the
// driver they are `never`.
type AnyEffectFn = (args: any) => Effect.Effect<unknown, unknown>

export interface AnyHandlers {
  readonly fns?: Readonly<Record<string, AnyFn>>
  readonly actions?: Readonly<Record<string, AnyEffectFn>>
  readonly tasks?: Readonly<Record<string, AnyEffectFn>>
  readonly machines?: Readonly<Record<string, AnyEffectFn | { readonly run: AnyEffectFn }>>
}

export class Program {
  readonly ir: Core.Ir
  readonly name: string
  readonly machine: Core.Machine
  readonly codec: Codec
  readonly handlers: AnyHandlers

  constructor(codec: Codec, name: string, handlers: AnyHandlers) {
    this.ir = codec.ir
    this.name = name
    this.machine = Core.getMachine(codec.ir, name)
    this.codec = codec
    this.handlers = handlers
  }

  /** The host's functions for a run whose values `codec` converts (its `ref`s among them). */
  fns(codec: Codec): Core.Fns {
    const { handlers, ir } = this
    return (fn, args) => {
      const decl = ir.functions[fn]
      const impl = handlers.fns?.[fn]
      try {
        if (decl === undefined || impl === undefined) throw new Error(`no implementation of function ${fn}`)
        const named: Record<string, unknown> = {}
        decl.params.forEach((param, i) => {
          named[param.name] = codec.decode(param.type, args[i] ?? null)
        })
        return { value: codec.encode(decl.returns, impl(named), `the result of ${fn}`) }
      } catch (error) {
        return { defect: hostDefect(error) }
      }
    }
  }

  /** The JSON form of one of the machine's events, given by host code. Throws a `CodecError`. */
  encodeEvent(event: unknown): Core.Tagged {
    const tag = Predicate.hasProperty(event, "_tag") ? String(event._tag) : ""
    const decl = Object.hasOwn(this.machine.events, tag) ? this.machine.events[tag] : undefined
    if (decl === undefined) throw new Error(`${this.name} has no event ${JSON.stringify(tag)}`)
    return this.codec.encodeTagged(event, "the event", decl.fields)
  }

  /** The host value a run with this snapshot ended with, as its Effect. */
  outcome(snapshot: Core.Snapshot): Effect.Effect<unknown, unknown> {
    const result = snapshot.result
    if (result === null) return Effect.interrupt
    if ("output" in result) {
      return Effect.succeed(this.machine.returns === null ? undefined : this.codec.decode(this.machine.returns, result.output))
    }
    if ("error" in result) return Effect.fail(this.codec.decodeError(result.error as Core.Tagged))
    return Effect.die(new Defect(result.defect))
  }
}

// ---- runs --------------------------------------------------------------------------------------

export interface Env {
  readonly runId: string
  readonly parent: string | undefined
  readonly sink: TraceSink
  readonly timer: Timer
  readonly observer: RunObserver
  /** The token this run claimed, if it is a child run. */
  readonly token: CancelToken | undefined
  /** For a child run invoked with `as`: its invoke id and the parent's port, for what it emits. */
  readonly invoker: { readonly invoke: number; readonly port: Port } | undefined
  /** Where a durable run keeps its journal and records; `undefined` for an ordinary run. */
  readonly durable: DurableRun | undefined
}

/**
 * A root run's environment. Its id comes from `RootRunId` and must be a run id: one that is not
 * would save a snapshot that `restore` refuses, so the run dies with a host defect before it
 * starts (no record, no snapshot).
 */
const rootEnv: Effect.Effect<Env> = Effect.gen(function*() {
  const nextId = yield* RootRunId
  const runId = nextId()
  if (!Core.isRunId(runId)) {
    return yield* Effect.die(
      new Defect({
        kind: "host",
        message: `RootRunId gave ${JSON.stringify(runId)}, which is not a run id: it must match ${Core.RUN_ID_PATTERN}`
      })
    )
  }
  return {
    runId,
    parent: undefined,
    sink: yield* TraceSink,
    timer: yield* Timer,
    observer: yield* RunObserver,
    token: undefined,
    invoker: undefined,
    durable: undefined
  }
})

/** The environment of a run: a child run if there is an unclaimed invoke context, else a root. */
const claimEnv: Effect.Effect<Env> = Effect.flatMap(CurrentInvoke, (ctx) => {
  if (ctx === undefined || ctx.token.claimed) return rootEnv
  ctx.token.claimed = true
  const runId = `${ctx.parentRun}.${ctx.invoke}`
  return Effect.succeed({
    runId,
    parent: ctx.parentRun,
    sink: ctx.sink,
    timer: ctx.timer,
    observer: ctx.observer,
    token: ctx.token,
    invoker: ctx.invoker === undefined ? undefined : { invoke: ctx.invoke, port: ctx.invoker },
    durable: ctx.durable === undefined ? undefined : { exec: ctx.durable, runId, entry: undefined }
  })
})

/**
 * How a run begins: from its input, from its input already in JSON form with the handles its
 * `ref`s decode to (a durable run's payload), or from a saved snapshot that passed every check.
 */
export type Begin =
  | { readonly kind: "start"; readonly input: unknown }
  | {
    readonly kind: "encoded"
    readonly input: { readonly [param: string]: Core.Json }
    readonly handles: ReadonlyMap<string, object>
  }
  | { readonly kind: "restore"; readonly saved: Core.Saved; readonly migrated: string | null }

interface Instance {
  readonly handle: Handle<any>
  readonly ref: SubscriptionRef.SubscriptionRef<Core.Snapshot>
  readonly started: Deferred.Deferred<void>
  /** Runs the machine to its end, or until it is suspended, and gives its outcome. */
  readonly loop: Effect.Effect<unknown, unknown>
  /** The saved snapshot as the last step left it. */
  readonly save: Effect.Effect<Core.Saved>
  /** Wait until the run is idle, then stop it; or until it ends. Gives its saved snapshot. */
  readonly suspend: Effect.Effect<Core.Saved>
  /** Whether `snapshot` is the run's last: it has ended, or this is the one set when suspended. */
  readonly isLast: (snapshot: Core.Snapshot) => boolean
  /** The core's snapshot as the last step left it. */
  readonly current: () => Core.Snapshot
}

const WAKE: Wake = { kind: "wake" }

const makeInstance = (program: Program, begin: Begin, env: Env): Effect.Effect<Instance> =>
  Effect.gen(function*() {
    const { ir, machine, name } = program
    // The handles in the arguments: the runs this one holds a `ref` to. A restored run has none.
    const refs = new Map<string, object>()
    const codec = program.codec.withRefs(refs)
    const fns = program.fns(codec)
    let coreInput: { readonly [param: string]: Core.Json }
    if (begin.kind === "restore") coreInput = begin.saved.input
    else if (begin.kind === "encoded") {
      coreInput = begin.input
      for (const [id, handle] of begin.handles) refs.set(id, handle)
    } else {
      try {
        coreInput = codec.encodeArgs(machine.params, begin.input, `the input of ${name}`)
      } catch (error) {
        return yield* Effect.die(new Defect(hostDefect(error)))
      }
    }
    const queue = yield* Queue.unbounded<Core.Input | Wake>()
    const durable = env.durable
    // A durable root run's handle outlives this attempt of it: its port is the registry
    // entry's, and the runs holding a `ref` to it subscribe there.
    const entry = durable?.entry
    const port: Port = {
      id: env.runId,
      machine: name,
      hash: machine.hash,
      queue,
      open: true,
      subscribers: entry === undefined ? new Set() : entry.port.subscribers
    }
    if (env.token !== undefined) {
      // A child run's queue starts with what its parent sent before it claimed the invoke;
      // from now on the parent's sends go straight to it.
      Queue.offerAllUnsafe(queue, env.token.held)
      env.token.held = []
      env.token.child = port
    }
    const peers = new Set<Port>()
    for (const handle of refs.values()) peers.add(portOf(handle)!)
    /** Closing: nothing more is pushed onto the queue, and the run hears from no peer. */
    const close = () => {
      if (!port.open) return
      port.open = false
      for (const peer of peers) {
        peer.subscribers.delete(port)
        // A durable run that only stops (it parks, or the process does) stays subscribed: what
        // its peers emit meanwhile wakes it, or waits for it.
        if (snapshot.status !== "running") entryOfPort(peer)?.unsubscribe(env.runId)
      }
      if (entry !== undefined && snapshot.status !== "running") entry.close()
    }
    const handle = entry !== undefined ? entry.handle as unknown as Handle<any> : registerHandle<Handle<any>>({
      id: env.runId,
      machine: name,
      send: (event: unknown) =>
        Effect.suspend(() => {
          let value: Core.Tagged
          try {
            value = program.encodeEvent(event)
          } catch (error) {
            return Effect.die(new Defect(hostDefect(error)))
          }
          push(port, { kind: "event", value })
          return Effect.void
        })
    } as unknown as Handle<any>, port)
    let snapshot: Core.Snapshot = {
      machine: name,
      run: env.runId,
      ir: machine.hash,
      status: "running",
      active: [],
      input: coreInput,
      vars: {},
      invokes: [],
      timers: [],
      nextInvoke: 1,
      nextTimer: 1,
      nextMessage: 1,
      history: {},
      result: null
    }
    const ref = yield* SubscriptionRef.make(snapshot)
    /** Completed once the start step's commands have been carried out. */
    const started = yield* Deferred.make<void>()

    // Saving. `dues` holds when each running timer is due on the driver's clock: the clock when
    // it carried out `timer.start` plus the delay, or, for a restored timer, its saved due.
    const dues = new Map<number, number>()
    const restoredDues = new Map<number, number>(
      begin.kind === "restore" ? begin.saved.timers.map((t) => [t.id, t.due]) : []
    )
    let saved: Core.Saved = Core.toSaved(snapshot, () => 0)
    // Suspending. The run checks after every step whether a suspend it was asked for can stop
    // it; while it waits for an input, `suspend` wakes it to check (`WAKE`).
    let suspendRequested = false
    let suspended = false
    let waiting = false
    let wakes = 0
    /** The copy of the snapshot set when the run was suspended. */
    let last: Core.Snapshot | undefined
    /** Completed with the run's last saved snapshot once it has stopped: ended or suspended. */
    const stopped = yield* Deferred.make<Core.Saved>()
    /** Whether a durable run has replayed its journal in this attempt. */
    let atEdge = false
    /** This attempt of a durable root run, as its registry entry sees it. */
    const attempt: Attempt = { queue, save: () => saved, snapshot: () => snapshot, live: () => atEdge }
    env.observer.started({
      id: env.runId,
      parent: env.parent,
      machine: name,
      snapshot: () => snapshot,
      offerAll: (inputs) => Effect.asVoid(Queue.offerAll(queue, inputs))
    })

    // A durable run does not write the records of the steps it takes again on replay.
    let quiet = false
    // The message and value of the last `sent` record: the core writes it, redacted, right
    // before the `send` command, and an `undelivered` record repeats it.
    let lastSent: { readonly message: number; readonly value: Core.Tagged } | undefined

    const write = (record: Core.CoreRecord) =>
      quiet ? Effect.void : Effect.map(env.timer.now, (t) => {
        env.sink.write(
          (env.parent === undefined
            ? { ...record, t, run: env.runId }
            : { ...record, t, run: env.runId, parent: env.parent }) as Core.TraceRecord
        )
      })

    const runAction = (action: string, args: { readonly [param: string]: Core.Json }) => {
      const impl = program.handlers.actions?.[action]
      const params = ir.actions[action]?.params ?? []
      return Effect.suspend(() => {
        if (impl === undefined) return Effect.die(new Error(`no implementation of action ${action}`))
        return impl(codec.decodeArgs(params, args))
      }).pipe(
        Effect.exit,
        Effect.map((exit) => (Exit.isSuccess(exit) ? undefined : causeDefect(exit.cause)))
      )
    }

    // ---- durable runs: the journal ----------------------------------------------------------
    // A durable run's inputs are journalled (`<run>/in/<k>`) before it takes them. On replay
    // the steps of the journalled inputs are taken again; their commands are carried out in
    // replay mode while the input after them is journalled too (`replaying`), which defers
    // starting tasks and arming timers to the live edge, so that work cancelled before a crash
    // is never started again. Inputs already journalled are dropped when they arrive again
    // (`consumed`): replay sends and emits again, and restarted tasks give stored outcomes.
    let replaying = false
    const consumed = new Set<string>()
    const deferredInvokes = new Map<number, Extract<Core.Command, { kind: "invoke.start" }>>()
    const deferredTimers = new Set<number>()
    let actionCount = 0

    /** A host action, durably: an activity, so that it is not run again on replay. */
    const doAction = (action: string, args: { readonly [param: string]: Core.Json }) => {
      if (durable === undefined) return runAction(action, args)
      return Effect.suspend(() =>
        Effect.map(
          activity(
            durable.exec,
            `${env.runId}/action/${++actionCount}`,
            Effect.map(runAction(action, args), (defect) => (defect ?? null) as Core.Json)
          ),
          (json) => (json === null ? undefined : json as unknown as Core.Defect)
        )
      )
    }

    const loop = Effect.gen(function*() {
      // Detached, so that dropping a task never waits for it (see `stopInvokes`).
      const invokes = new Map<number, Fiber.Fiber<void>>()
      const timers = yield* FiberMap.make<number>()
      const tokens = new Map<number, CancelToken>()

      const newToken = (invoke: number): CancelToken => {
        const token: CancelToken = {
          claimed: false,
          cancelled: false,
          cancelDefect: undefined,
          held: [],
          child: undefined,
          cancelRequested: false,
          starting: false
        }
        tokens.set(invoke, token)
        return token
      }

      const startInvoke = (command: Extract<Core.Command, { kind: "invoke.start" }>) => {
        const token = tokens.get(command.invoke) ?? newToken(command.invoke)
        const { src } = command
        const decl = command.srcKind === "task" ? ir.tasks[src] : ir.machines[src]
        const params = decl?.params ?? []
        const returns = decl?.returns ?? null
        const impl = command.srcKind === "task" ? program.handlers.tasks?.[src] : program.handlers.machines?.[src]
        const work = Effect.suspend(() => {
          if (impl === undefined) return Effect.die(new Error(`no implementation of ${command.srcKind} ${src}`))
          const args = codec.decodeArgs(params, command.args)
          return Predicate.hasProperty(impl, "run") ? (impl as { run: AnyEffectFn }).run(args) : (impl as AnyEffectFn)(args)
        })
        const context: InvokeContext | undefined = command.srcKind === "machine"
          ? {
            sink: env.sink,
            timer: env.timer,
            observer: env.observer,
            parentRun: env.runId,
            invoke: command.invoke,
            token,
            invoker: command.as === null ? undefined : port,
            durable: durable?.exec
          }
          : undefined
        const outcome = Effect.map(Effect.exit(work), (exit) => outcomeInput(codec, command.invoke, src, returns, exit))
        let recorded: Effect.Effect<Core.Input> = outcome
        if (durable !== undefined) {
          const name = `${env.runId}/invoke/${command.invoke}`
          if (command.srcKind === "machine" && Predicate.hasProperty(impl, "run")) {
            // A machine that runs from its chart claims the invoke and journals its own run;
            // only one that does not (a `run` of host code) has its outcome recorded here.
            token.starting = true
            recorded = Effect.flatMap(peek(durable.exec, name), (found) =>
              found !== undefined
                ? Effect.succeed(found as unknown as Core.Input)
                : Effect.tap(outcome, (input) =>
                  token.claimed ? Effect.void : writeRecord(durable.exec, name, input as unknown as Core.Json))).pipe(
                Effect.ensuring(Effect.sync(() => {
                  token.starting = false
                }))
              )
          } else {
            // A task, or a machine stubbed by a function: an activity.
            recorded = Effect.map(
              activity(durable.exec, name, outcome as unknown as Effect.Effect<Core.Json>),
              (json) => json as unknown as Core.Input
            )
          }
        }
        const fiber = recorded.pipe(
          Effect.flatMap((input) => {
            if (token.cancelled) return Effect.void
            return Queue.offer(queue, input)
          }),
          Effect.provideService(CurrentInvoke, context),
          Effect.annotateLogs("roadbook.invoke", command.invoke)
        )
        return Effect.map(Effect.forkDetach(fiber, { startImmediately: true }), (forked) => {
          invokes.set(command.invoke, forked)
          return undefined
        })
      }

      /**
       * Drop invokes. A task or stub (an unclaimed token) is interrupted at once and not waited
       * for: whatever it does while being interrupted is ignored. A child run (a claimed token)
       * is interrupted and waited for, since its cancel step runs its exit effects; a defect in
       * that step is the result. Several children are interrupted together. A durable child is
       * cancelled by asking it (it takes a cancel input) and waited for; when the run is only
       * stopping (`stop`: the process is, or the run parks), it is interrupted, which stops it
       * without a cancel step, to carry on from its journal later. A durable child whose outcome
       * was taken (`forget`) is left to finish on its own.
       */
      const stopInvokes = (ids: Iterable<number>, mode: "cancel" | "stop" | "forget" = "cancel") =>
        Effect.withFiber((self) => {
          const children: Array<{ fiber: Fiber.Fiber<void>; token: CancelToken }> = []
          const asked: Array<{ fiber: Fiber.Fiber<void>; token: CancelToken }> = []
          for (const id of ids) {
            const token = tokens.get(id)
            const fiber = invokes.get(id)
            tokens.delete(id)
            invokes.delete(id)
            deferredInvokes.delete(id)
            if (token === undefined || fiber === undefined) continue
            token.cancelled = true
            const child = token.claimed || (durable !== undefined && mode === "cancel" && token.starting)
            if (!child) fiber.interruptUnsafe(self.id)
            else if (durable === undefined) children.push({ fiber, token })
            else if (mode === "cancel") {
              token.cancelRequested = true
              if (token.child !== undefined) Queue.offerUnsafe(token.child.queue, WAKE)
              asked.push({ fiber, token })
            } else if (mode === "stop") children.push({ fiber, token })
          }
          if (children.length === 0 && asked.length === 0) return Effect.succeed(undefined)
          return Effect.map(
            Effect.andThen(
              Fiber.interruptAll(children.map((child) => child.fiber)),
              Effect.forEach(asked, (child) => Fiber.await(child.fiber), { discard: true })
            ),
            () =>
              mode === "stop"
                ? undefined
                : [...children, ...asked].find((child) => child.token.cancelDefect !== undefined)?.token.cancelDefect
          )
        })

      const stopInvoke = (id: number) => stopInvokes([id])

      /** Arm timer `id`, due at `due` on the driver's clock (durable runs) or after `delay`. */
      const arm = (id: number, delay: number) =>
        FiberMap.run(
          timers,
          id,
          env.timer.sleep({ run: env.runId, timer: id, delay }).pipe(
            Effect.andThen(Queue.offer(queue, { kind: "after", timer: id }))
          )
        )

      const startTimer = (command: Extract<Core.Command, { kind: "timer.start" }>) =>
        Effect.flatMap(env.timer.now, (now) =>
          Effect.gen(function*() {
            const restored = restoredDues.get(command.timer)
            restoredDues.delete(command.timer)
            // Capped at 2^53 - 1, the largest due time a saved snapshot holds.
            let due = restored ?? Math.min(now + command.delay, Number.MAX_SAFE_INTEGER)
            if (durable === undefined) {
              dues.set(command.timer, due)
              yield* arm(command.timer, command.delay)
              return undefined
            }
            // A durable timer keeps the due time it got when it first started.
            const name = `${env.runId}/timer/${command.timer}`
            const found = yield* peek(durable.exec, name)
            if (found !== undefined) due = found as number
            else yield* writeRecord(durable.exec, name, due)
            dues.set(command.timer, due)
            if (replaying) deferredTimers.add(command.timer)
            else yield* arm(command.timer, Math.max(0, due - now))
            return undefined
          }))

      const carry = (command: Core.Command): Effect.Effect<Core.Defect | undefined> => {
        switch (command.kind) {
          case "trace":
            if (command.record.kind === "sent") lastSent = { message: command.record.message, value: command.record.value }
            return Effect.as(write(command.record), undefined)
          case "action":
            return doAction(command.action, command.args)
          case "invoke.start":
            if (replaying) {
              // Started at the live edge, if it is still running then.
              newToken(command.invoke)
              deferredInvokes.set(command.invoke, command)
              return Effect.succeed(undefined)
            }
            return startInvoke(command)
          case "invoke.cancel":
            return stopInvoke(command.invoke)
          case "timer.start":
            return startTimer(command)
          case "timer.fire":
            // A restored timer already due: its expiry is queued at once, and it keeps its due.
            return Effect.sync(() => {
              dues.set(command.timer, restoredDues.get(command.timer) ?? 0)
              restoredDues.delete(command.timer)
              if (replaying) deferredTimers.add(command.timer)
              else Queue.offerUnsafe(queue, { kind: "after", timer: command.timer })
              return undefined
            })
          case "timer.cancel":
            dues.delete(command.timer)
            deferredTimers.delete(command.timer)
            return Effect.as(FiberMap.remove(timers, command.timer), undefined)
          case "send":
            return send(command)
          case "emit":
            return Effect.sync(() => {
              const { message, value } = command
              const from = env.runId
              if (env.invoker !== undefined) {
                push(env.invoker.port, { kind: "emitted", from, invoke: env.invoker.invoke, message, value })
              }
              for (const subscriber of port.subscribers) {
                push(subscriber, { kind: "emitted", from, invoke: null, message, value })
              }
              entry?.emit(message, value)
              return undefined
            })
        }
      }

      /** Queue a message for a child or a peer, or write `undelivered` if it has ended. */
      const send = (command: Extract<Core.Command, { kind: "send" }>): Effect.Effect<undefined> => {
        const { message, to, value } = command
        const input: Core.Input = { kind: "message", from: env.runId, message, value }
        if (command.invoke !== null) {
          const token = tokens.get(command.invoke)
          if (token === undefined) return Effect.succeed(undefined)
          if (token.child === undefined) {
            // Not claimed yet (or never: a stub), so kept for the run that claims it.
            token.held.push(input)
            return Effect.succeed(undefined)
          }
          if (push(token.child, input)) return Effect.succeed(undefined)
        } else {
          const peer = portOf(refs.get(to))
          if (peer !== undefined && push(peer, input)) return Effect.succeed(undefined)
        }
        // The value as the `sent` record showed it, redacted. The core always writes that record
        // first; were it missing, only the event's tag is shown.
        const shown = lastSent?.message === message ? lastSent.value : { _tag: value._tag }
        lastSent = undefined
        return Effect.as(write({ kind: "undelivered", message, to, value: shown }), undefined)
      }

      const carryOut = (stepped: Core.Stepped) =>
        Effect.gen(function*() {
          snapshot = stepped.snapshot
          // A run closes before the commands of its last step are carried out.
          if (snapshot.status !== "running") close()
          for (const command of stepped.commands) {
            const defect = yield* carry(command)
            if (defect === undefined) continue
            // A run that has already ended with a defect is not changed by another: this is a
            // child's cancel step failing while `failWithDefect`'s cancels are carried out. It
            // is ignored, and the rest of the commands (more cancels, the `defect` record) are
            // carried out.
            if (snapshot.status === "defect") continue
            // A host defect in an action, or in a child's cancel step: the rest of the step's
            // commands are dropped and the core's answer to the defect is carried out instead.
            const answer = Core.step(ir, snapshot, { kind: "defect", defect }, fns)
            snapshot = answer.snapshot
            close()
            for (const rest of answer.commands) yield* carry(rest)
            break
          }
          saved = Core.toSaved(snapshot, (timer) => dues.get(timer) ?? 0)
          yield* SubscriptionRef.set(ref, snapshot)
        })

      /** Idle: between steps, nothing queued, and no invoke running (execution.md, "Suspending"). */
      const idle = () => Queue.sizeUnsafe(queue) - wakes === 0 && snapshot.invokes.length === 0

      /**
       * Stop an idle run that was asked to suspend, which the check that found it idle has
       * closed: cancel its timers through its timer with no record (they are in the snapshot),
       * write `suspended`. No exit effect runs.
       */
      const stop = Effect.gen(function*() {
        yield* FiberMap.clear(timers)
        yield* write({ kind: "suspended" })
        // Once more, as a copy `changes` can tell from the others, so that it ends.
        last = { ...snapshot }
        yield* SubscriptionRef.set(ref, last)
      })

      const begun = (now: Effect.Effect<number>) =>
        begin.kind === "restore"
          ? Effect.map(now, (at) => Core.restore(ir, begin.saved, env.runId, at, begin.migrated, fns))
          : Effect.succeed(Core.start(ir, name, env.runId, coreInput, fns))

      const steps = Effect.uninterruptibleMask((restore) =>
        Effect.gen(function*() {
          // Subscribed to the peers' emitted events before `start`, until the run closes.
          for (const peer of peers) peer.subscribers.add(port)
          yield* carryOut(yield* begun(env.timer.now))
          yield* Deferred.succeed(started, undefined)
          while (snapshot.status === "running") {
            // A cancel that arrived during the step (an interrupt, held while the step ran) is
            // taken here, before a suspend can stop the run: a run with a cancel request
            // waiting is not idle.
            yield* restore(Effect.void)
            const stopping = yield* Effect.sync(() => {
              // Deciding and closing at once: nothing can be pushed in between and left untaken.
              if (suspendRequested && idle()) {
                suspended = true
                close()
                return true
              }
              waiting = true
              return false
            })
            if (stopping) return yield* stop
            const next = yield* restore(Queue.take(queue))
            waiting = false
            if (next.kind === "wake") {
              wakes -= 1
              continue
            }
            const stepped = Core.step(ir, snapshot, next, fns)
            // An invoke or timer is forgotten once its outcome or expiry is taken.
            if (next.kind === "invoke.done" || next.kind === "invoke.fail" || next.kind === "invoke.defect") {
              yield* stopInvoke(next.invoke)
            } else if (next.kind === "after") {
              dues.delete(next.timer)
              yield* FiberMap.remove(timers, next.timer)
            }
            yield* carryOut(stepped)
          }
        })
      )

      // ---- durable runs -----------------------------------------------------------------------

      /** What makes an input the same input when it arrives again: none for events and cancels. */
      const identity = (input: Core.Input): string | undefined => {
        switch (input.kind) {
          case "invoke.done":
          case "invoke.fail":
          case "invoke.defect":
            return `i${input.invoke}`
          case "after":
            return `t${input.timer}`
          case "message":
            return `m${input.from}/${input.message}`
          case "emitted":
            return `e${input.from}/${input.message}`
          default:
            return undefined
        }
      }

      /** Start what replay deferred: the child runs only (before the last journalled step), or all. */
      const startDeferred = (which: "children" | "all") =>
        Effect.gen(function*() {
          for (const [id, command] of [...deferredInvokes].sort(([a], [b]) => a - b)) {
            const impl = program.handlers.machines?.[command.src]
            const child = command.srcKind === "machine" && Predicate.hasProperty(impl, "run")
            if (which === "children" && !child) continue
            deferredInvokes.delete(id)
            yield* startInvoke(command)
          }
          if (which === "all") {
            const now = yield* env.timer.now
            for (const id of deferredTimers) yield* arm(id, Math.max(0, (dues.get(id) ?? now) - now))
            deferredTimers.clear()
          }
        })

      const durableSteps = (d: DurableRun) =>
        Effect.uninterruptibleMask((restore) =>
          Effect.gen(function*() {
            const { exec } = d
            const at = (k: number) => `${env.runId}/in/${k}`
            const journalled = (k: number) =>
              Effect.map(peek(exec, at(k)), (found) => found as unknown as Core.Input | undefined)
            // The run's first record: the hash it runs with, and the clock a restore reads.
            const beginName = `${env.runId}/begin`
            const first = (yield* peek(exec, beginName)) as { hash: string; now: number } | undefined
            let now0: number
            if (first === undefined) {
              now0 = yield* env.timer.now
              yield* writeRecord(exec, beginName, { hash: machine.hash, now: now0 })
            } else {
              if (first.hash !== machine.hash) {
                return yield* Effect.die(
                  new Defect({
                    kind: "host",
                    message:
                      `${name} changed while run ${env.runId} was in flight (${first.hash}, now ${machine.hash}): ` +
                      `a new version of a chart needs a new workflow name`
                  })
                )
              }
              now0 = first.now
              quiet = !exec.replayTrace
            }
            // Subscribed to the peers' emitted events: a durable holder from the message id it
            // first subscribed at, so that it is given again what it missed.
            for (const peer of peers) {
              const peerEntry = entryOfPort(peer)
              if (peerEntry === undefined || entry === undefined) {
                peer.subscribers.add(port)
                continue
              }
              const subName = `${env.runId}/sub/${peer.id}`
              let from = (yield* peek(exec, subName)) as number | undefined
              if (from === undefined) {
                // From what the peer emits next: what it has emitted is known once it replayed.
                yield* peerEntry.whenKnown()
                from = peerEntry.emitted + 1
                yield* writeRecord(exec, subName, from)
              }
              peerEntry.subscribe(entry, from)
            }
            let next = yield* journalled(1)
            replaying = next !== undefined
            yield* carryOut(yield* begun(Effect.succeed(now0)))
            yield* Deferred.succeed(started, undefined)
            let k = 0
            let live = false
            // Parking: the position it was considered at, and the next park's number there.
            let parkAt = 0
            let parkSeq = 1
            while (snapshot.status === "running") {
              let input: Core.Input
              if (next !== undefined) {
                input = next
                k += 1
                quiet = !exec.replayTrace
                next = yield* journalled(k + 1)
                const before = replaying
                replaying = next !== undefined
                // The last journalled step, which a crash may have cut short: carried out as a
                // live step, after starting the child runs, so that it can cancel them.
                if (before && !replaying) yield* startDeferred("children")
              } else {
                if (!live) {
                  live = true
                  quiet = false
                  replaying = false
                  yield* startDeferred("all")
                  atEdge = true
                  entry?.reachedEdge()
                }
                const taken = yield* takeLive(d, k + 1, restore, () => parkAt, (at, seq) => {
                  parkAt = at
                  parkSeq = seq
                }, () => parkSeq, () => parkSeq++)
                input = taken
                k += 1
                yield* writeRecord(exec, at(k), input as unknown as Core.Json)
                entry?.journalled(input)
              }
              const id = identity(input)
              if (id !== undefined) consumed.add(id)
              const stepped = Core.step(ir, snapshot, input, fns)
              if (input.kind === "invoke.done" || input.kind === "invoke.fail" || input.kind === "invoke.defect") {
                yield* stopInvokes([input.invoke], "forget")
              } else if (input.kind === "after") {
                dues.delete(input.timer)
                deferredTimers.delete(input.timer)
                yield* FiberMap.remove(timers, input.timer)
              }
              yield* carryOut(stepped)
              if (input.kind === "cancel" && env.token !== undefined) {
                const result = snapshot.result
                if (result !== null && "defect" in result) env.token.cancelDefect = result.defect
              }
            }
          })
        )

      /**
       * The next input of a live durable run: a cancel request first, then the queue, dropping
       * what it already took. An idle root run parks first, if its earliest timer is far enough.
       */
      const takeLive = (
        d: DurableRun,
        position: number,
        restore: <A, E, R>(effect: Effect.Effect<A, E, R>) => Effect.Effect<A, E, R>,
        parkAt: () => number,
        setPark: (at: number, seq: number) => void,
        nextPark: () => number,
        usePark: () => void
      ): Effect.Effect<Core.Input> =>
        Effect.gen(function*() {
          const { exec } = d
          while (true) {
            if (entry?.cancelRequested || env.token?.cancelRequested) return { kind: "cancel" } as const
            if (entry !== undefined && exec.park !== undefined && Queue.sizeUnsafe(queue) === 0 && invokes.size === 0) {
              if (parkAt() !== position) {
                // The parks a replay passes again at this position: each has its result.
                let seq = 1
                while ((yield* peek(exec, parkRecord(`${env.runId}/park/${position}/${seq}`))) !== undefined) seq++
                setPark(position, seq)
              }
              const now = yield* env.timer.now
              const earliest = dues.size === 0 ? undefined : Math.min(...dues.values())
              const delay = earliest === undefined ? undefined : earliest - now
              if (delay === undefined || delay > exec.park) {
                const at = `${env.runId}/park/${position}/${nextPark()}`
                // Deciding and parking at once: what reached the queue while the records above
                // were read is taken instead, and a park number is used only by a park.
                if (entry.parking(at)) {
                  usePark()
                  yield* restore(park(exec, at, delay))
                  entry.unparked()
                }
                continue
              }
            }
            waiting = true
            const item = yield* restore(Queue.take(queue))
            waiting = false
            if (item.kind === "wake") continue
            const id = identity(item)
            if (id !== undefined && consumed.has(id)) continue
            return item
          }
        })

      // Cancelling from outside: the step in progress has finished (steps are uninterruptible),
      // so the cancel step runs between steps, and the interruption completes after it.
      const cancel = Effect.suspend(() => {
        if (snapshot.status !== "running" || suspended) return Effect.void
        return Effect.gen(function*() {
          yield* carryOut(Core.step(ir, snapshot, { kind: "cancel" }, fns))
          const result = snapshot.result
          if (result !== null && "defect" in result) {
            if (env.token !== undefined) env.token.cancelDefect = result.defect
            return yield* Effect.die(new Defect(result.defect))
          }
        })
      })

      // Ending a run: anything still queued is discarded, and every invoke it still holds is
      // dropped as on `invoke.cancel` (a child run is cancelled and its cancel step waited for;
      // a defect there changes nothing). Its timers are dropped when the scope closes. A durable
      // run that is only stopping (the process is, or it parks) stops its children instead.
      const end = Effect.suspend(() => {
        close()
        Queue.shutdownUnsafe(queue)
        const mode = durable !== undefined && snapshot.status === "running" ? "stop" : "cancel"
        return Effect.asVoid(stopInvokes([...tokens.keys()], mode))
      }).pipe(
        Effect.ensuring(Effect.suspend(() => {
          if (entry !== undefined) entry.detach(attempt)
          return Deferred.succeed(stopped, saved)
        }))
      )

      if (durable === undefined) yield* steps.pipe(Effect.onInterrupt(() => cancel), Effect.ensuring(end))
      else {
        // Interrupting a durable run is the process stopping, or the run parking: no cancel step.
        entry?.attach(attempt)
        yield* durableSteps(durable).pipe(Effect.ensuring(end))
      }
      return yield* program.outcome(snapshot)
    }).pipe(
      Effect.scoped,
      Effect.provideService(CurrentInvoke, undefined),
      Effect.annotateLogs({ "roadbook.machine": name, "roadbook.run": env.runId }),
      Effect.withSpan(`roadbook ${name}`, {
        attributes: { "roadbook.machine": name, "roadbook.run": env.runId, ...(env.parent ? { "roadbook.parent": env.parent } : {}) }
      })
    )

    const suspend = Effect.suspend(() => {
      suspendRequested = true
      if (waiting) {
        // The run waits for an input, so it is between steps: wake it to check.
        wakes += 1
        Queue.offerUnsafe(queue, WAKE)
      }
      return Effect.flatMap(Deferred.await(stopped), (last) => {
        const result = last.result
        return result !== null && "defect" in result
          ? Effect.die(new Defect(result.defect))
          : Effect.succeed(Core.copyJson(last))
      })
    })

    return {
      handle,
      ref,
      started,
      loop,
      // A copy each time: host code that changes what it was given changes nothing here.
      save: Effect.sync(() => Core.copyJson(saved)),
      suspend,
      isLast: (s: Core.Snapshot) => s.status !== "running" || s === last,
      current: () => snapshot
    }
  })

/** The defect for a failed host Effect: a die's defect, else a failure's error. */
const causeDefect = (cause: Cause.Cause<unknown>): Core.Defect => {
  const die = cause.reasons.find(Cause.isDieReason)
  if (die !== undefined) return hostDefect(die.defect)
  const fail = cause.reasons.find(Cause.isFailReason)
  if (fail !== undefined) return hostDefect(fail.error)
  return { kind: "host", message: "interrupted" }
}

/** The input an invoke's exit gives its run. */
const outcomeInput = (
  codec: Codec,
  invoke: number,
  src: string,
  returns: string | null,
  exit: Exit.Exit<unknown, unknown>
): Core.Input => {
  try {
    if (Exit.isSuccess(exit)) {
      const value = returns === null ? null : codec.encode(returns, exit.value, `the result of ${src}`)
      return { kind: "invoke.done", invoke, value }
    }
    const reasons = exit.cause.reasons
    const fail = reasons.find(Cause.isFailReason)
    if (reasons.find(Cause.isDieReason) === undefined && fail !== undefined && Predicate.hasProperty(fail.error, "_tag")) {
      return { kind: "invoke.fail", invoke, error: codec.encodeTagged(fail.error, `the error of ${src}`) }
    }
    return { kind: "invoke.defect", invoke, defect: causeDefect(exit.cause) }
  } catch (error) {
    return { kind: "invoke.defect", invoke, defect: hostDefect(error) }
  }
}

// ---- the public entry points ---------------------------------------------------------------------

/**
 * Run a durable root run's attempt (an execution of its workflow, or a replay of one) in `env`,
 * which carries its journal: how the attempt ended, and the snapshot it left.
 */
export const runDurable = (program: Program, begin: Begin, env: Env) =>
  Effect.flatMap(
    makeInstance(program, begin, env),
    (instance) => Effect.map(Effect.exit(instance.loop), (exit) => ({ exit, snapshot: instance.current() }))
  )

export const run = (program: Program, input: unknown): Effect.Effect<unknown, unknown> =>
  Effect.flatMap(
    claimEnv,
    (env) => Effect.flatMap(makeInstance(program, { kind: "start", input }, env), (instance) => instance.loop)
  )

const toSnapshot = <D extends MachineDef>(core: Core.Snapshot): Snapshot<D> => ({
  status: core.status,
  active: core.active as ReadonlyArray<D["state"]>,
  matches: (state) => core.active.includes(state),
  core
})

/** Fork a top-level run into the scope and give its actor once it has started. */
const actorOf = <D extends MachineDef>(program: Program, begin: Begin) =>
  Effect.gen(function*() {
    const env = yield* rootEnv
    const instance = yield* makeInstance(program, begin, env)
    const fiber = yield* Effect.forkScoped(instance.loop)
    // The actor is returned once the run has started: its start step carried out.
    yield* Effect.raceFirst(Deferred.await(instance.started), Effect.asVoid(Fiber.await(fiber)))
    const handle = instance.handle as Handle<D>
    const actor: Actor<D> = {
      send: handle.send,
      handle,
      snapshot: Effect.map(SubscriptionRef.get(instance.ref), toSnapshot<D>),
      // Every step sets the snapshot, the last one included, and so does suspending, so the
      // stream ends with it.
      changes: SubscriptionRef.changes(instance.ref).pipe(
        Stream.takeUntil(instance.isLast),
        Stream.map(toSnapshot<D>)
      ),
      result: Fiber.join(fiber) as Effect.Effect<D["output"], D["error"]>,
      cancel: Fiber.interrupt(fiber),
      save: instance.save,
      suspend: instance.suspend
    }
    return actor
  })

export const spawn = <D extends MachineDef>(program: Program, input: unknown) =>
  actorOf<D>(program, { kind: "start", input })

/** What a thrown value says, whatever it is (a null-prototype object has no `toString`). */
const errorMessage = (error: unknown): string => {
  if (error instanceof Error) return error.message
  try {
    return String(error)
  } catch {
    try {
      return JSON.stringify(error) ?? typeof error
    } catch {
      return typeof error
    }
  }
}

/**
 * Check `saved` against the machine, migrating it if the hashes differ and `options.migrate`
 * is given. Refused (a `RestoreError`) before any run starts.
 */
export const check = (program: Program, saved: unknown, options?: RestoreOptions): Effect.Effect<Begin, RestoreError> =>
  Effect.suspend(() => {
    const migrate = options?.migrate
    const checked = Core.checkSaved(
      program.ir,
      program.name,
      saved,
      migrate === undefined ? undefined : (old) => {
        try {
          // The old snapshot has passed the checks of its shape, so it is a `Saved`.
          return { value: migrate(old as unknown as Core.Saved) as unknown as Core.Json }
        } catch (error) {
          return { error: errorMessage(error) }
        }
      }
    )
    if (!checked.ok) return Effect.fail(new RestoreError({ reason: checked.reason, message: checked.message }))
    return Effect.succeed<Begin>({ kind: "restore", saved: checked.saved, migrated: checked.migrated })
  })

/** Check `saved` (see `check`), then restore it as a new top-level run. */
export const restore = <D extends MachineDef>(program: Program, saved: unknown, options?: RestoreOptions) =>
  Effect.flatMap(check(program, saved, options), (begin) => actorOf<D>(program, begin))

/**
 * Restore, give the run `event` if there is one, and suspend it: the request handler's step.
 * Everything is checked before anything runs: the snapshot (a `RestoreError`), then the event,
 * which must be one of the machine's events and encode, as `send` requires (else the caller's
 * host defect, a die, as from `send`).
 */
export const resume = <D extends MachineDef>(
  program: Program,
  saved: unknown,
  event?: unknown,
  options?: RestoreOptions
) =>
  Effect.scoped(Effect.gen(function*() {
    const begin = yield* check(program, saved, options)
    let value: Core.Tagged | undefined
    if (event !== undefined) {
      try {
        value = program.encodeEvent(event)
      } catch (error) {
        return yield* Effect.die(new Defect(hostDefect(error)))
      }
    }
    const actor = yield* actorOf<D>(program, begin)
    if (value !== undefined) push(portOf(actor.handle)!, { kind: "event", value })
    return yield* actor.suspend
  }))
