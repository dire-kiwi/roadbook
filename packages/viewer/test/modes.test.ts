// Simulate and replay, through their models (src/sim.ts, src/replay.ts), against the
// conformance cases: driving the simulation with a case's script, as a reader would click
// through it, writes the case's trace; and every case's trace replays against its chart.

import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { canonicalJson, REDACTED } from '@dire-kiwi/roadbook-core'
import type { Json, Tagged } from '@dire-kiwi/roadbook-core'
import { describe, expect, it } from 'vitest'
import type { Ir } from '../src/ir.ts'
import { findState, signatureText, stateRows } from '../src/model.ts'
import { panelHtml } from '../src/panel.ts'
import { Replay, parseTrace, recordText } from '../src/replay.ts'
import { variablesHtml } from '../src/app/modes.ts'
import { Simulation, refusal } from '../src/sim.ts'
import type { Outcome } from '../src/sim.ts'
import { checkFields, checkValue, sampleFields, sampleValue } from '../src/values.ts'
import { chart } from './charts.ts'

const here = path.dirname(fileURLToPath(import.meta.url))
const conformance = path.resolve(here, '../../../spec/conformance')
const cases = fs.readdirSync(path.join(conformance, 'cases')).sort()

interface Case {
  chart: string
  machine: string
  input: { [k: string]: Json }
  stub: string[]
  fns: { [fn: string]: { args: Json[]; returns: Json }[] }
  actions: { [action: string]: { args: Json; defect: string }[] }
  script: { [k: string]: Json }[]
  status: string
  requires: string[]
}

const readCase = (name: string) => JSON.parse(fs.readFileSync(path.join(conformance, 'cases', name, 'case.json'), 'utf8')) as Case
const readTrace = (name: string) => fs.readFileSync(path.join(conformance, 'cases', name, 'trace.jsonl'), 'utf8')
const caseIr = (c: Case) => JSON.parse(fs.readFileSync(path.join(conformance, c.chart.replace(/\.road$/, '.ir.json')), 'utf8')) as Ir

/**
 * Why the page cannot play a case's script at all, or null: simulate mode runs one run and
 * offers no way to save, restore or suspend it.
 */
const unplayable = (c: Case): string | null =>
  c.script.some((s) => 'snapshot' in s || 'restore' in s || 'suspend' in s)
    ? 'its script saves, restores or suspends runs, which simulate mode does not offer'
    : null

/**
 * Drives a simulation with the case's script, choosing what a reader would choose in the page,
 * and answering the functions from the case's table when the simulation asks. Returns null for
 * a case the page cannot play by hand: one that runs a child machine from the chart, starts
 * other runs, or delivers inputs `together`.
 */
function simulate(c: Case): { sim: Simulation; asked: string[] } | null {
  const ir = caseIr(c)
  if (c.script.some((s) => 'together' in s || 'start' in s || 'run' in s)) return null
  const sim = new Simulation(ir, c.machine, {
    actionDefect: (action, args) => c.actions[action]?.find((a) => canonicalJson(a.args) === canonicalJson(args))?.defect ?? null,
  })
  const asked: string[] = []
  const settle = (o: Outcome): Outcome => {
    while (o.kind === 'ask') {
      const { fn, args } = o.ask
      asked.push(`${fn}(${args.map((a) => canonicalJson(a)).join(', ')})`)
      const entry = c.fns[fn]?.find((e) => canonicalJson(e.args) === canonicalJson(args))
      if (!entry) throw new Error(`the case has no result for ${fn}`)
      sim.answer(fn, args, { value: entry.returns })
      o = sim.retry()
    }
    return o
  }
  const ok = (o: Outcome) => expect(settle(o)).toEqual({ kind: 'ok' })
  ok(sim.start(c.input))
  for (const s of c.script) {
    if ('advance' in s) ok(sim.advance(s.advance as number))
    else if ('event' in s) ok(sim.send(s.event as Tagged))
    else if ('cancel' in s) ok(sim.cancel())
    else if ('task' in s) {
      const choices = sim.choices()
      const inv = choices.invokes.find((i) => i.src === s.task) ?? choices.spawned.find((i) => i.src === s.task)
      // A task of a child run: the page runs only the root.
      if (!inv) return null
      if (('srcKind' in inv ? inv.srcKind === 'machine' : true) && !c.stub.includes(inv.src)) return null
      if ('done' in s) ok(sim.complete(inv.id, { done: s.done as Json }))
      else if ('fail' in s) ok(sim.complete(inv.id, { fail: s.fail as Tagged }))
      else ok(sim.complete(inv.id, { defect: s.defect as string }))
    } else throw new Error(`unknown step ${JSON.stringify(s)}`)
  }
  // A child machine run from the chart, invoked or spawned, would have written records the page does not.
  if (sim.records().some((r) => (r.kind === 'invoke.started' || r.kind === 'spawn.started') && own(ir, r.src) && !c.stub.includes(r.src))) return null
  return { sim, asked }
}

