// What simulate and replay share: the page they drive (Host), what they give it back
// (Controller), and the HTML of their common parts: variables, records, active states.

import { REDACTED } from '@dire-kiwi/roadbook-core'
import type { Ir, Machine } from '../ir.ts'
import type { Selection } from '../panel.ts'
import { findState } from '../model.ts'
import { jsonText, recordText, stateName } from '../replay.ts'
import { esc } from '../render.ts'
import type { Annotations } from '../render.ts'

export type Mode = 'explore' | 'simulate' | 'replay' | 'diff' | 'coverage'

/** What a mode lights up on the machine drawn. */
export interface Highlight {
  /** Active states: drawn with a solid ring around the box. */
  active: string[]
  /** Transitions taken: drawn with a heavy line and a heavy label border. */
  taken: { state: string; index: number }[]
  /** States the current record names: drawn with a dashed ring. */
  named: string[]
}

/** The chart page, as the modes see it. */
export interface Host {
  ir: Ir
  /** The machine drawn. */
  current(): string
  /** Draws `machine`. `trail`: the machines leading to it, for the trail above the diagram. */
  open(machine: string, trail?: { name: string; via: string }[]): Promise<void>
  /** Draws the highlights again, after the mode's state changed. */
  refresh(): void
  /** Switches mode. */
  setMode(mode: Mode): void
  /** Draws the diagram again, keeping the view, after the mode's marks changed. */
  redraw(): void
  /** Selects a state or transition of the machine drawn, expanding what hides it. */
  select(sel: Selection): void
}

export interface Controller {
  /** Writes the mode's panel into its element. */
  render(): void
  /** What to light up on `machine`, if it is the one the mode is about. */
  highlight(machine: string): Highlight | null
  /** One line about where the mode is, shown over the diagram. */
  status(): string
  forward(): void
  back(): void
  first?(): void
  last?(): void
  /** What the mode marks on `machine`'s states and transitions (diff, coverage), drawn into the SVG. */
  annotate?(machine: string, collapsed: ReadonlySet<string>): Annotations | null
  /** HTML put above the details of what is selected. */
  details?(machine: string, selection: Selection): string
}

export const pretty = (v: unknown) => JSON.stringify(v, null, 2) ?? 'null'

/**
 * A value as the panels show it. A value a trace redacted whole (`"<redacted>"`, for a
 * sensitive parameter or variable) is marked as such; one redacted inside shows the marker
 * where its sensitive fields were.
 */
const valueHtml = (v: unknown) => (v === REDACTED ? '<span class="rb-redacted" title="sensitive: the trace does not show it">redacted</span>' : esc(jsonText(v, 400)))

/**
 * The parameters and variables of `m`: their values, or why there is none. Then, for each state
 * of `active` that declares parameters or variables, its own (`locals`, by state id), which exist
 * only while it is active.
 */
export function variablesHtml(
  m: Machine,
  input: { [k: string]: unknown },
  vars: { [k: string]: unknown },
  untracedInit: boolean,
  active: readonly string[] = [],
  locals: { readonly [state: string]: { readonly [name: string]: unknown } } = {},
): string {
  const rows: string[] = []
  const note = (x: { sensitive: boolean }, what: string) => `<span class="rb-vnote"> ${x.sensitive ? `sensitive ${what}` : what}</span>`
  const has = (o: { readonly [k: string]: unknown } | undefined, k: string) => o !== undefined && Object.prototype.hasOwnProperty.call(o, k)
  for (const p of m.params) rows.push(`<dt>${esc(p.name)}</dt><dd>${valueHtml(input[p.name])}${note(p, 'param')}</dd>`)
  for (const v of m.vars) {
    const value = has(vars, v.name)
      ? `${valueHtml(vars[v.name])}${v.sensitive ? note(v, 'var') : ''}`
      : untracedInit && v.init
        ? `<span class="rb-vnote">= ${esc(v.init.text)} (initial value; not in the trace)</span>`
        : `<span class="rb-vnote">no value yet</span>`
    rows.push(`<dt>${esc(v.name)}</dt><dd>${value}</dd>`)
  }
  let own = rows.length ? `<dl class="rb-kv rb-vars">${rows.join('')}</dl>` : ''
  // A state's locals are set in the trace when it is entered, so there is no untraced initial value.
  for (const id of active) {
    const s = findState(m, id)
    if (!s || (!s.params.length && !s.vars.length)) continue
    const mine = locals[id]
    const lrows: string[] = []
    for (const p of s.params)
      lrows.push(`<dt>${esc(p.name)}</dt><dd>${has(mine, p.name) ? valueHtml(mine![p.name]) : '<span class="rb-vnote">no value yet</span>'}${note(p, 'param')}</dd>`)
    for (const v of s.vars) {
      const value = has(mine, v.name) ? `${valueHtml(mine![v.name])}${v.sensitive ? note(v, 'var') : ''}` : `<span class="rb-vnote">no value yet</span>`
      lrows.push(`<dt>${esc(v.name)}</dt><dd>${value}</dd>`)
    }
    own += `<p class="rb-vstate">of <button class="rb-link rb-mono" type="button" data-m-state="${esc(id)}">${esc(stateName(id) || id)}</button></p><dl class="rb-kv rb-vars">${lrows.join('')}</dl>`
  }
  return own || '<p class="rb-empty">No parameters or variables.</p>'
}

/** The active states as a path, innermost last. */
export function activeHtml(active: string[]): string {
  if (!active.length) return '<p class="rb-empty">No active state.</p>'
  return `<p class="rb-active-path">${active.map((s) => `<button class="rb-link rb-mono" type="button" data-m-state="${esc(s)}">${esc(stateName(s) || s)}</button>`).join(' <span class="rb-sep-text">›</span> ')}</p>`
}

/** A list of records. `current`: the index (in `records`) to mark; `mark`: records to set apart. */
export function recordsHtml(
  records: { kind: string; t: number; run: string; [k: string]: unknown }[],
  opts: { current?: number; mark?: (i: number) => boolean; jump?: boolean; showRun?: boolean } = {},
): string {
  if (!records.length) return '<p class="rb-empty">No records yet.</p>'
  return `<ol class="rb-records">${records
    .map((r, i) => {
      const cls = [`rb-rk-${r.kind.replace('.', '-')}`, i === opts.current ? 'rb-current' : '', opts.mark?.(i) ? 'rb-recent' : ''].filter(Boolean).join(' ')
      const body = `<span class="rb-rt">${esc(String(r.t))}</span>${opts.showRun ? `<span class="rb-rrun">${esc(r.run)}</span>` : ''}<span class="rb-rtext">${esc(recordText(r))}</span>`
      const cur = i === opts.current ? ' aria-current="step"' : ''
      return `<li class="${cls}"${cur}>${opts.jump ? `<button class="rb-rec" type="button" data-m-record="${i}">${body}</button>` : `<span class="rb-rec">${body}</span>`}</li>`
    })
    .join('')}</ol>`
}

/** Scrolls a record into view inside its list only, leaving the panel where the reader has it. */
export function scrollWithin(li: HTMLElement) {
  const list = li.closest<HTMLElement>('.rb-records')
  if (!list) return
  const top = li.offsetTop - list.offsetTop
  if (top < list.scrollTop) list.scrollTop = top
  else if (top + li.offsetHeight > list.scrollTop + list.clientHeight) list.scrollTop = top + li.offsetHeight - list.clientHeight
}
