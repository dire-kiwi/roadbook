// Calls between runs (IR 0.4): `call` declarations, call invokes (`invoke inventory.reserve(…)`),
// `on reserve`, `reply` and `Unanswered`, in the model text, the diagram, the details panel,
// search, simulate and replay, for examples/orders (with examples/inventory) and
// spec/conformance/charts/calls.road.

import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { canonicalJson } from '@roadbook/core'
import { describe, expect, it } from 'vitest'
import type { Machine, State } from '../src/ir.ts'
import { layoutMachine } from '../src/layout.ts'
import { findState, messageSites, stateRows, talks, transitionLines, triggerKind, triggerSource, triggerText } from '../src/model.ts'
import { panelHtml } from '../src/panel.ts'
import { renderLegend, renderSvg } from '../src/render.ts'
import { Replay, inputText, recordText } from '../src/replay.ts'
import { buildSearchIndex, search } from '../src/search.ts'
import { Simulation } from '../src/sim.ts'
import { chart, conformanceChart, newElk } from './charts.ts'

const here = path.dirname(fileURLToPath(import.meta.url))
const traceOf = (name: string) => fs.readFileSync(path.resolve(here, '../../../spec/conformance/cases', name, 'trace.jsonl'), 'utf8')
/** The lines of a trace that belong to `run`. */
const runLines = (text: string, run: string) =>
  text
    .split('\n')
    .filter((l) => l && (JSON.parse(l) as { run: string }).run === run)
    .map((l) => l + '\n')
    .join('')
const lines = (records: unknown[]) => records.map((r) => canonicalJson(r) + '\n').join('')

const orders = chart('orders-orders')
const calls = conformanceChart('calls')
const state = (m: Machine, id: string): State => {
  const s = findState(m, id)
  if (!s) throw new Error(`no state ${id}`)
  return s
}
const rows = (s: State) => stateRows(s).map((r) => `${r.kw}|${r.text}`)
const ctx = (ir: typeof orders, machine: string) => ({ ir, machine, collapsed: new Set<string>() })
const reserve = { machine: 'Inventory', event: 'reserve' }

describe('model text', () => {
  it('shows a call invoke with its address, linked to the call', () => {
    const reserving = state(orders.machines.Order!, 'Order.Reserving')
    expect(rows(reserving)).toEqual(['invoke|inventory.reserve(id, draft.items)'])
    expect(stateRows(reserving)[0]!.event).toEqual(reserve)
  })

  it('labels `on reserve` as a message from another run, and links replies to their call', () => {
    const inv = calls.machines.Inventory!
    const ready = state(inv, 'Inventory.Ready')
    expect(triggerText(ready.transitions[0]!.trigger)).toBe('reserve')
    expect(triggerSource(ready.transitions[0]!.trigger)).toBe('on reserve')
    expect(triggerKind(ready.transitions[0]!.trigger)).toBe('message')
    expect(transitionLines(ready, 0)[0]).toEqual({ cls: 'trigger', text: 'reserve', guard: '', event: reserve })
    const reserving = state(inv, 'Inventory.Reserving')
    const done = transitionLines(reserving, 0)
    expect(done.map((l) => l.text)).toEqual(['done', 'set held = held + 1', 'reply pending done Reservation {', '  orderId: orderId, count: result }'])
    expect(done[2]!.event).toEqual(reserve)
    expect(rows(reserving).slice(1, 3)).toEqual(['on reserve|reply call fail OutOfStock {', '|             reason: "busy" }'])
    expect(stateRows(reserving)[1]).toMatchObject({ kwEvent: reserve, event: reserve, cls: 'message' })
    // The machine body's `on count` replies at once.
    expect(rows(inv.root)).toEqual(['on count|reply call done held'])
  })

  it('says which calls a machine makes, in document order', () => {
    expect(talks(orders, 'Order')).toEqual({
      invokes: [],
      refs: [{ machine: 'Inventory', names: ['inventory'] }],
      sends: [{ machine: 'Inventory', events: ['Release'] }],
      receives: [],
      calls: [{ machine: 'Inventory', calls: ['reserve'] }],
    })
    expect(talks(calls, 'Counter').calls).toEqual([{ machine: 'Inventory', calls: ['count'] }])
    expect(talks(calls, 'Inventory').calls).toEqual([])
    expect(messageSites('Order', calls.machines.Order!)).toEqual([
      { dir: 'call', state: 'Order.Reserving', at: 'invoke', address: { kind: 'ref', name: 'inventory', machine: 'Inventory', state: null }, event: reserve },
    ])
  })
})

