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
  /** A `strict` machine was given one of its own events, a message or a call that enabled no transition. */
  | {
    readonly kind: "unhandled"
    readonly event: Extract<Input, { readonly kind: "event" | "message" | "call" | "after" | "raised" }>
  }
  /** A `reply` was given a pending-call value whose call `call` is not pending. */
  | { readonly kind: "answered"; readonly call: string }

export interface InvokeEntry {
  readonly id: number
  readonly state: string
  readonly src: string
}

/** A call the run has taken and not answered yet: a pending call. */
export interface CallEntry {
  /** The caller's run id. */
  readonly from: string
  /** The id of the caller's invoke. */
  readonly invoke: number
  /** The call's name, a key of the machine's `calls`. */
  readonly call: string
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

/** A pending delayed raise (`raise E after d [as n]`): a timer of the run, not of a state. */
export interface ScheduledEntry {
  /** Its timer id, from the sequence `after` timers take theirs from. */
  readonly id: number
  readonly name: string | null
  readonly delay: number
  /** The event it raises, one of the machine's own events. */
  readonly value: Tagged
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
  /**
   * The locals of the active states, keyed by state id in document order: one member for each
   * active state that declares a parameter or a variable, holding those that have a value.
   */
  readonly locals: Locals
  readonly invokes: ReadonlyArray<InvokeEntry>
  /** The calls taken and not answered, in the order they were taken. */
  readonly calls: ReadonlyArray<CallEntry>
  readonly spawned: ReadonlyArray<SpawnedEntry>
  readonly timers: ReadonlyArray<TimerEntry>
  /** The pending delayed raises, in the order they were started. */
  readonly scheduled: ReadonlyArray<ScheduledEntry>
  readonly nextInvoke: number
  readonly nextTimer: number
  /** The id the next `send` or `emit` effect will get. */
  readonly nextMessage: number
  /**
   * What each history state has recorded, keyed by its id: state ids in document order. A
   * history state whose parent no microstep has exited yet has no member.
   */
  readonly history: History
  /**
   * The locals each history state recorded with the states it will re-enter, keyed by the
   * history state's id, then by state id. One whose record holds no state with locals has none.
   */
  readonly historyLocals: HistoryLocals
  readonly result: RunResult | null
}

/** A snapshot's `locals`: the parameters and variables of active states, keyed by state id, then by name. */
export interface Locals {
  readonly [state: string]: { readonly [name: string]: Json }
}

/** A snapshot's `historyLocals`: keyed by the history state's id, then as {@link Locals}. */
export interface HistoryLocals {
  readonly [historyState: string]: Locals
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

/** A delayed raise of a saved snapshot, with its due time. */
export interface SavedScheduled extends ScheduledEntry {
  readonly due: number
}

/**
 * A saved snapshot (execution.md, "The saved snapshot"): the core's snapshot as a run's last
 * step left it, with the format version, each timer's due time and, in version 4, the saved
 * snapshot of each running child run nested in its entry (`child`). Plain JSON, matching
 * `spec/snapshot.schema.json`; both runtimes save the same run as the same document. A run is
 * always saved as version 4; version 3, written before child runs were saved, has no `child`
 * and restarts its children; version 2, written before keyed spawn, has no `spawned` and
 * restores with no children, and version 1, written before history states, has no `history`
 * either and restores as if it had `{}`.
 */
export interface Saved
  extends Omit<Snapshot, "invokes" | "timers" | "calls" | "history" | "spawned" | "scheduled" | "locals" | "historyLocals">
{
  readonly snapshotVersion: 1 | 2 | 3 | 4
  readonly invokes: ReadonlyArray<SavedInvoke>
  /** Written only when the run has pending calls, in any version; a snapshot without it has none. */
  readonly calls?: ReadonlyArray<CallEntry>
  readonly timers: ReadonlyArray<SavedTimer>
  /** Written only when the run has pending delayed raises, in any version; a snapshot without it has none. */
  readonly scheduled?: ReadonlyArray<SavedScheduled>
  /** Written only when not `{}`, in any version; a snapshot without it has none. */
  readonly locals?: Locals
  /** Set exactly in versions 2 to 4. */
  readonly history?: History
  /** Written only when not `{}`, in any version; a snapshot without it has none. */
  readonly historyLocals?: HistoryLocals
  /** Set exactly in versions 3 and 4. */
  readonly spawned?: ReadonlyArray<SavedSpawned>
}

/** An invoke of a saved snapshot, with its child run's saved snapshot if it is one (version 4). */
export interface SavedInvoke extends InvokeEntry {
  readonly child?: Saved
}

/** A spawned child of a saved snapshot, with its child run's saved snapshot (version 4). */
export interface SavedSpawned extends SpawnedEntry {
  readonly child?: Saved
}

/** A tagged record: an event or an error value. */
export interface Tagged {
  readonly _tag: string
  readonly [field: string]: Json
}

/** How a peer's run closed: a variant of the built-in enum `Ended`. */
export type EndedStatus = "Done" | "Failed" | "Cancelled" | "Defect" | "Suspended"

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
  /**
   * A task this run invoked with `as`, or spawned, emitted one of its `emits` while it ran:
   * `invoke` is the id of the invoke or the spawned task. It has no sender run and no message
   * id; nothing but the task's emitter pushes it.
   */
  | { readonly kind: "task.emitted"; readonly invoke: number; readonly value: Tagged }
  /** Run `from` called this machine's call `call` with its invoke `invoke`. */
  | {
    readonly kind: "call"
    readonly from: string
    readonly invoke: number
    readonly call: string
    readonly args: { readonly [param: string]: Json }
  }
  /**
   * Run `from`, which this run holds a `ref` to, has closed; `status` says how. Given only to a
   * run whose machine hears `ended` from `from`'s machine, at most once per peer.
   */
  | { readonly kind: "ended"; readonly from: string; readonly status: EndedStatus }
  | { readonly kind: "invoke.done"; readonly invoke: number; readonly value: Json }
  | { readonly kind: "invoke.fail"; readonly invoke: number; readonly error: Tagged }
  | { readonly kind: "invoke.defect"; readonly invoke: number; readonly defect: Defect }
  | { readonly kind: "spawn.done"; readonly spawn: number; readonly value: Json }
  | { readonly kind: "spawn.fail"; readonly spawn: number; readonly error: Tagged }
  | { readonly kind: "spawn.defect"; readonly spawn: number; readonly defect: Defect }
  | { readonly kind: "after"; readonly timer: number }
  /** An event the run raised itself, as the core takes it from the internal queue: only in records. */
  | { readonly kind: "raised"; readonly value: Tagged }
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
  | { readonly kind: "transition"; readonly from: string; readonly index: number; readonly to: ReadonlyArray<string> }
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
  /** A call was answered: `to` is the caller's run id and `invoke` its invoke. */
  | {
    readonly kind: "replied"
    readonly to: string
    readonly invoke: number
    readonly call: string
    readonly outcome: "done" | "fail"
    readonly value: Json
  }
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
  | { readonly kind: "raised"; readonly value: Tagged }
  | {
    readonly kind: "raise.scheduled"
    readonly timer: number
    readonly name: string | null
    readonly delay: number
    readonly value: Tagged
  }
  | { readonly kind: "raise.cancelled"; readonly timer: number; readonly name: string | null }
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
    readonly srcKind: "task" | "machine" | "call"
    /** For a call, the callee's run id; `null` for a task or a machine. */
    readonly to: string | null
    /** The invoke's `as` name, or `null`. */
    readonly as: string | null
    readonly args: { readonly [param: string]: Json }
    /**
     * The saved snapshot the child run that claims the invoke restores instead of starting
     * (execution.md, "Restoring a child run"); `null` but from `restore`.
     */
    readonly saved: Saved | null
  }
  /**
   * Answer the call run `to` made with its invoke `invoke`: `done` with the result `value`, or
   * `fail` with the error `value`.
   */
  | {
    readonly kind: "reply"
    readonly to: string
    readonly invoke: number
    readonly call: string
    readonly outcome: "done" | "fail"
    readonly value: Json
  }
  | { readonly kind: "invoke.cancel"; readonly invoke: number }
  /**
   * Start machine or task `src` as spawned child `spawn`, which delivers what it emits to this
   * run, and queue its outcome as a `spawn.*` input when it ends.
   */
  | {
    readonly kind: "spawn.start"
    readonly spawn: number
    readonly src: string
    /** Whether `src` is a machine (a child run) or a task (host work with a live emitter). */
    readonly srcKind: "machine" | "task"
    readonly args: { readonly [param: string]: Json }
    /**
     * The saved snapshot the child run restores instead of starting; `null` but from `restore`,
     * and always `null` for a task.
     */
    readonly saved: Saved | null
  }
  /**
   * Stop spawned child `spawn` and wait for its cancel step, as `invoke.cancel` does; a spawned
   * task is dropped at once.
   */
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
  /**
   * Queue the emitted event for this run's parent (if it used `as` or spawned it) and its
   * subscribers, those of them whose machine has an `on` for it (`heard`).
   */
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
