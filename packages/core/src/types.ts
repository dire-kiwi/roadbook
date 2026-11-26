/**
 * The core's data: snapshots, inputs, commands, defects and trace records, exactly as
 * `spec/execution.md` and `spec/trace.md` define them. All of it is plain JSON.
 */
import type { Json } from "./json.ts"

export type Status = "running" | "done" | "failed" | "cancelled" | "defect"

/** A defect: the reason a run ended at once, never routed to a `fail` branch. */
export type Defect =
  | { readonly kind: "host"; readonly message: string }
  | { readonly kind: "unassigned"; readonly var: string }
  | { readonly kind: "overflow"; readonly expr: string }
  | { readonly kind: "undeclared"; readonly error: string }
  | { readonly kind: "limit" }

export interface InvokeEntry {
  readonly id: number
  readonly state: string
  readonly src: string
}

/** A running spawned child: its collection, its key and the arguments it was spawned with. */
export interface SpawnedEntry {
  readonly id: number
  readonly children: string
  readonly key: Json
  readonly args: { readonly [param: string]: Json }
}

export interface TimerEntry {
  readonly id: number
  readonly state: string
  readonly index: number
  readonly delay: number
}

export type RunResult =
  | { readonly output: Json }
  | { readonly error: Json }
  | { readonly defect: Defect }

export interface Snapshot {
  readonly machine: string
  /** The run's id, as given to `start`. */
  readonly run: string
  readonly ir: string
  readonly status: Status
  /** Active state ids, in document order. */
  readonly active: ReadonlyArray<string>
  readonly input: { readonly [param: string]: Json }
  /** Every variable that has a value. */
  readonly vars: { readonly [name: string]: Json }
  readonly invokes: ReadonlyArray<InvokeEntry>
  readonly spawned: ReadonlyArray<SpawnedEntry>
  readonly timers: ReadonlyArray<TimerEntry>
  readonly nextInvoke: number
  readonly nextTimer: number
  /** The id the next `send` or `emit` effect will get. */
  readonly nextMessage: number
  /**
   * What each history state has recorded, keyed by its id: state ids in document order. A
   * history state whose parent no microstep has exited yet has no member.
   */
  readonly history: History
  readonly result: RunResult | null
}

/** A snapshot's `history`: what each history state recorded, keyed by the history state's id. */
export interface History {
  readonly [historyState: string]: ReadonlyArray<string>
}

/** A timer of a saved snapshot: the core's entry with the time it is due on the driver's clock. */
export interface SavedTimer extends TimerEntry {
  /** When it expires, in integer milliseconds on the driver's clock. */
  readonly due: number
}

/**
 * A saved snapshot (execution.md, "The saved snapshot"): the core's snapshot as a run's last
 * step left it, with the format version and each timer's due time. Plain JSON, matching
 * `spec/snapshot.schema.json`; both runtimes save the same run as the same document. A run is
 * always saved as version 3; version 2, written before keyed spawn, has no `spawned` and
 * restores with no children, and version 1, written before history states, has no `history`
 * either and restores as if it had `{}`.
 */
export interface Saved extends Omit<Snapshot, "timers" | "history" | "spawned"> {
  readonly snapshotVersion: 1 | 2 | 3
  readonly timers: ReadonlyArray<SavedTimer>
  /** Set exactly in versions 2 and 3. */
  readonly history?: History
  /** Set exactly in version 3. */
  readonly spawned?: ReadonlyArray<SpawnedEntry>
}

/** A tagged record: an event or an error value. */
export interface Tagged {
  readonly _tag: string
  readonly [field: string]: Json
}

export type Input =
  | { readonly kind: "event"; readonly value: Tagged }
  /** Run `from` sent one of this machine's events as its message `message`. */
  | { readonly kind: "message"; readonly from: string; readonly message: number; readonly value: Tagged }
  /**
   * Run `from` emitted one of its machine's `emits` as its message `message`. `invoke` is the
   * id of this run's invoke or spawned child that is `from` when this run is its parent, else
   * `null`.
   */
  | {
    readonly kind: "emitted"
    readonly from: string
    readonly invoke: number | null
    readonly message: number
    readonly value: Tagged
  }
  | { readonly kind: "invoke.done"; readonly invoke: number; readonly value: Json }
  | { readonly kind: "invoke.fail"; readonly invoke: number; readonly error: Tagged }
  | { readonly kind: "invoke.defect"; readonly invoke: number; readonly defect: Defect }
  | { readonly kind: "spawn.done"; readonly spawn: number; readonly value: Json }
  | { readonly kind: "spawn.fail"; readonly spawn: number; readonly error: Tagged }
  | { readonly kind: "spawn.defect"; readonly spawn: number; readonly defect: Defect }
  | { readonly kind: "after"; readonly timer: number }
  | { readonly kind: "cancel" }
  | { readonly kind: "defect"; readonly defect: Defect }

/** The inputs that `event`, `dropped` and `stale` records can carry. */
export type TracedInput = Exclude<Input, { readonly kind: "cancel" } | { readonly kind: "defect" }>

