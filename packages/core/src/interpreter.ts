/**
 * The pure core: `start`, `step` and `restore`, exactly as `spec/execution.md` specifies them.
 * They do no I/O, read no clock and never modify the snapshot they are given.
 */
import {
  type Address,
  type ChildrenDecl,
  childrenOf,
  type Effect,
  type Expr,
  getMachine,
  type Ir,
  isHistory,
  type Machine,
  peerOf,
  type State,
  type Transition,
  UNANSWERED
} from "./ir.ts"
import type { Json } from "./json.ts"
import { redactor } from "./redact.ts"
import { withoutChildren } from "./saved.ts"
import { expandType, isOptionalType, parseTypeRef } from "./typeref.ts"
import type {
  CallEntry,
  Command,
  CoreRecord,
  Defect,
  Fns,
  Input,
  InvokeEntry,
  RunResult,
  Saved,
  ScheduledEntry,
  Snapshot,
  SpawnedEntry,
  Status,
  Stepped,
  Tagged,
  TimerEntry
} from "./types.ts"

/** The most microsteps settling one input may take before it is a `limit` defect. */
export const SETTLE_LIMIT = 1000

/**
 * Start run `run` of machine `name` with `input` (its arguments keyed by parameter name, a
 * `ref` as the id of the run it refers to), as execution.md's `start`. Throws an `IrError` if
 * the IR has no such machine.
 */
export const start = (
  ir: Ir,
  name: string,
  run: string,
  input: { readonly [param: string]: Json },
  fns: Fns
): Stepped => {
  const index = machineIndex(ir, name)
  const initial: Snapshot = {
    machine: name,
    run,
    ir: index.machine.hash,
    status: "running",
    active: [],
    input,
    vars: {},
    invokes: [],
    calls: [],
    spawned: [],
    timers: [],
    scheduled: [],
    nextInvoke: 1,
    nextTimer: 1,
    nextMessage: 1,
    history: {},
    result: null
  }
  const started: Command = {
    kind: "trace",
    record: redactor(ir).record(index.machine, { kind: "started", machine: name, ir: index.machine.hash, input })
  }
  const macro = new Macrostep(ir, index, initial, fns)
  macro.commands.push(started)
  try {
    for (const v of index.machine.vars) {
      if (v.init !== null) macro.vars[v.name] = macro.eval(v.init, NO_BINDINGS)
      else if (isOptionalType(ir, v.type)) macro.vars[v.name] = null
    }
    const root = index.machine.root
    macro.enterStates([root, ...index.defaultEntry(root)])
    macro.settle()
    if (macro.status === "running") macro.startWork()
    return macro.finishStep()
  } catch (error) {
    if (!(error instanceof Abort)) throw error
    const failed = new Macrostep(ir, index, initial, fns)
    failed.commands.push(started)
    failed.failWithDefect(error.defect)
    return failed.finishStep()
  }
}

/** Take one input, as execution.md's `step`. Returns a new snapshot; `snapshot` is untouched. */
export const step = (ir: Ir, snapshot: Snapshot, input: Input, fns: Fns): Stepped => {
  const index = machineIndex(ir, snapshot.machine)
  if (input.kind === "defect") {
    const macro = new Macrostep(ir, index, snapshot, fns)
    if (snapshot.status !== "defect") macro.failWithDefect(input.defect)
    return macro.finishStep()
  }
  if (snapshot.status !== "running") return { snapshot, commands: [] }
  try {
    const macro = new Macrostep(ir, index, snapshot, fns)
    macro.take(input)
    return macro.finishStep()
  } catch (error) {
    // A runtime error or a host defect inside the core: the step is atomic, so every command
    // it produced is discarded and the defect is applied to the snapshot it was given.
    if (!(error instanceof Abort)) throw error
    const failed = new Macrostep(ir, index, snapshot, fns)
    failed.failWithDefect(error.defect)
    return failed.finishStep()
  }
}

/**
 * Start run `run` again from a saved snapshot, as execution.md's `restore`: restart the invokes
 * it holds with their ids and fresh arguments, restart the timers not yet due for the time that
 * remains at `now`, and queue the expiry of the others (`timer.fire`). No state is entered or
 * exited. `saved` must have passed every check of `checkSaved`; `migrated` is the hash it had
 * before a migration, or `null`.
 */
export const restore = (
  ir: Ir,
  saved: Saved,
  run: string,
  now: number,
  migrated: string | null,
  fns: Fns
): Stepped => {
  const index = machineIndex(ir, saved.machine)
  const snapshot: Snapshot = {
    machine: saved.machine,
    run,
    ir: saved.ir,
    status: saved.status,
    active: saved.active,
    input: saved.input,
    vars: saved.vars,
    invokes: saved.invokes.map(({ id, src, state }) => ({ id, state, src })),
    // A snapshot with pending calls is refused (`calls`), so the restored run has none.
    calls: [],
    spawned: (saved.spawned ?? []).map(({ args, children, id, key }) => ({ id, children, key, args })),
    timers: saved.timers.map(({ delay, id, index, state }) => ({ id, state, index, delay })),
    scheduled: (saved.scheduled ?? []).map(({ delay, id, name, value }) => ({ id, name, delay, value })),
    nextInvoke: saved.nextInvoke,
    nextTimer: saved.nextTimer,
    nextMessage: saved.nextMessage,
    // A version 1 snapshot, written before history states, has none.
    history: saved.history ?? {},
    result: saved.result
  }
  const restored: Command = {
    kind: "trace",
    record: redactor(ir).record(index.machine, {
      kind: "restored",
      machine: saved.machine,
      ir: index.machine.hash,
      migrated,
      // Without the snapshots of child runs nested in it: each child run writes its own.
      snapshot: withoutChildren(saved)
    })
  }
  try {
    const macro = new Macrostep(ir, index, snapshot, fns)
    macro.commands.push(restored)
    macro.restartWork(saved, now)
    return macro.finishStep()
  } catch (error) {
    if (!(error instanceof Abort)) throw error
    const failed = new Macrostep(ir, index, snapshot, fns)
    failed.commands.push(restored)
    failed.failWithDefect(error.defect)
    return failed.finishStep()
  }
}

// ---- the machine, indexed ------------------------------------------------------------------

class MachineIndex {
  readonly states = new Map<string, State>()
  readonly parents = new Map<string, State | undefined>()
  /** Document order: the position of each state in a pre-order walk. */
  readonly order = new Map<string, number>()
  readonly params: ReadonlySet<string>
  readonly vars: ReadonlySet<string>
  readonly machine: Machine
  /**
   * The emitted events the machine has an `on x.E` for, anywhere in it, by the sender's
   * machine or task, and `ended` when it has an `on x.ended` for a run of that machine. A run
   * receives no others. A task's name is lower-case and a machine's upper-case, so the two
   * never share an entry.
   */
  readonly hears = new Map<string, Set<string>>()

