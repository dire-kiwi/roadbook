/**
 * The pure core: `start` and `step`, exactly as `spec/execution.md` specifies them. They do no
 * I/O, read no clock and never modify the snapshot they are given.
 */
import { getMachine, type Effect, type Expr, type Ir, type Machine, type State, type Transition } from "./ir.ts"
import type { Json } from "./json.ts"
import type {
  Command,
  CoreRecord,
  Defect,
  Fns,
  Input,
  InvokeEntry,
  RunResult,
  Snapshot,
  Status,
  Stepped,
  Tagged,
  TimerEntry
} from "./types.ts"

/** The most microsteps settling one input may take before it is a `limit` defect. */
export const SETTLE_LIMIT = 1000

/**
 * Start a run of machine `name` with `input` (its arguments keyed by parameter name), as
 * execution.md's `start`. Throws an `IrError` if the IR has no such machine or the machine has
 * a parallel state (not supported yet).
 */
export const start = (ir: Ir, name: string, input: { readonly [param: string]: Json }, fns: Fns): Stepped => {
  const index = machineIndex(ir, name)
  const initial: Snapshot = {
    machine: name,
    ir: index.machine.hash,
    status: "running",
    active: [],
    input,
    vars: {},
    invokes: [],
    timers: [],
    nextInvoke: 1,
    nextTimer: 1,
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

  defaultEntry(state: State): Array<State> {
    const entry: Array<State> = []
    let current = state
    while (current.kind === "compound" && current.initial !== null) {
      current = this.state(current.initial)
      entry.push(current)
    }
    return entry
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

class Macrostep {
  status: Status
  active: Array<string>
  readonly vars: Record<string, Json>
  invokes: Array<InvokeEntry>
  timers: Array<TimerEntry>
  nextInvoke: number
  nextTimer: number
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
    this.result = snapshot.result
  }

  finishStep(): Stepped {
    const s = this.snapshot
    return {
      snapshot: {
        machine: s.machine,
        ir: s.ir,
        status: this.status,
        active: this.active,
        input: s.input,
        vars: this.vars,
        invokes: this.invokes,
        timers: this.timers,
        nextInvoke: this.nextInvoke,
        nextTimer: this.nextTimer,
        result: this.result
      },
      commands: this.commands
    }
  }

  private trace(record: CoreRecord) {
    this.commands.push({ kind: "trace", record })
  }

  take(input: Exclude<Input, { readonly kind: "defect" }>) {
    let selected: Selected | undefined
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
        bindings = { event: input.value }
        selected = this.selectEvent(input.value, bindings)
        break
    }
    if (selected === undefined) {
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

  private selectEvent(value: Tagged, bindings: Bindings): Selected | undefined {
    const name = value._tag
    const isOn = (t: Transition) => t.trigger.kind === "event" && t.trigger.event === name
    const isAny = (t: Transition) => t.trigger.kind === "any"
    for (const state of this.innermostFirst()) {
      const matches = state.transitions.some(isOn) ? isOn : isAny
      const selected = this.firstEnabled(state, matches, bindings)
      if (selected !== undefined) return selected
    }
    return undefined
  }

  private selectOutcome(
    inv: InvokeEntry,
    matches: (t: Transition) => boolean,
    bindings: Bindings
  ): Selected | undefined {
    return this.firstEnabled(this.index.state(inv.state), matches, bindings)
  }

  private selectFail(inv: InvokeEntry, error: Tagged, bindings: Bindings): Selected | undefined {
    const state = this.index.state(inv.state)
    const isFail = (name: string) => (t: Transition) => t.trigger.kind === "invoke.fail" && t.trigger.error === name
    const named = isFail(error._tag)
    const matches = state.transitions.some(named) ? named : isFail("*")
    return this.firstEnabled(state, matches, bindings)
  }

  private selectAfter(tm: TimerEntry): Selected | undefined {
    const state = this.index.state(tm.state)
    const group = state.transitions[tm.index]?.group
    return this.firstEnabled(state, (t) => t.trigger.kind === "after" && t.group === group, NO_BINDINGS)
  }

  // ---- microsteps ----

  private microstep(selected: Selected, bindings: Bindings) {
    const { index, source, transition } = selected
    this.trace({ kind: "transition", from: source.id, index, to: transition.target })
    if (transition.target === null) {
      this.runEffects(transition.effects, bindings)
      return
    }
    const target = this.index.state(transition.target)
    const root = this.index.machine.root
    let domain: State = root
    if (source !== root) {
      for (let p = this.index.parent(source); p !== undefined; p = this.index.parent(p)) {
        if (this.index.isProperAncestor(p, target)) {
          domain = p
          break
        }
      }
    }
    for (const state of this.innermostFirst()) {
      if (this.index.isProperAncestor(domain, state)) this.exitState(state)
    }
    this.runEffects(transition.effects, bindings)
    const path: Array<State> = []
    for (let s: State | undefined = target; s !== undefined && s !== domain; s = this.index.parent(s)) {
      path.unshift(s)
    }
    this.enterStates([...path, ...this.index.defaultEntry(target)])
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
        const parent = this.index.parent(state)
        if (parent === this.index.machine.root) {
          this.finish(state)
          return
        }
        if (parent !== undefined) this.completions.push(parent.id)
      }
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
      if (effect.kind !== "do") continue
      const params = this.ir.actions[effect.action]?.params ?? []
      const args: Record<string, Json> = {}
      effect.args.forEach((arg, i) => {
        args[params[i]?.name ?? String(i)] = this.eval(arg, bindings)
      })
      this.trace({ kind: "action", action: effect.action, args })
      this.commands.push({ kind: "action", action: effect.action, args })
    }
  }

  // ---- settling, work, finishing ----

  settle() {
    let count = 0
    while (this.status === "running") {
      let selected: Selected | undefined
      for (const state of this.innermostFirst()) {
        selected = this.firstEnabled(state, (t) => t.trigger.kind === "always", NO_BINDINGS)
        if (selected !== undefined) break
      }
      while (selected === undefined && this.completions.length > 0) {
        const parent = this.index.state(this.completions.shift()!)
        selected = this.firstEnabled(parent, (t) => t.trigger.kind === "done", NO_BINDINGS)
      }
      if (selected === undefined) return
      count += 1
      if (count > SETTLE_LIMIT) throw new Abort({ kind: "limit" })
      this.microstep(selected, NO_BINDINGS)
    }
  }

  startWork() {
    for (const id of [...this.active]) {
      if (!this.entered.has(id)) continue
      const state = this.index.state(id)
      const invoke = state.invoke
      if (invoke !== null) {
        const invokeId = this.nextInvoke
        this.nextInvoke += 1
        const params = invoke.kind === "task"
          ? this.ir.tasks[invoke.src]?.params ?? []
          : this.ir.machines[invoke.src]?.params ?? []
        const args: Record<string, Json> = {}
        invoke.args.forEach((arg, i) => {
          args[params[i]?.name ?? String(i)] = this.eval(arg, NO_BINDINGS)
        })
        this.invokes.push({ id: invokeId, state: id, src: invoke.src })
        this.trace({ kind: "invoke.started", invoke: invokeId, state: id, src: invoke.src, args })
        this.commands.push({
          kind: "invoke.start",
          invoke: invokeId,
          state: id,
          src: invoke.src,
          srcKind: invoke.kind,
          args
        })
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
        else if (this.index.params.has(name)) value = this.snapshot.input[name] ?? null
        else if (this.index.vars.has(name)) {
          if (!Object.hasOwn(this.vars, name)) throw new Abort({ kind: "unassigned", var: name })
          value = this.vars[name]!
        } else value = null
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
        if (this.ir.types[expr.type]?.kind === "error") record["_tag"] = expr.type
        for (const field of expr.fields) record[field.name] = this.eval(field.value, bindings)
        return record
      }
      case "enum":
        return expr.variant
      case "op":
        return this.evalOp(expr, bindings)
    }
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
