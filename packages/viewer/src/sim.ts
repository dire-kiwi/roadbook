// Simulate mode's model: one run of a machine, driven by hand through `@dire-kiwi/roadbook-core`, with the
// history of every choice so the reader can step back. Pure and DOM-free; the page
// (app/simulate.ts) draws it, and the tests drive it with a conformance case's script.
//
// It is a small driver (spec/execution.md, "The driver") for a single run on a virtual clock:
// the reader is the host. Tasks, and machine invokes (treated as tasks: the child run is not
// started), end when the reader gives their outcome; timers fire when the reader lets them or
// advances the clock; functions are answered by the reader when the core calls them; actions
// do nothing. `send` and `emit` are carried out as their records only: there is no other run to
// deliver to. A child invoked with `as` can be made to emit one of its events by hand.

import { canonicalJson, getMachine, IrError, start as coreStart, step as coreStep } from '@dire-kiwi/roadbook-core'
import type { Command, Fns, FnResult, Input, Ir as CoreIr, Json, Snapshot, Stepped, Tagged, TraceRecord } from '@dire-kiwi/roadbook-core'
import type { Field, FunctionDecl, Ir, Machine } from './ir.ts'
import { own } from './ir.ts'
import { findState } from './model.ts'

/** The reader's answer to a function call: a value of the function's type, or a host defect. */
export type Answer = { value: Json } | { defect: string }

/** A call the core made that has no answer yet. */
export interface Ask {
  fn: string
  args: Json[]
  decl: FunctionDecl | undefined
}

/** What an action of the reader came to. */
export type Outcome =
  | { kind: 'ok' }
  /** The core called a function nobody has answered for these arguments: answer, then `retry()`. */
  | { kind: 'ask'; ask: Ask }
  /** Nothing happened: the machine cannot run here, or the action made no sense. */
  | { kind: 'error'; message: string }

/** One point in the history: the snapshot after a choice, and what the choice wrote. */
export interface Frame {
  /** What the reader chose, in words: `start`, `send Cancel`, `chargeCard done`, `advance 200 ms`. */
  label: string
  snapshot: Snapshot
  /** The virtual clock, in milliseconds from the start. */
  now: number
  /** When each running timer is due, by timer id. */
  due: Readonly<Record<number, number>>
  /** For each running child invoke with `as`, the id its next emitted event gets. */
  childMessages: Readonly<Record<number, number>>
  /** The records this choice wrote, in order. */
  records: TraceRecord[]
  /** The transitions this choice took, in order. */
  taken: { state: string; index: number }[]
}

export interface SimOptions {
  /** The run's id. Default `r1`. */
  run?: string
  /**
   * The host defect an action has, if any (a conformance case's `actions`). The page has none:
   * actions do nothing.
   */
  actionDefect?: (action: string, args: { readonly [param: string]: Json }) => string | null
}

/** A running invoke and what can end it. */
export interface InvokeChoice {
  id: number
  state: string
  src: string
  srcKind: 'task' | 'machine'
  /** The result type, or null for a task or machine that returns nothing. */
  returns: string | null
  /** The declared errors, with their fields. */
  errors: { name: string; fields: Field[] }[]
  /** The `as` name of a machine invoke, through which the child's emitted events reach this run. */
  as: string | null
  /** The events the child machine emits (for an invoke with `as`). */
  emits: { name: string; fields: Field[] }[]
}

export interface TimerChoice {
  id: number
  state: string
  index: number
  delay: number
  due: number
}

export interface EventChoice {
  name: string
  fields: Field[]
  /** Whether an active state has an `on` for it (or an `on *`); otherwise it would be dropped, unless a guard decides. */
  handled: boolean
}

export interface Choices {
  events: EventChoice[]
  invokes: InvokeChoice[]
  timers: TimerChoice[]
}

class NeedAnswer {
  constructor(readonly ask: Ask) {}
}

const answerKey = (fn: string, args: ReadonlyArray<Json>) => `${fn}(${args.map((a) => canonicalJson(a)).join(',')})`