describe('the diagram', () => {
  const elk = newElk()

  it('marks the call invoke, the `on reserve` and every reply with the call they open', async () => {
    const order = renderSvg(orders, 'Order', await layoutMachine(orders, 'Order', elk), { interactive: true })
    expect(order).toMatch(/<tspan class="rb-msg" data-event="Inventory\.reserve"><title>Open call Inventory\.reserve<\/title>inventory\.reserve\(id, draft\.items\)<\/tspan>/)
    const inv = renderSvg(calls, 'Inventory', await layoutMachine(calls, 'Inventory', elk), { interactive: true })
    expect(inv).toMatch(/<tspan class="rb-kw">on<\/tspan> <tspan class="rb-msg" data-event="Inventory\.reserve">/)
    expect(inv).toMatch(/<tspan class="rb-msg" data-event="Inventory\.reserve"><title>Open call Inventory\.reserve<\/title><tspan class="rb-msg-kw">reply<\/tspan> pending done/)
    expect(inv).toContain('data-event="Inventory.count"')
    // A transition on a call is drawn like one on an emitted event.
    expect(inv).toMatch(/class="rb-edge rb-k-message" data-state="Inventory\.Ready" data-transition="0"/)
    expect(renderLegend()).toContain('<code class="rb-t-msg">reserve</code> a call of this machine another run made; reply answers it')
  })
})

describe('the details panel', () => {
  it('lists the calls a machine answers, with their signatures, and who calls them', () => {
    const inv = panelHtml(ctx(orders, 'Inventory'), null)
    expect(inv).toContain(
      '<h3>Calls it answers</h3><ul class="rb-list"><li><button class="rb-link rb-mono" type="button" data-open-event="Inventory.reserve" title="Call Inventory.reserve">reserve</button>(orderId: string, items: Item[]) ! OutOfStock',
    )
    expect(inv).toMatch(/<h3>Talked to by<\/h3>.*data-open-machine="Order">Order<\/button> <span class="rb-rel rb-t-msg">calls<\/span> <button[^>]*data-open-event="Inventory\.reserve"/)
    const order = panelHtml(ctx(orders, 'Order'), null)
    expect(order).toMatch(/<span class="rb-rel rb-t-msg">calls<\/span> <button[^>]*data-open-event="Inventory\.reserve"[^>]*>reserve<\/button> <span class="rb-rel">on<\/span> <button[^>]*data-open-machine="Inventory"/)
  })

  it('opens a call: its declaration, where it is taken and answered, and who calls it', () => {
    const html = panelHtml(ctx(calls, 'Inventory'), { type: 'event', name: 'reserve' })
    expect(html).toContain('<span class="rb-chip">call</span>')
    expect(html).toContain('call reserve(orderId: string, items: Item[]) -&gt; Reservation ! OutOfStock')
    expect(html).toContain('<dt>fails with</dt><dd>OutOfStock | Unanswered</dd>')
    expect(html).toMatch(/Taken here.*data-select-transition="\[&quot;Inventory\.Ready&quot;,0\]"/)
    expect(html).toContain('Inventory · Reserving: done → Ready')
    expect(html).toContain('Inventory · Reserving: on reserve → (stays)')
    expect(html).toContain('Order · Reserving (invoke)')
    // Two replies in one transition are one place.
    expect(html.split('Inventory · Reserving: on Twice → (stays)').length).toBe(2)
    // `count` is taken by the machine body, and called by the Counter.
    const count = panelHtml(ctx(calls, 'Inventory'), { type: 'event', name: 'count' })
    expect(count).toContain('Counter · Asking (invoke)')
    expect(count).toContain('<dt>fails with</dt><dd>Unanswered</dd>')
  })

  it('shows a call invoke, its outcomes and `Unanswered`', () => {
    const st = panelHtml(ctx(orders, 'Order'), { type: 'state', id: 'Order.Reserving' })
    expect(st).toContain('<h3>Invokes call</h3><pre class="rb-code">invoke inventory.reserve(id, draft.items)</pre>')
    expect(st).toContain('call of Inventory declared at inventory/inventory.road:20')
    expect(st).toContain('data-open-event="Inventory.reserve" title="Call Inventory.reserve">Open call Inventory.reserve →</button>')
    const done = panelHtml(ctx(orders, 'Order'), { type: 'transition', state: 'Order.Reserving', index: 0 })
    expect(done).toContain('inventory.reserve was answered done; the call returns nothing.')
    expect(done).toContain('<h3>Invoked call</h3><pre class="rb-code">call reserve(orderId: string, items: Item[]) ! OutOfStock</pre>')
    const unanswered = panelHtml(ctx(orders, 'Order'), { type: 'transition', state: 'Order.Reserving', index: 2 })
    expect(unanswered).toContain('inventory.reserve failed with the built-in Unanswered')
    const any = panelHtml(ctx(calls, 'Counter'), { type: 'transition', state: 'Counter.Asking', index: 1 })
    expect(any).toContain('inventory.count was answered fail with a declared error not listed, or Unanswered')
  })

  it('shows `on reserve` and its replies, linked to the call', () => {
    const take = panelHtml(ctx(calls, 'Inventory'), { type: 'transition', state: 'Inventory.Ready', index: 0 })
    expect(take).toContain('<pre class="rb-code">on reserve</pre><p class="rb-empty">Another run calls reserve and waits for the answer.')
    expect(take).toContain('<h3>Call</h3><pre class="rb-code">call reserve(')
    const busy = panelHtml(ctx(calls, 'Inventory'), { type: 'transition', state: 'Inventory.Reserving', index: 2 })
    expect(busy).toContain(
      '<span class="rb-msg-kw">reply</span> <button class="rb-link rb-mono" type="button" data-open-event="Inventory.reserve" title="Call Inventory.reserve">call fail OutOfStock { reason: &quot;busy&quot; }</button><span class="rb-sub rb-note">answers the reserve call being taken, to its caller alone</span>',
    )
    const later = panelHtml(ctx(calls, 'Inventory'), { type: 'transition', state: 'Inventory.Reserving', index: 1 })
    expect(later).toContain('answers the reserve call pending holds, to its caller alone')
  })
})

