/**
 * The services a run reads from its context. Each is a `Context.Reference` with a default, so a
 * machine runs with no setup; provide one to trace, to control time, or to name runs.
 */
import type * as Core from "@roadbook/core"
import { Clock, Context, Duration, Effect } from "effect"

/** Where trace records go. The default drops them. */
export interface TraceSink {
  readonly write: (record: Core.TraceRecord) => void
}

export const TraceSink: Context.Reference<TraceSink> = Context.Reference<TraceSink>("@roadbook/effect/TraceSink", {
  defaultValue: () => ({ write: () => {} })
})

/** The timer a driver was given starts one: which run, which timer, how long. */
export interface TimerRequest {
  readonly run: string
  readonly timer: number
  readonly delay: number
}

/**
 * The clock and timer of a run. The driver reads the time only through `now` (for each trace
 * record's `t`) and waits only through `sleep`: an `after` timer is a fiber that runs `sleep`
 * and then queues the expiry, and cancelling the timer interrupts that fiber. The default reads
 * and sleeps on Effect's `Clock`, so `TestClock` controls it.
 */
export interface Timer {
  /** The current time in integer milliseconds. */
  readonly now: Effect.Effect<number>
  /** Complete after `request.delay` milliseconds. Interrupted when the timer is cancelled. */
  readonly sleep: (request: TimerRequest) => Effect.Effect<void>
}

export const ClockTimer: Timer = {
  now: Clock.currentTimeMillis,
  sleep: ({ delay }) => Effect.sleep(Duration.millis(delay))
}

export const Timer: Context.Reference<Timer> = Context.Reference<Timer>("@roadbook/effect/Timer", {
  defaultValue: () => ClockTimer
})

let lastRootRun = 0

/**
 * Names each root run. The default gives `r1`, `r2`, … in the order runs start in this
 * process. A child run is always named `<parent run>.<invoke id>`.
 */
export const RootRunId: Context.Reference<() => string> = Context.Reference<() => string>(
  "@roadbook/effect/RootRunId",
  { defaultValue: () => () => `r${++lastRootRun}` }
)

/** A run as a tool sees it: its ids, its snapshot, and a way to queue inputs on it. */
export interface RunHandle {
  readonly id: string
  readonly parent: string | undefined
  readonly machine: string
  /** The run's snapshot after its last step. */
  readonly snapshot: () => Core.Snapshot
  /**
   * Append inputs to the run's queue in one operation, so that the run takes no step between
   * them. Inputs are the core's, in their JSON form; a cancel request is not an input here.
   */
  readonly offerAll: (inputs: ReadonlyArray<Exclude<Core.Input, { readonly kind: "cancel" | "defect" }>>) => Effect.Effect<void>
}

/**
 * Told about every run as it starts, before its `started` record: for tools that drive runs
 * from outside, such as the conformance runner. The default ignores them. Child runs report to
 * the observer of their root run.
 */
export interface RunObserver {
  readonly started: (run: RunHandle) => void
}

export const RunObserver: Context.Reference<RunObserver> = Context.Reference<RunObserver>(
  "@roadbook/effect/RunObserver",
  { defaultValue: () => ({ started: () => {} }) }
)
