// A chart page: machine tabs and trail, pan and zoom, collapse and expand, selection and the
// details panel, search, and the mode switch. Everything drawn comes from renderSvg(); this
// file only wires up interaction.
//
// Explore is the default mode. Simulate (app/simulate.ts) and replay (app/replay-mode.ts) reuse
// the page: each has its own section of the panel, and decorates the drawn SVG with classes on
// the `data-state` and `data-transition` elements (rb-active, rb-taken, rb-named) and a ring
// around the active states. Diff (app/diff-mode.ts) and coverage (app/coverage-mode.ts) mark
// states and transitions in the SVG itself (renderSvg's `annotate`), and add to the details of
// what is selected. A diff page (`roadbook viz --diff`) draws the merged chart and offers diff
// mode only.

import type { Ir } from '../ir.ts'
import { own } from '../ir.ts'
import type { ElkLike, Layout } from '../layout.ts'
import { defaultCollapsed, layoutMachine } from '../layout.ts'
import { ancestorIds, openingMachine, parseEventKey, transitionId, walkStates } from '../model.ts'
import type { Measure } from '../model.ts'
import { panelHtml } from '../panel.ts'
import type { Selection } from '../panel.ts'
import type { PageData } from '../project.ts'
import { localHref } from '../project.ts'
import { esc, renderLegend, renderMachineHeader, renderSvg } from '../render.ts'
import { expandForDiff } from '../diff.ts'
import { buildSearchIndex, search } from '../search.ts'
import type { SearchEntry } from '../search.ts'
import { $, setupTheme } from './dom.ts'
import type { Controller, Highlight, Host, Mode } from './modes.ts'
import { coverageMode } from './coverage-mode.ts'
import { diffErrorsHtml, diffMode } from './diff-mode.ts'
import { replayMode } from './replay-mode.ts'
import { simulateMode } from './simulate.ts'
import { Viewport } from './viewport.ts'

type ChartData = Extract<PageData, { kind: 'chart' }>

/** How a machine opened from another was reached: drilled into from an invoke or a spawn, or followed. */
type Via = 'invoke' | 'spawn' | 'link'

/** A button's `data-via`: `invoke` or `spawn` when it drills into a child, else a link. */
const via = (b: HTMLElement): Via => (b.dataset.via === 'invoke' || b.dataset.via === 'spawn' ? b.dataset.via : 'link')

const SHELL = `
<header class="rb-top">
  <a class="rb-brand" id="rb-brand">Roadbook<small id="rb-source"></small></a>
  <nav class="rb-tabs" id="rb-tabs" aria-label="Machines"></nav>
  <div class="rb-search" role="search">
    <input id="rb-search" type="search" placeholder="Search states, events, tasks, errors (/)" aria-label="Search" autocomplete="off" spellcheck="false">
    <ul class="rb-results" id="rb-results" role="listbox" hidden></ul>
  </div>
  <div class="rb-modes" id="rb-modes" role="group" aria-label="Mode">
    <button class="rb-mode-btn" type="button" data-mode="explore" title="The chart as written">Explore</button>
    <button class="rb-mode-btn" type="button" data-mode="simulate" title="Run the machine by hand, in the page">Simulate</button>
    <button class="rb-mode-btn" type="button" data-mode="replay" title="Step through a recorded trace">Replay</button>
    <button class="rb-mode-btn" type="button" data-mode="coverage" title="How often each transition was taken, across many traces">Coverage</button>
    <button class="rb-mode-btn" type="button" data-mode="diff" title="What changed between two revisions">Diff</button>
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
      <div class="rb-mode-status" id="rb-mode-status" aria-live="polite" hidden></div>
      <div class="rb-hint">drag to pan · scroll to zoom · click a state or an arrow</div>
    </div>
    <div class="rb-legend" id="rb-legend" aria-label="Legend"></div>
  </div>
  <aside class="rb-panel" id="rb-aside">
    <section class="rb-mode-panel" id="rb-mode-simulate" aria-label="Simulate" hidden></section>
    <section class="rb-mode-panel" id="rb-mode-replay" aria-label="Replay" hidden></section>
    <section class="rb-mode-panel" id="rb-mode-coverage" aria-label="Coverage" hidden></section>
    <section class="rb-mode-panel" id="rb-mode-diff" aria-label="Diff" hidden></section>
    <div id="rb-panel" aria-live="polite"></div>
  </aside>
</main>`

const MODES: Mode[] = ['simulate', 'replay', 'coverage', 'diff']