const own = (ir: Ir, machine: string) => Object.prototype.hasOwnProperty.call(ir.machines, machine)
const lines = (records: unknown[]) => records.map((r) => canonicalJson(r) + '\n').join('')

describe('simulate', () => {
  const played: string[] = []
  const skipped = cases.flatMap((name) => {
    const reason = unplayable(readCase(name))
    return reason === null ? [] : [[name, reason]]
  })
  it.skip.each(skipped)('%s: skipped, %s', () => {})
  const playable = cases.filter((name) => unplayable(readCase(name)) === null)
  it.each(playable)('%s: the page writes the case’s trace, or the case is not one it plays', (name) => {
    const c = readCase(name)
    const result = simulate(c)
    if (!result) return
    played.push(name)
    const expected = readTrace(name)
    expect(lines(result.sim.records())).toBe(expected)
    expect(result.sim.current!.snapshot.status).toBe(c.status)
  })

  it('plays at least the cases with a stubbed child machine, timers and asked functions', () => {
    // A child machine treated as a task, a retry loop under virtual time, guards that ask.
    for (const name of ['place-order-checkout-stubbed', 'checkout-retry', 'gate-guards']) expect(played).toContain(name)
    // Parallel states: regions entered and exited in order, events taken in several regions.
    for (const name of ['parallel-entry-exit', 'parallel-two-regions', 'parallel-work']) expect(played).toContain(name)
    // History states: a deep one restoring a final state, one in a region, one while settling.
    for (const name of ['history-final', 'history-region', 'history-settle']) expect(played).toContain(name)
    expect(played.length).toBeGreaterThanOrEqual(8)
  })

  it('asks for each function result once per argument list, and remembers it', () => {
    const result = simulate(readCase('checkout-retry'))!
    expect(result.asked).toEqual(['retryDelay(1)', 'retryDelay(2)'])
    expect([...result.sim.answers.keys()]).toEqual(['retryDelay(1)', 'retryDelay(2)'])
  })

  it('steps back and forward through the history, and a new choice drops what came after', () => {
    const { sim } = simulate(readCase('checkout-retry'))!
    const all = sim.records().length
    const frames = sim.frames.length
    expect(sim.back()).toBe(true)
    expect(sim.records().length).toBeLessThan(all)
    expect(sim.forward()).toBe(true)
    expect(sim.records().length).toBe(all)
    while (sim.back());
    expect(sim.at).toBe(0)
    expect(sim.current!.snapshot.active).toEqual(['Checkout', 'Checkout.Charging'])
    // A different choice from the start: the card is declined.
    expect(sim.complete(1, { fail: { _tag: 'CardDeclined', reason: 'expired' } })).toEqual({ kind: 'ok' })
    expect(sim.frames.length).toBe(2)
    expect(sim.frames.length).toBeLessThan(frames)
    expect(sim.current!.snapshot.status).toBe('failed')
    expect(sim.current!.taken).toEqual([{ state: 'Checkout.Charging', index: 1 }])
  })

  it('offers the events, the invokes with their outcomes and the timers of the current state', () => {
    const c = readCase('place-order-checkout-stubbed')
    const sim = new Simulation(caseIr(c), 'PlaceOrder')
    expect(sim.start(c.input)).toEqual({ kind: 'ok' })
    let choices = sim.choices()
    expect(choices.events.map((e) => [e.name, e.handled])).toEqual([['Cancel', false]])
    expect(choices.invokes.map((i) => [i.src, i.srcKind, i.returns])).toEqual([['authenticate', 'task', 'User']])
    expect(choices.invokes[0]!.errors.map((e) => e.name)).toEqual(['Unauthenticated'])
    sim.complete(1, { done: { id: 'u1', role: 'Customer' } })
    choices = sim.choices()
    expect(choices.events[0]!.handled).toBe(true)
    expect(choices.timers).toEqual([{ id: 1, state: 'PlaceOrder.Processing', index: 1, delay: 30000, due: 30000 }])
    // The timer fires at its due time, once the reader has said what problem(504, "timeout") gives.
    const o = sim.fire(1)
    expect(o.kind === 'ask' && [o.ask.fn, o.ask.args, o.ask.decl?.returns]).toEqual(['problem', [504, 'timeout'], 'ApiResponse'])
    expect(sim.asking?.fn).toBe('problem')
    expect(sim.current!.now).toBe(0)
    sim.answer('problem', [504, 'timeout'], { value: { status: 504, body: { code: 'timeout' } } })
    expect(sim.retry()).toEqual({ kind: 'ok' })
    expect(sim.asking).toBeNull()
    expect(sim.current!.now).toBe(30000)
    expect(sim.current!.snapshot.status).toBe('done')
    expect(sim.current!.snapshot.result).toEqual({ output: { status: 504, body: { code: 'timeout' } } })
  })

  it('treats a child machine as a task, completed by hand with its result or an error', () => {
    const c = readCase('place-order-checkout-stubbed')
    const sim = new Simulation(caseIr(c), 'PlaceOrder')
    sim.start(c.input)
    sim.complete(1, { done: { id: 'u1', role: 'Customer' } })
    sim.complete(2, { done: { id: 'o1', items: [], totalCents: 0 } })
    sim.complete(3, { done: { id: 'o1', items: [], totalCents: 100 } })
    const child = sim.choices().invokes[0]!
    expect([child.src, child.srcKind, child.returns]).toEqual(['Checkout', 'machine', 'Receipt'])
    expect(child.errors.map((e) => e.name)).toEqual(['CardDeclined', 'GatewayUnavailable'])
    // A host defect as the answer ends the run with it.
    expect(sim.complete(child.id, { fail: { _tag: 'GatewayUnavailable' } }).kind).toBe('ask')
    sim.answer('problem', [402, 'payment_failed'], { defect: 'no answer' })
    expect(sim.retry()).toEqual({ kind: 'ok' })
    expect(sim.current!.snapshot.status).toBe('defect')
    expect(sim.current!.records.map((r) => r.kind)).toEqual(['defect'])
  })

  it('plays a machine with a parallel state: examples/large, both regions of Resolving at once', () => {
    const ir = chart('large-ticket')
    expect(refusal(ir, 'SupportTicket')).toBeNull()
    const sim = new Simulation(ir, 'SupportTicket')
    // Functions and invokes are answered with sample values of their declared types.
    const settle = (o: Outcome): Outcome => {
      while (o.kind === 'ask') {
        sim.answer(o.ask.fn, o.ask.args, { value: sampleValue(ir, o.ask.decl!.returns) })
        o = sim.retry()
      }
      return o
    }
    const complete = (src: string) => {
      const inv = sim.choices().invokes.find((i) => i.src === src)!
      expect(settle(sim.complete(inv.id, { done: inv.returns === null ? null : sampleValue(ir, inv.returns) }))).toEqual({ kind: 'ok' })
    }
    expect(settle(sim.start(sampleFields(ir, ir.machines.SupportTicket!.params)))).toEqual({ kind: 'ok' })
    complete('classify')
    complete('routeTicket')
    expect(settle(sim.send({ _tag: 'Resolve' }))).toEqual({ kind: 'ok' })
    const resolving = 'SupportTicket.Active.Resolving'
    expect(sim.current!.snapshot.active.filter((id) => id.startsWith(resolving))).toEqual([
      resolving,
      `${resolving}.FixCheck`,
      `${resolving}.FixCheck.Verifying`,
      `${resolving}.CustomerNotice`,
      `${resolving}.CustomerNotice.Drafting`,
    ])
    // Each region's invoke runs, in document order.
    expect(sim.choices().invokes.map((i) => i.src)).toEqual(['verifyFix', 'draftResolution'])
    complete('draftResolution')
    expect(sim.current!.snapshot.active).toContain(`${resolving}.CustomerNotice.Drafted`)
    expect(sim.current!.snapshot.active).toContain(`${resolving}.FixCheck.Verifying`)
  })

})

