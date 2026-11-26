// History states (`history Name`, `deep history Name`): drawn as a small circle marked `H` or
// `H*` inside their parent, never a region or an initial state; in the details panel, search,
// replay, coverage and diff.

import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'
import { coverage, coverageAnnotations, stateCounts } from '../src/coverage.ts'
import { diffEntries, historyCountsText, machineDiff } from '../src/diff.ts'
import type { ChartDiff } from '../src/diff.ts'
import type { Ir } from '../src/ir.ts'
import { buildElkGraph, layoutMachine, machineView } from '../src/layout.ts'
import { METRICS, childStates, descendantCount, findState, historyText, isHistory } from '../src/model.ts'
import { panelHtml } from '../src/panel.ts'
import { Replay } from '../src/replay.ts'
import { renderLegend, renderSvg } from '../src/render.ts'
import { buildSearchIndex, search } from '../src/search.ts'
import { chart, newElk } from './charts.ts'

const here = path.dirname(fileURLToPath(import.meta.url))
const conformance = path.resolve(here, '../../../spec/conformance')
const ir = chart('charts-history')
const regions = JSON.parse(fs.readFileSync(path.join(conformance, 'charts/history-regions.ir.json'), 'utf8')) as Ir
const editor = JSON.parse(fs.readFileSync(path.join(conformance, 'charts/history.ir.json'), 'utf8')) as Ir
const trace = (name: string) => fs.readFileSync(path.join(conformance, 'cases', name, 'trace.jsonl'), 'utf8')
const ctx = (chartIr: Ir, machine: string) => ({ ir: chartIr, machine, collapsed: new Set<string>() })

/** The `<g>` of a state in an SVG, up to the end of its title. */
const stateG = (svg: string, id: string) => svg.match(new RegExp(`<g class="([^"]*)" data-state="${id.replace(/\./g, '\\.')}"[^>]*>([\\s\\S]*?)</g>`))

describe('history states in the model', () => {
  const m = ir.machines.Histories!
  it('are not children, regions or initial states', () => {
    const both = findState(m, 'Histories.Both')!
    expect(both.states.map((s) => s.name)).toEqual(['Again', 'Regions', 'Left', 'Right'])
    expect(childStates(both).map((s) => s.name)).toEqual(['Left', 'Right'])
    expect(isHistory(findState(m, 'Histories.Both.Again')!)).toBe(true)
    expect(isHistory(both)).toBe(false)
    // Work: A, B, B1, B2 (Last and All are not counted).
    expect(descendantCount(findState(m, 'Histories.Work')!)).toBe(4)
  })

  it('say what entering them does', () => {
    const work = findState(m, 'Histories.Work')!
    const both = findState(m, 'Histories.Both')!
    expect(historyText(findState(m, 'Histories.Work.Last')!, work)).toBe(
      'Returns to where Work was when it was last left: the state inside it that was active then, entered from its start. Before Work has been left, it enters its initial state, A.',
    )
    expect(historyText(findState(m, 'Histories.Work.All')!, work)).toContain('all the way down')
    expect(historyText(findState(m, 'Histories.Both.Regions')!, both)).toBe(
      'Returns to where Both was when it was last left: its regions (Left, Right), each from its start. Before Both has been left, it enters every region from its start.',
    )
  })
})

