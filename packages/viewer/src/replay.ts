// Replay mode's model: a trace (spec/trace.md) read against the chart on screen. It follows
// each run's records to know, at any record, which states are active, which variables have
// which values, what is running and which transition was taken last; it checks the records
// against the chart as it goes, and warns when a run was recorded from another version of its
// machine (the `ir` of `started` differs from the machine's hash). A run restored from a saved
// snapshot begins with `restored` instead, and goes on from the snapshot's configuration,
// variables and work; a suspended run ends with `suspended`. A callee's view keeps the calls it
// has taken and not yet answered (`replied`), and a run its pending delayed raises
// (`raise.scheduled`, until their expiry or `raise.cancelled`). A peer's end (an `ended` input) is linked to the
// record that ends the peer's run, and back. Pure and DOM-free.

import type { Json } from '@dire-kiwi/roadbook-core'
import type { Ir, Machine, State } from './ir.ts'
import { own } from './ir.ts'
import { allStates, invokeNamed } from './model.ts'

/** A trace record as read: every member the line had. */
export type Rec = { kind: string; run: string; t: number; parent?: string } & { [member: string]: unknown }

const KINDS = new Set([
  'started', 'restored', 'suspended', 'entered', 'exited', 'event', 'dropped', 'stale', 'transition', 'set', 'action', 'sent', 'emitted', 'undelivered',
  'invoke.started', 'invoke.cancelled', 'replied', 'spawn.started', 'spawn.stopped', 'timer.started', 'timer.cancelled', 'raised', 'raise.scheduled', 'raise.cancelled',
  'done', 'failed', 'cancelled', 'defect',
])
/** The records after which a run writes nothing more. */
const ENDS = new Set(['done', 'failed', 'cancelled', 'defect', 'suspended'])
/** The statuses of an `ended` input, by the record that ends the peer's run. */
const ENDED_STATUS: Readonly<Record<string, string>> = { done: 'Done', failed: 'Failed', cancelled: 'Cancelled', defect: 'Defect', suspended: 'Suspended' }
/** The records that begin a run. */
const BEGINS = new Set(['started', 'restored'])

/** The records of a JSON Lines trace, and what could not be read (by line number, from 1). */
export function parseTrace(text: string): { records: Rec[]; problems: string[] } {
  const records: Rec[] = []
  const problems: string[] = []
  const lines = text.split('\n')
  if (lines[lines.length - 1] === '') lines.pop()
  lines.forEach((line, i) => {
    if (!line.trim()) return
    let v: unknown
    try {
      v = JSON.parse(line)
    } catch (e) {
      problems.push(`line ${i + 1}: not JSON (${e instanceof Error ? e.message : String(e)})`)
      return
    }
    if (typeof v !== 'object' || v === null || Array.isArray(v)) return void problems.push(`line ${i + 1}: not a JSON object`)
    const r = v as Rec
    if (typeof r.kind !== 'string' || !KINDS.has(r.kind)) return void problems.push(`line ${i + 1}: no record kind ${JSON.stringify(r.kind ?? null)}`)
    if (typeof r.run !== 'string' || !r.run) return void problems.push(`line ${i + 1}: no run`)
    records.push(r)
  })
  return { records, problems }
}

/** Run ids in trace order: the first segment as a string, the others as numbers, a prefix first. */
export function compareRunIds(a: string, b: string): number {
  const x = a.split('.')
  const y = b.split('.')
  if (x[0] !== y[0]) return x[0]! < y[0]! ? -1 : 1
  for (let i = 1; i < Math.min(x.length, y.length); i++) {
    const d = Number(x[i]) - Number(y[i])
    if (d) return d
  }
  return x.length - y.length
}

export interface RunInfo {
  id: string
  parent: string | null
  machine: string
  /** The `ir` of its `started` or `restored` record. */
  hash: string
  /** The index of its `started` or `restored` record, and of its last record. */
  first: number
  last: number
}

