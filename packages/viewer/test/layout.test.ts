import path from 'node:path'
import { describe, expect, it } from 'vitest'
import type { ElkNode } from 'elkjs/lib/elk-api.js'
import { ATTEMPTS, FINALS_PER_ROW, buildElkGraph, defaultCollapsed, layoutMachine, machineView } from '../src/layout.ts'
import type { Box, ElkLike, Layout } from '../src/layout.ts'
import { renderSvg } from '../src/render.ts'
import { descendantCount, transitionId } from '../src/model.ts'
import type { Ir } from '../src/ir.ts'
import { chart, chartFiles, loadIr, newElk } from './charts.ts'

const overlap = (a: Box, b: Box, slack = 0.5) =>
  a.x + slack < b.x + b.width && b.x + slack < a.x + a.width && a.y + slack < b.y + b.height && b.y + slack < a.y + a.height

/** Pairs of drawn states whose boxes overlap, where neither contains the other. */
function overlaps(ir: Ir, machine: string, layout: Layout, collapsed: ReadonlySet<string>): string[] {
  const drawn = machineView(ir.machines[machine]!, collapsed).states.map((d) => d.state.id)
  const out: string[] = []
  for (let i = 0; i < drawn.length; i++) {
    for (let j = i + 1; j < drawn.length; j++) {
      const a = drawn[i]!
      const b = drawn[j]!
      if (b.startsWith(a + '.') || a.startsWith(b + '.')) continue
      const ba = layout.nodes[a]
      const bb = layout.nodes[b]
      if (!ba || !bb) {
        out.push(`${!ba ? a : b} not laid out`)
        continue
      }
      if (overlap(ba, bb)) out.push(`${a} overlaps ${b}`)
    }
  }
  // Labels: never on a state box that is not a container, nor on another label.
  const leaves = machineView(ir.machines[machine]!, collapsed).states.filter((d) => !d.state.states.length || d.collapsed)
  const labels = Object.entries(layout.edges).flatMap(([id, e]) => (e.label ? [[id, e.label] as const] : []))
  labels.forEach(([id, l], i) => {
    for (const d of leaves) if (layout.nodes[d.state.id] && overlap(l, layout.nodes[d.state.id]!)) out.push(`label ${id} overlaps ${d.state.id}`)
    for (const [other, m] of labels.slice(i + 1)) if (overlap(l, m)) out.push(`label ${id} overlaps label ${other}`)
  })
  return out
}

const cases = chartFiles().flatMap((file) => {
  const ir = loadIr(file)
  return Object.keys(ir.machines).map((m) => ({ name: `${path.basename(file, '.ir.json')} ${m}`, ir, machine: m }))
})

describe.each(cases)('$name', ({ ir, machine }) => {
  const elk = newElk()
  const m = ir.machines[machine]!
  const variants: [string, Set<string>][] = [
    ['as opened', defaultCollapsed(m)],
    ['expanded', new Set()],
  ]
  it.each(variants)('lays out and renders %s without overlapping states or labels', async (_label, collapsed) => {
    const layout = await layoutMachine(ir, machine, elk, { collapsed })
    expect(layout.attempt).toBe(0)
    expect(overlaps(ir, machine, layout, collapsed)).toEqual([])
    expect(layout.width).toBeGreaterThan(0)
    const svg = renderSvg(ir, machine, layout, { collapsed, interactive: true })
    expect(svg).not.toMatch(/NaN|undefined/)
    for (const d of machineView(m, collapsed).states) expect(svg).toContain(`data-state="${d.state.id}"`)
  })
})