describe('drawing history states', () => {
  it('gives ELK a small square node inside the parent, and an edge to it', () => {
    const graph = buildElkGraph(ir, 'Histories')
    const work = graph.children!.find((c) => c.id === 'Histories.Work')!
    const last = work.children!.find((c) => c.id === 'Histories.Work.Last')!
    expect([last.width, last.height]).toEqual([METRICS.historyD, METRICS.historyD])
    expect(last.children).toBeUndefined()
    // The initial marker points at A, never at a history state.
    expect(graph.edges!.find((e) => e.id === 'Histories.Work/initial/edge')!.targets).toEqual(['Histories.Work.A'])
    expect(graph.edges!.find((e) => e.id === 'Histories.Paused#0')!.targets).toEqual(['Histories.Work.All'])
  })

  it('draws a circle marked H or H*, not a region, with the arrows to it ending on the circle', async () => {
    const layout = await layoutMachine(ir, 'Histories', newElk())
    const svg = renderSvg(ir, 'Histories', layout, { interactive: true })
    const all = stateG(svg, 'Histories.Work.All')!
    expect(all[1]).toBe('rb-state rb-kind-deepHistory rb-leaf rb-history')
    expect(all[2]).toContain('>H*</text>')
    expect(all[2]).toContain('rx="14"')
    expect(all[2]).toContain('Histories.Work.All (deep history)')
    expect(all[2]).toContain('Returns to where Work was when it was last left, all the way down')
    const last = stateG(svg, 'Histories.Work.Last')!
    expect(last[1]).toBe('rb-state rb-kind-history rb-leaf rb-history')
    expect(last[2]).toContain('>H</text>')
    expect(last[2]).toContain('Back to where `Work` was.')
    // In a parallel state: the regions are dashed, the history states are not regions.
    expect(stateG(svg, 'Histories.Both.Again')![1]).not.toContain('rb-region')
    expect(stateG(svg, 'Histories.Both.Regions')![1]).not.toContain('rb-region')
    expect(stateG(svg, 'Histories.Both.Left')![1]).toContain('rb-region')
    // No toggle on a history state, and its parent's is there.
    expect(svg).not.toContain('data-toggle="Histories.Work.Last"')
    expect(svg).toContain('data-toggle="Histories.Work"')
    // Every arrow to a history state ends on its circle.
    for (const [from, index, to] of [
      ['Histories.Paused', 0, 'Histories.Work.All'],
      ['Histories.Paused', 1, 'Histories.Work.Last'],
      ['Histories.Work.B.B2', 0, 'Histories.Work.Last'],
      ['Histories.Away', 0, 'Histories.Both.Again'],
      ['Histories.Away', 1, 'Histories.Both.Regions'],
      ['Histories.Away', 2, 'Histories.Both.Left.Inside'],
    ] as const) {
      const edge = svg.match(new RegExp(`<g class="rb-edge[^"]*" data-state="${from}" data-transition="${index}">.*?<path class="rb-edge-line" d="([^"]*)"`))
      expect(edge, `${from}#${index}`).not.toBeNull()
      const nums = edge![1]!.match(/-?[\d.]+/g)!.map(Number)
      const [x, y] = nums.slice(-2) as [number, number]
      const box = layout.nodes[to]!
      const d = Math.hypot(x - (box.x + box.width / 2), y - (box.y + box.height / 2))
      expect(Math.abs(d - METRICS.historyD / 2), `${from}#${index} ends ${d} from the centre of ${to}`).toBeLessThan(0.2)
    }
  })

  it('draws the arrows to a history state of a collapsed state to its box', async () => {
    const collapsed = new Set(['Histories.Work'])
    expect(machineView(ir.machines.Histories!, collapsed).visible('Histories.Work.Last')).toBe('Histories.Work')
    const layout = await layoutMachine(ir, 'Histories', newElk(), { collapsed })
    const svg = renderSvg(ir, 'Histories', layout, { collapsed, interactive: true })
    expect(svg).not.toContain('data-state="Histories.Work.Last"')
    expect(svg).toMatch(/<g class="rb-edge rb-k-event" data-state="Histories\.Paused" data-transition="1">/)
    expect(svg).toContain('4 states inside (collapsed)')
  })

  it('lays out and draws the conformance charts with history states', async () => {
    for (const [chartIr, machine, ids] of [
      [regions, 'Panels', ['Panels.Both.Deep', 'Panels.Both.Shallow']],
      [regions, 'Region', ['Region.P.A.H']],
      [editor, 'Editor', ['Editor.Work.Shallow', 'Editor.Work.Deep']],
      [editor, 'Steps', ['Steps.Flow.H']],
      [editor, 'Bounce', ['Bounce.P.H']],
    ] as const) {
      const layout = await layoutMachine(chartIr, machine, newElk())
      const svg = renderSvg(chartIr, machine, layout)
      for (const id of ids) expect(stateG(svg, id)?.[1], id).toContain('rb-history')
    }
  })

  it('has a line in the legend', () => {
    expect(renderLegend()).toContain('rb-history-mark')
    expect(renderLegend()).toContain('history state')
  })
})

describe('the details panel of a history state', () => {
  it('says what it does, whose history it is, and which transitions target it', () => {
    const html = panelHtml(ctx(ir, 'Histories'), { type: 'state', id: 'Histories.Work.Last' })
    expect(html).toContain('<span class="rb-chip">history state</span>')
    expect(html).toContain('Returns to where Work was when it was last left')
    expect(html).toContain('Back to where `Work` was.')
    expect(html).toContain('data-select-state="Histories.Work"')
    expect(html).toContain(`data-select-transition="${'["Histories.Paused",1]'.replace(/"/g, '&quot;')}"`)
    expect(html).toContain(`data-select-transition="${'["Histories.Work.B.B2",0]'.replace(/"/g, '&quot;')}"`)
    expect(panelHtml(ctx(ir, 'Histories'), { type: 'state', id: 'Histories.Both.Again' })).toContain('<span class="rb-chip">deep history state</span>')
  })

  it('lists a parent’s history states apart from its children and regions', () => {
    const html = panelHtml(ctx(ir, 'Histories'), { type: 'state', id: 'Histories.Both' })
    const regionsList = html.match(/<h3>Regions<\/h3>(.*?)<\/ul>/)![1]!
    expect(regionsList).toContain('>Left<')
    expect(regionsList).not.toContain('>Again<')
    const histories = html.match(/<h3>History states<\/h3>(.*?)<\/ul>/)![1]!
    expect(histories).toContain('>Again</button> <span class="rb-chip">deep history</span>')
    expect(histories).toContain('>Regions</button> <span class="rb-chip">history</span>')
  })

  it('says where a transition to a history state goes', () => {
    const html = panelHtml(ctx(ir, 'Histories'), { type: 'transition', state: 'Histories.Paused', index: 0 })
    expect(html).toContain('A deep history state. Returns to where Work was when it was last left, all the way down')
    const from = panelHtml(ctx(ir, 'Histories'), { type: 'state', id: 'Histories.Paused' })
    expect(from).toContain('Work.All (deep history)')
  })

  it('shows history states in the machine overview and in search', () => {
    expect(panelHtml(ctx(ir, 'Histories'), null)).toContain('>All</button> <span class="rb-chip">deep history</span>')
    const found = search(buildSearchIndex(ir), 'Last')
    expect(found[0]).toMatchObject({ kind: 'state', label: 'Histories.Work.Last (history)', target: { type: 'state', id: 'Histories.Work.Last' } })
  })
})