/** One run at one point of the trace. */
export interface RunView {
  run: string
  machine: string
  /** The active states, in the order they were entered. */
  active: string[]
  input: { [param: string]: Json }
  /** The variables set so far, by name. */
  vars: { [name: string]: Json }
  /** `running`, or how the run ended. A suspended run is still `running`. */
  status: string
  /** Whether the run was suspended: stopped and saved, to go on as another run. */
  suspended: boolean
  /** The output, error or defect of the record that ended it. */
  result: Json
  invokes: { id: number; state: string; src: string }[]
  /** The calls taken and not answered yet, oldest first: the caller's run, its invoke, the call. */
  calls: { from: string; invoke: number; call: string }[]
  /** The running spawned children: id, collection, key and machine. */
  spawned: { id: number; children: string; key: Json; src: string }[]
  timers: { id: number; state: string; index: number; delay: number }[]
  /** The pending delayed raises: a timer of the run, its `as` name, its delay and the event it raises. */
  scheduled: { id: number; name: string | null; delay: number; value: Json }[]
  /** The last transition the run took. */
  taken: { state: string; index: number } | null
}

/** What a record names on the diagram. */
export interface Named {
  states: string[]
  transition: { state: string; index: number } | null
}

const num = (v: unknown) => (typeof v === 'number' ? v : NaN)
const str = (v: unknown) => (typeof v === 'string' ? v : '')

export class Replay {
  readonly ir: Ir
  readonly records: Rec[]
  /** Records that could not be read, and records that do not fit the chart. */
  readonly problems: string[]
  /** Runs recorded from a machine that is not the one on screen: another hash, or not in the chart. */
  readonly warnings: string[]
  /** Every run, in run order. */
  readonly runs: RunInfo[]
  private readonly states = new Map<string, Map<string, State>>()

  constructor(ir: Ir, text: string) {
    this.ir = ir
    const parsed = parseTrace(text)
    this.records = parsed.records
    this.problems = [...parsed.problems]
    this.warnings = []
    const runs = new Map<string, RunInfo>()
    this.records.forEach((r, i) => {
      const info = runs.get(r.run)
      if (info) info.last = i
      else if (BEGINS.has(r.kind)) runs.set(r.run, { id: r.run, parent: r.parent ?? null, machine: str(r.machine), hash: str(r.ir), first: i, last: i })
    })
    this.runs = [...runs.values()].sort((a, b) => compareRunIds(a.id, b.id))
    for (const run of this.runs) {
      const m = own(ir.machines, run.machine)
      if (!m) this.warnings.push(`Run ${run.id} is of machine ${run.machine}, which this chart does not have.`)
      else if (m.hash !== run.hash)
        this.warnings.push(`Run ${run.id} was recorded from another version of ${run.machine}: the trace has IR ${short(run.hash)}, the chart on screen ${short(m.hash)}. States and transitions may not match.`)
    }
    this.problems.push(...this.check())
  }

  /**
   * Whether the child run `run` (`r1.4`) is a spawned child of its parent, rather than an
   * invoked one: its parent wrote a `spawn.started` with its id.
   */
  spawned(run: string): boolean {
    const dot = run.lastIndexOf('.')
    if (dot < 0) return false
    const parent = run.slice(0, dot)
    const id = Number(run.slice(dot + 1))
    return this.records.some((r) => r.run === parent && r.kind === 'spawn.started' && r.spawn === id)
  }

  machineOf(run: string): Machine | undefined {
    const info = this.runs.find((r) => r.id === run)
    return info && own(this.ir.machines, info.machine)
  }

  private stateMap(machine: string): Map<string, State> | undefined {
    let map = this.states.get(machine)
    const m = own(this.ir.machines, machine)
    if (!map && m) {
      map = new Map(allStates(m).map((s) => [s.id, s]))
      this.states.set(machine, map)
    }
    return map
  }

