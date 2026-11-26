// Messaging (IR 0.2): `ref` types, `emits`, `invoke … as`, `on name.Event`, `send` and `emit`,
// in the model text, the diagram, the details panel, search and the project index, for
// examples/upload and examples/orders (with examples/inventory).

import { describe, expect, it } from 'vitest'
import { layoutMachine } from '../src/layout.ts'
import type { Box } from '../src/layout.ts'
import { effectText, findState, stateRows, talks, transitionLines, triggerSource, triggerText } from '../src/model.ts'
import type { Machine, State } from '../src/ir.ts'
import { panelHtml } from '../src/panel.ts'
import { indexEdges, layoutIndex, projectIndex, renderIndexSvg, renderIndexTable } from '../src/project.ts'
import { renderMachineHeader, renderSvg } from '../src/render.ts'
import { buildSearchIndex, search } from '../src/search.ts'
import { chart, newElk } from './charts.ts'

const publish = chart('upload-publish')
const orders = chart('orders-orders')
const state = (m: Machine, id: string): State => {
  const s = findState(m, id)
  if (!s) throw new Error(`no state ${id}`)
  return s
}
const rows = (s: State) => stateRows(s).map((r) => `${r.kw}|${r.text}`)
const lines = (s: State, i: number) => transitionLines(s, i).map((l) => [l.cls, l.text, l.guard].filter(Boolean).join(' '))

describe('model text', () => {
  const uploading = state(publish.machines.Publish!, 'Publish.Uploading')
  const sending = state(publish.machines.Upload!, 'Upload.Sending')
  const reserving = state(orders.machines.Order!, 'Order.Reserving')

  it('shows an invoke with its as name, sends and emitted triggers as rows, one effect per line', () => {
    expect(rows(uploading)).toEqual([
      'invoke|machine Upload(draft.file) as upload',
      'on PauseClicked|send upload.Pause',
      'on ResumeClicked|send upload.Resume',
      'on upload.Progress|set percent = event.percent',
      `|${' '.repeat('on upload.Progress '.length)}do showProgress(event.percent)`,
    ])
    const r = stateRows(uploading)
    expect(r[0]).toMatchObject({ machine: 'Upload', alias: 'upload' })
    expect(r[1]!.event).toEqual({ machine: 'Upload', event: 'Pause' })
    expect(r[3]).toMatchObject({ kwEvent: { machine: 'Upload', event: 'Progress' }, cls: 'message', transition: 4 })
    expect(r[4]!.kwEvent).toBeUndefined()
  })

  it('shows a send in an entry, wrapped, every line naming the event', () => {
    expect(rows(reserving)).toEqual(['entry|send inventory.Reserve { orderId: id,', '|        items: draft.items }'])
    for (const r of stateRows(reserving)) expect(r.event).toEqual({ machine: 'Inventory', event: 'Reserve' })
  })

  it('labels an emitted trigger through its sender, and send and emit effects as written', () => {
    expect(triggerText(reserving.transitions[0]!.trigger)).toBe('inventory.Reserved')
    expect(triggerSource(reserving.transitions[0]!.trigger)).toBe('on inventory.Reserved')
    expect(lines(reserving, 0)).toEqual(['trigger inventory.Reserved', 'trigger [event.orderId == id]'])
    expect(transitionLines(reserving, 0)[0]!.event).toEqual({ machine: 'Inventory', event: 'Reserved' })
    expect(transitionLines(reserving, 0)[1]!.event).toBeUndefined()
    expect(lines(sending, 1)).toEqual(['trigger done', 'effect set offset = result', 'effect emit Progress { percent:', 'effect   percentOf(file, result) }'])
    expect(transitionLines(sending, 1)[2]!.event).toEqual({ machine: 'Upload', event: 'Progress' })
    const paying = state(orders.machines.Order!, 'Order.Paying')
    expect(effectText(paying.transitions[1]!.effects[0]!)).toBe('send inventory.Release { orderId: id }')
    expect(transitionLines(paying, 1)[1]!.event).toEqual({ machine: 'Inventory', event: 'Release' })
  })

  it('says who a machine talks to', () => {
    expect(talks(publish, 'Publish')).toEqual({
      invokes: [{ machine: 'Upload', as: 'upload', state: 'Publish.Uploading' }],
      spawns: [],
      refs: [],
      sends: [{ machine: 'Upload', events: ['Pause', 'Resume'] }],
      receives: [{ machine: 'Upload', events: ['Progress'] }],
    })
    expect(talks(orders, 'Order')).toEqual({
      invokes: [],
      spawns: [],
      refs: [{ machine: 'Inventory', names: ['inventory'] }],
      sends: [{ machine: 'Inventory', events: ['Reserve', 'Release'] }],
      receives: [{ machine: 'Inventory', events: ['Reserved', 'Rejected'] }],
    })
    expect(talks(orders, 'Inventory')).toEqual({ invokes: [], spawns: [], refs: [], sends: [], receives: [] })
  })
})