  constructor(machine: Machine) {
    this.machine = machine
    this.params = new Set(machine.params.map((p) => p.name))
    this.vars = new Set(machine.vars.map((v) => v.name))
    const walk = (state: State, parent: State | undefined) => {
      this.states.set(state.id, state)
      this.parents.set(state.id, parent)
      this.order.set(state.id, this.order.size)
      for (const t of state.transitions) {
        if (t.trigger.kind !== "emitted" && t.trigger.kind !== "ended") continue
        const sender = peerOf(t.trigger.from)
        const events = this.hears.get(sender) ?? new Set()
        events.add(t.trigger.kind === "ended" ? ENDED_INPUT : t.trigger.event)
        this.hears.set(sender, events)
      }
      for (const child of state.states) walk(child, state)
    }
    walk(machine.root, undefined)
  }

  state(id: string): State {
    const state = this.states.get(id)
    if (state === undefined) throw new Error(`no state ${id} in machine ${this.machine.root.id}`)
    return state
  }

  parent(state: State): State | undefined {
    return this.parents.get(state.id)
  }

  /** The states entered below `state` when it is entered with no more specific target. */
  defaultEntry(state: State): Array<State> {
    if (state.kind === "compound" && state.initial !== null) {
      const child = this.state(state.initial)
      return [child, ...this.defaultEntry(child)]
    }
    if (state.kind === "parallel") return childrenOf(state).flatMap((child) => [child, ...this.defaultEntry(child)])
    return []
  }

  position(state: State): number {
    return this.order.get(state.id)!
  }

  /** Whether `a` is a proper ancestor of `b`. */
  isProperAncestor(a: State, b: State): boolean {
    for (let p = this.parent(b); p !== undefined; p = this.parent(p)) {
      if (p === a) return true
    }
    return false
  }
}

const indexes = new WeakMap<Ir, Map<string, MachineIndex>>()

const machineIndex = (ir: Ir, name: string): MachineIndex => {
  let byName = indexes.get(ir)
  if (byName === undefined) {
    byName = new Map()
    indexes.set(ir, byName)
  }
  let index = byName.get(name)
  if (index === undefined) {
    index = new MachineIndex(getMachine(ir, name))
    byName.set(name, index)
  }
  return index
}

const NOTHING: ReadonlySet<string> = new Set()

/**
 * The name `heard` gives a peer's end: lower case, so it is never the name of an event.
 */
export const ENDED_INPUT = "ended"

/**
 * The events of a run of machine `sender` that a run of `machine` receives when the sender emits
 * them: those `machine` has an `on x.E` for, where `x` is a run of `sender`, in any state, and
 * {@link ENDED_INPUT} when it has an `on x.ended`, so it is told when such a run closes
 * (execution.md, "Messages between runs"). For a task `sender`, the events of its `emits` that
 * the run hears from it, through an `as` name or a collection of it: its emitter's heard set
 * (execution.md, "The emitter").
 */
export const heard = (ir: Ir, machine: string, sender: string): ReadonlySet<string> =>
  machineIndex(ir, machine).hears.get(sender) ?? NOTHING

// ---- one macrostep ---------------------------------------------------------------------------

/** A runtime error or host defect met inside the core, which aborts the call. */
class Abort {
  readonly defect: Defect
  constructor(defect: Defect) {
    this.defect = defect
  }
}

/** The implicit names in scope while a transition's guard and effects are evaluated. */
interface Bindings {
  readonly event?: Json
  readonly result?: Json
  readonly error?: Json
  /** In an `on c` transition, the pending-call value `{from, invoke}` of the call it takes. */
  readonly call?: Json
  readonly key?: Json
}

/** A child spawned during this macrostep that has not started yet. */
interface PendingSpawn {
  readonly children: string
  readonly key: Json
  readonly args: { readonly [param: string]: Json }
}

const NO_BINDINGS: Bindings = {}

interface Selected {
  readonly source: State
  readonly index: number
  readonly transition: Transition
}

/** Which of a state's transitions are candidates for the input being selected. */
type Candidates = (state: State) => (t: Transition) => boolean

class Macrostep {
  status: Status
  active: Array<string>
  readonly vars: Record<string, Json>
  invokes: Array<InvokeEntry>
  calls: Array<CallEntry>
  spawned: Array<SpawnedEntry>
  timers: Array<TimerEntry>
  scheduled: Array<ScheduledEntry>
  nextInvoke: number
  nextTimer: number
  nextMessage: number
  history: Record<string, ReadonlyArray<string>>
  result: RunResult | null
  readonly commands: Array<Command> = []
  /** The states entered during this macrostep. */
  private readonly entered = new Set<string>()
  /**
   * The internal queue, in the order queued: the completions of states that reached a final
   * state and have not been exited since, and the events `raise` effects raised.
   */
  private queue: Array<{ readonly completion: string } | { readonly raised: Tagged }> = []
  /** The children spawned during this macrostep, in order, not started yet. */
  private pending: Array<PendingSpawn> = []

  private readonly ir: Ir
  private readonly index: MachineIndex
  private readonly snapshot: Snapshot
  private readonly fns: Fns

  constructor(ir: Ir, index: MachineIndex, snapshot: Snapshot, fns: Fns) {
    this.ir = ir
    this.index = index
    this.snapshot = snapshot
    this.fns = fns
    this.status = snapshot.status
    this.active = [...snapshot.active]
    this.vars = { ...snapshot.vars }
    this.invokes = [...snapshot.invokes]
    this.calls = [...snapshot.calls]
    this.spawned = [...snapshot.spawned]
    this.timers = [...snapshot.timers]
    this.scheduled = [...snapshot.scheduled]
    this.nextInvoke = snapshot.nextInvoke
    this.nextTimer = snapshot.nextTimer
    this.nextMessage = snapshot.nextMessage
    this.history = { ...snapshot.history }
    this.result = snapshot.result
  }

  finishStep(): Stepped {
    const s = this.snapshot
    return {
      snapshot: {
        machine: s.machine,
        run: s.run,
        ir: s.ir,
        status: this.status,
        active: this.active,
        input: s.input,
        vars: this.vars,
        invokes: this.invokes,
        calls: this.calls,
        spawned: this.spawned,
        timers: this.timers,
        scheduled: this.scheduled,
        nextInvoke: this.nextInvoke,
        nextTimer: this.nextTimer,
        nextMessage: this.nextMessage,
        history: this.history,
        result: this.result
      },
      commands: this.commands
    }
  }

  /**
   * Emit a trace record, redacted (`spec/trace.md#redaction`). An `event`, `dropped` or `stale`
   * record holds an input {@link Macrostep.take} has already redacted.
   */
  private trace(record: CoreRecord) {
    const r = redactor(this.ir)
    this.commands.push({ kind: "trace", record: r.active ? r.record(this.index.machine, record) : record })
  }