  /** The run `run` after the records up to and including index `at`. */
  viewAt(at: number, run: string): RunView {
    const info = this.runs.find((r) => r.id === run)
    const view: RunView = emptyView(run, info?.machine ?? '')
    for (let i = 0; i <= at && i < this.records.length; i++) {
      const r = this.records[i]!
      if (r.run === run) apply(view, r)
    }
    return view
  }

  /** For an `event` or `dropped` record whose input is a peer's end, the index of the record that ends the peer's run, if the trace has it. */
  peerEnd(at: number): number | null {
    const from = endedFrom(this.records[at])
    if (from === null) return null
    for (let i = this.records.length - 1; i >= 0; i--) {
      const r = this.records[i]!
      if (r.run === from && ENDS.has(r.kind)) return i
    }
    return null
  }

  /** For the record that ends a run, the `event` and `dropped` records in which its holders heard it (an `ended` input). */
  heardBy(at: number): number[] {
    const r = this.records[at]
    if (!r || !ENDS.has(r.kind)) return []
    const out: number[] = []
    this.records.forEach((x, i) => {
      if (endedFrom(x) === r.run) out.push(i)
    })
    return out
  }

  /** The states and transition record `at` names. */
  named(at: number): Named {
    const r = this.records[at]
    if (!r) return { states: [], transition: null }
    switch (r.kind) {
      case 'transition':
        return { states: Array.isArray(r.to) ? r.to.map(str) : [], transition: { state: str(r.from), index: num(r.index) } }
      case 'timer.started':
      case 'timer.cancelled':
        return { states: [str(r.state)], transition: { state: str(r.state), index: num(r.index) } }
      case 'entered':
      case 'exited':
      case 'invoke.started':
      case 'invoke.cancelled':
      case 'done':
      case 'failed':
        return { states: [str(r.state)], transition: null }
      case 'event':
      case 'dropped':
      case 'stale': {
        // An outcome or an expiry names the state of its invoke or timer, as the run had it.
        const before = this.viewAt(at - 1, r.run)
        const input = r.event as { kind?: string; invoke?: number; timer?: number } | undefined
        if (input?.kind === 'after') {
          const t = before.timers.find((x) => x.id === input.timer)
          return t ? { states: [t.state], transition: { state: t.state, index: t.index } } : { states: [], transition: null }
        }
        if (input?.kind?.startsWith('invoke.')) {
          const inv = before.invokes.find((x) => x.id === input.invoke)
          return { states: inv ? [inv.state] : [], transition: null }
        }
        return { states: [], transition: null }
      }
      default:
        return { states: [], transition: null }
    }
  }