describe('values', () => {
  const ir = chart('shop-place_order')
  it('samples every parameter type, and the sample is valid', () => {
    for (const m of Object.values(ir.machines)) {
      const input = sampleFields(ir, m.params)
      expect(checkFields(ir, m.params, input, 'input')).toEqual([])
    }
    expect(sampleValue(ir, 'OrderDraft')).toEqual({ items: [], coupon: null })
    expect(sampleValue(ir, 'Role')).toBe('Customer')
  })
  it('says what is wrong and where', () => {
    const params = ir.machines.PlaceOrder!.params
    expect(checkFields(ir, params, { token: 1, draft: { items: [{ sku: 'a', qty: 1.5 }], coupon: null, x: 1 } }, 'input')).toEqual([
      'input.token: expected a string, got 1',
      'input.draft.items[0].qty: expected an integer within ±9007199254740991, got 1.5',
      'input.draft.x: not a field',
    ])
    expect(checkValue(ir, 'duration', -1)).toEqual(['value: expected a duration: a whole number of milliseconds, 0 or more, got -1'])
    expect(checkValue(ir, 'Role', 'Root')).toEqual(['value: expected one of "Customer", "Admin", got "Root"'])
  })
})

describe('replay', () => {
  it.each(cases)('%s: the expected trace replays against its chart', (name) => {
    const c = readCase(name)
    const ir = caseIr(c)
    const text = readTrace(name)
    const replay = new Replay(ir, text)
    expect(replay.problems).toEqual([])
    expect(replay.warnings).toEqual([])
    expect(replay.records.length).toBe(text.split('\n').length - 1)
    // Every run ends as its last record says, with nothing left active unless it is running.
    for (const run of replay.runs) {
      const view = replay.viewAt(replay.records.length - 1, run.id)
      if (run.id === 'r1') expect(view.status).toBe(c.status)
      if (view.status === 'done' || view.status === 'failed' || view.status === 'cancelled') expect(view.active).toEqual([])
    }
  })

  it('shows what a trace redacted as redacted, and the rest of a value as it was', () => {
    const c = readCase('redact-snapshot')
    const replay = new Replay(caseIr(c), readTrace('redact-snapshot'))
    expect(replay.problems).toEqual([])
    // The restored run goes on from the snapshot as the `restored` record shows it.
    const at = replay.records.findIndex((r) => r.kind === 'restored')
    const view = replay.viewAt(at, 'r2')
    expect(view.input.pin).toBe(REDACTED)
    expect(view.vars.current).toBe(REDACTED)
    expect(view.vars.session).toEqual({ id: 's1', token: REDACTED, expires: 60 })
    const html = variablesHtml(caseIr(c).machines.Vault!, view.input, view.vars, true)
    expect(html).toContain('<dt>pin</dt><dd><span class="rb-redacted"')
    expect(html).toContain('sensitive param')
    expect(html).toContain('&quot;token&quot;:&quot;&lt;redacted&gt;&quot;')
    expect(html).not.toContain('tok-s1')
  })

  it('follows a child run and lights up what each record names', () => {
    const ir = chart('shop-place_order')
    const replay = new Replay(ir, readTrace('place-order-checkout'))
    expect(replay.runs.map((r) => [r.id, r.machine, r.parent])).toEqual([
      ['r1', 'PlaceOrder', null],
      ['r1.4', 'Checkout', 'r1'],
    ])
    const i = replay.records.findIndex((r) => r.kind === 'invoke.started' && r.src === 'Checkout')
    const view = replay.viewAt(i, 'r1')
    expect(view.active).toEqual(['PlaceOrder', 'PlaceOrder.Processing', 'PlaceOrder.Processing.Paying'])
    expect(view.vars.order).toEqual({ id: 'o1', items: [{ sku: 'apple', qty: 2 }], totalCents: 4200 })
    expect(view.input.token).toBe('tok-1')
    expect(view.taken).toEqual({ state: 'PlaceOrder.Processing.Pricing', index: 0 })
    expect(replay.named(i)).toEqual({ states: ['PlaceOrder.Processing.Paying'], transition: null })
    const t = replay.records.findIndex((r) => r.kind === 'transition')
    expect(replay.named(t)).toEqual({ states: ['PlaceOrder.Processing'], transition: { state: 'PlaceOrder.Authenticating', index: 0 } })
    const child = replay.runs[1]!
    expect(replay.viewAt(child.first + 2, 'r1.4').active).toEqual(['Checkout', 'Checkout.Charging'])
  })

  it('warns when the trace was recorded from another version of the chart', () => {
    const ir = chart('shop-place_order')
    const text = readTrace('place-order-checkout').replace(/"ir":"sha256:[0-9a-f]{4}([0-9a-f]*","kind":"started","machine":"Checkout")/, '"ir":"sha256:0000$1')
    const replay = new Replay(ir, text)
    expect(replay.warnings).toHaveLength(1)
    expect(replay.warnings[0]).toMatch(/^Run r1\.4 was recorded from another version of Checkout: the trace has IR sha256:0000/)
    expect(new Replay(ir, text.replaceAll('"machine":"PlaceOrder"', '"machine":"Elsewhere"')).warnings[0]).toMatch(/^Run r1 is of machine Elsewhere, which this chart does not have/)
  })

  it('reports lines it cannot read and records that do not fit the chart', () => {
    expect(parseTrace('{"kind":"entered","run":"r1","t":0}\nnot json\n[1]\n{"kind":"nope","run":"r1"}\n').problems).toEqual([
      'line 2: not JSON (Unexpected token \'o\', "not json" is not valid JSON)',
      'line 3: not a JSON object',
      'line 4: no record kind "nope"',
    ])
    const ir = chart('shop-place_order')
    const text = readTrace('place-order-checkout').replace('"state":"PlaceOrder.Processing.Paying"', '"state":"PlaceOrder.Nowhere"')
    expect(new Replay(ir, text).problems[0]).toMatch(/no state PlaceOrder\.Nowhere in PlaceOrder/)
  })
})