  take(input: Exclude<Input, { readonly kind: "defect" }>) {
    // The input as its record shows it, redacted while the invoke or spawned child it may be
    // the outcome of is still in the snapshot.
    if (input.kind === "cancel") {
      this.cancelRun()
      return
    }
    const r = redactor(this.ir)
    const shown = r.active ? r.input(this.index.machine, this.invokes, input, this.spawned) : input
    let selected: ReadonlyArray<Selected> = []
    let bindings: Bindings = NO_BINDINGS
    // Whether the input is a delayed raise's expiry, which a strict machine must take.
    let raised = false
    switch (input.kind) {
      case "invoke.done":
      case "invoke.fail":
      case "invoke.defect": {
        const inv = this.invokes.find((i) => i.id === input.invoke)
        if (inv === undefined) {
          this.trace({ kind: "stale", event: shown })
          return
        }
        this.invokes = this.invokes.filter((i) => i !== inv)
        if (input.kind === "invoke.defect") {
          this.trace({ kind: "event", event: shown })
          this.failWithDefect(input.defect)
          return
        }
        if (input.kind === "invoke.fail" && !this.declaredErrors(inv).includes(input.error._tag)) {
          throw new Abort({ kind: "undeclared", error: input.error._tag })
        }
        if (input.kind === "invoke.done") {
          bindings = { result: input.value }
          selected = this.selectOutcome(inv, (t) => t.trigger.kind === "invoke.done", bindings)
        } else {
          bindings = { error: input.error }
          selected = this.selectFail(inv, input.error, bindings)
        }
        break
      }
      case "spawn.done":
      case "spawn.fail":
      case "spawn.defect": {
        const ch = this.spawned.find((c) => c.id === input.spawn)
        if (ch === undefined) {
          this.trace({ kind: "stale", event: shown })
          return
        }
        this.spawned = this.spawned.filter((c) => c !== ch)
        if (input.kind === "spawn.defect") {
          this.trace({ kind: "event", event: shown })
          this.failWithDefect(input.defect)
          return
        }
        const src = this.collection(ch.children).src
        if (input.kind === "spawn.fail" && !this.srcErrors(src).includes(input.error._tag)) {
          throw new Abort({ kind: "undeclared", error: input.error._tag })
        }
        if (input.kind === "spawn.done") {
          bindings = { result: input.value, key: ch.key }
          const isDone = (t: Transition) => t.trigger.kind === "spawn.done" && t.trigger.children === ch.children
          selected = this.selectByWalks(() => isDone, bindings)
        } else {
          bindings = { error: input.error, key: ch.key }
          const isFail = (name: string) => (t: Transition) =>
            t.trigger.kind === "spawn.fail" && t.trigger.children === ch.children && t.trigger.error === name
          const named = isFail(input.error._tag)
          const any = isFail("*")
          selected = this.selectByWalks((state) => (state.transitions.some(named) ? named : any), bindings)
        }
        break
      }
      case "after": {
        const tm = this.timers.find((t) => t.id === input.timer)
        if (tm !== undefined) {
          this.timers = this.timers.filter((t) => t !== tm)
          selected = this.selectAfter(tm)
          break
        }
        // A delayed raise's expiry consumes the raise and selects its event.
        const sc = this.scheduled.find((s) => s.id === input.timer)
        if (sc === undefined) {
          this.trace({ kind: "stale", event: shown })
          return
        }
        this.scheduled = this.scheduled.filter((s) => s !== sc)
        raised = true
        bindings = { event: sc.value }
        selected = this.selectEvent(sc.value, bindings)
        break
      }
      case "event":
      case "message":
      case "raised":
        bindings = { event: input.value }
        selected = this.selectEvent(input.value, bindings)
        break
      case "call":
        bindings = { event: input.args, call: { from: input.from, invoke: input.invoke } }
        selected = this.selectCall(input.call, bindings)
        break
      case "emitted": {
        const child = input.invoke === null ? undefined : this.spawned.find((c) => c.id === input.invoke)
        if (input.invoke !== null && child === undefined && !this.invokes.some((i) => i.id === input.invoke)) {
          this.trace({ kind: "stale", event: shown })
          return
        }
        bindings = child === undefined ? { event: input.value } : { event: input.value, key: child.key }
        selected = this.selectEmitted(input, bindings)
        break
      }
      case "task.emitted": {
        // Stale once the invoke or spawned task is no longer in the snapshot. Taking one
        // consumes nothing: the task goes on running.
        const child = this.spawned.find((c) => c.id === input.invoke)
        if (child === undefined && !this.invokes.some((i) => i.id === input.invoke)) {
          this.trace({ kind: "stale", event: shown })
          return
        }
        bindings = child === undefined ? { event: input.value } : { event: input.value, key: child.key }
        selected = this.selectTaskEmitted(input, bindings)
        break
      }
      case "ended":
        // Never stale: nothing is consumed, and the address is read now.
        bindings = { event: { status: input.status } }
        selected = this.selectEnded(input.from, bindings)
        break
    }
    if (selected.length === 0) {
      // A strict machine fails on one of its own events, a message, a call or a delayed raise
      // that it does not handle. Its driver answers the call `Unanswered` as the run ends.
      const strict = this.index.machine.strict
      if (
        strict && (input.kind === "event" || input.kind === "message" || input.kind === "raised" ||
          input.kind === "call" || (input.kind === "after" && raised))
      ) {
        throw new Abort({ kind: "unhandled", event: input })
      }
      this.trace({ kind: "dropped", event: shown })
      // A dropped call is answered at once, so its caller does not wait for it.
      if (input.kind === "call") this.answer(input.from, input.invoke, input.call, "fail", { _tag: UNANSWERED })
      return
    }
    this.trace({ kind: "event", event: shown })
    // A call that enables a transition is pending from now on, so a `reply` among the
    // transition's effects finds it.
    if (input.kind === "call") this.calls.push({ from: input.from, invoke: input.invoke, call: input.call })
    this.microstep(selected, bindings)
    this.settle()
    if (this.status === "running") this.startWork()
  }

  /** Collection `children` of the machine: what its children run, a machine or a task. */
  private collection(children: string): ChildrenDecl {
    const decl = Object.hasOwn(this.index.machine.children, children) ? this.index.machine.children[children] : undefined
    if (decl === undefined) throw new Error(`no collection ${children} in machine ${this.index.machine.root.id}`)
    return decl
  }

  /** The errors invoke `inv` may fail with: its task's, machine's, or call's, and `Unanswered` for a call. */
  private declaredErrors(inv: InvokeEntry): ReadonlyArray<string> {
    const invoke = this.index.state(inv.state).invoke
    if (invoke?.kind === "call") return [...(this.callDecl(invoke)?.errors ?? []), UNANSWERED]
    return this.srcErrors(invoke?.src ?? inv.src)
  }

  /** The errors task or machine `src` may fail with. */
  private srcErrors(src: string): ReadonlyArray<string> {
    if (Object.hasOwn(this.ir.tasks, src)) return this.ir.tasks[src]?.errors ?? []
    return this.ir.machines[src]?.errors ?? []
  }

  /** The declaration of the call a call invoke makes, in the callee's machine. */
  private callDecl(invoke: NonNullable<State["invoke"]>) {
    const callee = invoke.to === null ? undefined : this.ir.machines[invoke.to.machine]
    return callee !== undefined && Object.hasOwn(callee.calls, invoke.src) ? callee.calls[invoke.src] : undefined
  }

  // ---- selecting a transition ----

