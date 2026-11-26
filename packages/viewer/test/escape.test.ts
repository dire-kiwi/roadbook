// Every piece of IR text the viewer puts into HTML or SVG is escaped: docs, string literals,
// guards, effects, file names and titles holding `</script>`, `<img onerror=…>`, quotes, `&`
// (test/charts/escape.road). Also: links are relative paths, and names from a URL never reach
// Object.prototype.

import { describe, expect, it } from 'vitest'
import { layoutMachine, machineView } from '../src/layout.ts'
import { openingMachine } from '../src/model.ts'
import { panelHtml } from '../src/panel.ts'
import { layoutIndex, localHref, machineHref, projectIndex, renderIndexSvg, renderIndexTable } from '../src/project.ts'
import { renderMachineHeader, renderSvg } from '../src/render.ts'
import { own } from '../src/ir.ts'
import { chart, newElk } from './charts.ts'

const ir = chart('charts-escape')
const machine = 'Escape'

/** Problems in `html`: markup from the IR that was not escaped. */
function unescaped(html: string): string[] {
  const out: string[] = []
  for (const tag of html.match(/<[^>]*>/g) ?? []) {
    if (/onerror|onload|alert\(|<(img|script|svg\s+onload)\b/i.test(tag) && !/^<svg xmlns/.test(tag)) out.push(`tag ${tag}`)
  }
  for (const raw of ['</script>', '<script', '<img', '<svg onload', '<b>bold', '<!--']) if (html.includes(raw)) out.push(`raw ${raw}`)
  // Every & starts an entity.
  for (const m of html.matchAll(/&(?!amp;|lt;|gt;|quot;|#\d+;)/g)) out.push(`bare & at ${html.slice(m.index, m.index + 20)}`)
  return out
}

async function everything(): Promise<string[]> {
  const out: string[] = []
  const m = ir.machines[machine]!
  const collapsed = new Set<string>()
  const layout = await layoutMachine(ir, machine, newElk(), { collapsed })
  out.push(renderSvg(ir, machine, layout, { collapsed, interactive: true }))
  out.push(renderMachineHeader(ir, machine))
  const ctx = { ir, machine, collapsed }
  out.push(panelHtml(ctx, null))
  for (const d of machineView(m).states) {
    out.push(panelHtml(ctx, { type: 'state', id: d.state.id }))
    d.state.transitions.forEach((_t, index) => out.push(panelHtml(ctx, { type: 'transition', state: d.state.id, index })))
  }
  out.push(panelHtml(ctx, { type: 'state', id: m.root.id }))
  m.root.transitions.forEach((_t, index) => out.push(panelHtml(ctx, { type: 'transition', state: m.root.id, index })))
  const index = projectIndex('<b>project</b> & "q"', [{ href: 'javascript:void(0)"<x>.html', entry: '</script><img src=x onerror=alert(9)>.road', ir }])
  out.push(renderIndexTable(index))
  out.push(renderIndexSvg(index, await layoutIndex(index, newElk())))
  return out
}

describe('IR text in HTML and SVG', () => {
  it('is escaped everywhere: diagram, header, panels, index', async () => {
    const all = await everything()
    for (const html of all) expect(unescaped(html)).toEqual([])
    const text = all.join('\n')
    // And it is there, escaped: docs, string literals, guards, effects, an invoke's arguments.
    for (const shown of [
      'Doc &lt;/script&gt;&lt;script&gt;alert(&#39;doc&#39;)&lt;/script&gt;'.replace(/&#39;/g, "'"),
      'state doc &lt;img src=x onerror=alert(5)&gt; &quot;q&quot;',
      'event.who == &quot;&lt;/script&gt;&lt;svg onload=alert(6)&gt;&quot;',
      'work(&quot;&lt;img src=x onerror=alert(7)&gt;&quot;)',
      'set t = &quot;a\\&quot;b&amp;c&lt;d&gt;&quot;',
      'do note(&quot;root &amp; exit&quot;)',
      '&lt;/script&gt;&lt;img src=x onerror=alert(9)&gt;.road',
    ])
      expect(text).toContain(shown)
  })

  it('links every page by a relative, percent-encoded path', () => {
    expect(localHref('javascript:alert(1).html')).toBe('./javascript%3Aalert(1).html')
    expect(localHref('./shop.html')).toBe('./shop.html')
    expect(localHref('a b/c#d.html')).toBe('./a%20b/c%23d.html')
    expect(machineHref('javascript:x', 'A B')).toBe('./javascript%3Ax#A%20B')
    const index = projectIndex('t', [{ href: 'javascript:alert(1).html', entry: 'e', ir }])
    for (const href of renderIndexTable(index).match(/href="[^"]*"/g) ?? []) expect(href).toMatch(/^href="\.\//)
  })
})

describe('names from a URL', () => {
  it('never find members of Object.prototype', () => {
    for (const name of ['constructor', 'toString', '__proto__', 'hasOwnProperty']) {
      expect(own(ir.machines, name)).toBeUndefined()
      expect(openingMachine(ir, name)).toBe(machine)
      expect(renderMachineHeader(ir, name)).toBe('')
      expect(panelHtml({ ir, machine: name, collapsed: new Set() }, null)).toBe('')
    }
    expect(openingMachine(ir, machine)).toBe(machine)
    expect(openingMachine(chart('shop-place_order'), 'Checkout')).toBe('Checkout')
    expect(openingMachine(chart('shop-place_order'), '')).toBe('PlaceOrder')
  })
})