/** A trace record as the core emits it, without `t`, `run` and `parent`. */
export type CoreRecord =
  | { readonly kind: "started"; readonly machine: string; readonly ir: string; readonly input: Json }
  /** `migrated` is the hash the snapshot had before a migration, or `null`. */
  | {
    readonly kind: "restored"
    readonly machine: string
    readonly ir: string
    readonly migrated: string | null
    readonly snapshot: Saved
  }
  | { readonly kind: "entered"; readonly state: string }
  | { readonly kind: "exited"; readonly state: string }
  | { readonly kind: "event" | "dropped" | "stale"; readonly event: TracedInput }
  | { readonly kind: "transition"; readonly from: string; readonly index: number; readonly to: string | null }
  | { readonly kind: "set"; readonly var: string; readonly value: Json }
  | { readonly kind: "action"; readonly action: string; readonly args: { readonly [param: string]: Json } }
  | { readonly kind: "sent"; readonly message: number; readonly to: string; readonly value: Tagged }
  | { readonly kind: "emitted"; readonly message: number; readonly value: Tagged }
  /** `to` is `null` when the core finds no child to send to, else the receiving run's id. */
  | { readonly kind: "undelivered"; readonly message: number; readonly to: string | null; readonly value: Tagged }
  | {
    readonly kind: "invoke.started"
    readonly invoke: number
    readonly state: string
    readonly src: string
    readonly args: { readonly [param: string]: Json }
  }
  | { readonly kind: "invoke.cancelled"; readonly invoke: number; readonly state: string; readonly src: string }
  | {
    readonly kind: "spawn.started"
    readonly spawn: number
    readonly children: string
    readonly key: Json
    readonly src: string
    readonly args: { readonly [param: string]: Json }
  }
  | { readonly kind: "spawn.stopped"; readonly spawn: number; readonly children: string; readonly key: Json }
  | {
    readonly kind: "timer.started"
    readonly timer: number
    readonly state: string
    readonly index: number
    readonly delay: number
  }
  | { readonly kind: "timer.cancelled"; readonly timer: number; readonly state: string; readonly index: number }
  | { readonly kind: "done"; readonly state: string; readonly output: Json }
  | { readonly kind: "failed"; readonly state: string; readonly error: Json }
  | { readonly kind: "cancelled" }
  | { readonly kind: "defect"; readonly defect: Defect }
  /** Written by the driver, not the core, when it stops an idle run it has saved. */
  | { readonly kind: "suspended" }

/** A trace record as a driver writes it: the core's record with the clock and the run's ids. */
export type TraceRecord = CoreRecord & {
  readonly t: number
  readonly run: string
  readonly parent?: string
}

export type Command =
  | { readonly kind: "trace"; readonly record: CoreRecord }
  | { readonly kind: "action"; readonly action: string; readonly args: { readonly [param: string]: Json } }
  | {
    readonly kind: "invoke.start"
    readonly invoke: number
    readonly state: string
    readonly src: string
    readonly srcKind: "task" | "machine"
    /** The invoke's `as` name, or `null`. */
    readonly as: string | null
    readonly args: { readonly [param: string]: Json }
  }
  | { readonly kind: "invoke.cancel"; readonly invoke: number }
  /**
   * Start machine `src` as spawned child `spawn`, which delivers what it emits to this run, and
   * queue its outcome as a `spawn.*` input when it ends.
   */
  | {
    readonly kind: "spawn.start"
    readonly spawn: number
    readonly src: string
    readonly args: { readonly [param: string]: Json }
  }
  /** Stop spawned child `spawn` and wait for its cancel step, as `invoke.cancel` does. */
  | { readonly kind: "spawn.stop"; readonly spawn: number }
  | { readonly kind: "timer.start"; readonly timer: number; readonly delay: number }
  | { readonly kind: "timer.cancel"; readonly timer: number }
  /**
   * Queue the expiry of timer `timer` on this run's queue at once, without starting a timer.
   * Only `restore` emits it, for a restored timer that was already due.
   */
  | { readonly kind: "timer.fire"; readonly timer: number }
  /**
   * Queue message `message` for run `to`: the child run of this run's invoke or spawned child
   * `invoke`, or, when `invoke` is `null`, the peer `to`.
   */
  | {
    readonly kind: "send"
    readonly to: string
    readonly invoke: number | null
    readonly message: number
    readonly value: Tagged
  }
  /** Queue the emitted event for this run's parent (if it used `as` or spawned it) and its subscribers. */
  | { readonly kind: "emit"; readonly message: number; readonly value: Tagged }

/** What the host's function `name` gave: its value, or a host defect. */
export type FnResult = { readonly value: Json } | { readonly defect: Defect }

/**
 * The host's pure functions. Called with positional argument values; must not throw (the
 * binding layer catches what host code throws and returns a `host` defect instead).
 */
export type Fns = (name: string, args: ReadonlyArray<Json>) => FnResult

export interface Stepped {
  readonly snapshot: Snapshot
  readonly commands: ReadonlyArray<Command>
}
