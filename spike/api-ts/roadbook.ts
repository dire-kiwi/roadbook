/**
 * Type-level sketch of the proposed `@roadbook/effect` public API.
 * Nothing here is implemented: every value is `declare`d. The point is to
 * check that the types work against the real Effect v4 package.
 */
import type { Duration, Effect, Scope, Stream } from "effect"

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
  readonly state: string
  readonly fns: Record<string, { readonly args: object; readonly returns: unknown }>
  readonly actions: Record<string, object>
  readonly tasks: Record<string, Callable>
  readonly machines: Record<string, Callable>
}

/** The implementations a machine needs, with the types the chart declared. */
export type Handlers<D extends MachineDef> =
  & (keyof D["fns"] extends never ? {} : {
    readonly fns: { readonly [K in keyof D["fns"]]: (args: D["fns"][K]["args"]) => D["fns"][K]["returns"] }
  })
  & (keyof D["actions"] extends never ? {} : {
    readonly actions: { readonly [K in keyof D["actions"]]: (args: D["actions"][K]) => Effect.Effect<void, never, any> }
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
      // A child is either an implemented machine or any function with the
      // same signature, which is how tests stub a whole sub-machine.
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

/** Anything that can be run with an input: an implemented machine. */
export interface Runnable<In, Out, Err, R> {
  /** Run one instance to its final state. The machine is an Effect. */
  readonly run: (input: In) => Effect.Effect<Out, Err, R>
}

export interface Snapshot<D extends MachineDef> {
  /** Ids of every active state, outermost first. */
  readonly active: ReadonlyArray<D["state"]>
  readonly status: "active" | "done" | "failed"
  readonly matches: (state: D["state"]) => boolean
}

/** A running instance that accepts events. */
export interface Actor<D extends MachineDef> {
  readonly send: (event: D["event"]) => Effect.Effect<void>
  readonly snapshot: Effect.Effect<Snapshot<D>>
  readonly changes: Stream.Stream<Snapshot<D>>
  /** Completes when the machine reaches a top-level final state. */
  readonly result: Effect.Effect<D["output"], D["error"]>
}

export interface Implemented<D extends MachineDef, R> extends Runnable<D["input"], D["output"], D["error"], R> {
  readonly def: Machine<D>
  /** Start a long-lived instance. Closing the scope stops it. */
  readonly spawn: (input: D["input"]) => Effect.Effect<Actor<D>, never, R | Scope.Scope>
}

/** The typed handle the code generator exports for each machine. */
export interface Machine<D extends MachineDef> {
  readonly name: D["name"]
  /** The compiled chart: the same JSON the visualization is drawn from. */
  readonly ir: unknown
  /**
   * Bind implementations. Every declared function, action, task and child
   * machine must be present, and nothing else may be.
   */
  readonly implement: <const H extends Handlers<D>>(handlers: H & NoExtras<H, D>) => Implemented<D, Requirements<H>>
}

export declare const make: <D extends MachineDef>(ir: unknown, name: D["name"]) => Machine<D>

export type { Duration }
