// Diff mode's model: what `roadbook viz --diff` found changed between two versions of a chart
// (computed in Rust, crates/roadbook-cli/src/commands/viz/diff.rs, which documents how states
// and transitions are matched), and how the page shows it: the marks on the diagram, the
// summary and the before and after of what is selected. The page's IR is the merged chart: the
// after version with what was removed put back. Pure and DOM-free.

import type { Ir } from './ir.ts'
import { own } from './ir.ts'
import { ancestorIds, transitionId } from './model.ts'
import type { Annotations, Mark } from './render.ts'
import { esc } from './render.ts'

export type DiffStatus = 'added' | 'removed' | 'changed' | 'unchanged'

/** One difference: what it is about (`guard`, `target`, `var attempts`), its text before and after. */
export interface Change {
  what: string
  before: string | null
  after: string | null
}

export interface DiffElement {
  status: DiffStatus
  changes: Change[]
}

export interface Counts {
  added: number
  removed: number
  changed: number
}

export interface MachineDiff {
  status: DiffStatus
  /** Changes to the machine itself: signature, doc comment, variables, events. */
  changes: Change[]
  /** The states that are not unchanged, by id. */
  states: Record<string, DiffElement>
  /** The transitions that are not unchanged, by `stateId#index` (index in the merged state). */
  transitions: Record<string, DiffElement>
  counts: { states: Counts; transitions: Counts }
}

/** The `diff` member of a diff page's data. */
export interface ChartDiff {
  /** The revisions compared, as given: `main`, `HEAD`, `working tree`. */
  from: string
  to: string
  /** The chart's entry file: added, removed, changed, unchanged, or `error` when a side does not compile. */
  status: DiffStatus | 'error'
  /** The diagnostics of the sides where the chart does not compile. */
  errors: { side: 'before' | 'after'; label: string; text: string }[]
  /** Types, functions, actions and tasks that are not unchanged. */
  decls: (DiffElement & { name: string })[]
  machines: Record<string, MachineDiff>
}

/** The `diff` member of a diff index: the summary and every machine's status. */
export interface IndexDiff {
  from: string
  to: string
  summary: { machines: Counts; states: Counts; transitions: Counts }
  /** By index in the index's `machines`. */
  machines: { status: DiffStatus; states: Counts; transitions: Counts }[]
  /** By index in the index's `pages`. */
  charts: { entry: string; status: DiffStatus | 'error'; errors: ('before' | 'after')[] }[]
}

/** The sign each status is marked with, in text, so the diagram reads without colour. */
export const DIFF_SIGN: Record<DiffStatus, string> = { added: '+', removed: '−', changed: '~', unchanged: '' }

const WORD: Record<DiffStatus, string> = { added: 'added', removed: 'removed', changed: 'changed', unchanged: 'unchanged' }

export function emptyCounts(): Counts {
  return { added: 0, removed: 0, changed: 0 }
}

/** `+2 −1 ~3`, or `no change`. */
export function countsText(c: Counts): string {
  const parts = [c.added ? `+${c.added}` : '', c.removed ? `−${c.removed}` : '', c.changed ? `~${c.changed}` : ''].filter(Boolean)
  return parts.length ? parts.join(' ') : 'no change'
}

/** `2 added, 1 removed, 3 changed`, leaving out zeros; `none` if all are. */
export function countsWords(c: Counts): string {
  const parts = [c.added ? `${c.added} added` : '', c.removed ? `${c.removed} removed` : '', c.changed ? `${c.changed} changed` : ''].filter(Boolean)
  return parts.length ? parts.join(', ') : 'none'
}

/** The diff of one machine; unchanged if the diff does not name it. */
export function machineDiff(diff: ChartDiff, machine: string): MachineDiff {
  return (
    own(diff.machines, machine) ?? {
      status: 'unchanged',
      changes: [],
      states: {},
      transitions: {},
      counts: { states: emptyCounts(), transitions: emptyCounts() },
    }
  )
}

/** Whether something inside the state `id` (not itself) changed. */
export function changedInside(d: MachineDiff, id: string): boolean {
  const inside = (key: string) => key.startsWith(id + '.')
  return Object.keys(d.states).some(inside) || Object.keys(d.transitions).some((k) => inside(k.slice(0, k.lastIndexOf('#'))))
}

function mark(status: DiffStatus, what: string): Mark | null {
  if (status === 'unchanged') return null
  return { cls: `rb-diff-${status}`, badge: DIFF_SIGN[status], glyph: DIFF_SIGN[status], title: `${WORD[status]} (${what})` }
}

/**
 * The marks of diff mode on `machine`: each added, removed and changed state and transition,
 * and a collapsed state (in `collapsed`) with changes inside it marked `~`.
 */
