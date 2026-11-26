/**
 * The pure core: `start`, `step` and `restore`, exactly as `spec/execution.md` specifies them.
 * They do no I/O, read no clock and never modify the snapshot they are given.
 */
import {
  type Address,
  type Effect,
  type Expr,
  getMachine,
  type Ir,
  type Machine,
  type State,
  type Transition
} from "./ir.ts"
import type { Json } from "./json.ts"
import type {
  Command,
  CoreRecord,
  Defect,
  Fns,
  Input,
  InvokeEntry,
  RunResult,
  Saved,
  Snapshot,
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
    timers: [],
    nextInvoke: 1,
    nextTimer: 1,
    nextMessage: 1,
    result: null
  }
  const started: Command = {
    kind: "trace",
    record: { kind: "started", machine: name, ir: index.machine.hash, input }
  }
  const macro = new Macrostep(ir, index, initial, fns)
  macro.commands.push(started)
  try {
    for (const v of index.machine.vars) {
      if (v.init !== null) macro.vars[v.name] = macro.eval(v.init, NO_BINDINGS)
      else if (isOptional(ir, v.type)) macro.vars[v.name] = null
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
    timers: saved.timers.map(({ delay, id, index, state }) => ({ id, state, index, delay })),
    nextInvoke: saved.nextInvoke,
    nextTimer: saved.nextTimer,
    nextMessage: saved.nextMessage,
    result: saved.result
  }
  const restored: Command = {
    kind: "trace",
    record: { kind: "restored", machine: saved.machine, ir: index.machine.hash, migrated, snapshot: saved }
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

  constructor(machine: Machine) {
    this.machine = machine
    this.params = new Set(machine.params.map((p) => p.name))
    this.vars = new Set(machine.vars.map((v) => v.name))
    const walk = (state: State, parent: State | undefined) => {
      this.states.set(state.id, state)
      this.parents.set(state.id, parent)
      this.order.set(state.id, this.order.size)
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
    if (state.kind === "parallel") return state.states.flatMap((child) => [child, ...this.defaultEntry(child)])
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
  timers: Array<TimerEntry>
  nextInvoke: number
  nextTimer: number
  nextMessage: number
  result: RunResult | null
  readonly commands: Array<Command> = []
  /** The states entered during this macrostep. */
  private readonly entered = new Set<string>()
  /** Compound states that reached a final child and have not been exited since. */
  private completions: Array<string> = []

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
    this.timers = [...snapshot.timers]
    this.nextInvoke = snapshot.nextInvoke
    this.nextTimer = snapshot.nextTimer
    this.nextMessage = snapshot.nextMessage
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
        timers: this.timers,
        nextInvoke: this.nextInvoke,
        nextTimer: this.nextTimer,
        nextMessage: this.nextMessage,
        result: this.result
      },
      commands: this.commands
    }
  }

  private trace(record: CoreRecord) {
    this.commands.push({ kind: "trace", record })
  }

  take(input: Exclude<Input, { readonly kind: "defect" }>) {
    let selected: ReadonlyArray<Selected> = []
    let bindings: Bindings = NO_BINDINGS
    switch (input.kind) {
      case "cancel":
        this.cancelRun()
        return
      case "invoke.done":
      case "invoke.fail":
      case "invoke.defect": {
        const inv = this.invokes.find((i) => i.id === input.invoke)
        if (inv === undefined) {
          this.trace({ kind: "stale", event: input })
          return
        }
        this.invokes = this.invokes.filter((i) => i !== inv)
        if (input.kind === "invoke.defect") {
          this.trace({ kind: "event", event: input })
          this.failWithDefect(input.defect)
          return
        }
        if (input.kind === "invoke.fail" && !this.declaredErrors(inv.src).includes(input.error._tag)) {
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
      case "after": {
        const tm = this.timers.find((t) => t.id === input.timer)
        if (tm === undefined) {
          this.trace({ kind: "stale", event: input })
          return
        }
        this.timers = this.timers.filter((t) => t !== tm)
        selected = this.selectAfter(tm)
        break
      }
      case "event":
      case "message":
        bindings = { event: input.value }
        selected = this.selectEvent(input.value, bindings)
        break
      case "emitted": {
        if (input.invoke !== null && !this.invokes.some((i) => i.id === input.invoke)) {
          this.trace({ kind: "stale", event: input })
          return
        }
        bindings = { event: input.value }
        selected = this.selectEmitted(input, bindings)
        break
      }
    }
    if (selected.length === 0) {
      // A strict machine fails on one of its own events, or a message, that it does not handle.
      if (this.index.machine.strict && (input.kind === "event" || input.kind === "message")) {
        throw new Abort({ kind: "unhandled", event: input })
      }
      this.trace({ kind: "dropped", event: input })
      return
    }
    this.trace({ kind: "event", event: input })
    this.microstep(selected, bindings)
    this.settle()
    if (this.status === "running") this.startWork()
  }

  private declaredErrors(src: string): ReadonlyArray<string> {
    if (Object.hasOwn(this.ir.tasks, src)) return this.ir.tasks[src]?.errors ?? []
    return this.ir.machines[src]?.errors ?? []
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

  /** `on name.Event` for an emitted event: the address must name its sender. No `on *`. */
  private selectEmitted(input: Extract<Input, { readonly kind: "emitted" }>, bindings: Bindings): Array<Selected> {
    const name = input.value._tag
    const matches = (t: Transition) =>
      t.trigger.kind === "emitted" && t.trigger.event === name && this.namesSender(t.trigger.from, input)
    return this.selectByWalks(() => matches, bindings)
  }

  /** Whether `address` names the run that emitted `input`. */
  private namesSender(address: Address, input: Extract<Input, { readonly kind: "emitted" }>): boolean {
    if (address.kind === "child") {
      return input.invoke !== null && this.invokes.find((i) => i.id === input.invoke)?.state === address.state
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

  /** The nearest compound proper ancestor of the source that is a proper ancestor of the target. */
  private domain(t: Selected, target: State): State {
    const root = this.index.machine.root
    if (t.source === root) return root
    for (let p = this.index.parent(t.source); p !== undefined; p = this.index.parent(p)) {
      if ((p === root || p.kind === "compound") && this.index.isProperAncestor(p, target)) return p
    }
    return root
  }

  /** The active states a transition leaves: the proper descendants of its domain. */
  private exitSet(t: Selected): Array<string> {
    if (t.transition.target === null) return []
    const domain = this.domain(t, this.index.state(t.transition.target))
    return this.active.filter((id) => this.index.isProperAncestor(domain, this.index.state(id)))
  }

  /** The states a transition enters, below its domain down to the target and its default entry. */
  private entrySet(t: Selected, target: State): Array<State> {
    const domain = this.domain(t, target)
    const path: Array<State> = []
    for (let s: State | undefined = target; s !== undefined && s !== domain; s = this.index.parent(s)) path.unshift(s)
    const set = [...path, ...this.index.defaultEntry(target)]
    for (const p of path) {
      if (p === target || p.kind !== "parallel") continue
      for (const c of p.states) if (!path.includes(c)) set.push(c, ...this.index.defaultEntry(c))
    }
    return set
  }

  /** Take the transitions of `selected` together: every exit, then every effect, then every entry. */
  private microstep(selected: ReadonlyArray<Selected>, bindings: Bindings) {
    for (const { index, source, transition } of selected) {
      this.trace({ kind: "transition", from: source.id, index, to: transition.target })
    }
    const exits = new Set(selected.flatMap((t) => this.exitSet(t)))
    for (const state of this.innermostFirst()) {
      if (exits.has(state.id)) this.exitState(state)
    }
    for (const t of selected) this.runEffects(t.transition.effects, bindings)
    const entries = new Set<State>()
    for (const t of selected) {
      if (t.transition.target !== null) {
        for (const state of this.entrySet(t, this.index.state(t.transition.target))) entries.add(state)
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
    this.completions = this.completions.filter((id) => id !== state.id)
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
      this.completions.push(parent.id)
      s = parent
    }
    for (let p = this.index.parent(s); p !== undefined && p.kind === "parallel" && this.inFinalState(p); p = this.index.parent(s)) {
      s = p
      this.completions.push(s.id)
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
        return state.states.every((c) => this.inFinalState(c))
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
          const message = this.nextMessage
          this.nextMessage += 1
          let to: string
          let invoke: number | null
          let value: Tagged
          if (effect.to.kind === "child") {
            const state = effect.to.state
            const inv = this.invokes.find((i) => i.state === state)
            value = this.eval(effect.value, bindings) as Tagged
            if (inv === undefined) {
              this.trace({ kind: "undelivered", message, to: null, value })
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
          this.trace({ kind: "sent", message, to, value })
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
      }
    }
  }

  // ---- settling, work, finishing ----

  settle() {
    let count = 0
    const isAlways = (t: Transition) => t.trigger.kind === "always"
    while (this.status === "running") {
      let selected = this.selectByWalks(() => isAlways, NO_BINDINGS)
      while (selected.length === 0 && this.completions.length > 0) {
        const completed = this.index.state(this.completions.shift()!)
        selected = listOf(this.firstEnabled(completed, (t) => t.trigger.kind === "done", NO_BINDINGS))
      }
      if (selected.length === 0) return
      count += 1
      if (count > SETTLE_LIMIT) throw new Abort({ kind: "limit" })
      this.microstep(selected, NO_BINDINGS)
    }
  }

  startWork() {
    for (const id of [...this.active]) {
      if (!this.entered.has(id)) continue
      const state = this.index.state(id)
      if (state.invoke !== null) {
        const invokeId = this.nextInvoke
        this.nextInvoke += 1
        this.invokes.push({ id: invokeId, state: id, src: state.invoke.src })
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

  /** Trace and start invoke `invokeId` of `state`, its arguments evaluated now. */
  private startInvoke(state: State, invokeId: number) {
    const invoke = state.invoke!
    const params = invoke.kind === "task"
      ? this.ir.tasks[invoke.src]?.params ?? []
      : this.ir.machines[invoke.src]?.params ?? []
    const args: Record<string, Json> = {}
    invoke.args.forEach((arg, i) => {
      args[params[i]?.name ?? String(i)] = this.eval(arg, NO_BINDINGS)
    })
    this.trace({ kind: "invoke.started", invoke: invokeId, state: state.id, src: invoke.src, args })
    this.commands.push({
      kind: "invoke.start",
      invoke: invokeId,
      state: state.id,
      src: invoke.src,
      srcKind: invoke.kind,
      as: invoke.as,
      args
    })
  }

  /**
   * `restore`'s work: restart each saved invoke with its id, and each saved timer not yet due
   * for the time left, state by state in document order; then queue the expiry of the timers
   * already due, the earliest first.
   */
  restartWork(saved: Saved, now: number) {
    for (const id of this.active) {
      const state = this.index.state(id)
      const inv = this.invokes.find((i) => i.state === id)
      if (inv !== undefined) this.startInvoke(state, inv.id)
      const timers = saved.timers.filter((t) => t.state === id && t.due > now).sort((a, b) => a.id - b.id)
      for (const tm of timers) {
        const delay = tm.due - now
        this.trace({ kind: "timer.started", timer: tm.id, state: id, index: tm.index, delay })
        this.commands.push({ kind: "timer.start", timer: tm.id, delay })
      }
    }
    const due = saved.timers.filter((t) => t.due <= now).sort((a, b) => a.due - b.due || a.id - b.id)
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
    this.result = status === "done" ? { output: value } : { error: value }
    this.status = status
    this.completions = []
    if (status === "done") this.trace({ kind: "done", state: final.id, output: value })
    else this.trace({ kind: "failed", state: final.id, error: value })
  }

  private cancelRun() {
    for (const state of this.innermostFirst()) this.exitState(state)
    this.status = "cancelled"
    this.trace({ kind: "cancelled" })
  }

  failWithDefect(defect: Defect) {
    for (const inv of this.invokes) this.commands.push({ kind: "invoke.cancel", invoke: inv.id })
    for (const tm of this.timers) this.commands.push({ kind: "timer.cancel", timer: tm.id })
    this.invokes = []
    this.timers = []
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
   * (`Machine.Name`, or a bare event of this machine) without the machine.
   */
  private tagOf(type: string): string | undefined {
    if (Object.hasOwn(this.ir.types, type)) return this.ir.types[type]!.kind === "error" ? type : undefined
    const dot = type.indexOf(".")
    if (dot !== -1) return type.slice(dot + 1)
    const m = this.index.machine
    return Object.hasOwn(m.emits, type) || Object.hasOwn(m.events, type) ? type : undefined
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

  /** Check an arithmetic result against the node's type: `int` range or finite `float`. */
  private arith(expr: Expr, value: number): number {
    if (expr.type === "int") {
      if (!Number.isSafeInteger(value)) throw new Abort({ kind: "overflow", expr: expr.text })
      return value === 0 ? 0 : value
    }
    if (!Number.isFinite(value)) throw new Abort({ kind: "overflow", expr: expr.text })
    return value
  }
}

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

/** Whether a type reference is optional once aliases are expanded (`type Note = string?`). */
function isOptional(ir: Ir, type: string): boolean {
  if (type.endsWith("?")) return true
  if (type.endsWith("[]")) return false
  const decl = Object.hasOwn(ir.types, type) ? ir.types[type] : undefined
  return decl !== undefined && decl.kind === "alias" ? isOptional(ir, decl.type) : false
}
