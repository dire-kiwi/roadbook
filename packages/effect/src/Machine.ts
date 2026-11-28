/**
 * The public types of a machine: what the generated code declares about it (`MachineDef`), the
 * implementations it needs (`Handlers`), the values `make` and `implement` return, and the
 * handle of a running machine (`Handle`).
 */
import type * as Core from "@dire-kiwi/roadbook-core"
import type { Effect, Scope, Stream } from "effect"
import type { RestoreError, Saved } from "./Saved.ts"

/** What the compiler knows about a callable declared in a `.road` file. */
export interface Callable {
  readonly args: object
  readonly success: unknown
  readonly error: unknown
  /**
   * For a task that declares `emits`, the events it emits while it runs: what its emitter
   * takes. Absent for a task that emits nothing, and for a machine.
   */
  readonly emits?: unknown
}

/** The events a task emits, as its signature gives them: `never` for one that emits nothing. */
export type EmitsOf<C extends Callable> = C extends { readonly emits: infer E } ? E : never

/**
 * What a task's implementation is given, after its arguments, to emit the events its task
 * declares (`emits`) while it runs: each one goes to the run that invoked the task with `as`, or
 * spawned it, if that run's machine has an `on` for it. It never waits and never fails, except
 * that an event that does not encode makes the task die with a host `Defect`. Once the task's
 * invoke is cancelled, its spawned task stopped or its outcome queued, emitting does nothing.
 * For a task that emits nothing it takes `never`, so it cannot be called.
 */
export type Emit<E> = (event: E) => Effect.Effect<void>

/** Everything the code generator emits about one machine, as types. */
export interface MachineDef {
  readonly name: string
  readonly input: object
  readonly output: unknown
  readonly error: unknown
  /** The events the machine accepts: what `send` takes. */
  readonly event: unknown
  /** The events the machine emits (`emits`), to its invoker and to the runs holding a `ref` to it. */
  readonly emits: unknown
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
        args: D["tasks"][K]["args"],
        emit: Emit<EmitsOf<D["tasks"][K]>>
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
  /**
   * Ids of every active state in document order (outermost first, the regions of a parallel
   * state in the order they are written), the root's included while it runs.
   */
  readonly active: ReadonlyArray<D["state"]>
  /** Whether `state` is active. */
  readonly matches: (state: D["state"]) => boolean
  /** The snapshot as the core holds it: plain JSON, values in their JSON form. */
  readonly core: Core.Snapshot
}

/** Brands a {@link Handle}, so that only the runtime makes one. Exists only as a type. */
declare const HandleBrand: unique symbol

/**
 * A handle to a running top-level machine: what a `ref D` parameter of another machine takes,
 * and how host code sends to the run. It is the TypeScript counterpart of Rust's `Ref`. The
 * core holds it as the run's id; a run's `ref` arguments come back to host code (the arguments
 * of an invoke, an action, a task or a function) as the same handle objects.
 */
export interface Handle<D extends MachineDef> {
  readonly [HandleBrand]: D["name"]
  /** The run's id, which is how snapshots and traces name it (`"r2"`). */
  readonly id: string
  /** The machine's name. */
  readonly machine: D["name"]
  /**
   * Queue one of the machine's events, as an ordinary external event with no sender. Does
   * nothing once the run has ended.
   */
  readonly send: (event: D["event"]) => Effect.Effect<void>
}

/** A running instance that accepts events. */
export interface Actor<D extends MachineDef> {
  /** Queue an event. Does nothing once the run has ended. The same as `handle.send`. */
  readonly send: (event: D["event"]) => Effect.Effect<void>
  /** The run's handle, to pass as a `ref` argument to other machines. */
  readonly handle: Handle<D>
  readonly snapshot: Effect.Effect<Snapshot<D>>
  /**
   * The current snapshot, then one after every step. Ends after the run's final snapshot, or,
   * when the run is suspended, after the snapshot it was suspended with.
   */
  readonly changes: Stream.Stream<Snapshot<D>>
  /**
   * The events the run emits (`emits`), every one of them whatever any machine hears, from
   * when the stream starts until the run ends, after its last step, or is suspended. What the
   * run emitted before (its start step included, since the actor exists only after it) is not
   * in it. Each run of the stream is a listener of its own; nothing waits for it, and what it
   * has not taken yet is kept without bound. Listening writes nothing to the trace.
   */
  readonly emitted: Stream.Stream<D["emits"]>
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
  /**
   * The run's saved snapshot, as its last step left it, as a copy of its own, with the snapshot
   * of each running child run (invoked or spawned) nested in its entry as that child's last step
   * left it, to any depth. What is in flight between the runs (a message sent and not taken yet)
   * is in none of them. The run carries on.
   */
  readonly save: Effect.Effect<Saved>
  /**
   * Wait until the run's tree is idle (between steps, nothing queued, no pending call, and no
   * work running but child runs that are idle themselves: a task, a call or a stub is waited
   * for), then stop the whole tree without exit effects, each run writing `suspended`, and give
   * its saved snapshot, with its child runs' nested in it, whose status is still `running`. A
   * hub whose children wait for events is idle. A run that has ended gives its final snapshot
   * instead, and one that ended with a defect dies with the `Defect`. Once suspended, the actor
   * takes no more events, its `result` is interrupted, and closing its scope does nothing more.
   */
  readonly suspend: Effect.Effect<Saved>
}

/** How to restore a saved snapshot. */
export interface RestoreOptions {
  /**
   * Called once, only when the snapshot's hash is not the machine's: rewrites the old saved
   * snapshot into one of the current machine, both plain JSON. The old one has passed the
   * checks of its shape and version, which come first; the result is checked again from the
   * top. If it throws, the snapshot is refused with reason `migration`.
   */
  readonly migrate?: (saved: Saved) => Saved
}

export interface Implemented<D extends MachineDef, R> extends Runnable<D["input"], D["output"], D["error"], R> {
  readonly def: Machine<D>
  /** Start a long-lived instance. Closing the scope cancels it. */
  readonly spawn: (input: D["input"]) => Effect.Effect<Actor<D>, never, R | Scope.Scope>
  /**
   * Start a new long-lived instance from a saved snapshot, with a new run id: its invokes start
   * again, its timers keep their deadlines, and each child run nested in it is restored where it
   * was by the run that claims its invoke (host code around the child's `run` runs again, as a
   * task does; a stub starts again). The snapshot is checked, the nested ones too, and fails
   * with a `RestoreError` before starting anything when it is refused. Closing the scope cancels
   * it.
   */
  readonly restore: (saved: Saved, options?: RestoreOptions) => Effect.Effect<Actor<D>, RestoreError, R | Scope.Scope>
  /**
   * The request handler's step: `restore`, `send` the event if there is one, and `suspend`, in
   * a scope of its own. Gives the saved snapshot `suspend` gives, whatever the run's status; dies
   * with a `Defect` if the run ended with one. Interrupting it cancels the run. Nothing runs, and
   * nothing is written, until both inputs pass: a refused snapshot fails with a `RestoreError`,
   * and an event that is not one of the machine's or does not encode dies with the caller's host
   * `Defect`, as `send` does.
   */
  readonly resume: (
    saved: Saved,
    event?: D["event"],
    options?: RestoreOptions
  ) => Effect.Effect<Saved, RestoreError, R>
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