  /** The first of `state`'s transitions matching `matches` whose guard holds. */
  private firstEnabled(
    state: State,
    matches: (t: Transition) => boolean,
    bindings: Bindings
  ): Selected | undefined {
    const transitions = state.transitions
    for (let index = 0; index < transitions.length; index++) {
      const transition = transitions[index]!
      if (!matches(transition)) continue
      if (transition.guard === null || this.eval(transition.guard, bindings) === true) {
        return { source: state, index, transition }
      }
    }
    return undefined
  }

  private innermostFirst(): Array<State> {
    return this.active.map((id) => this.index.state(id)).reverse()
  }

  /** The active states with no active child, in document order. */
  private activeLeaves(): Array<State> {
    const parents = new Set<string>()
    for (const id of this.active) {
      const parent = this.index.parent(this.index.state(id))
      if (parent !== undefined) parents.add(parent.id)
    }
    return this.active.filter((id) => !parents.has(id)).map((id) => this.index.state(id))
  }

  /**
   * execution.md's `selectByWalks`: from each active leaf, in document order, outward to the
   * root, the first state that offers an enabled candidate; then conflicts are removed. A state
   * that several walks reach offers the same transition each time, so what it offered is kept.
   */
  private selectByWalks(candidates: Candidates, bindings: Bindings): Array<Selected> {
    const offered = new Map<State, Selected | undefined>()
    const enabled: Array<Selected> = []
    for (const leaf of this.activeLeaves()) {
      for (let state: State | undefined = leaf; state !== undefined; state = this.index.parent(state)) {
        let selected: Selected | undefined
        if (offered.has(state)) selected = offered.get(state)
        else {
          selected = this.firstEnabled(state, candidates(state), bindings)
          offered.set(state, selected)
        }
        if (selected === undefined) continue
        if (!enabled.includes(selected)) enabled.push(selected)
        break
      }
    }
    return this.removeConflicts(enabled)
  }

  /** SCXML's `removeConflictingTransitions`, as execution.md gives it. */
  private removeConflicts(enabled: ReadonlyArray<Selected>): Array<Selected> {
    let kept: Array<Selected> = []
    const exits = new Map<Selected, Set<string>>()
    const exitsOf = (t: Selected) => {
      let set = exits.get(t)
      if (set === undefined) exits.set(t, set = new Set(this.exitSet(t)))
      return set
    }
    for (const t1 of enabled) {
      let preempted = false
      const replaced: Array<Selected> = []
      for (const t2 of kept) {
        const e2 = exitsOf(t2)
        if (![...exitsOf(t1)].some((id) => e2.has(id))) continue
        if (this.index.isProperAncestor(t2.source, t1.source)) replaced.push(t2)
        else {
          preempted = true
          break
        }
      }
      if (!preempted) {
        kept = kept.filter((t) => !replaced.includes(t))
        kept.push(t1)
      }
    }
    return kept
  }

  private selectEvent(value: Tagged, bindings: Bindings): Array<Selected> {
    const name = value._tag
    const isOn = (t: Transition) => t.trigger.kind === "event" && t.trigger.event === name
    const isAny = (t: Transition) => t.trigger.kind === "any"
    return this.selectByWalks((state) => (state.transitions.some(isOn) ? isOn : isAny), bindings)
  }

  /** `on c` for a call of the machine's call `c`. No `on *`, which matches only events. */
  private selectCall(call: string, bindings: Bindings): Array<Selected> {
    const matches = (t: Transition) => t.trigger.kind === "call" && t.trigger.call === call
    return this.selectByWalks(() => matches, bindings)
  }

  /** `on name.Event` for an emitted event: the address must name its sender. No `on *`. */
  private selectEmitted(input: Extract<Input, { readonly kind: "emitted" }>, bindings: Bindings): Array<Selected> {
    const name = input.value._tag
    const matches = (t: Transition) =>
      t.trigger.kind === "emitted" && t.trigger.event === name && this.namesSender(t.trigger.from, input)
    return this.selectByWalks(() => matches, bindings)
  }

  /**
   * `on name.ended` for a peer's end: the address must name run `from` now, as a `ref M`, a
   * `ref M?` or one of a `ref M[]`. No `on *`.
   */
  private selectEnded(from: string, bindings: Bindings): Array<Selected> {
    const matches = (t: Transition) => t.trigger.kind === "ended" && this.namesRun(t.trigger.from.name, from)
    return this.selectByWalks(() => matches, bindings)
  }

  /**
   * Whether parameter or variable `name` holds run `run`, itself or among a list's items. A
   * variable with no value, and `null`, name no run.
   */
  private namesRun(name: string, run: string): boolean {
    let value: Json | undefined
    if (this.index.params.has(name)) value = this.snapshot.input[name]
    else if (Object.hasOwn(this.vars, name)) value = this.vars[name]
    return value === run || (Array.isArray(value) && value.includes(run))
  }

  /**
   * `on name.Event` for an event a task emitted: through an address of kind `task` whose state
   * owns the invoke, or a collection of tasks that holds the spawned task. No `on *`.
   */
  private selectTaskEmitted(input: Extract<Input, { readonly kind: "task.emitted" }>, bindings: Bindings): Array<Selected> {
    const name = input.value._tag
    const matches = (t: Transition) =>
      t.trigger.kind === "emitted" && t.trigger.event === name && this.namesTask(t.trigger.from, input.invoke)
    return this.selectByWalks(() => matches, bindings)
  }

  /** Whether `address` names the invoked or spawned task with id `id`. */
  private namesTask(address: Address, id: number): boolean {
    if (address.task === undefined) return false
    if (address.kind === "task") return this.invokes.find((i) => i.id === id)?.state === address.state
    return this.spawned.find((c) => c.id === id)?.children === address.name
  }

  /** Whether `address` names the run that emitted `input`. A task is not a run. */
  private namesSender(address: Address, input: Extract<Input, { readonly kind: "emitted" }>): boolean {
    if (address.task !== undefined) return false
    if (address.kind === "child") {
      return input.invoke !== null && this.invokes.find((i) => i.id === input.invoke)?.state === address.state
    }
    if (address.kind === "children") {
      return input.invoke !== null && this.spawned.find((c) => c.id === input.invoke)?.children === address.name
    }
    if (input.invoke !== null) return false
    // A parameter, or a variable with a value; one with no value names no sender.
    if (this.index.params.has(address.name)) return this.snapshot.input[address.name] === input.from
    return Object.hasOwn(this.vars, address.name) && this.vars[address.name] === input.from
  }

  private selectOutcome(
    inv: InvokeEntry,
    matches: (t: Transition) => boolean,
    bindings: Bindings
  ): Array<Selected> {
    return listOf(this.firstEnabled(this.index.state(inv.state), matches, bindings))
  }

  private selectFail(inv: InvokeEntry, error: Tagged, bindings: Bindings): Array<Selected> {
    const state = this.index.state(inv.state)
    const isFail = (name: string) => (t: Transition) => t.trigger.kind === "invoke.fail" && t.trigger.error === name
    const named = isFail(error._tag)
    const matches = state.transitions.some(named) ? named : isFail("*")
    return listOf(this.firstEnabled(state, matches, bindings))
  }