describe('search', () => {
  it('finds a call where it is declared, made, taken and answered', () => {
    const labels = search(buildSearchIndex(calls), 'reserve').map((e) => `${e.kind} ${e.label}`)
    expect(labels).toEqual(
      expect.arrayContaining([
        'call call reserve of Inventory',
        'call on reserve: Inventory.Ready → Reserving',
        'call reply pending fail error: Inventory.Reserving → Ready',
        'call invoke inventory.reserve(id, items) in Order.Reserving',
      ]),
    )
    expect(search(buildSearchIndex(calls), 'count')[0]).toMatchObject({ kind: 'call', machine: 'Inventory', target: { type: 'event', name: 'count' } })
  })
})

describe('simulate', () => {
  const input = { warehouse: 'w1' }
  const items = (sku: string, qty: number) => [{ sku, qty }]

  it('takes calls from simulated callers: the Inventory writes its records of call-reply', () => {
    const sim = new Simulation(calls, 'Inventory')
    const ok = (o: unknown) => expect(o).toEqual({ kind: 'ok' })
    ok(sim.start(input))
    let c = sim.choices()
    expect(c.calls.map((x) => [x.name, x.handled, x.returns, x.errors])).toEqual([
      ['reserve', true, 'Reservation', ['OutOfStock']],
      ['count', true, 'int', []],
    ])
    ok(sim.call('reserve', { orderId: 'o1', items: items('s1', 2) }))
    expect(sim.current!.label).toBe('call reserve')
    expect(sim.choices().pending).toEqual([{ from: 'r2', invoke: 1, call: 'reserve' }])
    ok(sim.call('reserve', { orderId: 'o2', items: items('s2', 1) }))
    ok(sim.call('count', {}))
    c = sim.choices()
    // The second reserve was answered at once; the first is still pending.
    expect(c.pending).toEqual([{ from: 'r2', invoke: 1, call: 'reserve' }])
    ok(sim.complete(c.invokes[0]!.id, { done: 2 }))
    expect(sim.choices().pending).toEqual([])
    ok(sim.call('reserve', { orderId: 'o3', items: items('s3', 9) }))
    ok(sim.complete(sim.choices().invokes[0]!.id, { fail: { _tag: 'OutOfStock', reason: 'none left' } }))
    expect(lines(sim.records())).toBe(runLines(traceOf('call-reply'), 'r1'))
    // Stepping back forgets the callers made since: the next call comes from r3 again.
    while (sim.at > 1) sim.back()
    ok(sim.call('reserve', { orderId: 'o9', items: [] }))
    expect(sim.current!.records.find((r) => r.kind === 'event')).toMatchObject({ event: { kind: 'call', from: 'r3', invoke: 1 } })
  })

  it('drops a call no transition takes, answered Unanswered: the Inventory of call-unanswered', () => {
    const sim = new Simulation(calls, 'Inventory')
    sim.start(input)
    sim.send({ _tag: 'Pause' })
    expect(sim.choices().calls.find((x) => x.name === 'reserve')!.handled).toBe(false)
    sim.call('reserve', { orderId: 'o1', items: items('s1', 2) })
    expect(sim.current!.records.map((r) => r.kind)).toEqual(['dropped', 'replied'])
    sim.send({ _tag: 'Resume' })
    sim.call('reserve', { orderId: 'o2', items: items('s2', 1) })
    sim.send({ _tag: 'Close' })
    expect(lines(sim.records())).toBe(runLines(traceOf('call-unanswered'), 'r1'))
  })

  it('answers a call invoke by hand, like a task: done, its errors or Unanswered', () => {
    const sim = new Simulation(calls, 'Order', { run: 'r2' })
    expect(sim.start({ id: 'o1', items: items('s1', 2), inventory: 'r1' })).toEqual({ kind: 'ok' })
    const inv = sim.choices().invokes[0]!
    expect(inv).toMatchObject({
      src: 'inventory.reserve',
      srcKind: 'call',
      callee: { machine: 'Inventory', name: 'inventory', run: 'r1' },
      returns: 'Reservation',
      errors: [
        { name: 'OutOfStock', fields: [{ name: 'reason', type: 'string', doc: '' }] },
        { name: 'Unanswered', fields: [] },
      ],
    })
    expect(sim.complete(inv.id, { fail: { _tag: 'Unanswered' } })).toEqual({ kind: 'ok' })
    expect(sim.current!.label).toBe('inventory.reserve fail Unanswered')
    expect(lines(sim.records())).toBe(runLines(traceOf('call-unanswered'), 'r2'))
    sim.back()
    expect(sim.complete(inv.id, { done: { orderId: 'o1', count: 2 } })).toEqual({ kind: 'ok' })
    expect(sim.current!.snapshot.result).toEqual({ output: { orderId: 'o1', count: 2 } })
  })
})

