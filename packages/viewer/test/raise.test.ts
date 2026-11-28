// Events a machine raises itself (IR 0.11): `raise E`, delayed raises (`raise E after d as n`),
// `cancel n` and internal events, in the model text, the diagram, the details panel, search,
// simulate and replay, for the conformance chart raise.road.

import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'
import type { Machine, State } from '../src/ir.ts'
import { effectText, eventDecl, eventDeclText, findState, messageSites, stateRows, transitionLines } from '../src/model.ts'
import { panelHtml } from '../src/panel.ts'
import { renderSvg } from '../src/render.ts'
import { layoutMachine } from '../src/layout.ts'
import { Replay, inputText, recordText } from '../src/replay.ts'
import { buildSearchIndex, search } from '../src/search.ts'
import { Simulation } from '../src/sim.ts'
import { conformanceChart, newElk } from './charts.ts'

const here = path.dirname(fileURLToPath(import.meta.url))
const conformance = path.resolve(here, '../../../spec/conformance')
const trace = (name: string) => fs.readFileSync(path.join(conformance, 'cases', name, 'trace.jsonl'), 'utf8')
const ir = conformanceChart('raise')
const reminder = ir.machines.Reminder!
const order = ir.machines.Order!
const state = (m: Machine, id: string): State => {
  const s = findState(m, id)
  if (!s) throw new Error(`no state ${id}`)
  return s
}
const ctx = (m: string) => ({ ir, machine: m, collapsed: new Set<string>() })

describe('model text', () => {
  it('writes raise and cancel as viz --diff does', () => {
    const idle = state(reminder, 'Reminder.Idle')
    expect(idle.transitions[0]!.effects.map(effectText)).toEqual(['raise Remind { attempt: 1 } after 1s as reminder'])
    expect(idle.transitions[1]!.effects.map(effectText)).toEqual(['cancel reminder'])
    expect(state(order, 'Order.Idle').transitions[0]!.effects.map(effectText)).toEqual(['raise Ping { n: 1 }', 'do log("go")'])
    // An unnamed delayed raise.
    expect(state(ir.machines.Lenient!, 'Lenient.A').transitions[1]!.effects.map(effectText)).toEqual(['raise Ping after 1s'])
  })

  it('finds internal events as the machine’s own, and links a raise to its event', () => {
    const found = eventDecl(ir, { machine: 'Reminder', event: 'Remind' })
    expect(found).toMatchObject({ emitted: false, internal: true })
    expect(eventDeclText('Remind', found!.decl, false, true)).toBe('internal event Remind { attempt: int }')
    expect(eventDecl(ir, { machine: 'Reminder', event: 'Start' })).toMatchObject({ emitted: false, internal: false })
    const lines = transitionLines(state(reminder, 'Reminder.Idle'), 0)
    expect(lines[1]).toMatchObject({ cls: 'effect', event: { machine: 'Reminder', event: 'Remind' } })
    // An entry raise is a row linked to the event too.
    const inner = stateRows(state(order, 'Order.Work.Inner'))
    expect(inner[0]).toMatchObject({ kw: 'entry', text: 'raise Pong', event: { machine: 'Order', event: 'Pong' } })
    const raises = messageSites('Order', order).filter((s) => s.dir === 'raise')
    expect(raises.map((s) => [s.state, s.at, s.event.event])).toEqual([
      ['Order.Idle', 0, 'Ping'],
      ['Order.Work.Inner', 'entry', 'Pong'],
      ['Order.Work.Finished', 'entry', 'Ping'],
    ])
  })

  it('draws a raise as a link to its event', async () => {
    const layout = await layoutMachine(ir, 'Reminder', newElk())
    const svg = renderSvg(ir, 'Reminder', layout, { interactive: true })
    expect(svg).toContain('<tspan class="rb-msg-kw">raise</tspan>')
    expect(svg).toContain('data-event="Reminder.Remind"')
  })
})

describe('the details panel', () => {
  it('lists the internal events of a machine, marked as internal', () => {
    const html = panelHtml(ctx('Reminder'), null)
    expect(html).toContain('<h3>Internal events</h3>')
    expect(html).toContain('data-open-event="Reminder.Remind"')
    expect(html).toContain('only this machine raises them')
    // Not among the events it accepts.
    const accepts = html.slice(html.indexOf('<h3>Events it accepts</h3>'), html.indexOf('<h3>Internal events</h3>'))
    expect(accepts).toContain('Reminder.Start')
    expect(accepts).not.toContain('Reminder.Remind')
    expect(panelHtml(ctx('Lenient'), null)).toContain('<h3>Internal events</h3>')
  })

  it('shows an internal event: its declaration, where it is handled and raised', () => {
    const html = panelHtml(ctx('Reminder'), { type: 'event', name: 'Remind' })
    expect(html).toContain('<span class="rb-chip">internal event</span>')
    expect(html).toContain('internal event Remind { attempt: int }')
    expect(html).toContain('Host code and other machines cannot send it')
    expect(html).toContain('<h3>Raised</h3>')
    expect(html).toContain('Reminder · Idle: on Start')
    expect(html).not.toContain('<h3>Sent by</h3>')
  })

  it('explains a raise, a delayed raise and a cancel', () => {
    const idle = panelHtml(ctx('Reminder'), { type: 'transition', state: 'Reminder.Idle', index: 0 })
    expect(idle).toContain('<span class="rb-msg-kw">raise</span>')
    expect(idle).toContain('a timer of the run, not of a state')
    expect(idle).toContain('<code>cancel reminder</code> cancels it')
    const cancel = panelHtml(ctx('Reminder'), { type: 'transition', state: 'Reminder.Idle', index: 1 })
    expect(cancel).toContain('cancels the pending delayed raise named reminder')
    const now = panelHtml(ctx('Order'), { type: 'transition', state: 'Order.Idle', index: 0 })
    expect(now).toContain('on its internal queue')
    const unnamed = panelHtml(ctx('Lenient'), { type: 'transition', state: 'Lenient.A', index: 1 })
    expect(unnamed).toContain('nothing cancels or replaces it')
    // A transition taking an internal event says so.
    const taken = panelHtml(ctx('Reminder'), { type: 'transition', state: 'Reminder.Waiting', index: 0 })
    expect(taken).toContain('an internal event')
    expect(taken).toContain('internal event Remind { attempt: int }')
  })
})

