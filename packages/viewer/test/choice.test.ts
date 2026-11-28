// Choice states (IR 0.14): drawn as a small diamond with the name beside it, the arrows into
// and out of it ending on the diamond, the branches labelled by their guards alone; in the
// details panel, for the conformance chart choice.road.

import { describe, expect, it } from 'vitest'
import type { Machine, State } from '../src/ir.ts'
import { buildElkGraph, layoutMachine } from '../src/layout.ts'
import { METRICS, findState, isChoice, transitionLines } from '../src/model.ts'
import { panelHtml } from '../src/panel.ts'
import { renderLegend, renderSvg } from '../src/render.ts'
import { conformanceChart, newElk } from './charts.ts'

const ir = conformanceChart('choice')
const m: Machine = ir.machines.Router!
const state = (id: string): State => {
  const s = findState(m, id)
  if (!s) throw new Error(`no state ${id}`)
  return s
}
const ctx = { ir, machine: 'Router', collapsed: new Set<string>() }

/** The `<g>` of a state in an SVG, up to its end. */
const stateG = (svg: string, id: string) => svg.match(new RegExp(`<g class="(rb-state [^"]*)" data-state="${id.replace(/\./g, '\\.')}"[^>]*>([\\s\\S]*?)</g>`))

/** The last point of the line of arrow `k` of transition `index` of `from`, and its first. */
const ends = (svg: string, from: string, index: number) => {
  const edge = svg.match(new RegExp(`<g class="rb-edge[^"]*" data-state="${from.replace(/\./g, '\\.')}" data-transition="${index}">.*?<path class="rb-edge-line" d="([^"]*)"`))
  expect(edge, `${from}#${index}`).not.toBeNull()
  const nums = edge![1]!.match(/-?[\d.]+/g)!.map(Number)
  return { first: nums.slice(0, 2) as [number, number], last: nums.slice(-2) as [number, number] }
}

describe('choice states', () => {
  it('are a node a diamond wide plus the name, a diamond high', () => {
    const graph = buildElkGraph(ir, 'Router')
    const start = graph.children!.find((c) => c.id === 'Router.Start')!
    expect(start.height).toBe(METRICS.choiceD)
    expect(start.width).toBeGreaterThan(METRICS.choiceD + METRICS.choiceGap)
    expect(start.children).toBeUndefined()
    expect(isChoice(state('Router.Desk.Route'))).toBe(true)
    expect(isChoice(state('Router.Desk'))).toBe(false)
  })

  it('label their branches with the guard alone', () => {
    const route = state('Router.Desk.Route')
    expect(transitionLines(route, 0)).toEqual([{ cls: 'trigger', text: '', guard: '[vip(amount)]' }])
    expect(transitionLines(route, 3)).toEqual([{ cls: 'trigger', text: '', guard: '[else]' }])
    expect(transitionLines(route, 2).map((l) => l.text)).toEqual(['', 'do flag("review")', '-> Review(amount)'])
  })

  it('are drawn as a diamond with the name beside it, the arrows ending and starting on it', async () => {
    const layout = await layoutMachine(ir, 'Router', newElk())
    const svg = renderSvg(ir, 'Router', layout, { interactive: true })
    const route = stateG(svg, 'Router.Desk.Route')!
    expect(route[1]).toBe('rb-state rb-kind-choice rb-leaf rb-choice')
    expect(route[2]).toContain('<polygon class="rb-box"')
    expect(route[2]).toContain('>Route</text>')
    expect(route[2]).toContain('Router.Desk.Route (choice)')
    expect(svg).not.toContain('data-toggle="Router.Desk.Route"')
    const onDiamond = ([x, y]: [number, number], id: string) => {
      const box = layout.nodes[id]!
      const r = METRICS.choiceD / 2
      const d = Math.abs(x - (box.x + r)) + Math.abs(y - (box.y + box.height / 2))
      expect(Math.abs(d - r), `${x},${y} is ${d} from the centre of ${id}`).toBeLessThan(0.2)
    }
    // Into a choice: from Idle and from the machine's transition into Auditing.
    onDiamond(ends(svg, 'Router.Idle', 2).last, 'Router.Auditing')
    // Out of one: every branch of Route and of Start.
    for (const i of [0, 1, 2, 3]) onDiamond(ends(svg, 'Router.Desk.Route', i).first, 'Router.Desk.Route')
    for (const i of [0, 1]) onDiamond(ends(svg, 'Router.Start', i).first, 'Router.Start')
    expect(renderLegend()).toContain('rb-choice')
  })

  it('say in the details what they do, and list their branches', () => {
    const html = panelHtml(ctx, { type: 'state', id: 'Router.Desk.Route' })
    expect(html).toContain('choice state, initial')
    expect(html).toContain('entered and left at once, by the first branch whose guard holds')
    expect(html).toContain('Branches')
  })
})