  private selectAfter(tm: TimerEntry): Array<Selected> {
    const state = this.index.state(tm.state)
    const group = state.transitions[tm.index]?.group
    return listOf(this.firstEnabled(state, (t) => t.trigger.kind === "after" && t.group === group, NO_BINDINGS))
  }

  // ---- microsteps ----

  /**
   * The domain of a transition with targets: the root for one of the root's, the source for an
   * internal one, else the nearest compound proper ancestor of the source that is a proper
   * ancestor of every target. Only an internal transition's domain can be a parallel state.
   */
  private domain(t: Selected): State {
    const root = this.index.machine.root
    if (t.source === root || t.transition.internal) return t.source
    const targets = t.transition.targets.map((id) => this.index.state(id))
    for (let p = this.index.parent(t.source); p !== undefined; p = this.index.parent(p)) {
      if ((p === root || p.kind === "compound") && targets.every((x) => this.index.isProperAncestor(p, x))) return p
    }
    return root
  }

  /**
   * The active states a transition leaves: the proper descendants of its domain; for a parallel
   * domain, only those in the regions the targets are in (every region, for a history state of
   * the domain).
   */
  private exitSet(t: Selected): Array<string> {
    if (t.transition.targets.length === 0) return []
    const domain = this.domain(t)
    if (domain.kind !== "parallel") {
      return this.active.filter((id) => this.index.isProperAncestor(domain, this.index.state(id)))
    }
    const targets = t.transition.targets.map((id) => this.index.state(id))
    const every = targets.some((x) => isHistory(x) && this.index.parent(x) === domain)
    const regions = childrenOf(domain).filter((c) =>
      every || targets.some((x) => x === c || this.index.isProperAncestor(c, x))
    )
    return this.active.filter((id) => {
      const s = this.index.state(id)
      return regions.some((r) => r === s || this.index.isProperAncestor(r, s))
    })
  }

  /** The states from just below `domain` down to `target`, outermost first. */
  private pathFrom(domain: State, target: State): Array<State> {
    const path: Array<State> = []
    for (let s: State | undefined = target; s !== undefined && s !== domain; s = this.index.parent(s)) path.unshift(s)
    return path
  }

  /**
   * execution.md's `entrySet`, for a transition with targets: for each target, the states it
   * stands for (what a history state restores), the paths from just below the domain down to
   * them and their default entries; then every region of a parallel state among them that none
   * of them is in, by default entry. A history state itself is never entered.
   */
  private entrySet(t: Selected): Array<State> {
    const domain = this.domain(t)
    const set = new Set<State>()
    for (const id of t.transition.targets) {
      for (const r of this.standsFor(this.index.state(id))) {
        for (const s of this.pathFrom(domain, r)) set.add(s)
        for (const s of this.index.defaultEntry(r)) set.add(s)
      }
    }
    const inSet = (c: State): boolean => [...set].some((s) => s === c || this.index.isProperAncestor(c, s))
    for (const q of [...set]) {
      if (q.kind !== "parallel") continue
      for (const c of childrenOf(q)) {
        if (!inSet(c)) for (const s of [c, ...this.index.defaultEntry(c)]) set.add(s)
      }
    }
    return [...set]
  }

  /**
   * The states target `x` stands for: `x`, or for a history state, what it recorded, or else the
   * states its parent's default entry starts with.
   */
  private standsFor(x: State): Array<State> {
    if (!isHistory(x)) return [x]
    const parent = this.index.parent(x)!
    const recorded = this.history[x.id]
    return recorded !== undefined
      ? recorded.map((id) => this.index.state(id))
      : parent.kind === "compound"
      ? [this.index.state(parent.initial!)]
      : [...childrenOf(parent)]
  }

  /**
   * execution.md's `recordHistory`: for each state about to be exited, what each of its history
   * states records, from the active states as they are before anything is exited.
   */
  private recordHistory(exits: ReadonlySet<string>) {
    for (const id of exits) {
      const s = this.index.state(id)
      for (const h of s.states) {
        if (h.kind === "history") {
          this.history[h.id] = this.active.filter((a) => this.index.parent(this.index.state(a)) === s)
        } else if (h.kind === "deepHistory") {
          const leaves = this.activeLeaves().filter((leaf) => this.index.isProperAncestor(s, leaf))
          this.history[h.id] = leaves.map((leaf) => leaf.id)
        }
      }
    }
  }

  /** Take the transitions of `selected` together: every exit, then every effect, then every entry. */
  private microstep(selected: ReadonlyArray<Selected>, bindings: Bindings) {
    for (const { index, source, transition } of selected) {
      this.trace({ kind: "transition", from: source.id, index, to: transition.targets })
    }
    const exits = new Set(selected.flatMap((t) => this.exitSet(t)))
    this.recordHistory(exits)
    for (const state of this.innermostFirst()) {
      if (exits.has(state.id)) this.exitState(state)
    }
    for (const t of selected) this.runEffects(t.transition.effects, bindings)
    const entries = new Set<State>()
    for (const t of selected) {
      if (t.transition.targets.length > 0) {
        for (const state of this.entrySet(t)) entries.add(state)
      }
    }
    this.enterStates([...entries].sort((a, b) => this.index.position(a) - this.index.position(b)))
  }

  private exitState(state: State) {
    const inv = this.invokes.find((i) => i.state === state.id)
    if (inv !== undefined) {
      this.invokes = this.invokes.filter((i) => i !== inv)
      this.trace({ kind: "invoke.cancelled", invoke: inv.id, state: state.id, src: inv.src })
      this.commands.push({ kind: "invoke.cancel", invoke: inv.id })
    }
    const timers = this.timers.filter((t) => t.state === state.id).sort((a, b) => a.id - b.id)
    for (const tm of timers) {
      this.timers = this.timers.filter((t) => t !== tm)
      this.trace({ kind: "timer.cancelled", timer: tm.id, state: state.id, index: tm.index })
      this.commands.push({ kind: "timer.cancel", timer: tm.id })
    }
    this.runEffects(state.exit, NO_BINDINGS)
    this.active = this.active.filter((id) => id !== state.id)
    // A state's completion goes with it, and so does every completion of a state around it: an
    // internal transition that exits a final state takes back its parent's. A raised event
    // stays: it belongs to the run.
    this.queue = this.queue.filter((q) => {
      if (!("completion" in q)) return true
      const c = this.index.state(q.completion)
      return c !== state && !this.index.isProperAncestor(c, state)
    })
    this.trace({ kind: "exited", state: state.id })
  }

  enterStates(states: ReadonlyArray<State>) {
    for (const state of states) {
      const order = this.index.order.get(state.id)!
      const at = this.active.findIndex((id) => this.index.order.get(id)! > order)
      if (at === -1) this.active.push(state.id)
      else this.active.splice(at, 0, state.id)
      this.trace({ kind: "entered", state: state.id })
      this.runEffects(state.entry, NO_BINDINGS)
      this.entered.add(state.id)
      if (state.kind === "final") {
        if (this.index.parent(state) === this.index.machine.root) {
          this.finish(state)
          return
        }
        this.complete(state)
      }
    }
  }

