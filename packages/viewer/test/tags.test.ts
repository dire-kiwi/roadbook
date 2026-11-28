// Tags and meta: tag pills under a state's title, tags and meta in the details panel, and the
// run's tags in simulate mode.

import { describe, expect, it } from 'vitest'
import { layoutMachine, stateBox } from '../src/layout.ts'
import { estimateWidth, findState } from '../src/model.ts'
import { panelHtml } from '../src/panel.ts'
import { renderSvg } from '../src/render.ts'
import { Simulation } from '../src/sim.ts'
import { conformanceChart, newElk } from './charts.ts'

const ir = conformanceChart('tags')
const m = ir.machines.Gallery!
const ctx = { ir, machine: 'Gallery', collapsed: new Set<string>() }

describe('tags and meta', () => {
  it('draw a state’s tags as pills under its title, in a band of their own', async () => {
    const svg = renderSvg(ir, 'Gallery', await layoutMachine(ir, 'Gallery', newElk()), { interactive: true })
    const full = svg.slice(svg.indexOf('data-state="Gallery.Viewing.Loading.Full"'))
    const pills = full.slice(0, full.indexOf('</g>'))
    expect([...pills.matchAll(/class="rb-tag-text"[^>]*>([^<]*)</g)].map((x) => x[1])).toEqual(['network', 'slow'])
    expect(stateBox(findState(m, 'Gallery.Closed')!, estimateWidth).tagsH).toBeGreaterThan(0)
    // A choice has no tags, and no band.
    expect(stateBox(findState(m, 'Gallery.Route')!, estimateWidth).tagsH).toBe(0)
  })

  it('list a state’s tags and meta in its details, and the machine body’s in the machine’s', () => {
    const viewing = panelHtml(ctx, { type: 'state', id: 'Gallery.Viewing' })
    expect(viewing).toContain('<h3>Tags</h3><p><span class="rb-chip">open</span></p>')
    expect(viewing).toContain('<dt>label</dt><dd class="rb-mono">&quot;Viewing&quot;</dd>')
    const machine = panelHtml(ctx, null)
    expect(machine).toContain('<span class="rb-chip">app</span>')
    expect(machine).toContain('<dt>ratio</dt><dd class="rb-mono">-0.5</dd>')
    expect(panelHtml(ctx, { type: 'state', id: 'Gallery.Route' })).not.toContain('<h3>Tags</h3>')
  })

  it('give simulate mode the run’s tags, none once it has ended', () => {
    const sim = new Simulation(ir, 'Gallery')
    expect(sim.tags()).toEqual([])
    sim.start({})
    expect(sim.tags()).toEqual(['app', 'idle'])
    sim.send({ _tag: 'Open' })
    expect(sim.tags()).toEqual(['app', 'open', 'busy', 'network', 'slow'])
    sim.send({ _tag: 'Close' })
    sim.send({ _tag: 'Quit' })
    expect(sim.tags()).toEqual([])
  })
})
