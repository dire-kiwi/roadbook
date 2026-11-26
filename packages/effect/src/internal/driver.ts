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
}

/** What a machine invoke's implementation runs with: what a child run needs from its parent. */
export interface InvokeContext {
  readonly sink: TraceSink
  readonly timer: Timer
  readonly observer: RunObserver
  readonly parentRun: string
  readonly invoke: number
  readonly token: CancelToken
  /**
   * The parent's port, when it invoked the child with `as`: where the child's emitted events go,
   * those in `heard`, which the parent's machine has an `on` for.
   */
  readonly invoker: { readonly port: Port; readonly heard: ReadonlySet<string> } | undefined
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

interface Env {
  readonly runId: string
  readonly parent: string | undefined
  readonly sink: TraceSink
  readonly timer: Timer
  readonly observer: RunObserver
  /** The token this run claimed, if it is a child run. */
  readonly token: CancelToken | undefined
  /**
   * For a child run invoked with `as`: its invoke id, the parent's port, and which of the events
   * it emits the parent receives.
   */
  readonly invoker:
    | { readonly invoke: number; readonly port: Port; readonly heard: ReadonlySet<string> }
    | undefined
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
    invoker: undefined
  }
})

/** The environment of a run: a child run if there is an unclaimed invoke context, else a root. */
const claimEnv: Effect.Effect<Env> = Effect.flatMap(CurrentInvoke, (ctx) => {
  if (ctx === undefined || ctx.token.claimed) return rootEnv
  ctx.token.claimed = true
  return Effect.succeed({
    runId: `${ctx.parentRun}.${ctx.invoke}`,
    parent: ctx.parentRun,
    sink: ctx.sink,
    timer: ctx.timer,
    observer: ctx.observer,
    token: ctx.token,
    invoker: ctx.invoker === undefined ? undefined : { invoke: ctx.invoke, ...ctx.invoker }
  })
})