  /** Checks every record against the chart, following each run. */
  private check(): string[] {
    const out: string[] = []
    const views = new Map<string, RunView>()
    const ended = new Set<string>()
    this.records.forEach((r, i) => {
      const at = `record ${i + 1} (${r.kind}, run ${r.run})`
      const info = this.runs.find((x) => x.id === r.run)
      if (BEGINS.has(r.kind)) {
        if (views.has(r.run)) out.push(`${at}: the run started twice`)
        const dot = r.run.lastIndexOf('.')
        const parent = dot < 0 ? undefined : r.run.slice(0, dot)
        if ((r.parent ?? undefined) !== parent) out.push(`${at}: parent should be ${parent ?? 'absent'}`)
      } else if (!views.has(r.run)) return void out.push(`${at}: before the run's started record`)
      if (ended.has(r.run)) return void out.push(`${at}: after the run ended`)
      const view = views.get(r.run) ?? emptyView(r.run, info?.machine ?? '')
      views.set(r.run, view)
      const m = own(this.ir.machines, view.machine)
      const states = this.stateMap(view.machine)
      if (m && states && m.hash === info?.hash) {
        const state = (id: unknown) => states.get(str(id))
        switch (r.kind) {
          case 'entered':
            if (!state(r.state)) out.push(`${at}: no state ${str(r.state)} in ${view.machine}`)
            else if (view.active.includes(str(r.state))) out.push(`${at}: ${str(r.state)} is already active`)
            break
          case 'exited':
            if (!view.active.includes(str(r.state))) out.push(`${at}: ${str(r.state)} is not active`)
            break
          case 'transition': {
            const s = state(r.from)
            const t = s?.transitions[num(r.index)]
            if (!s || !t) out.push(`${at}: ${str(r.from)} has no transition ${String(r.index)}`)
            else if (!view.active.includes(s.id)) out.push(`${at}: ${s.id} is not active`)
            else if (JSON.stringify(t.targets) !== JSON.stringify(r.to)) out.push(`${at}: the transition goes to ${t.targets.join(', ') || 'no state'}, not ${JSON.stringify(r.to)}`)
            break
          }
          case 'set':
            if (!m.vars.some((v) => v.name === r.var)) out.push(`${at}: ${view.machine} has no variable ${str(r.var)}`)
            break
          case 'invoke.started': {
            // The invoke the record names: by its `as` when its state has several.
            const inv = invokeNamed(state(r.state), typeof r.as === 'string' ? r.as : undefined)
            // A call's src names its address too: `inventory.reserve`.
            const src = inv ? (inv.kind === 'call' && inv.to ? `${inv.to.name}.${inv.src}` : inv.src) : null
            if (src !== r.src) out.push(`${at}: ${str(r.state)} does not invoke ${str(r.src)}`)
            break
          }
          case 'event':
          case 'dropped': {
            const input = r.event as { kind?: string; call?: unknown; from?: unknown } | undefined
            if (input?.kind === 'call' && !own(m.calls, str(input.call))) out.push(`${at}: ${view.machine} has no call ${str(input.call)}`)
            if (input?.kind === 'ended') {
              // Only a holder that hears the peer's machine's end is told it.
              const peer = this.runs.find((x) => x.id === input.from)?.machine
              const hears = allStates(m).some((s) => s.transitions.some((t) => t.trigger.kind === 'ended' && (peer === undefined || t.trigger.from.machine === peer)))
              if (!hears) out.push(`${at}: ${view.machine} has no on name.ended for ${peer ?? str(input.from)}`)
            }
            break
          }
          case 'raised':
          case 'raise.scheduled': {
            const tag = str((r.value as { _tag?: unknown } | null)?._tag)
            if (!own(m.events, tag) && !own(m.internalEvents ?? {}, tag)) out.push(`${at}: ${view.machine} has no event ${tag}`)
            break
          }
          case 'spawn.started': {
            const c = own(m.children, str(r.children))
            if (!c || c.src !== r.src) out.push(`${at}: ${view.machine} has no children ${str(r.children)} of ${str(r.src)}`)
            break
          }
          case 'timer.started': {
            const t = state(r.state)?.transitions[num(r.index)]
            if (t?.trigger.kind !== 'after') out.push(`${at}: ${str(r.state)} has no after at ${String(r.index)}`)
            break
          }
        }
      }
      if (r.kind === 'invoke.cancelled' && !view.invokes.some((x) => x.id === r.invoke)) out.push(`${at}: invoke ${String(r.invoke)} is not running`)
      if (r.kind === 'spawn.stopped' && !view.spawned.some((x) => x.id === r.spawn)) out.push(`${at}: spawned child ${String(r.spawn)} is not running`)
      if (r.kind === 'timer.cancelled' && !view.timers.some((x) => x.id === r.timer)) out.push(`${at}: timer ${String(r.timer)} is not running`)
      if (r.kind === 'raise.cancelled' && !view.scheduled.some((x) => x.id === r.timer)) out.push(`${at}: no delayed raise ${String(r.timer)} is pending`)
      if (r.kind === 'replied') {
        // A reply answers a pending call, or the call dropped by the record just before it.
        const prev = this.records.slice(0, i).reverse().find((x) => x.run === r.run)
        const dropped = prev?.kind === 'dropped' ? (prev.event as { kind?: string; from?: unknown; invoke?: unknown } | undefined) : undefined
        const answersDropped = dropped?.kind === 'call' && dropped.from === r.to && dropped.invoke === r.invoke
        if (!answersDropped && !view.calls.some((c) => c.from === r.to && c.invoke === r.invoke && c.call === r.call))
          out.push(`${at}: no pending call ${str(r.call)} from ${str(r.to)}'s invoke ${String(r.invoke)}`)
      }
      const from = endedFrom(r)
      if (from !== null || (r.kind === 'stale' && (r.event as { kind?: unknown } | undefined)?.kind === 'ended')) {
        const input = r.event as { status?: unknown }
        if (r.kind === 'stale') out.push(`${at}: a peer's end is never stale`)
        else if (!Object.values(ENDED_STATUS).includes(str(input.status))) out.push(`${at}: ${JSON.stringify(input.status ?? null)} is not a variant of Ended`)
        else {
          // It points at the record that ends the peer's run, which says the same.
          const end = this.peerEnd(i)
          const peerEnd = end === null ? undefined : this.records[end]
          if (peerEnd && ENDED_STATUS[peerEnd.kind] !== input.status) out.push(`${at}: ${from} ended ${peerEnd.kind}, not ${str(input.status)}`)
        }
      }
      if (r.kind === 'event') {
        const input = r.event as { kind?: string; invoke?: number; spawn?: number; timer?: number } | undefined
        if (input?.kind === 'after' && !view.timers.some((x) => x.id === input.timer) && !view.scheduled.some((x) => x.id === input.timer))
          out.push(`${at}: timer ${String(input.timer)} is not running`)
        if (input?.kind?.startsWith('invoke.') && !view.invokes.some((x) => x.id === input.invoke)) out.push(`${at}: invoke ${String(input.invoke)} is not running`)
        if (input?.kind?.startsWith('spawn.') && !view.spawned.some((x) => x.id === input.spawn)) out.push(`${at}: spawned child ${String(input.spawn)} is not running`)
      }
      apply(view, r)
      if (ENDS.has(r.kind)) ended.add(r.run)
    })
    return out
  }
}

