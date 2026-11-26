/**
 * The driver: execution.md's "The driver", on Effect. One run is one fiber running the loop
 * below in its own `Scope`, with one `Queue` that events, invoke fibers and timer fibers all
 * offer to. Invokes and timers are fibers in `FiberMap`s keyed by their ids. Each step runs
 * uninterruptibly; interrupting the run is a graceful cancel, whose step runs in
 * `Effect.onInterrupt`.
 */
import * as Core from "@roadbook/core"
import { Cause, Context, Deferred, Effect, Exit, Fiber, FiberMap, Predicate, Queue, Stream, SubscriptionRef } from "effect"
import { Defect, hostDefect } from "../Defect.ts"
import type { Actor, MachineDef, Snapshot } from "../Machine.ts"
import { RootRunId, RunObserver, TraceSink, Timer } from "../References.ts"
import type { Codec } from "./codec.ts"

// ---- the invoke context ----------------------------------------------------------------------

/**
 * The token a machine invoke's fiber carries. A machine's `run` that finds it unclaimed claims
 * it and becomes the child run; the parent sets `cancelled` before it interrupts the fiber
 * (after which the fiber queues nothing), and reads `cancelDefect` after the fiber has ended.
 */
export interface CancelToken {
  claimed: boolean
  cancelled: boolean
  cancelDefect: Core.Defect | undefined
}

/** What a machine invoke's implementation runs with: what a child run needs from its parent. */
export interface InvokeContext {
  readonly sink: TraceSink
  readonly timer: Timer
  readonly observer: RunObserver
  readonly parentRun: string
  readonly invoke: number
  readonly token: CancelToken
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
  readonly fns: Core.Fns

  constructor(codec: Codec, name: string, handlers: AnyHandlers) {
    this.ir = codec.ir
    this.name = name
    this.machine = Core.getMachine(codec.ir, name)
    this.codec = codec
    this.handlers = handlers
    this.fns = (fn, args) => {
      const decl = this.ir.functions[fn]
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
}

const rootEnv: Effect.Effect<Env> = Effect.gen(function*() {
  const nextId = yield* RootRunId
  return {
    runId: nextId(),
    parent: undefined,
    sink: yield* TraceSink,
    timer: yield* Timer,
    observer: yield* RunObserver,
    token: undefined
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
    token: ctx.token
  })
})

interface Instance {
  readonly queue: Queue.Queue<Core.Input>
  readonly ref: SubscriptionRef.SubscriptionRef<Core.Snapshot>
  readonly started: Deferred.Deferred<void>
  /** Runs the machine to its end and gives its outcome. */
  readonly loop: Effect.Effect<unknown, unknown>
}

const makeInstance = (program: Program, input: unknown, env: Env): Effect.Effect<Instance> =>
  Effect.gen(function*() {
    const { codec, ir, machine, name } = program
    let coreInput: { [param: string]: Core.Json }
    try {
      coreInput = codec.encodeArgs(machine.params, input, `the input of ${name}`)
    } catch (error) {
      return yield* Effect.die(new Defect(hostDefect(error)))
    }
    const queue = yield* Queue.unbounded<Core.Input>()
    let snapshot: Core.Snapshot = {
      machine: name,
      ir: machine.hash,
      status: "running",
      active: [],
      input: coreInput,
      vars: {},
      invokes: [],
      timers: [],
      nextInvoke: 1,
      nextTimer: 1,
      result: null
    }
    const ref = yield* SubscriptionRef.make(snapshot)
    /** Completed once the start step's commands have been carried out. */
    const started = yield* Deferred.make<void>()
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
      const invokes = yield* FiberMap.make<number>()
      const timers = yield* FiberMap.make<number>()
      const tokens = new Map<number, CancelToken>()

      const startInvoke = (command: Extract<Core.Command, { kind: "invoke.start" }>) => {
        const token: CancelToken = { claimed: false, cancelled: false, cancelDefect: undefined }
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
            token
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
        return Effect.as(FiberMap.run(invokes, command.invoke, fiber), undefined)
      }

      /** Interrupt an invoke's fiber and wait for it; a claimed child reports its cancel step. */
      const stopInvoke = (id: number) =>
        Effect.suspend(() => {
          const token = tokens.get(id)
          if (token === undefined) return Effect.succeed(undefined)
          tokens.delete(id)
          token.cancelled = true
          return Effect.map(FiberMap.remove(invokes, id), () => (token.claimed ? token.cancelDefect : undefined))
        })

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
            return Effect.as(FiberMap.run(timers, command.timer, expiry), undefined)
          }
          case "timer.cancel":
            return Effect.as(FiberMap.remove(timers, command.timer), undefined)
        }
      }

      const carryOut = (stepped: Core.Stepped) =>
        Effect.gen(function*() {
          snapshot = stepped.snapshot
          for (const command of stepped.commands) {
            const defect = yield* carry(command)
            if (defect === undefined) continue
            // A host defect in an action, or in a child's cancel step: the rest of the step's
            // commands are dropped and the core's answer to the defect is carried out instead.
            const answer = Core.step(ir, snapshot, { kind: "defect", defect }, program.fns)
            snapshot = answer.snapshot
            for (const rest of answer.commands) yield* carry(rest)
            break
          }
          yield* SubscriptionRef.set(ref, snapshot)
        })

      const steps = Effect.uninterruptibleMask((restore) =>
        Effect.gen(function*() {
          yield* carryOut(Core.start(ir, name, coreInput, program.fns))
          yield* Deferred.succeed(started, undefined)
          while (snapshot.status === "running") {
            const next = yield* restore(Queue.take(queue))
            const stepped = Core.step(ir, snapshot, next, program.fns)
            // An invoke or timer is forgotten once its outcome or expiry is taken.
            if (next.kind === "invoke.done" || next.kind === "invoke.fail" || next.kind === "invoke.defect") {
              yield* stopInvoke(next.invoke)
            } else if (next.kind === "after") {
              yield* FiberMap.remove(timers, next.timer)
            }
            yield* carryOut(stepped)
          }
        })
      )

      // Cancelling from outside: the step in progress has finished (steps are uninterruptible),
      // so the cancel step runs between steps, and the interruption completes after it.
      const cancel = Effect.suspend(() => {
        if (snapshot.status !== "running") return Effect.void
        return Effect.gen(function*() {
          yield* carryOut(Core.step(ir, snapshot, { kind: "cancel" }, program.fns))
          const result = snapshot.result
          if (result !== null && "defect" in result) {
            if (env.token !== undefined) env.token.cancelDefect = result.defect
            return yield* Effect.die(new Defect(result.defect))
          }
        })
      })

      // Ending a run: anything still queued is discarded, and every invoke and timer it still
      // holds is dropped when the scope closes (a child run is cancelled first).
      const end = Effect.sync(() => {
        Queue.shutdownUnsafe(queue)
        for (const token of tokens.values()) token.cancelled = true
        tokens.clear()
      })

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

    return { queue, ref, started, loop }
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
  Effect.flatMap(claimEnv, (env) => Effect.flatMap(makeInstance(program, input, env), (instance) => instance.loop))