describe('the fixes for the spike', () => {
  const elk = newElk()

  it('stack final states in rows: PlaceOrder is no wider than it is tall', async () => {
    const ir = chart('shop-place_order')
    const l = await layoutMachine(ir, 'PlaceOrder', elk)
    expect(l.width).toBeLessThan(1300)
    expect(l.width).toBeLessThanOrEqual(l.height * 1.1)
    const finals = ir.machines.PlaceOrder!.root.states.filter((s) => s.kind === 'final')
    const rows = new Set(finals.map((s) => Math.round(l.nodes[s.id]!.y)))
    expect(rows.size).toBeGreaterThanOrEqual(Math.ceil(finals.length / FINALS_PER_ROW))
    // The invisible edges that stack them are not drawn.
    expect(Object.keys(l.edges).some((id) => id.endsWith('/wrap'))).toBe(false)
  })

  it('put the label of a return edge next to the state it leaves', async () => {
    const ir = chart('shop-place_order')
    const l = await layoutMachine(ir, 'Checkout', elk)
    const backoff = l.nodes['Checkout.Backoff']!
    const charging = l.nodes['Checkout.Charging']!
    const label = l.edges[transitionId('Checkout.Backoff', 0)]!.label!
    // Between the two states, and within a label's width of Backoff horizontally.
    expect(label.y).toBeGreaterThan(charging.y + charging.height)
    expect(label.y + label.height).toBeLessThan(backoff.y)
    expect(label.x).toBeLessThan(backoff.x + backoff.width + label.width)
  })

  it('collapse deep levels of a large chart by default, and nothing in a small one', () => {
    const ticket = chart('large-ticket').machines.SupportTicket!
    expect(descendantCount(ticket.root)).toBeGreaterThan(40)
    const collapsed = defaultCollapsed(ticket)
    expect(collapsed.size).toBeGreaterThan(0)
    expect(machineView(ticket, collapsed).states.length).toBeLessThanOrEqual(33)
    expect(defaultCollapsed(chart('shop-place_order').machines.PlaceOrder!).size).toBe(0)
  })

  it('draw a transition from inside a collapsed state from the collapsed box', async () => {
    const ir = chart('large-ticket')
    const collapsed = new Set(['SupportTicket.Active.Working'])
    const graph = buildElkGraph(ir, 'SupportTicket', { collapsed })
    // WithAgent: on Resolve -> Resolving, outside Working.
    const edge = (graph.edges ?? []).find((e) => e.id === transitionId('SupportTicket.Active.Working.WithAgent', 3))
    expect(edge?.sources).toEqual(['SupportTicket.Active.Working'])
    expect(edge?.targets).toEqual(['SupportTicket.Active.Resolving'])
    // WithAgent: on Hold -> OnHold, between two states inside it: not drawn.
    expect((graph.edges ?? []).some((e) => e.id === transitionId('SupportTicket.Active.Working.WithAgent', 2))).toBe(false)
  })
})

describe('the layout fallback', () => {
  const ir = chart('charts-stress')
  const real = newElk()
  const hasOption = (n: ElkNode, key: string): boolean => (n.layoutOptions?.[key] !== undefined) || (n.children ?? []).some((c) => hasOption(c, key))

  it('sets model order on the root graph only', () => {
    const graph = buildElkGraph(ir, 'Stress')
    expect(Object.keys(ATTEMPTS[0]!.root)).toEqual(['elk.layered.considerModelOrder.strategy'])
    expect(hasOption(graph, 'elk.layered.considerModelOrder.strategy')).toBe(false)
  })

  it('retries without model order when ELK refuses it', async () => {
    const refusing: ElkLike = {
      layout(graph) {
        if (hasOption(graph, 'elk.layered.considerModelOrder.strategy')) return Promise.reject(new Error('UnsupportedConfigurationException'))
        return real.layout(graph)
      },
    }
    const l = await layoutMachine(ir, 'Stress', refusing)
    expect(l.attempt).toBe(1)
    expect(l.errors).toEqual(['UnsupportedConfigurationException'])
    expect(overlaps(ir, 'Stress', l, new Set())).toEqual([])
  })

  it('falls back to the layered defaults when ELK crashes on the preferred options', async () => {
    const crashing: ElkLike = {
      layout(graph) {
        if (hasOption(graph, 'elk.layered.nodePlacement.strategy')) throw new TypeError("Cannot read properties of undefined (reading 'a')")
        return real.layout(graph)
      },
    }
    const l = await layoutMachine(ir, 'Stress', crashing)
    expect(l.attempt).toBe(ATTEMPTS.length - 1)
    expect(l.errors).toHaveLength(ATTEMPTS.length - 1)
    expect(renderSvg(ir, 'Stress', l)).toContain('data-state="Stress.Running.Right.X.X2"')
  })

  it('reports every attempt when none works', async () => {
    const broken: ElkLike = { layout: () => Promise.reject(new Error('no')) }
    await expect(layoutMachine(ir, 'Stress', broken)).rejects.toThrow(/could not lay out Stress: no; no; no/)
  })
})