const emptyView = (run: string, machine: string): RunView => ({
  run,
  machine,
  active: [],
  input: {},
  vars: {},
  status: 'running',
  suspended: false,
  result: null,
  invokes: [],
  calls: [],
  spawned: [],
  timers: [],
  scheduled: [],
  taken: null,
})

/** The parts of a saved snapshot (spec/snapshot.schema.json) a `restored` record gives the view. */
interface SavedView {
  active?: string[]
  input?: { [param: string]: Json }
  vars?: { [name: string]: Json }
  timers?: { id: number; state: string; index: number; delay: number }[]
  scheduled?: { id: number; name: string | null; delay: number; value: Json }[]
  calls?: { from: string; invoke: number; call: string }[]
}

/** Follows one record of a run. */
function apply(view: RunView, r: Rec) {
  switch (r.kind) {
    case 'started':
      view.machine = str(r.machine)
      view.input = (r.input ?? {}) as { [param: string]: Json }
      break
    case 'restored': {
      // The run goes on from the snapshot. Its invokes and the timers not yet due restart in
      // the records that follow, with the same ids; the timers already due stay until their
      // expiry is taken.
      const saved = (r.snapshot ?? {}) as SavedView
      view.machine = str(r.machine)
      view.active = [...(saved.active ?? [])]
      view.input = saved.input ?? {}
      view.vars = saved.vars ?? {}
      view.timers = (saved.timers ?? []).map(({ id, state, index, delay }) => ({ id, state, index, delay }))
      view.scheduled = (saved.scheduled ?? []).map(({ id, name, delay, value }) => ({ id, name, delay, value }))
      view.calls = (saved.calls ?? []).map(({ from, invoke, call }) => ({ from, invoke, call }))
      break
    }
    case 'suspended':
      view.suspended = true
      break
    case 'entered':
      if (!view.active.includes(str(r.state))) view.active.push(str(r.state))
      break
    case 'exited':
      view.active = view.active.filter((s) => s !== r.state)
      break
    case 'transition':
      view.taken = { state: str(r.from), index: num(r.index) }
      break
    case 'set':
      view.vars = { ...view.vars, [str(r.var)]: (r.value ?? null) as Json }
      break
    case 'invoke.started':
      view.invokes = [...view.invokes, { id: num(r.invoke), state: str(r.state), src: str(r.src) }]
      break
    case 'invoke.cancelled':
      view.invokes = view.invokes.filter((x) => x.id !== r.invoke)
      break
    case 'spawn.started':
      // A restored child restarts with the id it was saved with: the record replaces it.
      view.spawned = [...view.spawned.filter((x) => x.id !== r.spawn), { id: num(r.spawn), children: str(r.children), key: (r.key ?? null) as Json, src: str(r.src) }]
      break
    case 'spawn.stopped':
      view.spawned = view.spawned.filter((x) => x.id !== r.spawn)
      break
    case 'timer.started':
      // A restored timer restarts with the id it was saved with: the record replaces it.
      view.timers = [
        ...view.timers.filter((x) => x.id !== r.timer),
        { id: num(r.timer), state: str(r.state), index: num(r.index), delay: num(r.delay) },
      ]
      break
    case 'timer.cancelled':
      view.timers = view.timers.filter((x) => x.id !== r.timer)
      break
    case 'raise.scheduled':
      // A restored delayed raise restarts with the id it was saved with: the record replaces it.
      view.scheduled = [
        ...view.scheduled.filter((x) => x.id !== r.timer),
        { id: num(r.timer), name: typeof r.name === 'string' ? r.name : null, delay: num(r.delay), value: (r.value ?? null) as Json },
      ]
      break
    case 'raise.cancelled':
      view.scheduled = view.scheduled.filter((x) => x.id !== r.timer)
      break
    case 'event':
    case 'dropped': {
      // An outcome or an expiry is consumed whether or not it enabled a transition.
      const input = r.event as { kind?: string; invoke?: number; spawn?: number; timer?: number; from?: unknown; call?: unknown } | undefined
      if (input?.kind === 'after') {
        view.timers = view.timers.filter((x) => x.id !== input.timer)
        view.scheduled = view.scheduled.filter((x) => x.id !== input.timer)
      }
      if (input?.kind?.startsWith('invoke.')) view.invokes = view.invokes.filter((x) => x.id !== input.invoke)
      if (input?.kind?.startsWith('spawn.')) view.spawned = view.spawned.filter((x) => x.id !== input.spawn)
      // A call taken is pending until a reply answers it; a dropped one is answered at once.
      if (input?.kind === 'call' && r.kind === 'event') view.calls = [...view.calls, { from: str(input.from), invoke: num(input.invoke), call: str(input.call) }]
      break
    }
    case 'replied':
      view.calls = view.calls.filter((c) => !(c.from === r.to && c.invoke === r.invoke))
      break
    // A run that ends leaves no call pending: its driver answers each one Unanswered, writing nothing.
    case 'done':
      view.status = 'done'
      view.result = (r.output ?? null) as Json
      view.active = []
      view.calls = []
      break
    case 'failed':
      view.status = 'failed'
      view.result = (r.error ?? null) as Json
      view.active = []
      view.calls = []
      break
    case 'cancelled':
      view.status = 'cancelled'
      view.active = []
      view.calls = []
      break
    case 'defect':
      view.status = 'defect'
      view.result = (r.defect ?? null) as Json
      view.invokes = []
      view.spawned = []
      view.timers = []
      view.scheduled = []
      view.calls = []
      break
  }
}

