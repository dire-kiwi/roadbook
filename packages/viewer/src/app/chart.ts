// A chart page in explore mode: machine tabs and trail, pan and zoom, collapse and expand,
// selection and the details panel, search. Everything drawn comes from renderSvg(); this file
// only wires up interaction.
//
// Later modes (simulate and replay in M6, diff and coverage in M7) reuse this page: they add
// their own controls, decorate the drawn SVG (classes on the `data-state` and
// `data-transition` elements) and add sections to the panel.

import type { Ir } from '../ir.ts'
import { own } from '../ir.ts'
import type { ElkLike, Layout } from '../layout.ts'
import { defaultCollapsed, layoutMachine } from '../layout.ts'
import { ancestorIds, openingMachine, transitionId } from '../model.ts'
import type { Measure } from '../model.ts'
import { panelHtml } from '../panel.ts'
import type { Selection } from '../panel.ts'
import type { PageData } from '../project.ts'
import { localHref } from '../project.ts'
import { esc, renderLegend, renderMachineHeader, renderSvg } from '../render.ts'
import { buildSearchIndex, search } from '../search.ts'
import type { SearchEntry } from '../search.ts'
import { $, setupTheme } from './dom.ts'
import { Viewport } from './viewport.ts'

type ChartData = Extract<PageData, { kind: 'chart' }>

const SHELL = `
<header class="rb-top">
  <a class="rb-brand" id="rb-brand">Roadbook<small id="rb-source"></small></a>
  <nav class="rb-tabs" id="rb-tabs" aria-label="Machines"></nav>
  <div class="rb-search" role="search">
    <input id="rb-search" type="search" placeholder="Search states, events, tasks, errors (/)" aria-label="Search" autocomplete="off" spellcheck="false">
    <ul class="rb-results" id="rb-results" role="listbox" hidden></ul>
  </div>
  <div class="rb-tools">
    <button class="rb-btn" id="rb-zoom-out" type="button" title="Zoom out (-)" aria-label="Zoom out">−</button>
    <button class="rb-btn" id="rb-zoom-in" type="button" title="Zoom in (+)" aria-label="Zoom in">+</button>
    <button class="rb-btn" id="rb-fit" type="button" title="Fit the diagram to the view (F)">Fit</button>
    <button class="rb-btn" id="rb-expand" type="button" hidden></button>
    <button class="rb-btn" id="rb-theme" type="button"></button>
  </div>
</header>
<div class="rb-machine-head">
  <div>
    <nav class="rb-trail" id="rb-trail" aria-label="Machines opened"></nav>
    <div id="rb-machine-head"></div>
  </div>
</div>
<main class="rb-main">
  <div class="rb-stage">
    <div class="rb-canvas" id="rb-canvas">
      <div class="rb-busy" id="rb-busy" hidden>Laying out…</div>
      <div class="rb-hint">drag to pan · scroll to zoom · click a state or an arrow</div>
    </div>
    <div class="rb-legend" id="rb-legend" aria-label="Legend"></div>
  </div>
  <aside class="rb-panel" id="rb-panel" aria-live="polite"></aside>
</main>`