export function mountChart(data: ChartData, elk: ElkLike, measure: Measure) {
  const ir: Ir = data.ir
  const diff = data.mode === 'diff' ? (data.diff ?? null) : null
  document.body.className = 'rb-app'
  if (diff?.errors.length) {
    // A chart that does not compile at one of the revisions: its diagnostics instead.
    document.body.className = 'rb-index'
    document.title = `${data.entry} · Roadbook`
    document.body.innerHTML =
      `<header class="rb-top"><a class="rb-brand"${data.index ? ` href="${esc(localHref(data.index))}"` : ''}>Roadbook<small>${esc(data.entry)}</small></a><div class="rb-tools"><button class="rb-btn" id="rb-theme" type="button"></button></div></header>` +
      diffErrorsHtml(data.entry, diff)
    setupTheme($(document.body, '#rb-theme'))
    return
  }
  document.body.innerHTML = SHELL
  const root = document.body
  // A diff page draws the merged chart, which is neither version: only diff mode makes sense.
  for (const b of root.querySelectorAll<HTMLElement>('[data-mode]')) b.hidden = diff ? b.dataset.mode !== 'diff' : b.dataset.mode === 'diff'
  const canvas = $(root, '#rb-canvas')
  const aside = $(root, '#rb-aside')
  const panel = $(root, '#rb-panel')
  const modeStatus = $(root, '#rb-mode-status')
  const busy = $(root, '#rb-busy')
  const searchInput = $<HTMLInputElement>(root, '#rb-search')
  const results = $(root, '#rb-results')
  const expandBtn = $(root, '#rb-expand')
  const viewport = new Viewport(canvas)
  const machineNames = Object.keys(ir.machines)
  const searchIndex = buildSearchIndex(ir)

  let current = ''
  /** The machines opened, each with how it was reached from the one before (`invokes ›`, `›`). */
  let trail: { name: string; via: string }[] = []
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
      // A diff opens with what changed in view.
      if (diff) expandForDiff(diff, name, d)
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
    const annotate = controller()?.annotate?.(name, collapsedOf(name)) ?? undefined
    canvas.insertAdjacentHTML('afterbegin', renderSvg(ir, name, l, { measure, collapsed: collapsedOf(name), interactive: true, annotate }))
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
    if (changed) controller()?.render()
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
      .map(
        (m, i) =>
          (i > 0 ? `<span class="rb-sep-text">${esc(m.via)}</span> ` : '') +
          (i === trail.length - 1 ? `<span aria-current="page">${esc(m.name)}</span>` : `<button class="rb-link rb-mono" type="button" data-trail="${i}">${esc(m.name)}</button>`),
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

  /**
   * Opens a machine. `invoke`: drilled into from an invoke, `spawn`: from a spawn or a
   * collection of children, `link`: reached from a ref or a message, all added to the trail;
   * `tab`: a fresh trail.
   */
  function openMachine(name: string, how: Via | 'tab', select: Selection = null) {
    if (!own(ir.machines, name)) return
    if (how === 'tab') trail = [{ name, via: '' }]
    else {
      const at = trail.findIndex((t) => t.name === name)
      trail = at >= 0 ? trail.slice(0, at + 1) : [...trail, { name, via: how === 'invoke' ? 'invokes ›' : how === 'spawn' ? 'spawns ›' : '›' }]
    }
    return show(name, { select })
  }

  /** Opens an event (`Upload.Progress`): selects it, in its own machine. */
  function openEvent(key: string) {
    const ref = parseEventKey(key)
    if (!ref || !own(ir.machines, ref.machine)) return
    const sel: Selection = { type: 'event', name: ref.event }
    if (ref.machine === current) select(sel)
    else void openMachine(ref.machine, 'link', sel)
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
    if (collapse && selection && selection.type !== 'event') {
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
    applyHighlight()
    if (!svg) return
    svg.querySelectorAll('.rb-selected, .rb-match').forEach((el) => el.classList.remove('rb-selected', 'rb-match'))
    for (const m of matches) {
      if (m.machine !== current || !m.target) continue
      if (m.target.type === 'state') stateEl(visibleId(m.target.id))?.classList.add('rb-match')
      else if (m.target.type === 'transition') transitionEls(m.target.state, m.target.index).forEach((el) => el.classList.add('rb-match'))
    }
    if (!selection) return
    if (selection.type === 'state') stateEl(visibleId(selection.id))?.classList.add('rb-selected')
    else if (selection.type === 'transition') transitionEls(selection.state, selection.index).forEach((el) => el.classList.add('rb-selected'))
    else {
      // An event: where it is emitted or sent here, and the transitions it triggers.
      const name = selection.name
      svg.querySelectorAll(`[data-event="${q(`${current}.${name}`)}"]`).forEach((el) => el.classList.add('rb-selected'))
      const m = own(ir.machines, current)!
      walkStates(m.root, (st) =>
        st.transitions.forEach((t, i) => {
          if (t.trigger.kind === 'event' && t.trigger.event === name) transitionEls(st.id, i).forEach((el) => el.classList.add('rb-selected'))
        }),
      )
    }
  }

  function reveal(sel: Selection) {
    if (!sel || !layout) return
    if (sel.type === 'state') viewport.reveal(layout.nodes[visibleId(sel.id)])
    else if (sel.type === 'transition') {
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

  /** Selects a state, transition or event, expanding the collapsed states that hide it. */
  async function goTo(machine: string, sel: Selection, how: 'link' | 'tab' = 'tab') {
    if (machine !== current) await openMachine(machine, how)
    if (sel && sel.type !== 'event') {
      const id = sel.type === 'state' ? sel.id : sel.state
      if (setCollapsed(ancestorIds(id), false)) await draw({ keepView: true })
    }
    select(sel, { reveal: true })
  }

  function renderPanel() {
    // In simulate and replay the panel is the mode's; the details follow when something is selected.
    const details = mode === 'explore' || selection !== null
    panel.hidden = !details
    panel.classList.toggle('rb-details-below', mode !== 'explore')
    const extra = details ? (controller()?.details?.(current, selection) ?? '') : ''
    panel.innerHTML = details ? extra + panelHtml({ ir, machine: current, collapsed: collapsedOf(current) }, selection) : ''
    if (mode === 'explore') aside.scrollTop = 0
  }

  // ───────────── modes ─────────────
  let mode: Mode = 'explore'
  const host: Host = {
    ir,
    current: () => current,
    async open(name, path) {
      if (!own(ir.machines, name)) return
      if (path?.length) trail = path.map((t, i) => ({ name: t.name, via: i ? t.via : '' }))
      else if (!trail.some((t) => t.name === name)) trail = [{ name, via: '' }]
      if (name === current) renderChrome()
      await show(name)
      renderChrome()
    },
    refresh() {
      applyClasses()
      // Keep what the mode is about in view: the state the record names, or the innermost active one.
      const h = controller()?.highlight(current)
      const id = h?.named[0] ?? h?.active[h.active.length - 1]
      if (id && layout && id !== own(ir.machines, current)?.root.id) viewport.reveal(layout.nodes[visibleId(id)])
    },
    setMode: (m) => setMode(m),
    redraw: () => void draw({ keepView: true }),
    select: (sel) => void goTo(current, sel),
  }
  const controllers: Partial<Record<Mode, Controller>> = {}
  const controller = (): Controller | undefined => (mode === 'explore' ? undefined : controllers[mode])
  function makeController(m: Mode): Controller | undefined {
    if (m === 'explore') return undefined
    let c = controllers[m]
    if (!c) {
      const el = $(root, `#rb-mode-${m}`)
      c =
        m === 'simulate'
          ? simulateMode(host, el)
          : m === 'replay'
            ? replayMode(host, el, data.trace ?? null)
            : m === 'coverage'
              ? coverageMode(host, el, data.coverage ?? [])
              : diffMode(host, el, diff ?? { from: '', to: '', status: 'unchanged', errors: [], decls: [], machines: {} })
      controllers[m] = c
    }
    return c
  }
  function setMode(m: Mode) {
    const changed = m !== mode
    // The marks of diff and coverage are drawn into the SVG: draw again when they come or go.
    const marked = (x: Mode) => x === 'diff' || x === 'coverage'
    const redraw = changed && (marked(m) || marked(mode))
    mode = m
    for (const b of root.querySelectorAll<HTMLElement>('[data-mode]')) b.setAttribute('aria-pressed', String(b.dataset.mode === m))
    for (const x of MODES) $(root, `#rb-mode-${x}`).hidden = x !== m
    document.body.dataset.mode = m
    if (changed) selection = null
    const c = makeController(m)
    renderPanel()
    c?.render()
    if (redraw && current) void draw({ keepView: true })
    else applyClasses()
  }

  /** Marks what the mode lights up: rings around active and named states, heavy taken transitions. */
  function applyHighlight() {
    if (!svg) return
    svg.querySelectorAll('.rb-active, .rb-named, .rb-taken').forEach((el) => el.classList.remove('rb-active', 'rb-named', 'rb-taken'))
    svg.querySelectorAll('.rb-ring').forEach((el) => el.remove())
    const c = controller()
    const h: Highlight | null = c?.highlight(current) ?? null
    modeStatus.hidden = !c
    modeStatus.textContent = c ? c.status() : ''
    if (!h) return
    const ring = (id: string, cls: string, gap: number) => {
      const g = stateEl(visibleId(id))
      const box = g?.querySelector(':scope > .rb-box')
      if (!g || !box) return
      g.classList.add(cls)
      const w = Number(box.getAttribute('width'))
      const ht = Number(box.getAttribute('height'))
      const r = document.createElementNS('http://www.w3.org/2000/svg', 'rect')
      r.setAttribute('class', `rb-ring rb-ring-${cls.slice(3)}`)
      r.setAttribute('x', String(-gap))
      r.setAttribute('y', String(-gap))
      r.setAttribute('width', String(w + 2 * gap))
      r.setAttribute('height', String(ht + 2 * gap))
      r.setAttribute('rx', String(8 + gap))
      g.insertBefore(r, box)
    }
    const root = own(ir.machines, current)?.root.id
    for (const id of h.active) if (id !== root) ring(id, 'rb-active', 4)
    for (const id of h.named) if (id !== root) ring(id, 'rb-named', 9)
    for (const t of h.taken) transitionEls(t.state, t.index).forEach((el) => el.classList.add('rb-taken'))
  }

  // ───────────── events ─────────────
  canvas.addEventListener('click', (ev) => {
    if (viewport.suppressClick) return
    const target = ev.target as Element
    const tog = target.closest<SVGElement>('[data-toggle]')
    if (tog) return void toggle(tog.dataset.toggle!)
    const msg = target.closest<SVGElement>('[data-event]')
    if (msg) return openEvent(msg.dataset.event!)
    const link = target.closest<SVGElement>('.rb-machine-link')
    if (link) return void openMachine(link.dataset.machine!, 'invoke')
    const spawn = target.closest<SVGElement>('.rb-spawn')
    if (spawn) return void openMachine(spawn.dataset.machine!, 'spawn')
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
    if (b && b.dataset.machine !== current) void openMachine(b.dataset.machine!, 'tab')
  })
  $(root, '#rb-trail').addEventListener('click', (ev) => {
    const b = (ev.target as Element).closest<HTMLElement>('[data-trail]')
    if (!b) return
    trail = trail.slice(0, Number(b.dataset.trail) + 1)
    void show(trail[trail.length - 1]!.name)
  })
  // The signature's `ref Machine` types, and the machines of its collections of children, open that machine.
  $(root, '#rb-machine-head').addEventListener('click', (ev) => {
    const b = (ev.target as Element).closest<HTMLElement>('[data-open-machine]')
    if (b) void openMachine(b.dataset.openMachine!, via(b))
  })
  aside.addEventListener('click', (ev) => {
    const b = (ev.target as Element).closest<HTMLElement>('button')
    if (!b) return
    if (b.dataset.mState) return void goTo(current, { type: 'state', id: b.dataset.mState })
    if (b.dataset.mTransition) {
      const [state, index] = JSON.parse(b.dataset.mTransition) as [string, number]
      return void goTo(current, { type: 'transition', state, index })
    }
    if (b.closest('.rb-mode-panel')) return // the mode's own controls
    if (b.dataset.openMachine) {
      // Drilling into an invoke or a spawn, or following a ref, a message or a caller.
      void openMachine(b.dataset.openMachine, via(b))
    } else if (b.dataset.openEvent) openEvent(b.dataset.openEvent)
    else if (b.dataset.goto) {
      const [machine, sel] = JSON.parse(b.dataset.goto) as [string, Selection]
      void goTo(machine, sel, 'link')
    } else if (b.dataset.toggle) void toggle(b.dataset.toggle)
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
  $(root, '#rb-modes').addEventListener('click', (ev) => {
    const b = (ev.target as Element).closest<HTMLElement>('[data-mode]')
    if (b) setMode(b.dataset.mode as Mode)
  })
  document.addEventListener('keydown', (ev) => {
    const t = ev.target as Element
    if (t.closest?.('input, textarea, select') || ev.metaKey || ev.ctrlKey || ev.altKey) return
    const c = controller()
    if (c && (ev.key === 'ArrowRight' || ev.key === 'n' || ev.key === '.')) {
      ev.preventDefault()
      c.forward()
    } else if (c && (ev.key === 'ArrowLeft' || ev.key === 'p' || ev.key === ',')) {
      ev.preventDefault()
      c.back()
    } else if (c && ev.key === 'Home' && c.first) c.first()
    else if (c && ev.key === 'End' && c.last) c.last()
    else if (ev.key === 'f') viewport.fit()
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
    else void openMachine(m.machine, 'tab')
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
  trail = [{ name: first, via: '' }]
  // The page's mode, before the first drawing, so it is drawn with the mode's marks.
  if (data.mode !== 'explore') setMode(MODES.includes(data.mode) ? data.mode : 'explore')
  void show(first).then(() => setMode(MODES.includes(data.mode) ? data.mode : 'explore'))
  window.addEventListener('hashchange', () => {
    const name = fromHash()
    if (own(ir.machines, name) && name !== current) void openMachine(name, 'tab')
  })
}