describe('replay', () => {
  it('reads calls, replies and Unanswered in words', () => {
    const replay = new Replay(calls, traceOf('call-unanswered'))
    const text = replay.records.map(recordText)
    expect(text).toContain(
      'dropped call reserve from r2 (its invoke 1) {"items":[{"qty":2,"sku":"s1"}],"orderId":"o1"}: no transition takes it, so it is answered Unanswered',
    )
    expect(text).toContain("replied fail Unanswered to r2's call reserve (its invoke 1)")
    expect(text).toContain('invoke 1 started: inventory.reserve(items: [{"qty":2,"sku":"s1"}], orderId: "o1") in Reserving')
    expect(text).toContain('event invoke 1 fail Unanswered')
    const reply = new Replay(calls, traceOf('call-reply')).records.map(recordText)
    expect(reply).toContain('replied done {"count":2,"orderId":"o1"} to r2\'s call reserve (its invoke 1)')
    expect(reply).toContain('replied fail OutOfStock {"reason":"busy"} to r3\'s call reserve (its invoke 1)')
    expect(reply).toContain("replied done 0 to r4's call count (its invoke 1)")
    expect(inputText({ kind: 'call', from: 'r4', invoke: 1, call: 'count', args: {} })).toBe('call count from r4 (its invoke 1)')
  })

  it('keeps the calls a run has taken and not answered', () => {
    const replay = new Replay(calls, traceOf('call-reply'))
    const at = (pred: (r: (typeof replay.records)[number]) => boolean) => replay.records.findIndex(pred)
    const taken = at((r) => r.kind === 'event' && (r.event as { from?: string }).from === 'r2')
    expect(replay.viewAt(taken, 'r1').calls).toEqual([{ from: 'r2', invoke: 1, call: 'reserve' }])
    const answered = at((r) => r.kind === 'replied' && r.to === 'r2')
    expect(replay.viewAt(answered - 1, 'r1').calls).toHaveLength(1)
    expect(replay.viewAt(answered, 'r1').calls).toEqual([])
  })

  it('reports a reply to no pending call and a call the machine does not have', () => {
    const text = traceOf('call-reply')
    const bad = text.replace('"to":"r2"', '"to":"r9"').replace('"call":"count","from":"r4"', '"call":"nope","from":"r4"')
    const problems = new Replay(calls, bad).problems
    expect(problems).toEqual(expect.arrayContaining([expect.stringMatching(/no pending call reserve from r9's invoke 1/), expect.stringMatching(/Inventory has no call nope/)]))
  })
})