describe('the diagram', () => {
  const elk = newElk()

  it('marks every send, emit and emitted trigger with the event it opens', async () => {
    const svg = renderSvg(publish, 'Publish', await layoutMachine(publish, 'Publish', elk), { interactive: true })
    expect(svg).toContain('<tspan class="rb-kw">as</tspan> <tspan class="rb-alias">upload</tspan>')
    expect(svg).toMatch(/<tspan class="rb-msg" data-event="Upload\.Pause"><title>Open event Upload\.Pause<\/title><tspan class="rb-msg-kw">send<\/tspan> upload\.Pause<\/tspan>/)
    expect(svg).toMatch(/<tspan class="rb-kw">on<\/tspan> <tspan class="rb-msg" data-event="Upload\.Progress">/)
    const upload = renderSvg(publish, 'Upload', await layoutMachine(publish, 'Upload', elk))
    expect(upload).toMatch(/class="rb-label-effect"[^>]*><tspan class="rb-msg" data-event="Upload\.Progress">.*?<tspan class="rb-msg-kw">emit<\/tspan> Progress/)
    const order = renderSvg(orders, 'Order', await layoutMachine(orders, 'Order', elk))
    for (const e of ['Inventory.Reserve', 'Inventory.Reserved', 'Inventory.Rejected', 'Inventory.Release']) expect(order).toContain(`data-event="${e}"`)
    // Transitions on an emitted event are their own kind of edge.
    expect(order).toMatch(/class="rb-edge rb-k-message" data-state="Order\.Reserving" data-transition="0"/)
    expect(order).toContain('id="rb-arrow-message"')
  })

  it('links a ref parameter in the signature to its machine', () => {
    expect(renderMachineHeader(orders, 'Order')).toContain(
      'inventory: <button class="rb-link rb-mono rb-ref" type="button" data-open-machine="Inventory" title="Open machine Inventory">ref Inventory</button>',
    )
  })
})

