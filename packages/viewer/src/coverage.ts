// Coverage mode's model: many traces (spec/trace.md), from a test run or from production, read
// against the chart on screen, counting how often each transition was taken (its `transition`
// records) and each state entered (its `entered` records), across every run of each machine,
// child runs included. A run recorded from another version of its machine (the `ir` of its
// `started` or `restored` record differs from the machine's hash) is left out, and its records
// counted in a warning, since its states and transitions may not be the chart's. Pure and
// DOM-free.

import type { Ir, Machine } from './ir.ts'
import { own } from './ir.ts'
import { allStates, transitionId } from './model.ts'
import type { Annotations, Mark } from './render.ts'
import { parseTrace } from './replay.ts'

export interface MachineCoverage {
  /** Runs of the machine counted. */
  runs: number
  /** Times each state was entered, by id. */
  states: Record<string, number>
  /** Times each transition was taken, by `stateId#index`. */
  transitions: Record<string, number>
}

export interface Coverage {
  /** The trace files read. */
  files: string[]
  /** Runs counted, of machines this chart has. */
  runs: number
  /** Records of the runs counted. */
  records: number
  /** Runs (and their records) recorded from another version of their machine, left out. */
  stale: { runs: number; records: number; machines: string[] }
  /** Runs of machines this chart does not have. */
  other: { runs: number; machines: string[] }
  /** Lines that are not trace records, `file: line n: why`. */
  problems: string[]
  machines: Record<string, MachineCoverage>
}

/** Counts every run of `traces` (file name and JSON Lines text) against `ir`. */
export function coverage(ir: Ir, traces: { name: string; text: string }[]): Coverage {
  const out: Coverage = {
    files: traces.map((t) => t.name),
    runs: 0,
    records: 0,
    stale: { runs: 0, records: 0, machines: [] },
    other: { runs: 0, machines: [] },
    problems: [],
    machines: {},
  }
  for (const name of Object.keys(ir.machines)) out.machines[name] = { runs: 0, states: {}, transitions: {} }
  for (const t of traces) {
    const { records, problems } = parseTrace(t.text)
    out.problems.push(...problems.map((p) => `${t.name}: ${p}`))
    // Runs are per file: `r1` of one trace is not `r1` of another.
    const runs = new Map<string, { machine: string; counted: boolean }>()
    for (const r of records) {
      let run = runs.get(r.run)
      if (!run && (r.kind === 'started' || r.kind === 'restored')) {
        const machine = typeof r.machine === 'string' ? r.machine : ''
        const m = own(ir.machines, machine)
        const counted = m !== undefined && m.hash === r.ir
        run = { machine, counted }
        runs.set(r.run, run)
        if (counted) {
          out.runs++
          out.machines[machine]!.runs++
        } else if (m) {
          out.stale.runs++
          if (!out.stale.machines.includes(machine)) out.stale.machines.push(machine)
        } else {
          out.other.runs++
          if (!out.other.machines.includes(machine)) out.other.machines.push(machine)
        }
      }
      if (!run) continue
      if (!run.counted) {
        if (own(ir.machines, run.machine)) out.stale.records++
        continue
      }
      out.records++
      const mc = out.machines[run.machine]!
      if (r.kind === 'entered' && typeof r.state === 'string') mc.states[r.state] = (mc.states[r.state] ?? 0) + 1
      else if (r.kind === 'transition' && typeof r.from === 'string' && typeof r.index === 'number') {
        const key = transitionId(r.from, r.index)
        mc.transitions[key] = (mc.transitions[key] ?? 0) + 1
      }
    }
  }
  return out
}

/** Every transition of a machine, with how often it was taken. */
export function transitionCounts(m: Machine, mc: MachineCoverage | undefined): { state: string; index: number; count: number }[] {
  const out: { state: string; index: number; count: number }[] = []
  for (const s of allStates(m)) s.transitions.forEach((_t, i) => out.push({ state: s.id, index: i, count: mc?.transitions[transitionId(s.id, i)] ?? 0 }))
  return out
}

/** Every state of a machine but its root, with how often it was entered. */
export function stateCounts(m: Machine, mc: MachineCoverage | undefined): { state: string; count: number }[] {
  return allStates(m)
    .filter((s) => s !== m.root)
    .map((s) => ({ state: s.id, count: mc?.states[s.id] ?? 0 }))
}

/**
 * The shade of a count, 1 to 4, relative to the largest count of its kind in the machine (on a
 * logarithmic scale, so a transition taken once among ones taken hundreds of times still
 * shows), or `never` for 0.
 */
export function shade(count: number, max: number): 'never' | 1 | 2 | 3 | 4 {
  if (count <= 0) return 'never'
  if (max <= 1) return 4
  return Math.min(4, 1 + Math.floor((3 * Math.log(count)) / Math.log(max) + 1e-9)) as 1 | 2 | 3 | 4
}

const times = (n: number) => `${n} time${n === 1 ? '' : 's'}`

/** The marks of coverage mode on `machine`. With no run of it counted, nothing is marked. */
export function coverageAnnotations(ir: Ir, cov: Coverage, machine: string): Annotations {
  const m = own(ir.machines, machine)
  const mc = own(cov.machines, machine)
  if (!m || !mc || !mc.runs) return {}
  const tmax = Math.max(0, ...Object.values(mc.transitions))
  const smax = Math.max(0, ...Object.values(mc.states))
  const runs = `${mc.runs} run${mc.runs === 1 ? '' : 's'}`
  return {
    state(id): Mark | null {
      if (id === m.root.id) return null
      const n = mc.states[id] ?? 0
      const s = shade(n, smax)
      return n
        ? { cls: `rb-cov-${s}`, badge: `×${n}`, title: `entered ${times(n)} in ${runs}` }
        : { cls: 'rb-cov-never', badge: 'never', title: `never entered in ${runs}` }
    },
    transition(state, index): Mark | null {
      const n = mc.transitions[transitionId(state, index)] ?? 0
      const s = shade(n, tmax)
      return n
        ? { cls: `rb-cov-${s}`, badge: `×${n}`, glyph: '✓', title: `taken ${times(n)} in ${runs}` }
        : { cls: 'rb-cov-never', badge: 'never', glyph: '0', title: `never taken in ${runs}` }
    },
  }
}
