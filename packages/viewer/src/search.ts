// Search by state, event, call, task, machine, error or action name, across every machine of an IR. Pure.

import type { Effect, Ir } from './ir.ts'
import { own } from './ir.ts'
import { callOfType, effectText, invokeText, isHistory, kindText, triggerText, walkStates } from './model.ts'

export type SearchKind = 'state' | 'event' | 'call' | 'task' | 'machine' | 'error' | 'action'

export interface SearchEntry {
  kind: SearchKind
  /** The name searched: a state, event, call, task, machine or error name. */
  name: string
  /** What the result shows: where the name occurs. */
  label: string
  /** The machine to open. */
  machine: string
  /** What to select there: a state, a transition, an event or call, or nothing (the machine itself). */
  target: { type: 'state'; id: string } | { type: 'transition'; state: string; index: number } | { type: 'event'; name: string } | null
}

export function buildSearchIndex(ir: Ir): SearchEntry[] {
  const out: SearchEntry[] = []
  for (const [machine, m] of Object.entries(ir.machines)) {
    out.push({ kind: 'machine', name: machine, label: `machine ${machine}`, machine, target: null })
    for (const name of Object.keys(m.events)) out.push({ kind: 'event', name, label: `event ${name} of ${machine}`, machine, target: { type: 'event', name } })
    for (const name of Object.keys(m.emits)) out.push({ kind: 'event', name, label: `emits ${name} of ${machine}`, machine, target: { type: 'event', name } })
    for (const name of Object.keys(m.calls)) out.push({ kind: 'call', name, label: `call ${name} of ${machine}`, machine, target: { type: 'event', name } })
    const short = (id: string) => id.slice(machine.length + 1) || id
    const effects = (list: Effect[], where: string, target: SearchEntry['target']) => {
      for (const e of list) {
        if (e.kind === 'do') out.push({ kind: 'action', name: e.action, label: `${effectText(e)}: ${where}`, machine, target })
        if (e.kind === 'send' || e.kind === 'emit') out.push({ kind: 'event', name: e.event, label: `${effectText(e)}: ${where}`, machine, target })
        if (e.kind === 'spawn') out.push({ kind: 'machine', name: e.machine, label: `${effectText(e)}: ${where}`, machine, target })
        const call = e.kind === 'reply' ? callOfType(e.call.type) : null
        if (call) out.push({ kind: 'call', name: call, label: `${effectText(e)}: ${where}`, machine, target })
      }
    }
    walkStates(m.root, (s, _parent, depth) => {
      if (depth > 0) out.push({ kind: 'state', name: s.name, label: `${machine}.${short(s.id)}${isHistory(s) ? ` (${kindText(s)})` : ''}`, machine, target: { type: 'state', id: s.id } })
      if (s.invoke) {
        out.push({
          kind: s.invoke.kind,
          name: s.invoke.src,
          label: `invoke ${invokeText(s.invoke)} in ${machine}.${short(s.id)}`,
          machine,
          target: { type: 'state', id: s.id },
        })
      }
      if (s.fail && own(ir.types, s.fail.type)?.kind === 'error') {
        out.push({ kind: 'error', name: s.fail.type, label: `fail ${s.fail.text} in ${machine}.${short(s.id)}`, machine, target: { type: 'state', id: s.id } })
      }
      const state = { type: 'state' as const, id: s.id }
      effects(s.entry, `entry of ${machine}.${short(s.id)}`, state)
      effects(s.exit, `exit of ${machine}.${short(s.id)}`, state)
      s.transitions.forEach((t, index) => {
        const where = `${machine}.${short(s.id)} → ${t.target ? short(t.target) : '(stays)'}`
        const target = { type: 'transition' as const, state: s.id, index }
        if (t.trigger.kind === 'event' || t.trigger.kind === 'emitted') out.push({ kind: 'event', name: t.trigger.event, label: `${triggerText(t.trigger)}: ${where}`, machine, target })
        if (t.trigger.kind === 'call') out.push({ kind: 'call', name: t.trigger.call, label: `on ${t.trigger.call}: ${where}`, machine, target })
        if (t.trigger.kind === 'invoke.fail' || t.trigger.kind === 'spawn.fail') out.push({ kind: 'error', name: t.trigger.error, label: `${triggerText(t.trigger)}: ${where}`, machine, target })
        effects(t.effects, where, target)
      })
    })
  }
  return out
}

/** Entries whose name matches `query` (case-insensitive): exact names first, then prefixes, then substrings. */
export function search(index: SearchEntry[], query: string, limit = 60): SearchEntry[] {
  const q = query.trim().toLowerCase()
  if (!q) return []
  const scored: [number, number, SearchEntry][] = []
  index.forEach((e, i) => {
    const name = e.name.toLowerCase()
    const at = name.indexOf(q)
    if (at < 0) return
    scored.push([name === q ? 0 : at === 0 ? 1 : 2, i, e])
  })
  scored.sort((a, b) => a[0] - b[0] || a[1] - b[1])
  return scored.slice(0, limit).map(([, , e]) => e)
}