describe('the details panel', () => {
  const ctx = (ir: typeof publish, machine: string) => ({ ir, machine, collapsed: new Set<string>() })

  it('shows what a machine accepts, emits and talks to, refs linked', () => {
    const order = panelHtml(ctx(orders, 'Order'), null)
    expect(order).toContain('<dt>takes</dt><dd>id: string, draft: OrderDraft, inventory: <button class="rb-link rb-mono rb-ref"')
    expect(order).toContain('<h3>Talks to</h3>')
    expect(order).toMatch(/holds a ref to.*data-open-machine="Inventory".*inventory/)
    expect(order).toMatch(/sends<\/span> <button[^>]*data-open-event="Inventory\.Reserve"[^>]*>Reserve<\/button>, <button[^>]*data-open-event="Inventory\.Release"/)
    expect(order).toMatch(/handles<\/span> <button[^>]*data-open-event="Inventory\.Reserved"/)
    const inventory = panelHtml(ctx(orders, 'Inventory'), null)
    expect(inventory).toContain('<h3>Events it accepts</h3>')
    expect(inventory).toContain('<h3>Events it emits</h3>')
    expect(inventory).toContain('data-open-event="Inventory.Rejected"')
    expect(inventory).toMatch(/<h3>Talked to by<\/h3>.*data-open-machine="Order".*holds a ref to it in/)
    const pub = panelHtml(ctx(publish, 'Publish'), { type: 'state', id: 'Publish.Uploading' })
    expect(pub).toContain('invoke Upload(draft.file) as upload')
    expect(pub).toContain('<code>upload</code> names the child')
  })

  it('opens an accepted event: where it is handled and who sends it', () => {
    const html = panelHtml(ctx(publish, 'Upload'), { type: 'event', name: 'Pause' })
    expect(html).toContain('<span class="rb-chip">event</span>')
    expect(html).toContain('event Pause')
    expect(html).toContain('Upload.Pause · examples/upload/upload.road:')
    expect(html).toMatch(/Handled here.*data-select-transition="\[&quot;Upload\.Sending&quot;,3\]"/)
    expect(html).toContain(`data-goto="${'[&quot;Publish&quot;,{&quot;type&quot;:&quot;transition&quot;,&quot;state&quot;:&quot;Publish.Uploading&quot;,&quot;index&quot;:2}]'}"`)
    expect(html).toContain('Publish · Uploading: on PauseClicked → (stays)')
  })

  it('opens an emitted event: where it is emitted and who handles it', () => {
    const html = panelHtml(ctx(orders, 'Inventory'), { type: 'event', name: 'Rejected' })
    expect(html).toContain('<span class="rb-chip">emitted event</span>')
    expect(html).toContain('emits Rejected { orderId: string, reason: string }')
    expect(html).toContain('Inventory · Reserving: fail OutOfStock → Ready')
    expect(html).toContain('Inventory · Reserving: on Reserve → (stays)')
    expect(html).toContain('Order · Reserving: on inventory.Rejected → Unavailable')
    // An entry send is listed by its state.
    expect(panelHtml(ctx(orders, 'Inventory'), { type: 'event', name: 'Reserve' })).toContain('Order · Reserving (entry)')
    // A name that is no event falls back to the machine.
    expect(panelHtml(ctx(orders, 'Inventory'), { type: 'event', name: 'Nope' })).toContain('<h3>Signature</h3>')
  })

  it('shows a transition on an emitted event and its send effects, linked to the events', () => {
    const t = panelHtml(ctx(orders, 'Order'), { type: 'transition', state: 'Order.Reserving', index: 0 })
    expect(t).toContain('Inventory emitted Reserved: the ref inventory.')
    expect(t).toContain('emits Reserved { orderId: string }')
    expect(t).toContain('data-open-event="Inventory.Reserved"')
    const send = panelHtml(ctx(orders, 'Order'), { type: 'transition', state: 'Order.Paying', index: 1 })
    expect(send).toContain('<span class="rb-msg-kw">send</span> <button class="rb-link rb-mono" type="button" data-open-event="Inventory.Release"')
    expect(send).toContain('to Inventory, the ref inventory')
    const progress = panelHtml(ctx(publish, 'Publish'), { type: 'transition', state: 'Publish.Uploading', index: 4 })
    expect(progress).toContain('Upload emitted Progress: the child upload, invoked in Uploading.')
  })
})

describe('search', () => {
  it('finds declared, sent, emitted and received events', () => {
    const index = buildSearchIndex(orders)
    const labels = search(index, 'Reserved').map((e) => `${e.kind} ${e.label}`)
    expect(labels).toEqual(
      expect.arrayContaining([
        'event emits Reserved of Inventory',
        'event inventory.Reserved: Order.Reserving → Paying',
        'event emit Reserved { orderId: orderId }: Inventory.Reserving → Ready',
      ]),
    )
    expect(search(index, 'Reserve').map((e) => e.label)).toContain('send inventory.Reserve { orderId: id, items: draft.items }: entry of Order.Reserving')
    expect(search(index, 'Reserved').find((e) => e.label === 'emits Reserved of Inventory')).toMatchObject({ machine: 'Inventory', target: { type: 'event', name: 'Reserved' } })
  })
})