describe('strict machines', () => {
  const ir = caseIr(readCase('strict-ignore'))
  const lock = ir.machines['Lock']!

  it('says strict in the signature and the panel, and shows ignore inside states', () => {
    expect(signatureText('machine', 'Lock', lock)).toBe('strict machine Lock()')
    expect(panelHtml({ ir, machine: 'Lock', collapsed: new Set<string>() }, null)).toContain('Strict: one of these events')
    expect(stateRows(lock.root).map((r) => `${r.kw}|${r.text}`)).toContain('on Knock|[event.loud] ignore')
    expect(stateRows(findState(lock, 'Lock.Closed')!).map((r) => `${r.kw}|${r.text}`)).toContain('on Close|ignore')
    const panel = panelHtml({ ir, machine: 'Lock', collapsed: new Set<string>() }, { type: 'state', id: 'Lock.Closed' })
    expect(panel).toContain('ignores it (internal)')
  })

  it('writes the unhandled defect in words', () => {
    const record = { kind: 'defect', defect: { kind: 'unhandled', event: { kind: 'event', value: { _tag: 'Knock', loud: false } } } }
    expect(recordText(record)).toBe('defect: Knock enabled no transition in this strict machine')
  })

  it('warns in simulate mode that an unhandled event would end the run', () => {
    const sim = new Simulation(ir, 'Lock')
    expect(sim.start({})).toEqual({ kind: 'ok' })
    expect(sim.choices().events.map((e) => [e.name, e.handled])).toEqual([['Open', true], ['Close', true], ['Knock', true]])
    expect(sim.send({ _tag: 'Open' })).toEqual({ kind: 'ok' })
    expect(sim.send({ _tag: 'Close' })).toEqual({ kind: 'ok' })
    expect(sim.send({ _tag: 'Knock', loud: false })).toEqual({ kind: 'ok' })
    expect(sim.current!.snapshot.status).toBe('defect')
  })
})
