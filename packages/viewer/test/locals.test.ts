// Data local to a state (IR 0.12): state parameters and state variables in the diagram, the
// details panel, simulate and replay, for the test chart locals.road.

import { describe, expect, it } from 'vitest'
import type { Machine, State } from '../src/ir.ts'
import { argsText, findState, stateRows, transitionLines } from '../src/model.ts'
import { panelHtml } from '../src/panel.ts'
import { renderSvg } from '../src/render.ts'
import { layoutMachine } from '../src/layout.ts'
import { Replay, localOf, recordText } from '../src/replay.ts'
import { Simulation } from '../src/sim.ts'
import { variablesHtml } from '../src/app/modes.ts'
import { chart, newElk } from './charts.ts'

const ir = chart('charts-locals')
const m: Machine = ir.machines.Confirm!
const state = (id: string): State => {
  const s = findState(m, id)
  if (!s) throw new Error(`no state ${id}`)
  return s
}
const ctx = { ir, machine: 'Confirm', collapsed: new Set<string>() }

/** Runs the chart in simulate: the token arrives, one wrong code, then `Again` re-enters Confirming. */
function simulate(): Simulation {
  const sim = new Simulation(ir, 'Confirm')
  expect(sim.start({ email: 'a@b' })).toEqual({ kind: 'ok' })
  expect(sim.complete(1, { done: { id: 't1', user: 'ada' } })).toEqual({ kind: 'ok' })
  sim.answer('matches', [{ id: 't1', user: 'ada' }, 'x'], { value: false })
  expect(sim.send({ _tag: 'Submit', code: 'x' })).toEqual({ kind: 'ok' })
  return sim
}

describe('model text', () => {
  it('lists a state’s parameters and variables in its box', () => {
    const rows = stateRows(state('Confirm.Confirming'))
    expect(rows.slice(0, 2)).toMatchObject([
      { kw: 'param', text: 'token: Token' },
      { kw: 'var', text: 'tries: int = 0' },
    ])
    expect(stateRows(state('Confirm.Accepted'))[0]).toMatchObject({ kw: 'param', text: 'user: string' })
  })

  it('writes the arguments a transition passes on its label, after the effects', () => {
    const done = state('Confirm.Requesting').transitions[0]!
    expect(done.args.map(argsText)).toEqual(['Confirming(result)'])
    expect(transitionLines(state('Confirm.Requesting'), 0).map((l) => l.text)).toEqual(['done', '-> Confirming(result)'])
    const again = state('Confirm.Confirming').transitions.findIndex((t) => t.trigger.kind === 'event' && t.trigger.event === 'Again')
    expect(transitionLines(state('Confirm.Confirming'), again).at(-1)).toMatchObject({ cls: 'effect', text: '-> Confirming(token)' })
  })

  it('draws the rows and the argument lines', async () => {
    const svg = renderSvg(ir, 'Confirm', await layoutMachine(ir, 'Confirm', newElk()))
    expect(svg).toContain('tries: int = 0')
    expect(svg).toContain('-&gt; Confirming(result)')
  })
})

describe('details panel', () => {
  it('shows a state’s parameters and variables', () => {
    const html = panelHtml(ctx, { type: 'state', id: 'Confirm.Confirming' })
    expect(html).toContain('<h3>Parameters</h3>')
    expect(html).toContain('<h3>Variables</h3>')
    expect(html).toContain('tries')
    expect(html).toContain('Wrong codes so far.')
  })

  it('shows the arguments of a transition', () => {
    const html = panelHtml(ctx, { type: 'transition', state: 'Confirm.Requesting', index: 0 })
    expect(html).toContain('<h3>Arguments</h3>')
    expect(html).toContain('Confirming(result)')
  })
})

describe('simulate', () => {
  it('keeps each active state’s locals, and shows them under it', () => {
    const sim = simulate()
    const snap = sim.current!.snapshot
    expect(snap.locals).toEqual({ 'Confirm.Confirming': { token: { id: 't1', user: 'ada' }, tries: 1 } })
    const html = variablesHtml(m, snap.input, snap.vars, false, snap.active, snap.locals)
    expect(html).toContain('data-m-state="Confirm.Confirming"')
    expect(html).toMatch(/<dt>tries<\/dt><dd>1/)
    // Entering Confirming again starts its variables again.
    expect(sim.send({ _tag: 'Again' })).toEqual({ kind: 'ok' })
    expect(sim.current!.snapshot.locals['Confirm.Confirming']).toEqual({ token: { id: 't1', user: 'ada' }, tries: 0 })
  })
})

describe('replay', () => {
  const lines = () => {
    const sim = simulate()
    sim.send({ _tag: 'Again' })
    return sim.records().map((r, t) => JSON.stringify({ ...r, t, run: 'r1' })).join('\n') + '\n'
  }

  it('follows the locals set on entry and drops them on exit', () => {
    const replay = new Replay(ir, lines())
    expect(replay.problems).toEqual([])
    const at = (pred: (r: { kind: string; [k: string]: unknown }) => boolean, from = 0) => replay.records.findIndex((r, i) => i >= from && pred(r))
    const firstTries = at((r) => r.kind === 'set' && r.var === 'Confirm.Confirming.tries')
    expect(replay.viewAt(firstTries, 'r1').locals).toEqual({ 'Confirm.Confirming': { token: { id: 't1', user: 'ada' }, tries: 0 } })
    const exited = at((r) => r.kind === 'exited' && r.state === 'Confirm.Confirming')
    expect(replay.viewAt(exited - 1, 'r1').locals['Confirm.Confirming']).toEqual({ token: { id: 't1', user: 'ada' }, tries: 1 })
    expect(replay.viewAt(exited, 'r1').locals).toEqual({})
    const last = replay.records.length - 1
    expect(replay.viewAt(last, 'r1').locals['Confirm.Confirming']!.tries).toBe(0)
  })

  it('names a local by its state, and checks it against the chart', () => {
    expect(localOf('Confirm.Confirming.tries')).toEqual({ state: 'Confirm.Confirming', name: 'tries' })
    expect(localOf('attempts')).toBeNull()
    expect(recordText({ kind: 'set', var: 'Confirm.Confirming.tries', value: 1 })).toBe('set Confirming.tries = 1')
    const bad = lines().replace('"Confirm.Confirming.tries"', '"Confirm.Confirming.nope"')
    expect(new Replay(ir, bad).problems.some((p) => p.endsWith('Confirm.Confirming has no parameter or variable nope'))).toBe(true)
  })
})