export function diffAnnotations(diff: ChartDiff, machine: string, collapsed: ReadonlySet<string> = new Set()): Annotations {
  const d = machineDiff(diff, machine)
  return {
    state(id) {
      const own_ = own(d.states, id)
      if (own_) return mark(own_.status, own_.status === 'changed' ? own_.changes.map((c) => c.what).join(', ') : 'state')
      if (collapsed.has(id) && changedInside(d, id)) return { cls: 'rb-diff-inside', badge: '~', title: 'changes inside (collapsed)' }
      return null
    },
    transition(state, index) {
      const t = own(d.transitions, transitionId(state, index))
      if (!t) return null
      return mark(t.status, t.status === 'changed' ? t.changes.map((c) => c.what).join(', ') : 'transition')
    },
  }
}

/** The states to open expanded: every collapsed state with a change inside, and its ancestors. */
export function expandForDiff(diff: ChartDiff, machine: string, collapsed: Set<string>): void {
  const d = machineDiff(diff, machine)
  const ids = [...Object.keys(d.states), ...Object.keys(d.transitions).map((k) => k.slice(0, k.lastIndexOf('#')))]
  for (const id of ids) for (const a of [...ancestorIds(id), id]) collapsed.delete(a)
}

const changeRow = (c: Change) =>
  `<tr><th scope="row">${esc(c.what)}</th><td class="rb-before">${c.before === null ? '<span class="rb-note">(none)</span>' : `<pre class="rb-code">${esc(c.before)}</pre>`}</td><td class="rb-after">${c.after === null ? '<span class="rb-note">(none)</span>' : `<pre class="rb-code">${esc(c.after)}</pre>`}</td></tr>`

/** A table of changes, before and after. */
export function changesTable(diff: ChartDiff, changes: Change[]): string {
  if (!changes.length) return ''
  return `<table class="rb-changes"><thead><tr><th></th><th scope="col">before · ${esc(diff.from)}</th><th scope="col">after · ${esc(diff.to)}</th></tr></thead><tbody>${changes.map(changeRow).join('')}</tbody></table>`
}

/** `~ changed`, `+ added`: the status in words with its sign. */
export function statusChip(status: DiffStatus | 'error'): string {
  const sign = status === 'error' ? '!' : DIFF_SIGN[status]
  return `<span class="rb-chip rb-diff-chip rb-diff-chip-${status}">${sign ? `${esc(sign)} ` : ''}${status === 'error' ? 'does not compile' : WORD[status]}</span>`
}

/** What diff mode shows about a selected state or transition: its status, and before and after. */
export function diffDetailsHtml(diff: ChartDiff, machine: string, sel: { type: 'state'; id: string } | { type: 'transition'; state: string; index: number }): string {
  const d = machineDiff(diff, machine)
  const e = sel.type === 'state' ? own(d.states, sel.id) : own(d.transitions, transitionId(sel.state, sel.index))
  const what = sel.type === 'state' ? 'state' : 'transition'
  if (!e) return `<div class="rb-diff-details"><p>${statusChip('unchanged')} This ${what} is the same in ${esc(diff.from)} and ${esc(diff.to)}.</p></div>`
  const body =
    e.status === 'added'
      ? `<p class="rb-empty">Only in ${esc(diff.to)}: the details below are the new ${what}.</p>`
      : e.status === 'removed'
        ? `<p class="rb-empty">Only in ${esc(diff.from)}: the details below are the ${what} as it was, and its source line is in that version.</p>`
        : changesTable(diff, e.changes)
  return `<div class="rb-diff-details"><p>${statusChip(e.status)} This ${what} ${e.status === 'changed' ? 'changed' : `was ${WORD[e.status]}`}.</p>${body}</div>`
}

/** Every element of a machine's diff, in document order: kind, key and status, for the change list. */
export function diffEntries(ir: Ir, diff: ChartDiff, machine: string): { type: 'state' | 'transition'; state: string; index: number; status: DiffStatus; changes: Change[] }[] {
  const d = machineDiff(diff, machine)
  const out: { type: 'state' | 'transition'; state: string; index: number; status: DiffStatus; changes: Change[] }[] = []
  const m = own(ir.machines, machine)
  if (!m) return out
  const walk = (s: (typeof m)['root']) => {
    const e = own(d.states, s.id)
    if (e) out.push({ type: 'state', state: s.id, index: -1, status: e.status, changes: e.changes })
    s.transitions.forEach((_t, i) => {
      const t = own(d.transitions, transitionId(s.id, i))
      if (t) out.push({ type: 'transition', state: s.id, index: i, status: t.status, changes: t.changes })
    })
    s.states.forEach(walk)
  }
  walk(m.root)
  return out
}
