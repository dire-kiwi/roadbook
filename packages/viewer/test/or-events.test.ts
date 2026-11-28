// `on A | B` (IR 0.14): the IR holds one copy of each branch per event, sharing its group and
// span, and the viewer draws them as one edge, for the conformance chart or-events.road.

import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'
import type { Machine, State } from '../src/ir.ts'
import { copiesOf, drawnIndex, findState, guardText, isCopy, messageSites, stateRows, transitionLines, transitionSource, transitionTrigger } from '../src/model.ts'
import { coverage, transitionCounts } from '../src/coverage.ts'
import { layoutMachine } from '../src/layout.ts'
import { panelHtml } from '../src/panel.ts'
import { renderSvg } from '../src/render.ts'
import { buildSearchIndex, search } from '../src/search.ts'
import { conformanceChart, newElk } from './charts.ts'

const ir = conformanceChart('or-events')
const m: Machine = ir.machines.Link!
const state = (id: string): State => {
  const s = findState(m, id)
  if (!s) throw new Error(`no state ${id}`)
  return s
}
const ctx = { ir, machine: 'Link', collapsed: new Set<string>() }
const here = path.dirname(fileURLToPath(import.meta.url))
const trace = fs.readFileSync(path.join(here, '../../../spec/conformance/cases/or-events/trace.jsonl'), 'utf8')

describe('the copies of an on A | B', () => {
  it('are found by their group and span', () => {
    const idle = state('Link.Idle')
    expect(copiesOf(idle, 0)).toEqual([0, 1])
    expect(copiesOf(idle, 1)).toEqual([])
    expect(isCopy(idle, 1)).toBe(true)
    expect(drawnIndex(idle, 1)).toBe(0)
    // A block: each branch with its copies, the events' copies one after the other.
    const busy = state('Link.Connecting')
    expect([0, 1, 2, 3, 4].map((i) => drawnIndex(busy, i))).toEqual([0, 1, 0, 1, 4])
    expect(copiesOf(busy, 1)).toEqual([1, 3])
  })

  it('are labelled with every event, and only a block’s last branch is an else', () => {
    const busy = state('Link.Connecting')
    expect(transitionTrigger(busy, 0)).toBe('Drop | Retry')
    expect(transitionTrigger(busy, 2)).toBe('Drop | Retry')
    expect(transitionSource(state('Link'), 1)).toBe('on Reset | Drop')
    expect(transitionLines(state('Link.Idle'), 0).slice(0, 2)).toMatchObject([
      { text: 'Retry | Reconnect', guard: '' },
      { text: '', guard: '[attempts < 2]' },
    ])
    expect([0, 1, 2, 3, 4].map((i) => guardText(busy, i))).toEqual(['[event.reason == ""]', '[else]', '[event.reason == ""]', '[else]', ''])
    // The copy of an unguarded single branch is no else.
    expect(guardText(state('Link'), 1)).toBe('')
  })

  it('are one row of a state box when they have no target', () => {
    const idle = state('Link.Idle')
    const stays: State = { ...idle, transitions: idle.transitions.map((t) => ({ ...t, targets: [] })) }
    const rows = stateRows(stays).filter((r) => r.transition !== undefined)
    expect(rows.filter((r) => r.kw).map((r) => [r.kw, r.text])).toEqual([['on Retry | Reconnect', '[attempts < 2]']])
    expect(rows.every((r) => r.transition === 0)).toBe(true)
  })

  it('are one edge in the layout and the drawing', async () => {
    const layout = await layoutMachine(ir, 'Link', newElk())
    const edges = Object.keys(layout.edges).filter((k) => k.includes('#'))
    expect(edges.filter((k) => /^Link\.Idle#/.test(k)).map((k) => k.split('/')[0])).toEqual(['Link.Idle#0'])
    expect(edges.some((k) => k.startsWith('Link.Connecting#2') || k.startsWith('Link.Connecting#3') || k.startsWith('Link#1'))).toBe(false)
    const svg = renderSvg(ir, 'Link', layout)
    expect(svg).toContain('Retry | Reconnect')
    expect(svg).not.toContain('data-state="Link.Idle" data-transition="1"')
  })

  it('count together in coverage', () => {
    const cov = coverage(ir, [{ name: 'or-events', text: trace }])
    expect(cov.runs).toBe(1)
    const counts = transitionCounts(m, cov.machines.Link)
    const at = (s: string, i: number) => counts.find((c) => c.state === s && c.index === i)?.count
    expect(counts.filter((c) => c.state === 'Link.Idle')).toHaveLength(1)
    expect(at('Link.Idle', 0)).toBe(4)
    expect(at('Link.Connecting', 0)).toBe(1)
    expect(at('Link.Connecting', 1)).toBe(2)
    expect(at('Link.Connecting', 4)).toBe(3)
    expect(at('Link', 0)).toBe(2)
    expect(counts).toHaveLength(5)
  })

  it('are one transition in the details panel, found by each event', () => {
    const html = panelHtml(ctx, { type: 'transition', state: 'Link.Connecting', index: 1 })
    expect(html).toContain('on Drop | Retry')
    expect(html).toContain('Branch 2 of 2')
    expect(html).toContain('Any of Drop, Retry arrives')
    // Its own transitions, the machine's, and the ones into it: each copy of an `on A | B` once.
    const busy = panelHtml(ctx, { type: 'state', id: 'Link.Connecting' })
    const links = busy.match(/data-select-transition="[^"]*"/g)?.map((l) => JSON.parse(l.slice(24, -1).replaceAll('&quot;', '"')))
    expect(links).toEqual([['Link.Connecting', 0], ['Link.Connecting', 1], ['Link.Connecting', 4], ['Link', 0], ['Link.Idle', 0], ['Link.Connecting', 1]])
    // Searching for the second event finds the edge drawn for both.
    const found = search(buildSearchIndex(ir), 'Reconnect').filter((e) => e.target?.type === 'transition')
    expect(found.map((e) => e.target)).toContainEqual({ type: 'transition', state: 'Link.Idle', index: 0 })
    expect(found.map((e) => e.target)).not.toContainEqual({ type: 'transition', state: 'Link.Idle', index: 1 })
    // A copy's effects are written once.
    expect(messageSites('Link', m)).toEqual([])
  })
})