export function mountChart(data: ChartData, elk: ElkLike, measure: Measure) {
  const ir: Ir = data.ir
  document.body.className = 'rb-app'
  document.body.innerHTML = SHELL
  const root = document.body
  const canvas = $(root, '#rb-canvas')
  const panel = $(root, '#rb-panel')
  const busy = $(root, '#rb-busy')
  const searchInput = $<HTMLInputElement>(root, '#rb-search')
  const results = $(root, '#rb-results')
  const expandBtn = $(root, '#rb-expand')
  const viewport = new Viewport(canvas)
  const machineNames = Object.keys(ir.machines)
  const searchIndex = buildSearchIndex(ir)

  let current = ''
  let trail: string[] = []
  let selection: Selection = null
  let svg: SVGSVGElement | null = null
  let layout: Layout | null = null
  let matches: SearchEntry[] = []
  const collapsedBy = new Map<string, Set<string>>()
  const defaults = new Map<string, Set<string>>()
  const layouts = new Map<string, Promise<Layout>>()

  const collapsedOf = (name: string) => {
    let set = collapsedBy.get(name)
    if (!set) {
      const d = defaultCollapsed(own(ir.machines, name)!)
      defaults.set(name, d)
      set = new Set(d)
      collapsedBy.set(name, set)
    }
    return set
  }

  function layoutFor(name: string): Promise<Layout> {
    const collapsed = collapsedOf(name)
    const key = `${name}|${[...collapsed].sort().join(',')}`
    let p = layouts.get(key)
    if (!p) {
      p = layoutMachine(ir, name, elk, { measure, collapsed: new Set(collapsed) })
      p.then((l) => {
        if (l.attempt > 0) console.warn(`roadbook: ELK refused the preferred options for ${name}; used fallback ${l.attempt}:`, l.errors)
      })
      layouts.set(key, p)
    }
    return p
  }

  // ───────────── drawing ─────────────
  let drawing = 0
  async function draw(opts: { keepView?: boolean } = {}) {
    const name = current
    const ticket = ++drawing
    busy.hidden = false
    let l: Layout
    try {
      l = await layoutFor(name)
    } catch (e) {
      busy.hidden = true
      canvas.querySelector('.rb-error')?.remove()
      canvas.insertAdjacentHTML('beforeend', `<div class="rb-error">Could not lay out ${esc(name)}:\n${esc(e instanceof Error ? e.message : String(e))}</div>`)
      return
    }
    if (ticket !== drawing) return
    busy.hidden = true
    layout = l
    canvas.querySelector('.rb-error')?.remove()
    svg?.remove()
    canvas.insertAdjacentHTML('afterbegin', renderSvg(ir, name, l, { measure, collapsed: collapsedOf(name), interactive: true }))
    svg = canvas.querySelector<SVGSVGElement>('svg.rb-svg')
    if (svg) viewport.attach(svg, l.width, l.height, opts.keepView)
    applyClasses()
    renderExpandButton()
  }

  async function show(name: string, opts: { select?: Selection; keepView?: boolean } = {}) {
    if (!own(ir.machines, name)) return
    const changed = name !== current
    current = name
    selection = opts.select ?? null
    if (changed) {
      try {
        history.replaceState(null, '', '#' + encodeURIComponent(name))
      } catch {
        /* some file:// setups refuse; the hash is only a convenience */
      }
      document.title = `${name} · Roadbook`
      renderChrome()
    }
    renderPanel()
    await draw({ keepView: !changed && (opts.keepView ?? false) })
    if (selection) reveal(selection)
  }

  function renderChrome() {
    $(root, '#rb-machine-head').innerHTML = renderMachineHeader(ir, current)
    $(root, '#rb-tabs').innerHTML =
      `<span class="rb-tabs-label">${machineNames.length === 1 ? 'Machine' : 'Machines'}</span>` +
      machineNames
        .map((m) => `<button class="rb-tab" type="button" aria-selected="${m === current}" data-machine="${esc(m)}">${esc(m)}</button>`)
        .join('')
    const t = $(root, '#rb-trail')
    t.hidden = trail.length < 2
    t.innerHTML = trail
      .map((m, i) =>
        i === trail.length - 1
          ? `<span aria-current="page">${esc(m)}</span>`
          : `<button class="rb-link rb-mono" type="button" data-trail="${i}">${esc(m)}</button><span class="rb-sep-text">invokes ›</span>`,
      )
      .join(' ')
  }

  function renderExpandButton() {
    const set = collapsedOf(current)
    const d = defaults.get(current) ?? new Set()
    expandBtn.hidden = set.size === 0 && d.size === 0
    expandBtn.textContent = set.size ? 'Expand all' : 'Collapse deep levels'
    expandBtn.title = set.size ? `${set.size} state${set.size === 1 ? ' is' : 's are'} collapsed` : 'Collapse the states this chart opens with collapsed'
  }

  function openMachine(name: string, how: 'invoke' | 'tab') {
    if (!own(ir.machines, name)) return
    if (how === 'invoke') {
      const at = trail.indexOf(name)
      trail = at >= 0 ? trail.slice(0, at + 1) : [...trail, name]
    } else trail = [name]
    void show(name)
  }

  // ───────────── collapse and expand ─────────────
  function setCollapsed(ids: Iterable<string>, collapse: boolean): boolean {
    const set = collapsedOf(current)
    let changed = false
    for (const id of ids) {
      if (collapse === set.has(id)) continue
      if (collapse) set.add(id)
      else set.delete(id)
      changed = true
    }
    return changed
  }

  async function toggle(id: string) {
    const collapse = !collapsedOf(current).has(id)
    setCollapsed([id], collapse)
    // Collapsing hides what was selected inside.
    if (collapse && selection) {
      const sid = selection.type === 'state' ? selection.id : selection.state
      if (sid.startsWith(id + '.')) selection = { type: 'state', id }
    }
    renderPanel()
    await draw({ keepView: true })
    reveal({ type: 'state', id })
  }

  expandBtn.addEventListener('click', async () => {
    const set = collapsedOf(current)
    if (set.size) set.clear()
    else for (const id of defaults.get(current) ?? []) set.add(id)
    renderPanel()
    await draw()
  })

  // ───────────── selection ─────────────
  const q = (s: string) => s.replace(/["\\]/g, '\\$&')
  function transitionEls(stateId: string, index: number | string): Element[] {
    if (!svg) return []
    return [...svg.querySelectorAll(`[data-state="${q(stateId)}"][data-transition="${index}"], [data-state="${q(stateId)}"] .rb-row[data-transition="${index}"]`)]
  }
  function stateEl(id: string): Element | null {
    return svg?.querySelector(`.rb-state[data-state="${q(id)}"]`) ?? null
  }
  /** The drawn box that stands for a state: itself, or the collapsed state hiding it. */
  function visibleId(id: string): string {
    const set = collapsedOf(current)
    return ancestorIds(id).find((a) => set.has(a)) ?? id
  }

  function applyClasses() {
    if (!svg) return
    svg.querySelectorAll('.rb-selected, .rb-match').forEach((el) => el.classList.remove('rb-selected', 'rb-match'))
    for (const m of matches) {
      if (m.machine !== current || !m.target) continue
      if (m.target.type === 'state') stateEl(visibleId(m.target.id))?.classList.add('rb-match')
      else transitionEls(m.target.state, m.target.index).forEach((el) => el.classList.add('rb-match'))
    }
    if (!selection) return
    if (selection.type === 'state') stateEl(visibleId(selection.id))?.classList.add('rb-selected')
    else transitionEls(selection.state, selection.index).forEach((el) => el.classList.add('rb-selected'))
  }

  function reveal(sel: Selection) {
    if (!sel || !layout) return
    if (sel.type === 'state') viewport.reveal(layout.nodes[visibleId(sel.id)])
    else {
      const e = layout.edges[transitionId(sel.state, sel.index)]
      viewport.reveal(e?.label ?? layout.nodes[visibleId(sel.state)])
    }
  }

  function select(sel: Selection, opts: { reveal?: boolean } = {}) {
    selection = sel
    applyClasses()
    renderPanel()
    if (opts.reveal) reveal(sel)
  }

  /** Selects a state or transition, expanding the collapsed states that hide it. */
  async function goTo(machine: string, sel: Selection) {
    if (machine !== current) {
      trail = [machine]
      await show(machine)
    }
    if (sel) {
      const id = sel.type === 'state' ? sel.id : sel.state
      if (setCollapsed(ancestorIds(id), false)) await draw({ keepView: true })
    }
    select(sel, { reveal: true })
  }

  function renderPanel() {
    panel.innerHTML = panelHtml({ ir, machine: current, collapsed: collapsedOf(current) }, selection)
    panel.scrollTop = 0
  }

  // ───────────── events ─────────────
  canvas.addEventListener('click', (ev) => {
    if (viewport.suppressClick) return
    const target = ev.target as Element
    const tog = target.closest<SVGElement>('[data-toggle]')
    if (tog) return void toggle(tog.dataset.toggle!)
    const link = target.closest<SVGElement>('.rb-machine-link')
    if (link) return openMachine(link.dataset.machine!, 'invoke')
    const tr = target.closest<SVGElement>('[data-transition]')
    if (tr) {
      const owner = tr.closest<SVGElement>('[data-state]')
      if (owner) return select({ type: 'transition', state: owner.dataset.state!, index: Number(tr.dataset.transition) })
    }
    const st = target.closest<SVGElement>('[data-state]')
    select(st ? { type: 'state', id: st.dataset.state! } : null)
  })
  canvas.addEventListener('dblclick', (ev) => {
    const st = (ev.target as Element).closest<SVGElement>('.rb-state[data-state]')
    const id = st?.dataset.state
    if (id && st.querySelector(':scope > .rb-toggle')) void toggle(id)
  })
  // Hovering an arrow lights up its label, and the other way round.
  const hover = (ev: Event, on: boolean) => {
    const el = (ev.target as Element).closest?.<SVGElement>('.rb-edge[data-transition], .rb-label[data-transition]')
    if (el) transitionEls(el.dataset.state!, el.dataset.transition!).forEach((e) => e.classList.toggle('rb-hover', on))
  }
  canvas.addEventListener('pointerover', (ev) => hover(ev, true))
  canvas.addEventListener('pointerout', (ev) => hover(ev, false))

  $(root, '#rb-tabs').addEventListener('click', (ev) => {
    const b = (ev.target as Element).closest<HTMLElement>('[data-machine]')
    if (b && b.dataset.machine !== current) openMachine(b.dataset.machine!, 'tab')
  })
  $(root, '#rb-trail').addEventListener('click', (ev) => {
    const b = (ev.target as Element).closest<HTMLElement>('[data-trail]')
    if (!b) return
    trail = trail.slice(0, Number(b.dataset.trail) + 1)
    void show(trail[trail.length - 1]!)
  })
  panel.addEventListener('click', (ev) => {
    const b = (ev.target as Element).closest<HTMLElement>('button')
    if (!b) return
    if (b.dataset.openMachine) openMachine(b.dataset.openMachine, 'invoke')
    else if (b.dataset.toggle) void toggle(b.dataset.toggle)
    else if (b.dataset.selectState) void goTo(current, { type: 'state', id: b.dataset.selectState })
    else if (b.dataset.selectTransition) {
      const [state, index] = JSON.parse(b.dataset.selectTransition) as [string, number]
      void goTo(current, { type: 'transition', state, index })
    } else if (b.dataset.clear !== undefined) select(null)
  })

  $(root, '#rb-fit').addEventListener('click', () => viewport.fit())
  $(root, '#rb-zoom-in').addEventListener('click', () => viewport.zoomBy(1.25))
  $(root, '#rb-zoom-out').addEventListener('click', () => viewport.zoomBy(0.8))
  setupTheme($(root, '#rb-theme'))
  document.addEventListener('keydown', (ev) => {
    const t = ev.target as Element
    if (t.closest('input, textarea, select') || ev.metaKey || ev.ctrlKey || ev.altKey) return
    if (ev.key === 'f') viewport.fit()
    else if (ev.key === '+' || ev.key === '=') viewport.zoomBy(1.25)
    else if (ev.key === '-') viewport.zoomBy(0.8)
    else if (ev.key === '/') {
      ev.preventDefault()
      searchInput.focus()
      searchInput.select()
    } else if (ev.key === 'Escape') select(null)
  })

  // ───────────── search ─────────────
  let active = 0
  function renderResults() {
    const q = searchInput.value
    matches = search(searchIndex, q)
    applyClasses()
    if (!q.trim()) {
      results.hidden = true
      return
    }
    results.hidden = false
    active = Math.min(active, Math.max(matches.length - 1, 0))
    results.innerHTML = matches.length
      ? matches
          .map(
            (m, i) =>
              `<li><button type="button" role="option" data-result="${i}" aria-selected="${i === active}"><span class="rb-what">${m.kind}</span>${esc(m.label)}</button></li>`,
          )
          .join('')
      : `<li class="rb-none">No state, event, task, error or machine matches.</li>`
  }
  function choose(i: number) {
    const m = matches[i]
    if (!m) return
    results.hidden = true
    if (m.target) void goTo(m.machine, m.target)
    else openMachine(m.machine, 'tab')
  }
  searchInput.addEventListener('input', () => {
    active = 0
    renderResults()
  })
  searchInput.addEventListener('focus', () => searchInput.value && renderResults())
  searchInput.addEventListener('keydown', (ev) => {
    if (ev.key === 'ArrowDown' || ev.key === 'ArrowUp') {
      ev.preventDefault()
      active = Math.max(0, Math.min(matches.length - 1, active + (ev.key === 'ArrowDown' ? 1 : -1)))
      renderResults()
      results.querySelector('[aria-selected="true"]')?.scrollIntoView({ block: 'nearest' })
    } else if (ev.key === 'Enter') choose(active)
    else if (ev.key === 'Escape') {
      searchInput.value = ''
      renderResults()
      searchInput.blur()
    }
  })
  results.addEventListener('mousedown', (ev) => ev.preventDefault()) // keep focus in the input
  results.addEventListener('click', (ev) => {
    const b = (ev.target as Element).closest<HTMLElement>('[data-result]')
    if (b) choose(Number(b.dataset.result))
  })
  searchInput.addEventListener('blur', () => (results.hidden = true))

  // ───────────── start ─────────────
  const brand = $<HTMLAnchorElement>(root, '#rb-brand')
  if (data.index) {
    brand.href = localHref(data.index)
    brand.title = 'All machines of the project'
  }
  $(root, '#rb-source').textContent = data.entry || (ir.sources[0]?.path ?? '')
  $(root, '#rb-legend').innerHTML = renderLegend({ interactive: true })
  const fromHash = () => {
    try {
      return decodeURIComponent(location.hash.slice(1))
    } catch {
      return ''
    }
  }
  const first = openingMachine(ir, fromHash())
  if (!first) {
    panel.innerHTML = '<p class="rb-empty">This chart declares no machine.</p>'
    return
  }
  trail = [first]
  void show(first)
  window.addEventListener('hashchange', () => {
    const name = fromHash()
    if (own(ir.machines, name) && name !== current) openMachine(name, 'tab')
  })
}