/** The run whose end an `event` or `dropped` record's input is (`{"kind": "ended", "from": "r1", …}`), or null. */
function endedFrom(r: Rec | undefined): string | null {
  if (!r || (r.kind !== 'event' && r.kind !== 'dropped')) return null
  const input = r.event as { kind?: unknown; from?: unknown } | undefined
  return input?.kind === 'ended' && typeof input.from === 'string' ? input.from : null
}

const short = (hash: string) => (hash.length > 19 ? `${hash.slice(0, 19)}…` : hash || '(none)')

/** A value as compact JSON, cut short when long. */
export function jsonText(v: unknown, max = 160): string {
  let s: string
  try {
    s = JSON.stringify(v) ?? 'undefined'
  } catch {
    s = String(v)
  }
  return s.length > max ? s.slice(0, max - 1) + '…' : s
}

/** A state id without its machine: `Processing.Paying` for `PlaceOrder.Processing.Paying`. */
export const stateName = (id: unknown): string => {
  const s = str(id)
  const dot = s.indexOf('.')
  return dot < 0 ? s : s.slice(dot + 1)
}

/** The fields of a tagged value besides `_tag`, as ` {"reason":"busy"}`, or nothing. */
function jsonFields(v: unknown): string {
  const { _tag, ...rest } = (v ?? {}) as { [k: string]: unknown }
  void _tag
  return Object.keys(rest).length ? ' ' + jsonText(rest, 100) : ''
}

