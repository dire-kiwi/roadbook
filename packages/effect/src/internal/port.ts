/**
 * Ports: how a run is reached from outside its own fiber (execution.md, "Delivering
 * messages"). A port is a run's queue with its open flag and the runs subscribed to what it
 * emits. Pushing checks the flag and offers synchronously, so it is one operation with respect
 * to the run's closing, which also happens synchronously in the run's own fiber.
 */
import type * as Core from "@dire-kiwi/roadbook-core"
import { Queue } from "effect"

export interface Port {
  /** The run's id. */
  readonly id: string
  /** The run's machine. */
  readonly machine: string
  /** The machine's hash in the IR the run runs: which version of the machine it is. */
  readonly hash: string
  readonly queue: Queue.Queue<Core.Input | Wake>
  /** From just before `start` until the core returns a snapshot that is not `running`. */
  open: boolean
  /**
   * The runs that hold a `ref` to this one and are open, each with the events it emits that they
   * receive: those their machine has an `on` for.
   */
  readonly subscribers: Map<Port, ReadonlySet<string>>
  /**
   * For a durable run's handle, whose run may be live in this process, parked, recovering or
   * ended: how an input reaches it. Returns whether the run will take it (false once it ended).
   */
  readonly deliver?: (input: Core.Input) => boolean
}

/**
 * Not an input: what `suspend` puts on a run's queue when the run is waiting for one, so that
 * it checks again whether it is idle. The run never gives it to the core.
 */
export interface Wake {
  readonly kind: "wake"
}

/** Push `input` onto the run's queue if the run is open. Returns whether it was pushed. */
export const push = (port: Port, input: Core.Input): boolean =>
  port.deliver !== undefined ? port.deliver(input) : port.open && Queue.offerUnsafe(port.queue, input)

/**
 * A call's way to answer (execution.md, "Carrying out calls"): pushes the outcome of the call's
 * invoke onto the caller's queue, unless the caller has closed or cancelled that invoke.
 */
export type Answer = (outcome: Core.Input) => void

/** The way to answer of each call input pushed onto a callee's queue and not yet taken. */
const answers = new WeakMap<Core.Input, Answer>()

/**
 * Push call input `input` onto the callee's queue if it is open, with its way to answer.
 * Returns whether it was pushed.
 */
export const pushCall = (port: Port, input: Core.Input, answer: Answer): boolean => {
  answers.set(input, answer)
  if (push(port, input)) return true
  answers.delete(input)
  return false
}

/** The way to answer of a call input taken from a queue, which forgets it. */
export const takeAnswer = (input: Core.Input): Answer | undefined => {
  const answer = answers.get(input)
  answers.delete(input)
  return answer
}

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