describe('the project index of examples/', () => {
  const names = ['shop-place_order', 'large-ticket', 'password-reset-confirm', 'webhook-webhook', 'import-job-import', 'upload-publish', 'orders-orders']
  const index = projectIndex(
    'examples',
    names.map((n) => ({ href: `${n}.html`, entry: n, ir: chart(n) })),
  )
  const at = (n: string) => index.machines.findIndex((m) => m.name === n)

  it('records refs, sends and handled emitted events, by machine', () => {
    expect(index.machines).toHaveLength(11)
    const order = index.machines[at('Order')]!
    expect(order.refs).toEqual([{ to: at('Inventory'), names: ['inventory'] }])
    expect(order.sends).toEqual([{ to: at('Inventory'), events: ['Reserve', 'Release'] }])
    expect(order.receives).toEqual([{ from: at('Inventory'), events: ['Reserved', 'Rejected'] }])
    const pub = index.machines[at('Publish')]!
    expect(pub.invokes).toEqual([at('Upload')])
    expect(pub.sends).toEqual([{ to: at('Upload'), events: ['Pause', 'Resume'] }])
    expect(pub.receives).toEqual([{ from: at('Upload'), events: ['Progress'] }])
    // A machine of another package is named by its package.
    expect(index.machines[at('Inventory')]!.source).toBe('inventory/inventory.road:19')
  })

  it('draws an edge for each kind of relation, labelled with the events', () => {
    const edges = indexEdges(index).map((e) => `${e.kind} ${index.machines[e.from]!.name} -> ${index.machines[e.to]!.name}: ${e.lines.join(' / ')}`)
    expect(edges).toEqual([
      'invoke PlaceOrder -> Checkout: ',
      'invoke SupportTicket -> SatisfactionSurvey: ',
      'invoke Publish -> Upload: ',
      'ref Order -> Inventory: ref inventory',
      'send Publish -> Upload: sends Pause, Resume',
      'send Order -> Inventory: sends Reserve, Release',
      'emit Upload -> Publish: emits Progress',
      'emit Inventory -> Order: emits Reserved, Rejected',
    ])
  })

  it('lays them out with no overlapping boxes or labels, and draws each kind distinctly', async () => {
    const layout = await layoutIndex(index, newElk())
    const overlap = (a: Box, b: Box) => a.x + 0.5 < b.x + b.width && b.x + 0.5 < a.x + a.width && a.y + 0.5 < b.y + b.height && b.y + 0.5 < a.y + a.height
    const labels = layout.edges.flatMap((e) => (e.label ? [e.label] : []))
    expect(labels).toHaveLength(5)
    const boxes = [...layout.nodes, ...labels]
    const found: string[] = []
    for (let i = 0; i < boxes.length; i++) for (let j = i + 1; j < boxes.length; j++) if (overlap(boxes[i]!, boxes[j]!)) found.push(`${i} overlaps ${j}`)
    expect(found).toEqual([])
    for (const e of layout.edges) expect(e.points.length).toBeGreaterThanOrEqual(2)
    const svg = renderIndexSvg(index, layout)
    for (const kind of ['invoke', 'ref', 'send', 'emit']) {
      expect(svg).toContain(`class="rb-edge rb-iedge rb-i-${kind}"`)
      expect(svg).toContain(`marker-end="url(#rbi-arrow-${kind})"`)
    }
    expect(svg).toContain('<tspan class="rb-ikw">emits</tspan> Reserved, Rejected')
    expect(svg).not.toMatch(/NaN|undefined/)
    const table = renderIndexTable(index)
    expect(table).toContain('<code>ref Inventory (inventory)</code><br><code>sends Reserve, Release to Inventory</code><br><code>handles Reserved, Rejected from Inventory</code>')
  })
})
