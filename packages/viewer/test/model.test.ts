import { describe, expect, it } from 'vitest'
import { findState, stateRows, transitionLines, triggerText, guardText, wrapCode, signatureText, descendantCount } from '../src/model.ts'
import type { Machine, State } from '../src/ir.ts'
import { chart } from './charts.ts'

const shop = chart('shop-place_order')
const ticket = chart('large-ticket')
const stress = chart('charts-stress')
const state = (m: Machine, id: string): State => {
  const s = findState(m, id)
  if (!s) throw new Error(`no state ${id}`)
  return s
}
const lines = (s: State, i: number) => transitionLines(s, i).map((l) => [l.cls, l.text, l.guard].filter(Boolean).join(' '))

describe('transition labels', () => {
  const checkout = shop.machines.Checkout!
  const charging = state(checkout, 'Checkout.Charging')

  it('show the trigger, then each effect on its own line', () => {
    expect(lines(charging, 0)).toEqual(['trigger done', 'effect set receipt = result'])
    expect(lines(charging, 1)).toEqual(['trigger fail CardDeclined', 'effect set declined = error'])
  })

  it('put a long guard on its own line for every branch of the block, with [else] last', () => {
    expect(lines(charging, 2)).toEqual(['trigger fail GatewayUnavailable', 'trigger [attempts < 3]'])
    expect(lines(charging, 3)).toEqual(['trigger fail GatewayUnavailable', 'trigger [else]'])
    expect(guardText(charging, 3)).toBe('[else]')
  })

  it('name every kind of trigger', () => {
    const backoff = state(checkout, 'Checkout.Backoff')
    expect(lines(backoff, 0)).toEqual(['trigger after retryDelay(attempts)'])
    const po = shop.machines.PlaceOrder!
    const processing = state(po, 'PlaceOrder.Processing')
    expect(processing.transitions.map((t) => triggerText(t.trigger))).toEqual(['Cancel', 'after 30s'])
    const paying = state(po, 'PlaceOrder.Processing.Paying')
    expect(lines(paying, 1)).toEqual(['trigger fail CardDeclined', 'effect do audit("card_declined", order)'])
    expect(lines(state(po, 'PlaceOrder.Processing.Confirming'), 0)).toEqual(['trigger always'])
    const start = state(stress.machines.Stress!, 'Stress.Start')
    expect(start.transitions.map((t) => triggerText(t.trigger))).toEqual(['Go', 'Poke', 'Stop', 'on *'])
    const b = state(stress.machines.Stress!, 'Stress.Running.Left.B')
    expect(b.transitions.map((t) => triggerText(t.trigger))).toContain('fail *')
  })
})

describe('state rows', () => {
  const po = shop.machines.PlaceOrder!
  const row = (s: State) => stateRows(s).map((r) => `${r.kw}|${r.text}`)

  it('show the invoke, with the machine for a machine invoke', () => {
    expect(row(state(po, 'PlaceOrder.Authenticating'))).toEqual(['invoke|authenticate(token)'])
    const paying = stateRows(state(po, 'PlaceOrder.Processing.Paying'))
    expect(paying).toEqual([{ kw: 'invoke', text: 'machine Checkout(order, user)', cont: false, machine: 'Checkout' }])
  })

  it('wrap long entry effects with an indented continuation', () => {
    expect(row(state(po, 'PlaceOrder.Processing.Confirming'))).toEqual(['entry|do sendConfirmationEmail(user, order,', '|      receipt)'])
  })

  it('wrap the output of a final state narrower', () => {
    expect(row(state(po, 'PlaceOrder.Unauthorized'))).toEqual(['output|problem(401,', '|       "unauthenticated")'])
    expect(row(state(shop.machines.Checkout!, 'Checkout.Declined'))).toEqual(['fail|declined'])
  })

  it('list internal transitions with their trigger as the keyword', () => {
    const triage = state(ticket.machines.SupportTicket!, 'SupportTicket.Triage')
    expect(row(triage)).toEqual(['after triageSla(priority)|do pageOnCall(ticket, 1)'])
    const active = state(ticket.machines.SupportTicket!, 'SupportTicket.Active')
    expect(row(active)[1]).toBe('on InternalNote|do addNote(ticket, event.text)')
    expect(stateRows(active)[1]!.transition).toBe(1)
    const start = state(stress.machines.Stress!, 'Stress.Start')
    expect(row(start)).toEqual(['on *|do note("ignored")'])
  })

  it('say how many states a collapsed state hides', () => {
    const working = state(ticket.machines.SupportTicket!, 'SupportTicket.Active.Working')
    const rows = stateRows(working, true)
    expect(rows[rows.length - 1]).toMatchObject({ cls: 'collapsed', text: `${descendantCount(working)} states inside (collapsed)` })
  })
})

describe('text helpers', () => {
  it('wrap code at commas, then spaces', () => {
    expect(wrapCode('short')).toEqual(['short'])
    expect(wrapCode('f(aaaaaaaaaa, bbbbbbbbbb, cccccccccc)', 20)).toEqual(['f(aaaaaaaaaa,', '  bbbbbbbbbb,', '  cccccccccc)'])
  })

  it('never wrap inside a string literal, so its spaces show as written', () => {
    const text = 'do note("one  two,  three   four five", x, "a \\" b  c")'
    const lines = wrapCode(text, 20)
    expect(lines).toEqual(['do note("one  two,  three   four five",', '  x, "a \\" b  c")'])
    // Joined back, nothing inside the literals changed.
    expect(lines.map((l, i) => (i ? l.slice(2) : l)).join(' ')).toBe(text)
    // stateRows keeps them too.
    const rows = stateRows({ ...shop.machines.Checkout!.root.states[0]!, invoke: null, entry: [{ kind: 'do', action: 'note', args: [{ text: '"' + 'x  '.repeat(30) + '"' }] }], exit: [], transitions: [], states: [] } as unknown as State)
    expect(rows.map((r) => r.text).join('')).toContain('x  '.repeat(30))
  })

  it('write signatures in source form', () => {
    expect(signatureText('machine', 'Checkout', shop.machines.Checkout!)).toBe(
      'machine Checkout(order: Order, user: User) -> Receipt ! CardDeclined | GatewayUnavailable',
    )
    expect(signatureText('task', 'chargeCard', shop.tasks.chargeCard!)).toMatch(/^task chargeCard\(order: Order, user: User\) -> Receipt ! /)
  })
})