/** A working copy of the frame a choice starts from. */
interface Work {
  snapshot: Snapshot
  now: number
  due: Record<number, number>
  childMessages: Record<number, number>
  records: TraceRecord[]
}

export class Simulation {
  readonly ir: Ir
  readonly machine: string
  readonly run: string
  /** The history: `frames[0]` is the start. */
  frames: Frame[] = []
  /** The frame shown; frames after it can be stepped forward to again until a new choice is made. */
  at = -1
  /** The reader's answers, by function and canonical arguments: `retryDelay(1)`. */
  readonly answers = new Map<string, { fn: string; args: Json[]; answer: Answer }>()
  private readonly options: SimOptions
  private pending: { label: string; act: (w: Work) => void; from: number; ask: Ask } | null = null

  constructor(ir: Ir, machine: string, options: SimOptions = {}) {
    this.ir = ir
    this.machine = machine
    this.run = options.run ?? 'r1'
    this.options = options
  }

  get def(): Machine {
    return own(this.ir.machines, this.machine)!
  }

  /** The frame shown, or undefined before the run has started. */
  get current(): Frame | undefined {
    return this.frames[this.at]
  }

  get started(): boolean {
    return this.at >= 0
  }

  /** The records from the start up to the frame shown. */
  records(): TraceRecord[] {
    return this.frames.slice(0, this.at + 1).flatMap((f) => f.records)
  }

  /** Starts the run with `input` (its arguments by parameter name). Forgets any earlier run. */
  start(input: { [param: string]: Json }): Outcome {
    this.frames = []
    this.at = -1
    const ir = this.ir as unknown as CoreIr
    return this.attempt('start', -1, (w) => {
      this.carryOut(w, coreStart(ir, this.machine, this.run, input, this.fns()))
    })
  }

  /** Gives the run an external event, a tagged record of one of its events. */
  send(value: Tagged): Outcome {
    return this.choose(`send ${value._tag}`, (w) => this.step(w, { kind: 'event', value }))
  }

  /** Ends a running invoke: with a value, one of its errors, or a host defect. */
  complete(invoke: number, outcome: { done: Json } | { fail: Tagged } | { defect: string }): Outcome {
    const inv = this.current?.snapshot.invokes.find((i) => i.id === invoke)
    if (!inv) return { kind: 'error', message: `invoke ${invoke} is not running` }
    const input: Input =
      'done' in outcome
        ? { kind: 'invoke.done', invoke, value: outcome.done }
        : 'fail' in outcome
          ? { kind: 'invoke.fail', invoke, error: outcome.fail }
          : { kind: 'invoke.defect', invoke, defect: { kind: 'host', message: outcome.defect } }
    const what = 'done' in outcome ? 'done' : 'fail' in outcome ? `fail ${outcome.fail._tag}` : 'defect'
    return this.choose(`${inv.src} ${what}`, (w) => this.step(w, input))
  }

  /** Lets a running timer fire, moving the clock to when it is due if that is later. */
  fire(timer: number): Outcome {
    const due = this.current?.due[timer]
    if (due === undefined) return { kind: 'error', message: `timer ${timer} is not running` }
    return this.choose(`timer ${timer} fires`, (w) => {
      w.now = Math.max(w.now, due)
      this.step(w, { kind: 'after', timer })
    })
  }

  /**
   * Moves the clock on by `ms`, firing the timers that fall due on the way one at a time, the
   * earliest first (as a conformance runner's `advance`).
   */
  advance(ms: number): Outcome {
    if (!(Number.isInteger(ms) && ms >= 0)) return { kind: 'error', message: 'advance by a whole number of milliseconds, 0 or more' }
    return this.choose(`advance ${ms} ms`, (w) => {
      const end = w.now + ms
      for (;;) {
        if (w.snapshot.status !== 'running') break
        let next: { id: number; due: number } | null = null
        for (const t of w.snapshot.timers) {
          const due = w.due[t.id]
          if (due !== undefined && due <= end && (!next || due < next.due)) next = { id: t.id, due }
        }
        if (!next) break
        w.now = next.due
        this.step(w, { kind: 'after', timer: next.id })
      }
      w.now = Math.max(w.now, end)
    })
  }

