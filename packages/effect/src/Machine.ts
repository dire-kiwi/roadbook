/**
 * The public types of a machine: what the generated code declares about it (`MachineDef`), the
 * implementations it needs (`Handlers`), and the handles `make` and `implement` return.
 */
import type * as Core from "@roadbook/core"
import type { Effect, Scope, Stream } from "effect"

/** What the compiler knows about a callable declared in a `.road` file. */
export interface Callable {
  readonly args: object
  readonly success: unknown
  readonly error: unknown
}

/** Everything the code generator emits about one machine, as types. */
export interface MachineDef {
  readonly name: string
  readonly input: object
  readonly output: unknown
  readonly error: unknown
  readonly event: unknown
  /** Every state id, the root's included. */
  readonly state: string
  readonly fns: Record<string, { readonly args: object; readonly returns: unknown }>
  readonly actions: Record<string, object>
  readonly tasks: Record<string, Callable>
  readonly machines: Record<string, Callable>
}

/** Anything that can be run with an input: an implemented machine. */
export interface Runnable<In, Out, Err, R> {
  /** Run one instance to its final state. The machine is an Effect. */
  readonly run: (input: In) => Effect.Effect<Out, Err, R>
}

/** The implementations a machine needs, with the types the chart declared. */
export type Handlers<D extends MachineDef> =
  & (keyof D["fns"] extends never ? {} : {
    readonly fns: { readonly [K in keyof D["fns"]]: (args: D["fns"][K]["args"]) => D["fns"][K]["returns"] }
  })
  & (keyof D["actions"] extends never ? {} : {
    readonly actions: {
      readonly [K in keyof D["actions"]]: (args: D["actions"][K]) => Effect.Effect<void, never, any>
    }
  })
  & (keyof D["tasks"] extends never ? {} : {
    readonly tasks: {
      readonly [K in keyof D["tasks"]]: (
        args: D["tasks"][K]["args"]
      ) => Effect.Effect<D["tasks"][K]["success"], D["tasks"][K]["error"], any>
    }
  })
  & (keyof D["machines"] extends never ? {} : {
    readonly machines: {
      // A child is either an implemented machine or any function with the same signature,
      // which is how tests stub a whole sub-machine.
      readonly [K in keyof D["machines"]]:
        | Runnable<D["machines"][K]["args"], D["machines"][K]["success"], D["machines"][K]["error"], any>
        | ((
          args: D["machines"][K]["args"]
        ) => Effect.Effect<D["machines"][K]["success"], D["machines"][K]["error"], any>)
    }
  })

/** Rejects implementations of names the chart does not declare. */
export type NoExtras<H, D extends MachineDef> = {
  readonly [G in keyof H]: G extends "fns" | "actions" | "tasks" | "machines"
    ? { readonly [K in keyof H[G]]: K extends keyof D[G] ? unknown : never }
    : never
}

/** The services required by a set of handlers: the union of every `R`. */
export type Requirements<H> = H extends { readonly [group: string]: infer G }
  ? G extends { readonly [name: string]: infer F }
    ? F extends (...args: any) => Effect.Effect<any, any, infer R> ? R
    : F extends Runnable<any, any, any, infer R> ? R
    : never
  : never
  : never

/** A run's state between steps. */
export interface Snapshot<D extends MachineDef> {
  readonly status: Core.Status
  /** Ids of every active state, outermost first, the root's included while it runs. */
  readonly active: ReadonlyArray<D["state"]>
  /** Whether `state` is active. */
  readonly matches: (state: D["state"]) => boolean
  /** The snapshot as the core holds it: plain JSON, values in their JSON form. */
  readonly core: Core.Snapshot
}

/** A running instance that accepts events. */
export interface Actor<D extends MachineDef> {
  /** Queue an event. Does nothing once the run has ended. */
  readonly send: (event: D["event"]) => Effect.Effect<void>
  readonly snapshot: Effect.Effect<Snapshot<D>>
  /** The current snapshot, then one after every step. */
  readonly changes: Stream.Stream<Snapshot<D>>
  /**
   * Completes when the run ends: with the output on `done`, the error on `failed`; it dies with
   * a `Defect` on a defect and is interrupted if the run was cancelled.
   */
  readonly result: Effect.Effect<D["output"], D["error"]>
  /**
   * Cancel the run gracefully and wait for it: the step in progress finishes, every active
   * state's exit effects run, and the trace ends with `cancelled`. Closing the actor's scope
   * does the same.
   */
  readonly cancel: Effect.Effect<void>
}

export interface Implemented<D extends MachineDef, R> extends Runnable<D["input"], D["output"], D["error"], R> {
  readonly def: Machine<D>
  /** Start a long-lived instance. Closing the scope cancels it. */
  readonly spawn: (input: D["input"]) => Effect.Effect<Actor<D>, never, R | Scope.Scope>
}

/** The typed handle the code generator exports for each machine. */
export interface Machine<D extends MachineDef> {
  readonly name: D["name"]
  /** The compiled chart: the same JSON the visualization is drawn from. */
  readonly ir: Core.Ir
  /**
   * Bind implementations. Every declared function, action, task and child machine must be
   * present, and nothing else may be.
   */
  readonly implement: <const H extends Handlers<D>>(handlers: H & NoExtras<H, D>) => Implemented<D, Requirements<H>>
}
