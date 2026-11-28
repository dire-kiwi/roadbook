// Several invokes in one state, each named with `as` (IR 0.12): their rows, the labels of their
// outcomes, the details panel, simulate and replay, for test/charts/invokes.road and the
// conformance chart invokes.road.

import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'
import type { Machine, State } from '../src/ir.ts'
import { findState, invokeNamed, stateRows, transitionLines, triggerText } from '../src/model.ts'
import { panelHtml } from '../src/panel.ts'
import { Replay } from '../src/replay.ts'
import { Simulation } from '../src/sim.ts'
import { chart, conformanceChart } from './charts.ts'

const here = path.dirname(fileURLToPath(import.meta.url))
const conformance = path.resolve(here, '../../../spec/conformance')
const ir = chart('charts-invokes')
const cases = conformanceChart('invokes')
const trace = (name: string) => fs.readFileSync(path.join(conformance, 'cases', name, 'trace.jsonl'), 'utf8')
const state = (m: Machine, id: string): State => {
  const s = findState(m, id)
  if (!s) throw new Error(`no state ${id}`)
  return s
}
const watching = state(ir.machines.Watch!, 'Watch.Watching')

describe('model text', () => {
  it('shows a row for each invoke, in the order written', () => {
    const rows = stateRows(watching).filter((r) => r.kw === 'invoke')
    expect(rows.map((r) => r.text)).toEqual(['heartbeat(server) as beat', 'syncFiles(folder) as sync', 'machine Helper() as helper'])
    expect(rows[2]).toMatchObject({ machine: 'Helper', alias: 'helper' })
  })

  it('names the invoke an outcome belongs to when the state has several', () => {
    expect(watching.transitions.map((t) => triggerText(t.trigger, watching))).toEqual([
      'beat: fail Lost',
      'sync: done',
      'sync: fail *',
      'helper.Poked',
      'Stop',
    ])
    expect(transitionLines(watching, 1)[0]).toMatchObject({ cls: 'trigger', text: 'sync: done' })
    // Without the state, or with one invoke, it reads as before.
    expect(triggerText(watching.transitions[1]!.trigger)).toBe('done')
    expect(invokeNamed(watching, 'sync')?.src).toBe('syncFiles')
    expect(invokeNamed(watching, undefined)?.src).toBe('heartbeat')
  })
})

describe('details panel', () => {
  it('has a section for each invoke, and says which have no outcomes', () => {
    const html = panelHtml({ ir, machine: 'Watch', collapsed: new Set<string>() }, { type: 'state', id: 'Watch.Watching' })
    expect(html.match(/Invokes (task|machine)/g)).toEqual(['Invokes task', 'Invokes task', 'Invokes machine'])
    expect(html).toContain('<code>beat</code> names this invoke among the 3 of Watching')
    // Only the helper's invoke has no outcomes.
    expect(html.match(/No outcomes are written/g)?.length).toBe(1)
  })

  it('describes an outcome by its own invoke', () => {
    const html = panelHtml({ ir, machine: 'Watch', collapsed: new Set<string>() }, { type: 'transition', state: 'Watch.Watching', index: 0 })
    expect(html).toContain('heartbeat failed with Lost')
  })
})

describe('simulate', () => {
  it('lists each running invoke, and ends one without the others', () => {
    const sim = new Simulation(ir, 'Watch')
    expect(sim.start({ server: 's', folder: 'f' })).toEqual({ kind: 'ok' })
    const invokes = sim.choices().invokes
    expect(invokes.map((i) => [i.src, i.as, i.open])).toEqual([
      ['heartbeat', 'beat', false],
      ['syncFiles', 'sync', false],
      ['Helper', 'helper', true],
    ])
    expect(sim.current!.snapshot.invokes.map((i) => i.as)).toEqual(['beat', 'sync', 'helper'])
    // `sync` fails: its `fail *` leaves, which cancels the other two.
    expect(sim.complete(invokes[1]!.id, { fail: { _tag: 'Busy' } })).toEqual({ kind: 'ok' })
    // The run has ended in Offline.
    expect(sim.current!.snapshot.status).toBe('done')
    expect(sim.current!.records.filter((r) => r.kind === 'invoke.cancelled').map((r) => (r as { invoke: number }).invoke)).toEqual([
      invokes[0]!.id,
      invokes[2]!.id,
    ])
  })
})

describe('replay', () => {
  it('reads the conformance traces of several invokes without a problem', () => {
    for (const name of fs.readdirSync(path.join(conformance, 'cases')).filter((n) => n.startsWith('invokes-'))) {
      expect(new Replay(cases, trace(name)).problems, name).toEqual([])
    }
  })
})
