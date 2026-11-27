// A peer's end (IR 0.8): `on name.ended` for a run a `ref` holds, in the model text, the
// diagram, the details panel, search, the project index, simulate and replay, for the
// conformance chart ended.road and its peer-ended-* cases.

import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'
import type { Ir, Machine, State } from '../src/ir.ts'
import { layoutMachine } from '../src/layout.ts'
import { findState, talks, transitionLines, triggerKind, triggerSource, triggerText } from '../src/model.ts'
import { panelHtml } from '../src/panel.ts'
import { indexEdges, layoutIndex, projectIndex, renderIndexLegend, renderIndexSvg, renderIndexTable } from '../src/project.ts'
import { renderLegend, renderSvg } from '../src/render.ts'
import { Replay, recordText } from '../src/replay.ts'
import { buildSearchIndex, search } from '../src/search.ts'
import { ENDED_STATUSES, Simulation } from '../src/sim.ts'
import { conformanceChart, newElk } from './charts.ts'

const here = path.dirname(fileURLToPath(import.meta.url))
const conformance = path.resolve(here, '../../../spec/conformance')
const ended = conformanceChart('ended')
const trace = (name: string) => fs.readFileSync(path.join(conformance, 'cases', name, 'trace.jsonl'), 'utf8')

const state = (m: Machine, id: string): State => {
  const s = findState(m, id)
  if (!s) throw new Error(`no state ${id}`)
  return s
}
const waiting = state(ended.machines.Order!, 'Order.Waiting')
const ctx = (ir: Ir, machine: string) => ({ ir, machine, collapsed: new Set<string>() })

describe('model text', () => {
  it('labels the trigger through the ref, as written, and as a message from another run', () => {
    const t = waiting.transitions[0]!.trigger
    expect(t).toEqual({ kind: 'ended', from: { kind: 'ref', name: 'inventory', machine: 'Inventory', state: null } })
    expect(triggerText(t)).toBe('inventory.ended')
    expect(triggerSource(t)).toBe('on inventory.ended')
    expect(triggerKind(t)).toBe('message')
    expect(triggerText(state(ended.machines.Switcher!, 'Switcher.Watching').transitions[1]!.trigger)).toBe('current.ended')
  })

  it('labels each branch of a block on event.status', () => {
    expect([0, 1, 2].map((i) => transitionLines(waiting, i).map((l) => l.text || l.guard))).toEqual([
      ['inventory.ended', '[event.status == Ended.Done]'],
      ['inventory.ended', '[event.status == Ended.Defect]', 'do note(event.status)'],
      ['inventory.ended', '[else]', 'do note(event.status)'],
    ])
    // Nothing to open: the trigger names no event.
    expect(transitionLines(waiting, 0)[0]!.event).toBeUndefined()
  })

  it('says whose end a machine hears, through which names, beside the ref it holds', () => {
    expect(talks(ended, 'Order').ends).toEqual([{ machine: 'Inventory', names: ['inventory'] }])
    expect(talks(ended, 'Order').refs).toEqual([{ machine: 'Inventory', names: ['inventory'] }])
    // Only `current` is watched; `a` and `b` are held all the same.
    expect(talks(ended, 'Switcher').ends).toEqual([{ machine: 'Inventory', names: ['current'] }])
    expect(talks(ended, 'Switcher').refs).toEqual([{ machine: 'Inventory', names: ['a', 'b', 'current'] }])
    expect(talks(ended, 'Clerk').ends).toEqual([])
    expect(talks(ended, 'Inventory').ends).toEqual([])
  })
})

describe('the diagram', () => {
  const elk = newElk()

  it('draws the trigger as a message, labelled inventory.ended', async () => {
    const svg = renderSvg(ended, 'Order', await layoutMachine(ended, 'Order', elk), { interactive: true })
    expect(svg).toContain('inventory.ended')
    expect(svg).toContain('<g class="rb-edge rb-k-message" data-state="Order.Waiting" data-transition="0">')
    expect(svg).toMatch(/<g class="rb-label rb-k-message" data-state="Order\.Waiting" data-transition="0">.*?>inventory\.ended<\/text>/)
    expect(svg).toContain('[event.status == Ended.Defect]')
    expect(svg).not.toMatch(/NaN|undefined/)
    const switcher = renderSvg(ended, 'Switcher', await layoutMachine(ended, 'Switcher', elk), { interactive: true })
    expect(switcher).toContain('current.ended')
  })

  it('has it in the legend', () => {
    expect(renderLegend()).toContain('<code class="rb-t-msg">inventory.ended</code> the run the ref inventory holds ended, however it ended; event.status says how')
  })
})