/** A tagged value in words: its tag, then its other fields as JSON. */
const tagged = (v: unknown) => `${str((v as { _tag?: unknown } | null)?._tag)}${jsonFields(v)}`

/** An input as an `event`, `dropped` or `stale` record holds it, in words. */
export function inputText(input: unknown): string {
  const i = (input ?? {}) as { [k: string]: unknown }
  const tag = (v: unknown) => str((v as { _tag?: unknown } | null)?._tag)
  const fields = jsonFields
  switch (i.kind) {
    case 'event':
      return `${tag(i.value)}${fields(i.value)}`
    case 'message':
      return `message ${String(i.message)} from ${str(i.from)}: ${tag(i.value)}${fields(i.value)}`
    case 'emitted':
      return `${tag(i.value)} emitted by ${str(i.from)}${fields(i.value)}`
    case 'task.emitted':
      return `${tag(i.value)} emitted by the task of invoke ${String(i.invoke)}${fields(i.value)}`
    case 'raised':
      return `${tag(i.value)} raised by the run${fields(i.value)}`
    case 'call': {
      const args = (i.args ?? {}) as { [k: string]: unknown }
      return `call ${str(i.call)} from ${str(i.from)} (its invoke ${String(i.invoke)})${Object.keys(args).length ? ' ' + jsonText(args, 100) : ''}`
    }
    case 'invoke.done':
      return `invoke ${String(i.invoke)} done ${jsonText(i.value, 100)}`
    case 'invoke.fail':
      return `invoke ${String(i.invoke)} fail ${tag(i.error)}${fields(i.error)}`
    case 'invoke.defect':
      return `invoke ${String(i.invoke)} defect ${jsonText(i.defect, 100)}`
    case 'spawn.done':
      return `spawned child ${String(i.spawn)} done ${jsonText(i.value, 100)}`
    case 'spawn.fail':
      return `spawned child ${String(i.spawn)} fail ${tag(i.error)}${fields(i.error)}`
    case 'spawn.defect':
      return `spawned child ${String(i.spawn)} defect ${jsonText(i.defect, 100)}`
    case 'after':
      return `timer ${String(i.timer)} expired`
    case 'ended':
      return `${str(i.from)} ended ${str(i.status)}`
    default:
      return jsonText(input, 100)
  }
}