describe('search', () => {
  it('finds internal events and the raises of an event', () => {
    const found = search(buildSearchIndex(ir), 'Remind').filter((e) => e.machine === 'Reminder')
    expect(found.map((e) => e.label)).toContain('internal event Remind of Reminder')
    expect(found.some((e) => e.label.startsWith('raise Remind { attempt: 1 } after 1s as reminder: '))).toBe(true)
  })
})

describe('simulate', () => {
  it('offers no internal event to send, and fires a delayed raise like a timer', () => {
    const sim = new Simulation(ir, 'Reminder')
    expect(sim.start({})).toEqual({ kind: 'ok' })
    expect(sim.choices().events.map((e) => e.name)).toEqual(['Start', 'Stop', 'Again', 'Confirm', 'Boom'])
    expect(sim.send({ _tag: 'Start' })).toEqual({ kind: 'ok' })
    let c = sim.choices()
    expect(c.scheduled).toEqual([{ id: 1, name: 'reminder', delay: 1000, due: 1000, value: { _tag: 'Remind', attempt: 1 } }])
    expect(c.timers.map((t) => t.id)).toEqual([2])
    expect(sim.fire(1)).toEqual({ kind: 'ok' })
    expect(sim.current!.label).toBe('raise Remind fires (timer 1)')
    expect(sim.current!.now).toBe(1000)
    expect(sim.current!.snapshot.vars.attempts).toBe(1)
    c = sim.choices()
    // The transition raised the next reminder and re-entered Waiting, restarting its timer.
    expect(c.scheduled.map((r) => [r.id, r.due, r.value])).toEqual([[3, 2000, { _tag: 'Remind', attempt: 2 }]])
    expect(c.timers.map((t) => [t.id, t.due])).toEqual([[4, 4000]])
    // Advancing fires the delayed raises due on the way, one at a time.
    expect(sim.advance(2500)).toEqual({ kind: 'ok' })
    expect(sim.current!.snapshot.vars.attempts).toBe(3)
    // Cancelling it leaves none pending.
    expect(sim.send({ _tag: 'Stop' })).toEqual({ kind: 'ok' })
    expect(sim.choices().scheduled).toEqual([])
    expect(sim.records().some((r) => r.kind === 'raise.cancelled')).toBe(true)
  })
})

describe('replay', () => {
  it('follows the pending delayed raises of a run', () => {
    const replay = new Replay(ir, trace('raise-replace'))
    expect(replay.problems).toEqual([])
    const at = (kind: string, n = 0) => replay.records.map((r, i) => [r, i] as const).filter(([r]) => r.kind === kind)[n]![1]
    expect(replay.viewAt(at('raise.scheduled'), 'r1').scheduled).toEqual([{ id: 1, name: 'reminder', delay: 1000, value: { _tag: 'Remind', attempt: 1 } }])
    expect(replay.viewAt(at('raise.cancelled'), 'r1').scheduled).toEqual([])
    expect(replay.viewAt(at('raise.scheduled', 1), 'r1').scheduled.map((x) => x.id)).toEqual([3])
    // Its expiry consumes it.
    expect(replay.viewAt(at('raise.scheduled', 1) + 1, 'r1').scheduled).toEqual([])
  })

  it('describes the new records and the raised input', () => {
    expect(recordText({ kind: 'raised', value: { _tag: 'Ping', n: 1 } })).toBe('raised Ping {"n":1}')
    expect(recordText({ kind: 'raise.scheduled', timer: 1, name: 'reminder', delay: 1000, value: { _tag: 'Remind', attempt: 1 } })).toBe(
      'raise Remind {"attempt":1} scheduled: timer 1, 1000 ms as reminder',
    )
    expect(recordText({ kind: 'raise.scheduled', timer: 2, name: null, delay: 1000, value: { _tag: 'Ping' } })).toBe('raise Ping scheduled: timer 2, 1000 ms')
    expect(recordText({ kind: 'raise.cancelled', timer: 1, name: 'reminder' })).toBe('delayed raise cancelled: timer 1 (reminder)')
    expect(inputText({ kind: 'raised', value: { _tag: 'Ping', n: 2 } })).toBe('Ping raised by the run {"n":2}')
    expect(recordText({ kind: 'defect', defect: { kind: 'unhandled', event: { kind: 'after', timer: 1 } } })).toBe(
      'defect: the expiry of the delayed raise of timer 1 enabled no transition in this strict machine',
    )
  })

  it('checks a raised event against the machine’s own events', () => {
    const lines = trace('raise-delayed').replace(/"_tag":"Remind"/, '"_tag":"Nope"')
    const replay = new Replay(ir, lines)
    expect(replay.problems.some((p) => p.endsWith('Reminder has no event Nope'))).toBe(true)
  })
})