describe('the details panel', () => {
  it('explains the trigger, with the built-in Ended and the peer machine', () => {
    const html = panelHtml(ctx(ended, 'Order'), { type: 'transition', state: 'Order.Waiting', index: 1 })
    expect(html).toContain('<pre class="rb-code">on inventory.ended</pre>')
    expect(html).toContain('The run of Inventory that the ref inventory holds closed, however it closed')
    expect(html).toContain('event.status says which')
    expect(html).toContain('<h3>Peer Inventory</h3><pre class="rb-code">enum Ended { Done, Failed, Cancelled, Defect, Suspended }</pre>')
    expect(html).toContain('data-open-machine="Inventory">Open machine Inventory →</button>')
  })

  it('lists the peers whose end a machine hears, and who hears a machine end', () => {
    const order = panelHtml(ctx(ended, 'Order'), null)
    expect(order).toContain('<span class="rb-rel rb-t-msg">hears when</span> <button class="rb-link rb-mono" type="button" data-open-machine="Inventory">Inventory</button> <span class="rb-rel">ends, through</span> inventory')
    const inventory = panelHtml(ctx(ended, 'Inventory'), null)
    expect(inventory).toContain('data-open-machine="Switcher">Switcher</button> <span class="rb-rel rb-t-msg">hears when it ends, through</span> current')
    expect(inventory).not.toContain('data-open-machine="Clerk">Clerk</button> <span class="rb-rel rb-t-msg">hears')
  })
})

describe('search', () => {
  it('finds the holders that hear a machine end, by its name', () => {
    const labels = search(buildSearchIndex(ended), 'Inventory').map((e) => `${e.kind} ${e.label}`)
    expect(labels).toContain('machine on inventory.ended: Order.Waiting → Alarm')
    expect(labels).toContain('machine on current.ended: Switcher.Watching → Gone')
  })
})

describe('the project index', () => {
  const index = projectIndex('ended', [{ href: 'ended.html', entry: 'ended', ir: ended }])
  const at = (n: string) => index.machines.findIndex((m) => m.name === n)

  it('records the peers each machine hears the end of, as roadbook viz does', () => {
    expect(index.machines[at('Order')]!.ends).toEqual([{ to: at('Inventory'), names: ['inventory'] }])
    expect(index.machines[at('Switcher')]!.ends).toEqual([{ to: at('Inventory'), names: ['current'] }])
    expect(index.machines[at('Clerk')]!.ends).toEqual([])
    expect(index.machines[at('Clerk')]!.refs).toEqual([{ to: at('Inventory'), names: ['inventory'] }])
  })

  it('draws an ended edge from the holder to the peer, beside the ref, with its legend', async () => {
    const edges = indexEdges(index)
    expect(edges.filter((e) => e.kind === 'ended').map((e) => [index.machines[e.from]!.name, index.machines[e.to]!.name, e.lines])).toEqual([
      ['Order', 'Inventory', ['on inventory.ended']],
      ['Picker', 'Inventory', ['on inventory.ended']],
      ['Guard', 'Inventory', ['on inventory.ended']],
      ['Switcher', 'Inventory', ['on current.ended']],
    ])
    expect(edges.some((e) => e.kind === 'ref' && e.from === at('Order') && e.to === at('Inventory'))).toBe(true)
    const svg = renderIndexSvg(index, await layoutIndex(index, newElk()))
    expect(svg).toContain('<g class="rb-edge rb-iedge rb-i-ended"><title>Order hears when Inventory ends</title>')
    expect(svg).toContain('<marker id="rbi-arrow-ended"')
    expect(svg).toMatch(/<g class="rb-label rb-ilabel rb-i-ended">.*<tspan class="rb-ikw">on<\/tspan> inventory\.ended/)
    expect(svg).not.toMatch(/NaN|undefined/)
    expect(renderIndexLegend()).toContain('<code>on name.ended</code> A hears when B, which it holds a ref to in name, ends (dashed, barred head)')
    expect(renderIndexTable(index)).toContain('<code>hears when Inventory ends (current)</code>')
  })

  it('reads an index written without ends', () => {
    const old = { ...index, machines: index.machines.map(({ ends: _ends, ...m }) => m) } as unknown as typeof index
    expect(indexEdges(old).some((e) => e.kind === 'ended')).toBe(false)
    expect(renderIndexTable(old)).not.toContain('undefined')
  })
})