  /** Final state `final`, not a child of the root, was just entered: queue what it completes. */
  private complete(final: State) {
    let s = final
    const parent = this.index.parent(final)!
    if (parent.kind === "compound") {
      this.queue.push({ completion: parent.id })
      s = parent
    }
    for (let p = this.index.parent(s); p !== undefined && p.kind === "parallel" && this.inFinalState(p); p = this.index.parent(s)) {
      s = p
      this.queue.push({ completion: s.id })
    }
  }

  /** SCXML's `isInFinalState`, over the active states. */
  private inFinalState(state: State): boolean {
    if (!this.active.includes(state.id)) return false
    switch (state.kind) {
      case "final":
        return true
      case "compound":
        return state.states.some((c) => c.kind === "final" && this.active.includes(c.id))
      case "parallel":
        return childrenOf(state).every((c) => this.inFinalState(c))
      default:
        return false
    }
  }

  private runEffects(effects: ReadonlyArray<Effect>, bindings: Bindings) {
    for (const effect of effects) {
      if (effect.kind !== "set") continue
      const value = this.eval(effect.value, bindings)
      this.vars[effect.var] = value
      this.trace({ kind: "set", var: effect.var, value })
    }
    for (const effect of effects) {
      switch (effect.kind) {
        case "set":
          break
        case "do": {
          const params = this.ir.actions[effect.action]?.params ?? []
          const args: Record<string, Json> = {}
          effect.args.forEach((arg, i) => {
            args[params[i]?.name ?? String(i)] = this.eval(arg, bindings)
          })
          this.trace({ kind: "action", action: effect.action, args })
          this.commands.push({ kind: "action", action: effect.action, args })
          break
        }
        case "send": {
          if (effect.to.kind === "children" && effect.key === null) {
            // `send name.*.E`: the value once, then a message of its own to every running child.
            const value = this.eval(effect.value, bindings) as Tagged
            const name = effect.to.name
            for (const ch of this.spawned.filter((c) => c.children === name)) {
              const message = this.nextMessage
              this.nextMessage += 1
              const to = `${this.snapshot.run}.${ch.id}`
              this.trace({ kind: "sent", message, to, value })
              this.commands.push({ kind: "send", to, invoke: ch.id, message, value })
            }
            break
          }
          const message = this.nextMessage
          this.nextMessage += 1
          let to: string
          let invoke: number | null
          let value: Tagged
          if (effect.to.kind === "children") {
            const key = this.eval(effect.key!, bindings)
            value = this.eval(effect.value, bindings) as Tagged
            const name = effect.to.name
            const ch = this.spawned.find((c) => c.children === name && c.key === key)
            if (ch === undefined) {
              this.trace({ kind: "undelivered", message, to: null, value })
              break
            }
            to = `${this.snapshot.run}.${ch.id}`
            invoke = ch.id
          } else if (effect.to.kind === "child") {
            const state = effect.to.state
            const inv = this.invokes.find((i) => i.state === state)
            value = this.eval(effect.value, bindings) as Tagged
            if (inv === undefined) {
              const shown = redactor(this.ir).sent(effect.to.machine, value) as Tagged
              this.trace({ kind: "undelivered", message, to: null, value: shown })
              break
            }
            to = `${this.snapshot.run}.${inv.id}`
            invoke = inv.id
          } else {
            // The address is read before the value, as an expression `ref` would read it.
            to = this.readName(effect.to.name) as string
            invoke = null
            value = this.eval(effect.value, bindings) as Tagged
          }
          this.trace({ kind: "sent", message, to, value: redactor(this.ir).sent(effect.to.machine, value) as Tagged })
          this.commands.push({ kind: "send", to, invoke, message, value })
          break
        }
        case "emit": {
          const message = this.nextMessage
          this.nextMessage += 1
          const value = this.eval(effect.value, bindings) as Tagged
          this.trace({ kind: "emitted", message, value })
          this.commands.push({ kind: "emit", message, value })
          break
        }
        case "reply": {
          // The pending-call value is read before the result or error is evaluated.
          const c = this.eval(effect.call, bindings) as { readonly from: string; readonly invoke: number }
          const pending = this.calls.find((p) => p.from === c.from && p.invoke === c.invoke)
          if (pending === undefined) {
            const t = parseTypeRef(effect.call.type)
            throw new Abort({ kind: "answered", call: t.kind === "call" ? t.call : effect.call.type })
          }
          const value = effect.value === null ? null : this.eval(effect.value, bindings)
          this.calls = this.calls.filter((p) => p !== pending)
          this.answer(pending.from, pending.invoke, pending.call, effect.outcome, value)
          break
        }
        case "spawn": {
          const key = this.eval(effect.key, bindings)
          const params = (this.collection(effect.children).kind === "task" ? this.ir.tasks : this.ir.machines)[effect.src]
            ?.params ?? []
          const args: Record<string, Json> = {}
          effect.args.forEach((arg, i) => {
            args[params[i]?.name ?? String(i)] = this.eval(arg, bindings)
          })
          this.stopChild(effect.children, key)
          this.pending.push({ children: effect.children, key, args })
          break
        }
        case "stop":
          this.stopChild(effect.children, this.eval(effect.key, bindings))
          break
        case "raise": {
          if (effect.delay === null) {
            const value = this.eval(effect.value, bindings) as Tagged
            this.trace({ kind: "raised", value })
            this.queue.push({ raised: value })
            break
          }
          // The delay, then the value; then the pending raise of its name goes.
          const delay = this.eval(effect.delay, bindings) as number
          const value = this.eval(effect.value, bindings) as Tagged
          if (effect.name !== null) this.cancelRaise(effect.name)
          const timer = this.nextTimer
          this.nextTimer += 1
          this.scheduled.push({ id: timer, name: effect.name, delay, value })
          this.trace({ kind: "raise.scheduled", timer, name: effect.name, delay, value })
          this.commands.push({ kind: "timer.start", timer, delay })
          break
        }
        case "cancel":
          this.cancelRaise(effect.name)
          break
      }
    }
  }

  /** Answer the call run `to` made with its invoke `invoke`: the `replied` record and the `reply` command. */
  private answer(to: string, invoke: number, call: string, outcome: "done" | "fail", value: Json) {
    this.trace({ kind: "replied", to, invoke, call, outcome, value })
    this.commands.push({ kind: "reply", to, invoke, call, outcome, value })
  }

  /** Stop the child of `children` with `key`: a running one with a record, a pending one silently. */
  private stopChild(children: string, key: Json) {
    this.pending = this.pending.filter((p) => !(p.children === children && p.key === key))
    const ch = this.spawned.find((c) => c.children === children && c.key === key)
    if (ch === undefined) return
    this.spawned = this.spawned.filter((c) => c !== ch)
    this.trace({ kind: "spawn.stopped", spawn: ch.id, children, key })
    this.commands.push({ kind: "spawn.stop", spawn: ch.id })
  }

