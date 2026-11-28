// Internal transitions (`-> .X`) and transitions with several targets: drawn as one arrow per
// target that select and highlight together, described in the details panel, and replayed from
// a trace whose `transition` record lists the targets.

import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'
import { coverage } from '../src/coverage.ts'
import type { Ir } from '../src/ir.ts'
import { buildElkGraph, layoutMachine } from '../src/layout.ts'
import { panelHtml } from '../src/panel.ts'
import { Replay } from '../src/replay.ts'
import { renderSvg } from '../src/render.ts'
import { newElk } from './charts.ts'

const here = path.dirname(fileURLToPath(import.meta.url))
const conformance = path.resolve(here, '../../../spec/conformance')
const load = (name: string) => JSON.parse(fs.readFileSync(path.join(conformance, 'charts', `${name}.ir.json`), 'utf8')) as Ir
const targets = load('targets')
const internal = load('internal')
const sides = load('internal-parallel')
const trace = (name: string) => fs.readFileSync(path.join(conformance, 'cases', name, 'trace.jsonl'), 'utf8')
const ctx = (ir: Ir, machine: string) => ({ ir, machine, collapsed: new Set<string>() })

describe('several targets', () => {
  it('give ELK one edge per target, the first with the label', () => {
    const graph = JSON.stringify(buildElkGraph(targets, 'Player'))
    expect(graph).toContain('"id":"Player.Idle#0","sources":["Player.Idle"],"targets":["Player.On.Playback.Paused"]')
    expect(graph).toContain('"id":"Player.Idle#0/to1","sources":["Player.Idle"],"targets":["Player.On.Volume.Muted"]}')
  })

  it('are drawn as arrows that all belong to the one transition', async () => {
    const layout = await layoutMachine(targets, 'Player', newElk())
    const svg = renderSvg(targets, 'Player', layout, { interactive: true })
    const arrows = svg.match(/<g class="rb-edge[^"]*" data-state="Player\.Idle" data-transition="0"/g) ?? []
    expect(arrows).toHaveLength(2)
    // Written in `On` with dots: internal, and marked so.
    expect(svg.match(/data-state="Player\.On" data-transition="0" data-internal="true"/g)).toHaveLength(2)
  })

  it('are listed in the details panel', () => {
    const html = panelHtml(ctx(targets, 'Player'), { type: 'transition', state: 'Player.Idle', index: 0 })
    expect(html).toContain('On.Playback.Paused, On.Volume.Muted')
    expect(html).toContain('Several targets')
    const muted = panelHtml(ctx(targets, 'Player'), { type: 'state', id: 'Player.On.Volume.Muted' })
    expect(muted).toContain('Idle: </span>')
  })

  it('are replayed and counted from a trace', () => {
    const replay = new Replay(targets, trace('targets-history'))
    expect(replay.problems).toEqual([])
    const at = replay.records.findIndex((r) => r.kind === 'transition' && r.from === 'Player.Idle' && r.index === 1)
    expect(replay.named(at)).toEqual({ states: ['Player.On.Playback.Last', 'Player.On.Volume.Loud'], transition: { state: 'Player.Idle', index: 1 } })
    const cov = coverage(targets, [{ name: 'targets-history', text: trace('targets-history') }])
    expect(cov.machines.Player!.states['Player.On.Playback.Last']).toBe(1)
  })
})

describe('internal transitions', () => {
  it('say in the details panel that their state is kept', () => {
    const html = panelHtml(ctx(internal, 'Kept'), { type: 'transition', state: 'Kept.Session', index: 2 })
    expect(html).toContain('Internal (written with a leading dot): <code>Session</code> is not left')
    const parallel = panelHtml(ctx(sides, 'Sides'), { type: 'transition', state: 'Sides.P', index: 0 })
    expect(parallel).toContain('in the regions the targets are in')
  })

  it('replay without problems', () => {
    for (const [ir, name] of [[internal, 'internal-keeps-source'], [sides, 'internal-region'], [targets, 'targets-internal']] as const) {
      expect(new Replay(ir, trace(name)).problems, name).toEqual([])
    }
  })
})
