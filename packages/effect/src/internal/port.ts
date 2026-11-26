/**
 * Ports: how a run is reached from outside its own fiber (execution.md, "Delivering
 * messages"). A port is a run's queue with its open flag and the runs subscribed to what it
 * emits. Pushing checks the flag and offers synchronously, so it is one operation with respect
 * to the run's closing, which also happens synchronously in the run's own fiber.
 */
import type * as Core from "@roadbook/core"
import { Queue } from "effect"

export interface Port {
  /** The run's id. */
  readonly id: string
  /** The run's machine. */
  readonly machine: string
  readonly queue: Queue.Queue<Core.Input>
  /** From just before `start` until the core returns a snapshot that is not `running`. */
  open: boolean
  /** The runs that hold a `ref` to this one and are open: they receive what it emits. */
  readonly subscribers: Set<Port>
}

/** Push `input` onto the run's queue if the run is open. Returns whether it was pushed. */
export const push = (port: Port, input: Core.Input): boolean => port.open && Queue.offerUnsafe(port.queue, input)

/** The port behind every handle, keyed by the handle object. */
const ports = new WeakMap<object, Port>()

/** Register `handle` as the handle of the run behind `port`. */
export const registerHandle = <H extends object>(handle: H, port: Port): H => {
  ports.set(handle, port)
  return handle
}

/** The port of a handle, or `undefined` if `value` is not one. */
export const portOf = (value: unknown): Port | undefined =>
  typeof value === "object" && value !== null ? ports.get(value) : undefined