  /** Makes the child of a running machine invoke with `as` emit one of its events to this run. */
  emitFromChild(invoke: number, value: Tagged): Outcome {
    const inv = this.current?.snapshot.invokes.find((i) => i.id === invoke)
    if (!inv) return { kind: 'error', message: `invoke ${invoke} is not running` }
    return this.choose(`${inv.src} emits ${value._tag}`, (w) => {
      const message = w.childMessages[invoke] ?? 1
      w.childMessages[invoke] = message + 1
      this.step(w, { kind: 'emitted', from: `${this.run}.${invoke}`, invoke, message, value })
    })
  }

  /** Cancels the run from outside. */
  cancel(): Outcome {
    return this.choose('cancel', (w) => this.step(w, { kind: 'cancel' }))
  }

  /** Steps back one choice; the later ones stay until a new choice is made. */
  back(): boolean {
    if (this.at <= 0) return false
    this.at -= 1
    this.pending = null
    return true
  }

  /** Steps forward to the next choice of the history, if the reader stepped back. */
  forward(): boolean {
    if (this.at >= this.frames.length - 1) return false
    this.at += 1
    this.pending = null
    return true
  }

  /** Records the answer to a function call. It is kept for every later call with those arguments. */
  answer(fn: string, args: Json[], answer: Answer) {
    this.answers.set(answerKey(fn, args), { fn, args, answer })
  }

  forget(key: string) {
    this.answers.delete(key)
  }

  /** The question the last choice is waiting on, if any. */
  get asking(): Ask | null {
    return this.pending?.ask ?? null
  }

  /** Tries the choice that asked for an answer again. */
  retry(): Outcome {
    const p = this.pending
    if (!p) return { kind: 'error', message: 'nothing is waiting for an answer' }
    return this.attempt(p.label, p.from, p.act)
  }

  /** Drops the choice that asked for an answer. */
  abandon() {
    this.pending = null
  }

  /** What the reader can do in the frame shown. */
  choices(): Choices {
    const f = this.current
    if (!f || f.snapshot.status !== 'running') return { events: [], invokes: [], timers: [] }
    const m = this.def
    const active = f.snapshot.active.map((id) => findState(m, id)).filter((s) => s !== null)
    const events = Object.entries(m.events).map(([name, decl]) => ({
      name,
      fields: decl.fields,
      handled: active.some((s) => s.transitions.some((t) => (t.trigger.kind === 'event' && t.trigger.event === name) || t.trigger.kind === 'any')),
    }))
    const invokes = f.snapshot.invokes.map((inv): InvokeChoice => {
      const state = findState(m, inv.state)
      const srcKind = state?.invoke?.kind ?? 'task'
      const decl = srcKind === 'machine' ? own(this.ir.machines, inv.src) : own(this.ir.tasks, inv.src)
      const as = state?.invoke?.as ?? null
      const child = srcKind === 'machine' ? own(this.ir.machines, inv.src) : undefined
      return {
        id: inv.id,
        state: inv.state,
        src: inv.src,
        srcKind,
        returns: decl?.returns ?? null,
        errors: (decl?.errors ?? []).map((name) => {
          const t = own(this.ir.types, name)
          return { name, fields: t && t.kind === 'error' ? t.fields : [] }
        }),
        as,
        emits: as && child ? Object.entries(child.emits).map(([name, d]) => ({ name, fields: d.fields })) : [],
      }
    })
    const timers = f.snapshot.timers.map((t) => ({ id: t.id, state: t.state, index: t.index, delay: t.delay, due: f.due[t.id] ?? f.now + t.delay }))
    return { events, invokes, timers }
  }

  // ───────────── the driver ─────────────

  private choose(label: string, act: (w: Work) => void): Outcome {
    const f = this.current
    if (!f) return { kind: 'error', message: 'start the run first' }
    if (f.snapshot.status !== 'running') return { kind: 'error', message: `the run has ended (${f.snapshot.status})` }
    return this.attempt(label, this.at, act)
  }