  /** Cancel the pending delayed raise named `name`, if there is one. */
  private cancelRaise(name: string) {
    const sc = this.scheduled.find((s) => s.name === name)
    if (sc === undefined) return
    this.scheduled = this.scheduled.filter((s) => s !== sc)
    this.trace({ kind: "raise.cancelled", timer: sc.id, name: sc.name })
    this.commands.push({ kind: "timer.cancel", timer: sc.id })
  }

  /** When the run finishes or is cancelled, after its children are stopped: cancel every pending delayed raise. */
  private cancelRaises() {
    for (const sc of this.scheduled) {
      this.trace({ kind: "raise.cancelled", timer: sc.id, name: sc.name })
      this.commands.push({ kind: "timer.cancel", timer: sc.id })
    }
    this.scheduled = []
  }

  /** When the run finishes or is cancelled, after every exit: stop every child still running. */
  private stopChildren() {
    for (const ch of this.spawned) {
      this.trace({ kind: "spawn.stopped", spawn: ch.id, children: ch.children, key: ch.key })
      this.commands.push({ kind: "spawn.stop", spawn: ch.id })
    }
    this.spawned = []
    this.pending = []
  }

  /**
   * Trace and start spawned child `ch`, restoring it from `saved` if it has one: a child run,
   * or for a collection of tasks a task, which never has one.
   */
  private startChild(ch: SpawnedEntry, saved: Saved | null = null) {
    const { kind, src } = this.collection(ch.children)
    this.trace({ kind: "spawn.started", spawn: ch.id, children: ch.children, key: ch.key, src, args: ch.args })
    this.commands.push({
      kind: "spawn.start",
      spawn: ch.id,
      src,
      srcKind: kind,
      args: ch.args,
      saved: kind === "task" ? null : saved
    })
  }

  // ---- settling, work, finishing ----

  /** `always` first, then the internal queue front to back: completions and raised events. */
  settle() {
    let count = 0
    const isAlways = (t: Transition) => t.trigger.kind === "always"
    while (this.status === "running") {
      let selected = this.selectByWalks(() => isAlways, NO_BINDINGS)
      let bindings: Bindings = NO_BINDINGS
      while (selected.length === 0 && this.queue.length > 0) {
        const q = this.queue.shift()!
        if ("completion" in q) {
          const completed = this.index.state(q.completion)
          selected = listOf(this.firstEnabled(completed, (t) => t.trigger.kind === "done", NO_BINDINGS))
          continue
        }
        const input = { kind: "raised", value: q.raised } as const
        bindings = { event: q.raised }
        selected = this.selectEvent(q.raised, bindings)
        if (selected.length === 0 && this.index.machine.strict) throw new Abort({ kind: "unhandled", event: input })
        const r = redactor(this.ir)
        const shown = r.active ? r.input(this.index.machine, this.invokes, input, this.spawned) : input
        this.trace({ kind: selected.length === 0 ? "dropped" : "event", event: shown })
      }
      if (selected.length === 0) return
      count += 1
      if (count > SETTLE_LIMIT) throw new Abort({ kind: "limit" })
      this.microstep(selected, bindings)
    }
  }

  startWork() {
    for (const p of this.pending) {
      const ch: SpawnedEntry = { id: this.nextInvoke, children: p.children, key: p.key, args: p.args }
      this.nextInvoke += 1
      this.spawned.push(ch)
      this.startChild(ch)
    }
    this.pending = []
    for (const id of [...this.active]) {
      if (!this.entered.has(id)) continue
      const state = this.index.state(id)
      if (state.invoke !== null) {
        const invokeId = this.nextInvoke
        this.nextInvoke += 1
        this.invokes.push({ id: invokeId, state: id, src: invokeSrc(state.invoke) })
        this.startInvoke(state, invokeId)
      }
      const groups = new Set<number>()
      state.transitions.forEach((transition, index) => {
        if (transition.trigger.kind !== "after" || groups.has(transition.group)) return
        groups.add(transition.group)
        const delay = this.eval(transition.trigger.delay, NO_BINDINGS) as number
        const timerId = this.nextTimer
        this.nextTimer += 1
        this.timers.push({ id: timerId, state: id, index, delay })
        this.trace({ kind: "timer.started", timer: timerId, state: id, index, delay })
        this.commands.push({ kind: "timer.start", timer: timerId, delay })
      })
    }
  }

  /**
   * Trace and start invoke `invokeId` of `state`, its arguments evaluated now, its child run
   * restored from `saved` if it has one.
   */
  private startInvoke(state: State, invokeId: number, saved: Saved | null = null) {
    const invoke = state.invoke!
    // A call's peer is read before its arguments are evaluated, as a `send`'s address is.
    const to = invoke.kind === "call" && invoke.to !== null ? this.readName(invoke.to.name) as string : null
    const params = invoke.kind === "task"
      ? this.ir.tasks[invoke.src]?.params ?? []
      : invoke.kind === "call"
      ? this.callDecl(invoke)?.params ?? []
      : this.ir.machines[invoke.src]?.params ?? []
    const args: Record<string, Json> = {}
    invoke.args.forEach((arg, i) => {
      args[params[i]?.name ?? String(i)] = this.eval(arg, NO_BINDINGS)
    })
    this.trace({ kind: "invoke.started", invoke: invokeId, state: state.id, src: invokeSrc(invoke), args })
    this.commands.push({
      kind: "invoke.start",
      invoke: invokeId,
      state: state.id,
      src: invoke.src,
      srcKind: invoke.kind,
      to,
      as: invoke.as,
      args,
      saved
    })
  }

  /**
   * `restore`'s work: restart each saved invoke with its id, and each saved timer not yet due
   * for the time left, state by state in document order; then queue the expiry of the timers
   * already due, the earliest first.
   */
  restartWork(saved: Saved, now: number) {
    for (const ch of this.spawned) this.startChild(ch, saved.spawned?.find((c) => c.id === ch.id)?.child ?? null)
    for (const id of this.active) {
      const state = this.index.state(id)
      const inv = this.invokes.find((i) => i.state === id)
      if (inv !== undefined) this.startInvoke(state, inv.id, saved.invokes.find((i) => i.id === inv.id)?.child ?? null)
      const timers = saved.timers.filter((t) => t.state === id && t.due > now).sort((a, b) => a.id - b.id)
      for (const tm of timers) {
        const delay = tm.due - now
        this.trace({ kind: "timer.started", timer: tm.id, state: id, index: tm.index, delay })
        this.commands.push({ kind: "timer.start", timer: tm.id, delay })
      }
    }
    // Delayed raises belong to no state: after every state's work.
    for (const sc of saved.scheduled ?? []) {
      if (sc.due <= now) continue
      const delay = sc.due - now
      this.trace({ kind: "raise.scheduled", timer: sc.id, name: sc.name, delay, value: sc.value })
      this.commands.push({ kind: "timer.start", timer: sc.id, delay })
    }
    const due = [...saved.timers, ...(saved.scheduled ?? [])]
      .filter((t) => t.due <= now)
      .sort((a, b) => a.due - b.due || a.id - b.id)
    for (const tm of due) this.commands.push({ kind: "timer.fire", timer: tm.id })
  }