/** One record in words, without its clock and run: what the trace lists show. */
export function recordText(r: { kind: string; [member: string]: unknown }): string {
  const args = (a: unknown) =>
    Object.entries((a ?? {}) as { [k: string]: unknown })
      .map(([k, v]) => `${k}: ${jsonText(v, 60)}`)
      .join(', ')
  switch (r.kind) {
    case 'started':
      return `started ${str(r.machine)}(${args(r.input)})`
    case 'restored': {
      const saved = (r.snapshot ?? {}) as { run?: unknown }
      return `restored ${str(r.machine)} from the snapshot of run ${str(saved.run)}${r.migrated ? `, migrated from ${short(str(r.migrated))}` : ''}`
    }
    case 'suspended':
      return 'suspended'
    case 'entered':
    case 'exited':
      return `${r.kind} ${stateName(r.state)}`
    case 'event':
    case 'stale':
      return `${r.kind} ${inputText(r.event)}`
    case 'dropped': {
      const call = (r.event as { kind?: unknown } | null)?.kind === 'call'
      // The `replied` record that follows answers it.
      return `dropped ${inputText(r.event)}${call ? ': no transition takes it, so it is answered Unanswered' : ''}`
    }
    case 'replied': {
      const value = r.outcome === 'fail' ? `${str((r.value as { _tag?: unknown } | null)?._tag)}${jsonFields(r.value)}` : r.value === null ? '' : jsonText(r.value, 100)
      return `replied ${str(r.outcome)}${value ? ` ${value}` : ''} to ${str(r.to)}'s call ${str(r.call)} (its invoke ${String(r.invoke)})`
    }
    case 'transition':
      return `transition ${stateName(r.from)} #${String(r.index)} → ${Array.isArray(r.to) && r.to.length ? r.to.map(stateName).join(', ') : '(no target)'}`
    case 'set':
      return `set ${str(r.var)} = ${jsonText(r.value, 100)}`
    case 'action':
      return `action ${str(r.action)}(${args(r.args)})`
    case 'sent':
    case 'undelivered':
      return `${r.kind} message ${String(r.message)} to ${r.to === null ? '(no child)' : str(r.to)}: ${jsonText(r.value, 100)}`
    case 'emitted':
      return `emitted message ${String(r.message)}: ${jsonText(r.value, 100)}`
    case 'invoke.started':
      return `invoke ${String(r.invoke)} started: ${str(r.src)}(${args(r.args)}) in ${stateName(r.state)}`
    case 'invoke.cancelled':
      return `invoke ${String(r.invoke)} cancelled: ${str(r.src)} in ${stateName(r.state)}`
    case 'spawn.started':
      return `spawned child ${String(r.spawn)} started: ${str(r.children)}[${jsonText(r.key, 60)}] = ${str(r.src)}(${args(r.args)})`
    case 'spawn.stopped':
      return `spawned child ${String(r.spawn)} stopped: ${str(r.children)}[${jsonText(r.key, 60)}]`
    case 'timer.started':
      return `timer ${String(r.timer)} started: ${String(r.delay)} ms in ${stateName(r.state)}`
    case 'timer.cancelled':
      return `timer ${String(r.timer)} cancelled in ${stateName(r.state)}`
    case 'raised':
      return `raised ${tagged(r.value)}`
    case 'raise.scheduled':
      return `raise ${tagged(r.value)} scheduled: timer ${String(r.timer)}, ${String(r.delay)} ms${typeof r.name === 'string' ? ` as ${r.name}` : ''}`
    case 'raise.cancelled':
      return `delayed raise cancelled: timer ${String(r.timer)}${typeof r.name === 'string' ? ` (${r.name})` : ''}`
    case 'done':
      return `done in ${stateName(r.state)}: ${jsonText(r.output, 100)}`
    case 'failed':
      return `failed in ${stateName(r.state)}: ${jsonText(r.error, 100)}`
    case 'cancelled':
      return 'cancelled'
    case 'defect': {
      const d = r.defect as { kind?: unknown; event?: { kind?: unknown; call?: unknown; timer?: unknown; value?: { _tag?: unknown } } } | null
      if (d?.kind === 'unhandled') {
        const what =
          d.event?.kind === 'call'
            ? `a call of ${String(d.event.call)}`
            : d.event?.kind === 'after'
              ? `the expiry of the delayed raise of timer ${String(d.event.timer)}`
              : d.event?.kind === 'raised'
                ? `${String(d.event.value?._tag)}, raised by the run,`
                : String(d.event?.value?._tag)
        return `defect: ${what} enabled no transition in this strict machine`
      }
      return `defect ${jsonText(r.defect, 100)}`
    }
    default:
      return jsonText(r, 120)
  }
}
