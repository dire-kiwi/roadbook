import { describe, expect, it } from 'vitest'
import { panelHtml } from '../src/panel.ts'
import { buildSearchIndex, search } from '../src/search.ts'
import { projectIndex } from '../src/project.ts'
import { chart } from './charts.ts'

const shop = chart('shop-place_order')
const ctx = (machine: string) => ({ ir: shop, machine, collapsed: new Set<string>() })

describe('the details panel', () => {
  it('shows a machine: signature, source, callers', () => {
    const html = panelHtml(ctx('Checkout'), null)
    expect(html).toContain('examples/shop/checkout.road:4')
    expect(html).toContain('machine Checkout(order: Order, user: User) -&gt; Receipt ! CardDeclined | GatewayUnavailable')
    expect(html).toContain('data-open-machine="PlaceOrder"')
  })

  it('shows a state: doc, file:line, the invoked task in full, its transitions', () => {
    const html = panelHtml(ctx('Checkout'), { type: 'state', id: 'Checkout.Charging' })
    expect(html).toContain('One attempt against the payment gateway.')
    expect(html).toContain('examples/shop/checkout.road:12')
    expect(html).toMatch(/task chargeCard\(order: Order, user: User\) -&gt; Receipt ! CardDeclined \| GatewayUnavailable/)
    expect(html).toContain('data-select-transition')
  })

  it('shows a transition: trigger, guard, branch, effects, file:line', () => {
    const html = panelHtml(ctx('Checkout'), { type: 'transition', state: 'Checkout.Charging', index: 2 })
    expect(html).toContain('examples/shop/checkout.road:17')
    expect(html).toContain('attempts &lt; 3')
    expect(html).toContain('Branch 1 of 2')
    const effects = panelHtml(ctx('PlaceOrder'), { type: 'transition', state: 'PlaceOrder.Processing.Paying', index: 1 })
    expect(effects).toContain('do audit(&quot;card_declined&quot;, order)')
  })

  it('offers to open an invoked machine, and to expand a collapsed state', () => {
    expect(panelHtml(ctx('PlaceOrder'), { type: 'state', id: 'PlaceOrder.Processing.Paying' })).toContain('data-open-machine="Checkout"')
    const collapsed = { ir: shop, machine: 'PlaceOrder', collapsed: new Set(['PlaceOrder.Processing']) }
    expect(panelHtml(collapsed, { type: 'state', id: 'PlaceOrder.Processing' })).toContain('>Expand Processing<')
  })

  it('falls back to the machine for a selection that does not exist', () => {
    expect(panelHtml(ctx('Checkout'), { type: 'state', id: 'Checkout.Nope' })).toContain('<h3>Signature</h3>')
  })
})

describe('search', () => {
  const index = buildSearchIndex(shop)
  const kinds = (q: string) => search(index, q).map((e) => `${e.kind} ${e.name}`)

  it('finds states, events, tasks, machines, errors and actions by name', () => {
    expect(kinds('paying')[0]).toBe('state Paying')
    expect(kinds('Cancel')).toEqual(expect.arrayContaining(['event Cancel', 'state Cancelled']))
    expect(kinds('chargeCard')).toEqual(['task chargeCard'])
    expect(kinds('checkout')).toEqual(expect.arrayContaining(['machine Checkout']))
    expect(kinds('CardDeclined')).toContain('error CardDeclined')
    expect(kinds('audit')).toContain('action audit')
    expect(kinds('  ')).toEqual([])
  })

  it('ranks exact names first', () => {
    expect(search(index, 'cancel')[0]!.name).toBe('Cancel')
  })
})

describe('the project index', () => {
  it('lists every machine once and which invokes which', () => {
    const ticket = chart('large-ticket')
    const index = projectIndex('t', [
      { href: 'a.html', entry: 'examples/shop/place_order.road', ir: shop },
      { href: 'b.html', entry: 'examples/large/ticket.road', ir: ticket },
      { href: 'c.html', entry: 'again', ir: shop },
    ])
    expect(index.machines.map((m) => m.name)).toEqual(['PlaceOrder', 'Checkout', 'SupportTicket', 'SatisfactionSurvey'])
    const at = (n: string) => index.machines.findIndex((m) => m.name === n)
    expect(index.machines[at('PlaceOrder')]!.invokes).toEqual([at('Checkout')])
    expect(index.machines[at('SupportTicket')]!.invokes).toEqual([at('SatisfactionSurvey')])
    expect(index.machines[at('SupportTicket')]!.page).toBe(1)
    expect(index.machines[at('Checkout')]!.states).toBe(5)
  })
})