/** How a run begins: from its input, or from a saved snapshot that passed every check. */
type Begin =
  | { readonly kind: "start"; readonly input: unknown }
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
    else {
      try {
        coreInput = codec.encodeArgs(machine.params, begin.input, `the input of ${name}`)
      } catch (error) {
        return yield* Effect.die(new Defect(hostDefect(error)))
      }
    }
    const queue = yield* Queue.unbounded<Core.Input | Wake>()
    const port: Port = { id: env.runId, machine: name, hash: machine.hash, queue, open: true, subscribers: new Map() }
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
      for (const peer of peers) peer.subscribers.delete(port)
    }
    const handle = registerHandle<Handle<any>>({
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
    env.observer.started({
      id: env.runId,
      parent: env.parent,
      machine: name,
      snapshot: () => snapshot,
      offerAll: (inputs) => Effect.asVoid(Queue.offerAll(queue, inputs))
    })

    const write = (record: Core.CoreRecord) =>
      Effect.map(env.timer.now, (t) => {
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

    const loop = Effect.gen(function*() {
      // Detached, so that dropping a task never waits for it (see `stopInvokes`).
      const invokes = new Map<number, Fiber.Fiber<void>>()
      const timers = yield* FiberMap.make<number>()
      const tokens = new Map<number, CancelToken>()

      const startInvoke = (command: Extract<Core.Command, { kind: "invoke.start" }>) => {
        const token: CancelToken = {
          claimed: false,
          cancelled: false,
          cancelDefect: undefined,
          held: [],
          child: undefined
        }
        tokens.set(command.invoke, token)
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
            invoker: command.as === null ? undefined : { port, heard: Core.heard(ir, name, src) }
          }
          : undefined
        const fiber = work.pipe(
          Effect.exit,
          Effect.flatMap((exit) => {
            if (token.cancelled) return Effect.void
            return Queue.offer(queue, outcomeInput(codec, command.invoke, src, returns, exit))
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
       * that step is the result. Several children are interrupted together.
       */
      const stopInvokes = (ids: Iterable<number>) =>
        Effect.withFiber((self) => {
          const children: Array<{ fiber: Fiber.Fiber<void>; token: CancelToken }> = []
          for (const id of ids) {
            const token = tokens.get(id)
            const fiber = invokes.get(id)
            tokens.delete(id)
            invokes.delete(id)
            if (token === undefined || fiber === undefined) continue
            token.cancelled = true
            if (token.claimed) children.push({ fiber, token })
            else fiber.interruptUnsafe(self.id)
          }
          if (children.length === 0) return Effect.succeed(undefined)
          return Effect.map(
            Fiber.interruptAll(children.map((child) => child.fiber)),
            () => children.find((child) => child.token.cancelDefect !== undefined)?.token.cancelDefect
          )
        })

      const stopInvoke = (id: number) => stopInvokes([id])

      const carry = (command: Core.Command): Effect.Effect<Core.Defect | undefined> => {
        switch (command.kind) {
          case "trace":
            return Effect.as(write(command.record), undefined)
          case "action":
            return runAction(command.action, command.args)
          case "invoke.start":
            return startInvoke(command)
          case "invoke.cancel":
            return stopInvoke(command.invoke)
          case "timer.start": {
            const expiry = env.timer.sleep({ run: env.runId, timer: command.timer, delay: command.delay }).pipe(
              Effect.andThen(Queue.offer(queue, { kind: "after", timer: command.timer }))
            )
            return Effect.flatMap(env.timer.now, (now) => {
              const restored = restoredDues.get(command.timer)
              restoredDues.delete(command.timer)
              // Capped at 2^53 - 1, the largest due time a saved snapshot holds.
              dues.set(command.timer, restored ?? Math.min(now + command.delay, Number.MAX_SAFE_INTEGER))
              return Effect.as(FiberMap.run(timers, command.timer, expiry), undefined)
            })
          }
          case "timer.fire":
            // A restored timer already due: its expiry is queued at once, and it keeps its due.
            return Effect.sync(() => {
              dues.set(command.timer, restoredDues.get(command.timer) ?? 0)
              restoredDues.delete(command.timer)
              Queue.offerUnsafe(queue, { kind: "after", timer: command.timer })
              return undefined
            })
          case "timer.cancel":
            dues.delete(command.timer)
            return Effect.as(FiberMap.remove(timers, command.timer), undefined)
          case "send":
            return send(command)
          case "emit":
            return Effect.sync(() => {
              const { message, value } = command
              const from = env.runId
              const event = value._tag
              if (env.invoker !== undefined && env.invoker.heard.has(event)) {
                push(env.invoker.port, { kind: "emitted", from, invoke: env.invoker.invoke, message, value })
              }
              for (const [subscriber, heard] of port.subscribers) {
                if (heard.has(event)) push(subscriber, { kind: "emitted", from, invoke: null, message, value })
              }
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
        return Effect.as(write({ kind: "undelivered", message, to, value }), undefined)
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

      const begun = begin.kind === "start"
        ? Effect.succeed(Core.start(ir, name, env.runId, coreInput, fns))
        : Effect.map(env.timer.now, (now) => Core.restore(ir, begin.saved, env.runId, now, begin.migrated, fns))

      const steps = Effect.uninterruptibleMask((restore) =>
        Effect.gen(function*() {
          // Subscribed to the peers' emitted events before `start`, until the run closes.
          for (const peer of peers) peer.subscribers.set(port, Core.heard(ir, name, peer.machine))
          yield* carryOut(yield* begun)
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
      // a defect there changes nothing). Its timers are dropped when the scope closes.
      const end = Effect.suspend(() => {
        close()
        Queue.shutdownUnsafe(queue)
        return Effect.asVoid(stopInvokes([...tokens.keys()]))
      }).pipe(Effect.ensuring(Effect.suspend(() => Deferred.succeed(stopped, saved))))

      yield* steps.pipe(Effect.onInterrupt(() => cancel), Effect.ensuring(end))
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
      isLast: (s: Core.Snapshot) => s.status !== "running" || s === last
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
const check = (program: Program, saved: unknown, options?: RestoreOptions): Effect.Effect<Begin, RestoreError> =>
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