  private finish(final: State) {
    let status: "done" | "failed" = "done"
    let value: Json = null
    if (final.output !== null) value = this.eval(final.output, NO_BINDINGS)
    else if (final.fail !== null) {
      status = "failed"
      value = this.eval(final.fail, NO_BINDINGS)
    }
    for (const state of this.innermostFirst()) this.exitState(state)
    this.stopChildren()
    this.cancelRaises()
    this.result = status === "done" ? { output: value } : { error: value }
    this.status = status
    this.queue = []
    if (status === "done") this.trace({ kind: "done", state: final.id, output: value })
    else this.trace({ kind: "failed", state: final.id, error: value })
  }

  private cancelRun() {
    for (const state of this.innermostFirst()) this.exitState(state)
    this.stopChildren()
    this.cancelRaises()
    this.queue = []
    this.status = "cancelled"
    this.trace({ kind: "cancelled" })
  }

  failWithDefect(defect: Defect) {
    for (const inv of this.invokes) this.commands.push({ kind: "invoke.cancel", invoke: inv.id })
    for (const ch of this.spawned) this.commands.push({ kind: "spawn.stop", spawn: ch.id })
    for (const tm of this.timers) this.commands.push({ kind: "timer.cancel", timer: tm.id })
    for (const sc of this.scheduled) this.commands.push({ kind: "timer.cancel", timer: sc.id })
    this.invokes = []
    this.spawned = []
    this.pending = []
    this.timers = []
    this.scheduled = []
    this.status = "defect"
    this.result = { defect }
    this.trace({ kind: "defect", defect })
  }

  // ---- expressions ----

  eval(expr: Expr, bindings: Bindings): Json {
    switch (expr.kind) {
      case "lit":
        return expr.value
      case "ref": {
        const [name, ...fields] = expr.path
        let value: Json
        if (name === undefined) throw new Error("empty ref path")
        if (Object.hasOwn(bindings, name)) value = (bindings as Record<string, Json>)[name]!
        else value = this.readName(name)
        for (const field of fields) value = readField(value, field)
        return value
      }
      case "field":
        return readField(this.eval(expr.of, bindings), expr.name)
      case "call": {
        const args = expr.args.map((arg) => this.eval(arg, bindings))
        const result = this.fns(expr.fn, args)
        if ("defect" in result) throw new Abort(result.defect)
        return result.value
      }
      case "record": {
        const record: Record<string, Json> = {}
        const tag = this.tagOf(expr.type)
        if (tag !== undefined) record["_tag"] = tag
        for (const field of expr.fields) record[field.name] = this.eval(field.value, bindings)
        return record
      }
      case "enum":
        return expr.variant
      case "op":
        return this.evalOp(expr, bindings)
    }
  }

  /** A parameter, or a variable (an `unassigned` defect if it has no value). */
  private readName(name: string): Json {
    if (this.index.params.has(name)) return this.snapshot.input[name] ?? null
    if (this.index.vars.has(name)) {
      if (!Object.hasOwn(this.vars, name)) throw new Abort({ kind: "unassigned", var: name })
      return this.vars[name]!
    }
    return null
  }

  /**
   * The `_tag` of a record of type `type`, if it has one: an error's name, or an event's
   * (`Machine.Name`, or a bare event of this machine) without the machine. Aliases are expanded
   * first; an instance of a generic record (`Page<Order>`) has none.
   */
  private tagOf(type: string): string | undefined {
    const t = expandType(this.ir, type)
    if (t.kind === "event") return t.event
    if (t.kind !== "named") return undefined
    if (Object.hasOwn(this.ir.types, t.name)) return this.ir.types[t.name]!.kind === "error" ? t.name : undefined
    const m = this.index.machine
    return Object.hasOwn(m.emits, t.name) || Object.hasOwn(m.events, t.name) ? t.name : undefined
  }

  private evalOp(expr: Extract<Expr, { kind: "op" }>, bindings: Bindings): Json {
    const [left, right] = expr.args
    if (left === undefined) throw new Error(`operator ${expr.op} without operands`)
    switch (expr.op) {
      case "and":
        return this.eval(left, bindings) === true ? this.eval(right!, bindings) : false
      case "or":
        return this.eval(left, bindings) === true ? true : this.eval(right!, bindings)
      case "not":
        return this.eval(left, bindings) !== true
      case "neg":
        return this.arith(expr, -(this.eval(left, bindings) as number))
    }
    const a = this.eval(left, bindings)
    const b = this.eval(right!, bindings)
    switch (expr.op) {
      case "==":
        return a === b
      case "!=":
        return a !== b
      case "<":
      case "<=":
      case ">":
      case ">=": {
        const c = typeof a === "string" ? compareCodePoints(a, b as string) : (a as number) - (b as number)
        return expr.op === "<" ? c < 0 : expr.op === "<=" ? c <= 0 : expr.op === ">" ? c > 0 : c >= 0
      }
      case "+":
        return this.arith(expr, (a as number) + (b as number))
      case "-":
        return this.arith(expr, (a as number) - (b as number))
      case "*":
        return this.arith(expr, (a as number) * (b as number))
    }
  }

  /**
   * Check an arithmetic result against the node's type, aliases expanded: `int` range or
   * finite `float`.
   */
  private arith(expr: Expr, value: number): number {
    if (scalarOf(this.ir, expr.type) === "int") {
      if (!Number.isSafeInteger(value)) throw new Abort({ kind: "overflow", expr: expr.text })
      return value === 0 ? 0 : value
    }
    if (!Number.isFinite(value)) throw new Abort({ kind: "overflow", expr: expr.text })
    return value
  }
}

/** An invoke's `src` in the snapshot and the records: for a call, the address and the call, `inventory.reserve`. */
const invokeSrc = (invoke: NonNullable<State["invoke"]>): string =>
  invoke.kind === "call" && invoke.to !== null ? `${invoke.to.name}.${invoke.src}` : invoke.src

const listOf = <A>(a: A | undefined): Array<A> => (a === undefined ? [] : [a])

const readField = (value: Json, field: string): Json => {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return null
  const record = value as { readonly [key: string]: Json }
  return Object.hasOwn(record, field) ? record[field]! : null
}

/** Compare strings by Unicode scalar values (code points), not UTF-16 code units. */
export const compareCodePoints = (a: string, b: string): number => {
  let i = 0
  let j = 0
  while (i < a.length && j < b.length) {
    const ca = a.codePointAt(i)!
    const cb = b.codePointAt(j)!
    if (ca !== cb) return ca - cb
    i += ca > 0xffff ? 2 : 1
    j += cb > 0xffff ? 2 : 1
  }
  return (a.length - i) - (b.length - j)
}

/** The builtin an operand of type `type` is, aliases and `?` removed, or `undefined`. */
function scalarOf(ir: Ir, type: string): string | undefined {
  let t = expandType(ir, type)
  while (t.kind === "optional") t = expandType(ir, t.of)
  return t.kind === "builtin" ? t.name : undefined
}