  /** Runs `act` on a copy of frame `from` (-1: nothing yet), and adds the result to the history. */
  private attempt(label: string, from: number, act: (w: Work) => void): Outcome {
    const base = this.frames[from]
    const w: Work = base
      ? { snapshot: base.snapshot, now: base.now, due: { ...base.due }, childMessages: { ...base.childMessages }, records: [] }
      : { snapshot: undefined as unknown as Snapshot, now: 0, due: {}, childMessages: {}, records: [] }
    try {
      act(w)
    } catch (e) {
      if (e instanceof NeedAnswer) {
        this.pending = { label, act, from, ask: e.ask }
        return { kind: 'ask', ask: e.ask }
      }
      this.pending = null
      if (e instanceof IrError) return { kind: 'error', message: e.message }
      return { kind: 'error', message: e instanceof Error ? e.message : String(e) }
    }
    this.pending = null
    const taken = w.records.flatMap((r) => (r.kind === 'transition' ? [{ state: r.from, index: r.index }] : []))
    for (const id of Object.keys(w.due).map(Number)) if (!w.snapshot.timers.some((t) => t.id === id)) delete w.due[id]
    for (const id of Object.keys(w.childMessages).map(Number)) if (!w.snapshot.invokes.some((i) => i.id === id)) delete w.childMessages[id]
    const frame: Frame = { label, snapshot: w.snapshot, now: w.now, due: w.due, childMessages: w.childMessages, records: w.records, taken }
    this.frames = [...this.frames.slice(0, from + 1), frame]
    this.at = this.frames.length - 1
    return { kind: 'ok' }
  }

  private fns(): Fns {
    return (name, args): FnResult => {
      const known = this.answers.get(answerKey(name, args))
      if (!known) throw new NeedAnswer({ fn: name, args: [...args], decl: own(this.ir.functions, name) })
      return 'value' in known.answer ? { value: known.answer.value } : { defect: { kind: 'host', message: known.answer.defect } }
    }
  }

  private step(w: Work, input: Input) {
    this.carryOut(w, coreStep(this.ir as unknown as CoreIr, w.snapshot, input, this.fns()))
  }

  /** Carries out a step's commands in order, as execution.md's `carryOut`. */
  private carryOut(w: Work, stepped: Stepped) {
    w.snapshot = stepped.snapshot
    const commands = stepped.commands
    for (let i = 0; i < commands.length; i++) {
      const c = commands[i]!
      const defect = this.command(w, c)
      if (defect !== null) {
        // A host defect in an action stops the commands; the core's answer is carried out instead.
        const rest = coreStep(this.ir as unknown as CoreIr, w.snapshot, { kind: 'defect', defect: { kind: 'host', message: defect } }, this.fns())
        w.snapshot = rest.snapshot
        for (const r of rest.commands) this.command(w, r)
        return
      }
    }
  }

  /** Carries out one command. Returns the host defect of an action that has one. */
  private command(w: Work, c: Command): string | null {
    switch (c.kind) {
      case 'trace':
        w.records.push({ ...c.record, t: w.now, run: this.run } as TraceRecord)
        return null
      case 'action':
        return this.options.actionDefect?.(c.action, c.args) ?? null
      case 'timer.start':
        w.due[c.timer] = w.now + c.delay
        return null
      case 'timer.cancel':
        delete w.due[c.timer]
        return null
      case 'invoke.cancel':
        delete w.childMessages[c.invoke]
        return null
      case 'invoke.start':
      case 'send':
      case 'emit':
        // The invoke waits for the reader; messages have no other run to go to.
        return null
      case 'timer.fire':
        // Only `restore` emits it, and simulate mode never restores a run.
        return null
    }
  }
}

/** Why the machine cannot be simulated, or null: the core refuses a machine the IR does not have. */
export function refusal(ir: Ir, machine: string): string | null {
  try {
    getMachine(ir as unknown as CoreIr, machine)
    return null
  } catch (e) {
    return e instanceof Error ? e.message : String(e)
  }
}