describe('replay and coverage of history states', () => {
  it('replays a transition to a history state, naming its circle, then the states it restores', () => {
    const replay = new Replay(regions, trace('history-parallel'))
    expect(replay.problems).toEqual([])
    const at = replay.records.findIndex((r) => r.kind === 'transition' && r.to === 'Panels.Both.Deep')
    expect(replay.named(at)).toEqual({ states: ['Panels.Both.Deep'], transition: { state: 'Panels.Out', index: 0 } })
    // The restored states are entered after it; the history state never is.
    const view = replay.viewAt(replay.records.length - 1, 'r1')
    expect(view.active).not.toContain('Panels.Both.Deep')
  })

  it('counts a history state as taken when a transition to it was, and one never taken as never', async () => {
    const cov = coverage(editor, [{ name: 'history-editor', text: trace('history-editor') }])
    const mc = cov.machines.Editor!
    expect(mc.states['Editor.Work.Shallow']).toBe(3)
    expect(mc.states['Editor.Work.Deep']).toBe(2)
    const steps = coverage(editor, [{ name: 'history-final', text: trace('history-final') }])
    expect(steps.machines.Steps!.states['Steps.Flow.H']).toBe(1)
    const counts = stateCounts(editor.machines.Editor!, mc)
    expect(counts.find((s) => s.state === 'Editor.Work.Shallow')).toEqual({ state: 'Editor.Work.Shallow', count: 3, history: true })
    // A run of Bounce that never goes back: its history state is never taken.
    const bounce = coverage(editor, [
      {
        name: 'bounce',
        text: `{"input":{},"ir":"${editor.machines.Bounce!.hash}","kind":"started","machine":"Bounce","run":"r1","t":0}\n{"kind":"entered","run":"r1","state":"Bounce","t":0}\n`,
      },
    ])
    const marks = coverageAnnotations(editor, bounce, 'Bounce')
    expect(marks.state!('Bounce.P.H')).toMatchObject({ cls: 'rb-cov-never', badge: 'never', title: 'no transition to it taken in 1 run' })
    expect(coverageAnnotations(editor, cov, 'Editor').state!('Editor.Work.Shallow')).toMatchObject({ badge: '×3', title: 'returned to through a transition 3 times in 1 run' })
    const layout = await layoutMachine(editor, 'Bounce', newElk())
    const svg = renderSvg(editor, 'Bounce', layout, { annotate: marks })
    expect(stateG(svg, 'Bounce.P.H')![1]).toContain('rb-cov-never')
    expect(stateG(svg, 'Bounce.P.H')![2]).toContain('>never</text>')
  })
})

describe('diff of history states', () => {
  const diff: ChartDiff = {
    from: 'main',
    to: 'HEAD',
    status: 'changed',
    errors: [],
    decls: [],
    machines: {
      Histories: {
        status: 'changed',
        changes: [],
        states: {
          'Histories.Work.Last': { status: 'added', changes: [] },
          'Histories.Work.All': { status: 'changed', changes: [{ what: 'kind', before: 'history', after: 'deep history' }] },
        },
        transitions: {},
        counts: { states: { added: 1, removed: 0, changed: 1 }, transitions: { added: 0, removed: 0, changed: 0 }, history: { added: 1, removed: 0, changed: 1 } },
      },
    },
  }

  it('lists and counts them as history states', () => {
    expect(diffEntries(ir, diff, 'Histories').map((e) => [e.state, e.status])).toEqual([
      ['Histories.Work.Last', 'added'],
      ['Histories.Work.All', 'changed'],
    ])
    expect(historyCountsText(machineDiff(diff, 'Histories'))).toBe(' (history +1 ~1)')
    expect(historyCountsText(machineDiff(diff, 'Other'))).toBe('')
  })
})
