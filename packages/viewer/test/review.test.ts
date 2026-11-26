// Review tooling: coverage over the conformance traces, and the marks of diff and coverage on
// the drawn diagram. The diff itself is computed by the CLI (crates/roadbook-cli, viz/diff.rs)
// and tested there.

import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'
import { coverage, coverageAnnotations, shade, stateCounts, transitionCounts } from '../src/coverage.ts'
import { changedInside, diffAnnotations, diffDetailsHtml, diffEntries, expandForDiff } from '../src/diff.ts'
import type { ChartDiff } from '../src/diff.ts'
import type { Ir } from '../src/ir.ts'
import { layoutMachine } from '../src/layout.ts'
import { allStates, transitionId } from '../src/model.ts'
import { parseTrace } from '../src/replay.ts'
import { renderSvg } from '../src/render.ts'
import { newElk } from './charts.ts'

const here = path.dirname(fileURLToPath(import.meta.url))
const conformance = path.resolve(here, '../../../spec/conformance')
const shop = JSON.parse(fs.readFileSync(path.join(conformance, 'charts/shop/place_order.ir.json'), 'utf8')) as Ir
const traces = fs
  .readdirSync(path.join(conformance, 'cases'))
  .sort()
  .map((c) => path.join(conformance, 'cases', c, 'trace.jsonl'))
  .filter((f) => fs.existsSync(f))
  .map((f) => ({ name: path.basename(path.dirname(f)), text: fs.readFileSync(f, 'utf8') }))

/** The transitions the records of runs of `machine` take, counted straight from the traces. */
function taken(machine: string): Map<string, number> {
  const out = new Map<string, number>()
  for (const t of traces) {
    const { records } = parseTrace(t.text)
    const runs = new Set(records.filter((r) => (r.kind === 'started' || r.kind === 'restored') && r.machine === machine && r.ir === shop.machines[machine]!.hash).map((r) => r.run))
    for (const r of records) if (r.kind === 'transition' && runs.has(r.run)) out.set(transitionId(r.from as string, r.index as number), (out.get(transitionId(r.from as string, r.index as number)) ?? 0) + 1)
  }
  return out
}

describe('coverage', () => {
  const cov = coverage(shop, traces)

  it('counts every transition of Checkout that the conformance cases take', () => {
    const mc = cov.machines.Checkout!
    // Four cases run Checkout alone, four run it as PlaceOrder's child.
    expect(mc.runs).toBe(8)
    const expected = taken('Checkout')
    expect(expected.size).toBeGreaterThan(0)
    for (const [key, n] of expected) expect(mc.transitions[key], key).toBe(n)
    const counts = transitionCounts(shop.machines.Checkout!, mc)
    expect(counts.map((t) => [t.state, t.index, t.count])).toEqual([
      ['Checkout.Charging', 0, expected.get('Checkout.Charging#0')],
      ['Checkout.Charging', 1, expected.get('Checkout.Charging#1')],
      ['Checkout.Charging', 2, expected.get('Checkout.Charging#2')],
      ['Checkout.Charging', 3, expected.get('Checkout.Charging#3')],
      ['Checkout.Backoff', 0, expected.get('Checkout.Backoff#0')],
    ])
    // Every one is taken by some case: charged, declined, retried, and given up.
    expect(counts.every((t) => t.count > 0)).toBe(true)
    expect(stateCounts(shop.machines.Checkout!, mc).every((s) => s.count > 0)).toBe(true)
  })

  it('flags the transitions of PlaceOrder no case takes', () => {
    const mc = cov.machines.PlaceOrder!
    expect(mc.runs).toBe(5)
    const counts = transitionCounts(shop.machines.PlaceOrder!, mc)
    for (const [key, n] of taken('PlaceOrder')) expect(mc.transitions[key], key).toBe(n)
    const never = counts.filter((t) => t.count === 0).map((t) => transitionId(t.state, t.index))
    const ids = (s: string) => allStates(shop.machines.PlaceOrder!).find((x) => x.id === s)!
    // Each one never taken is a `fail` no case makes happen.
    expect(never.length).toBeGreaterThan(0)
    for (const key of never) {
      const [state, index] = key.split('#') as [string, string]
      expect(ids(state).transitions[Number(index)]!.trigger.kind, key).toBe('invoke.fail')
    }
    const marks = coverageAnnotations(shop, cov, 'PlaceOrder')
    for (const key of never) {
      const [state, index] = key.split('#') as [string, string]
      expect(marks.transition!(state, Number(index))).toMatchObject({ cls: 'rb-cov-never', badge: 'never' })
    }
    const takenOnce = counts.find((t) => t.count > 0)!
    expect(marks.transition!(takenOnce.state, takenOnce.index)).toMatchObject({ badge: `×${takenOnce.count}` })
  })

  it('leaves out runs recorded from another version of the chart, and counts them', () => {
    const one = traces.find((t) => t.name === 'checkout-retry')!
    const stale = { name: 'old.jsonl', text: one.text.replaceAll(shop.machines.Checkout!.hash, 'sha256:' + '0'.repeat(64)) }
    const records = parseTrace(one.text).records.length
    const c = coverage(shop, [one, stale])
    expect(c.machines.Checkout!.runs).toBe(1)
    expect(c.stale).toEqual({ runs: 1, records, machines: ['Checkout'] })
    expect(c.records).toBe(records)
    // Runs of machines the chart does not have are not counted, nor warned about as stale.
    const other = traces.find((t) => t.name === 'gate-guards')!
    const d = coverage(shop, [other])
    expect(d.runs).toBe(0)
    expect(d.other.runs).toBeGreaterThan(0)
    expect(d.stale.runs).toBe(0)
  })

  it('shades on a logarithmic scale and says never in text', () => {
    expect(shade(0, 100)).toBe('never')
    expect(shade(1, 100)).toBe(1)
    expect(shade(100, 100)).toBe(4)
    expect(shade(5, 5)).toBe(4)
    expect(shade(1, 1)).toBe(4)
  })

  it('draws counts and never on the diagram', async () => {
    const layout = await layoutMachine(shop, 'PlaceOrder', newElk())
    const svg = renderSvg(shop, 'PlaceOrder', layout, { annotate: coverageAnnotations(shop, cov, 'PlaceOrder') })
    expect(svg).toContain('rb-cov-never')
    expect(svg).toMatch(/<g class="rb-badge"[^>]*><rect[^>]*\/><text[^>]*>never<\/text><\/g>/)
    expect(svg).toMatch(/<text[^>]*>×\d+<\/text>/)
    expect(svg).toContain('never taken in 5 runs')
  })
})