describe('simulate', () => {
  const start = (machine: string, input: Record<string, string>) => {
    const sim = new Simulation(ended, machine, { run: 'r9' })
    expect(sim.start(input)).toEqual({ kind: 'ok' })
    return sim
  }

  it('offers the end of each peer the machine hears, in every status', () => {
    const sim = start('Order', { id: 'o1', inventory: 'r1' })
    expect(sim.choices().peers).toEqual([{ run: 'r1', machine: 'Inventory', names: ['inventory'], handled: true }])
    expect(ENDED_STATUSES).toEqual(['Done', 'Failed', 'Cancelled', 'Defect', 'Suspended'])
    expect(sim.peerEnded('r1', 'Defect')).toEqual({ kind: 'ok' })
    const f = sim.current!
    expect(f.label).toBe('r1 ended Defect')
    expect(f.records.map(recordText).slice(0, 3)).toEqual(['event r1 ended Defect', 'transition Waiting #1 → Alarm', 'timer 1 cancelled in Waiting'])
    expect(f.records.map(recordText)).toContain('action note(status: "Defect")')
    expect(f.snapshot.active).toEqual([])
    expect(f.snapshot.status).toBe('done')
  })

  it('takes each status down its own branch', () => {
    for (const [status, to] of [
      ['Done', 'Closed'],
      ['Failed', 'Gone'],
      ['Suspended', 'Gone'],
    ] as const) {
      const sim = start('Order', { id: 'o1', inventory: 'r1' })
      sim.peerEnded('r1', status)
      expect(sim.current!.records.map(recordText)).toContain(`transition Waiting #${to === 'Closed' ? 0 : 2} → ${to}`)
    }
  })

  it('drops an end no active state takes, and tells each peer once', () => {
    const sim = start('Picker', { inventory: 'r1' })
    expect(sim.choices().peers).toEqual([{ run: 'r1', machine: 'Inventory', names: ['inventory'], handled: false }])
    expect(sim.peerEnded('r1', 'Done')).toEqual({ kind: 'ok' })
    expect(sim.current!.records.map(recordText)).toEqual(['dropped r1 ended Done'])
    expect(sim.choices().peers).toEqual([])
    expect(sim.peerEnded('r1', 'Done')).toEqual({ kind: 'error', message: 'the run has already been told that r1 ended' })
    // Stepping back forgets it.
    sim.back()
    expect(sim.choices().peers.map((p) => p.run)).toEqual(['r1'])
  })

  it('drops it in a strict machine too, rather than ending with a defect', () => {
    const sim = start('Guard', { inventory: 'r1' })
    sim.peerEnded('r1', 'Done')
    expect(sim.current!.records.map(recordText)).toEqual(['dropped r1 ended Done'])
    expect(sim.current!.snapshot.status).toBe('running')
  })

  it('follows the run a variable holds now', () => {
    const sim = start('Switcher', { a: 'r1', b: 'r2' })
    expect(sim.choices().peers).toEqual([{ run: 'r1', machine: 'Inventory', names: ['current'], handled: true }])
    sim.send({ _tag: 'Swap' })
    expect(sim.choices().peers).toEqual([{ run: 'r2', machine: 'Inventory', names: ['current'], handled: true }])
    sim.peerEnded('r2', 'Cancelled')
    expect(sim.current!.records.map(recordText).slice(0, 2)).toEqual(['event r2 ended Cancelled', 'transition Watching #1 → Gone'])
  })

  it('offers nothing for a holder without an on name.ended', () => {
    expect(start('Clerk', { inventory: 'r1' }).choices().peers).toEqual([])
  })
})

describe('replay', () => {
  it('reads an ended input, and links it to the record that ended the peer, both ways', () => {
    const replay = new Replay(ended, trace('peer-ended-done'))
    expect(replay.problems).toEqual([])
    const heard = replay.records.findIndex((r) => (r.event as { kind?: string } | undefined)?.kind === 'ended')
    expect(recordText(replay.records[heard]!)).toBe('event r1 ended Done')
    const end = replay.peerEnd(heard)!
    expect(replay.records[end]).toMatchObject({ kind: 'done', run: 'r1' })
    expect(replay.heardBy(end)).toEqual([heard])
    expect(replay.peerEnd(end)).toBeNull()
    expect(replay.heardBy(heard)).toEqual([])
  })

  it('reads every peer-ended case without a problem', () => {
    for (const name of ['peer-ended-done', 'peer-ended-status', 'peer-ended-unheard', 'peer-ended-var', 'peer-ended-strict', 'peer-ended-suspended', 'peer-ended-before-start']) {
      const replay = new Replay(ended, trace(name))
      expect(replay.problems, name).toEqual([])
      for (let i = 0; i < replay.records.length; i++) {
        const end = replay.peerEnd(i)
        if (end !== null) expect(replay.heardBy(end), name).toContain(i)
      }
    }
    const dropped = new Replay(ended, trace('peer-ended-unheard')).records.find((r) => r.kind === 'dropped')!
    expect(recordText(dropped)).toBe('dropped r1 ended Done')
  })

  it('finds an end that does not match the peer’s last record', () => {
    const text = trace('peer-ended-done').replace('"status":"Done"', '"status":"Failed"')
    expect(new Replay(ended, text).problems).toEqual([expect.stringMatching(/: r1 ended done, not Failed$/)])
    const stale = trace('peer-ended-done').replace(/("kind":"ended","status":"Done"\},"kind":)"event"/, '$1"stale"')
    expect(new Replay(ended, stale).problems).toContainEqual(expect.stringMatching(/a peer's end is never stale$/))
  })
})