const toSnapshot = <D extends MachineDef>(core: Core.Snapshot): Snapshot<D> => ({
  status: core.status,
  active: core.active as ReadonlyArray<D["state"]>,
  matches: (state) => core.active.includes(state),
  core
})

export const spawn = <D extends MachineDef>(program: Program, input: unknown) =>
  Effect.gen(function*() {
    const env = yield* rootEnv
    const instance = yield* makeInstance(program, input, env)
    const fiber = yield* Effect.forkScoped(instance.loop)
    // The actor is returned once the run has started: its start step carried out.
    yield* Effect.raceFirst(Deferred.await(instance.started), Effect.asVoid(Fiber.await(fiber)))
    const actor: Actor<D> = {
      send: (event) =>
        Effect.suspend(() => {
          let value: Core.Tagged
          try {
            const tag = Predicate.hasProperty(event, "_tag") ? String(event._tag) : ""
            const decl = Object.hasOwn(program.machine.events, tag) ? program.machine.events[tag] : undefined
            if (decl === undefined) throw new Error(`${program.name} has no event ${JSON.stringify(tag)}`)
            value = program.codec.encodeTagged(event, "the event", decl.fields)
          } catch (error) {
            return Effect.die(new Defect(hostDefect(error)))
          }
          return Effect.asVoid(Queue.offer(instance.queue, { kind: "event", value }))
        }),
      snapshot: Effect.map(SubscriptionRef.get(instance.ref), toSnapshot<D>),
      changes: Stream.map(SubscriptionRef.changes(instance.ref), toSnapshot<D>),
      result: Fiber.join(fiber) as Effect.Effect<D["output"], D["error"]>,
      cancel: Fiber.interrupt(fiber)
    }
    return actor
  })