describe('diff marks', () => {
  const checkout = shop.machines.Checkout!
  const diff: ChartDiff = {
    from: 'main',
    to: 'HEAD',
    status: 'changed',
    errors: [],
    decls: [],
    machines: {
      Checkout: {
        status: 'changed',
        changes: [],
        states: { 'Checkout.Backoff': { status: 'added', changes: [] }, 'Checkout.GaveUp': { status: 'removed', changes: [] } },
        transitions: {
          'Checkout.Charging#2': { status: 'changed', changes: [{ what: 'guard', before: '[attempts < 3]', after: '[attempts < 5]' }] },
          'Checkout.Backoff#0': { status: 'added', changes: [] },
        },
        counts: { states: { added: 1, removed: 1, changed: 0 }, transitions: { added: 1, removed: 0, changed: 1 } },
      },
    },
  }

  it('marks added, removed and changed in text as well as by class', async () => {
    const layout = await layoutMachine(shop, 'Checkout', newElk())
    const svg = renderSvg(shop, 'Checkout', layout, { annotate: diffAnnotations(diff, 'Checkout') })
    const g = (id: string) => svg.match(new RegExp(`<g class="([^"]*)" data-state="${id.replace('.', '\\.')}"`))![1]
    expect(g('Checkout.Backoff')).toContain('rb-diff-added')
    expect(g('Checkout.GaveUp')).toContain('rb-diff-removed')
    expect(g('Checkout.Charging')).not.toContain('rb-diff')
    for (const sign of ['+', '−', '~']) expect(svg).toContain(`>${sign}</text></g>`)
    expect(svg).toMatch(/<g class="rb-label rb-k-fail rb-diff-changed" data-state="Checkout\.Charging" data-transition="2">/)
    expect(svg).toContain('changed (guard)')
  })

  it('lists the changes in document order and shows before and after', () => {
    expect(diffEntries(shop, diff, 'Checkout').map((e) => [e.type, e.state, e.index, e.status])).toEqual([
      ['transition', 'Checkout.Charging', 2, 'changed'],
      ['state', 'Checkout.Backoff', -1, 'added'],
      ['transition', 'Checkout.Backoff', 0, 'added'],
      ['state', 'Checkout.GaveUp', -1, 'removed'],
    ])
    const html = diffDetailsHtml(diff, 'Checkout', { type: 'transition', state: 'Checkout.Charging', index: 2 })
    expect(html).toContain('[attempts &lt; 3]')
    expect(html).toContain('[attempts &lt; 5]')
    expect(html).toContain('before · main')
    expect(diffDetailsHtml(diff, 'Checkout', { type: 'state', id: 'Checkout.Charging' })).toContain('the same in main and HEAD')
    expect(diffDetailsHtml(diff, 'Checkout', { type: 'state', id: 'Checkout.GaveUp' })).toContain('Only in main')
    expect(checkout.root.states.length).toBeGreaterThan(0)
  })

  it('marks a collapsed state with changes inside, and opens it', () => {
    const place: ChartDiff = { ...diff, machines: { PlaceOrder: { ...diff.machines.Checkout!, states: { 'PlaceOrder.Processing.Paying': { status: 'changed', changes: [] } }, transitions: {} } } }
    const marks = diffAnnotations(place, 'PlaceOrder', new Set(['PlaceOrder.Processing']))
    expect(changedInside(place.machines.PlaceOrder!, 'PlaceOrder.Processing')).toBe(true)
    expect(marks.state!('PlaceOrder.Processing')).toMatchObject({ cls: 'rb-diff-inside', badge: '~' })
    expect(marks.state!('PlaceOrder.Authenticating')).toBeNull()
    const collapsed = new Set(['PlaceOrder.Processing', 'PlaceOrder.Other'])
    expandForDiff(place, 'PlaceOrder', collapsed)
    expect([...collapsed]).toEqual(['PlaceOrder.Other'])
  })
})
