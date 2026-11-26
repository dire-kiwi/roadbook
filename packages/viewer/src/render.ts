// Drawing: IR + layout -> SVG string. Pure and DOM-free, so the browser viewer and static SVG
// made in Node draw exactly the same picture. Colours come from the CSS custom properties in
// style.css; nothing here sets a colour.

import type { Ir, State } from './ir.ts'
import { own } from './ir.ts'
import type { Layout, Point, Box } from './layout.ts'
import { machineView, stateBox } from './layout.ts'
import { METRICS, eventKey, estimateWidth, refMachine, transitionId, transitionLines, triggerKind, walkStates } from './model.ts'
import type { EdgeKind, EventRef, Measure, Row } from './model.ts'
import { DARK, DIAGRAM_CSS, FONTS, LIGHT } from './style.ts'

const KINDS: EdgeKind[] = ['event', 'message', 'done', 'fail', 'after', 'always', 'initial']

export function esc(s: string | number): string {
  return String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;')
}
const n = (v: number) => Math.round(v * 10) / 10

/** An orthogonal polyline with rounded corners. */
function roundedPath(all: Point[], r = 7): string {
  const points = all.filter((p, i) => i === 0 || Math.hypot(p.x - all[i - 1]!.x, p.y - all[i - 1]!.y) > 0.01)
  if (points.length < 2) return ''
  let d = `M${n(points[0]!.x)},${n(points[0]!.y)}`
  for (let i = 1; i < points.length - 1; i++) {
    const p = points[i - 1]!
    const c = points[i]!
    const q = points[i + 1]!
    const inLen = Math.hypot(c.x - p.x, c.y - p.y)
    const outLen = Math.hypot(q.x - c.x, q.y - c.y)
    const k = Math.min(r, inLen / 2, outLen / 2)
    if (k < 0.5) {
      d += ` L${n(c.x)},${n(c.y)}`
      continue
    }
    const a = { x: c.x - ((c.x - p.x) / inLen) * k, y: c.y - ((c.y - p.y) / inLen) * k }
    const b = { x: c.x + ((q.x - c.x) / outLen) * k, y: c.y + ((q.y - c.y) / outLen) * k }
    d += ` L${n(a.x)},${n(a.y)} Q${n(c.x)},${n(c.y)} ${n(b.x)},${n(b.y)}`
  }
  const last = points[points.length - 1]!
  return `${d} L${n(last.x)},${n(last.y)}`
}

/**
 * What a mode adds to a state or transition it is about (diff, coverage): a class on its
 * elements, a short badge (`+`, `×12`, `never`) drawn on the box's or the label's corner, a
 * one-character `glyph` in the left margin of a row inside a state (an internal transition),
 * where a badge would not fit, and a line added to its tooltip.
 */
export interface Mark {
  cls: string
  badge?: string
  glyph?: string
  title?: string
}

export interface Annotations {
  state?(id: string): Mark | null | undefined
  transition?(state: string, index: number): Mark | null | undefined
}

/** A badge: a small pill centred on (x, y), its text in the diagram's font. */
function badge(text: string, x: number, y: number): string {
  const w = Math.max(14, text.length * 6.4 + 8)
  return `<g class="rb-badge" aria-hidden="true"><rect x="${n(x - w / 2)}" y="${n(y - 7.5)}" width="${n(w)}" height="15" rx="7.5"/><text x="${n(x)}" y="${n(y + 3.6)}" text-anchor="middle">${esc(text)}</text></g>`
}

const baseline = (top: number, lineH: number, fontSize: number) => n(top + lineH / 2 + fontSize * 0.34)

/** An attribute naming another machine's event, which the viewer opens on a click. */
const eventAttr = (ref: EventRef) => ` data-event="${esc(eventKey(ref))}"`

/**
 * Text that names another machine's event, marked so the viewer can link it: a `send` or
 * `emit` effect (its keyword stressed), or the `upload.Progress` of a trigger.
 */
function messageText(text: string, ref: EventRef): string {
  const lead = text.length - text.trimStart().length
  const body = text.slice(lead)
  const m = /^(send|emit) /.exec(body)
  const inner = m ? `<tspan class="rb-msg-kw">${m[1]}</tspan>${esc(body.slice(m[1]!.length))}` : esc(body)
  return `${' '.repeat(lead)}<tspan class="rb-msg"${eventAttr(ref)}><title>Open event ${esc(eventKey(ref))}</title>${inner}</tspan>`
}

/** A row's keyword; `on upload.Progress` with the event marked as a link. */
function rowKeyword(r: Row): string {
  if (!r.kw) return ''
  if (r.kwEvent && r.kw.startsWith('on ')) return `<tspan class="rb-kw">on</tspan> ${messageText(r.kw.slice(3), r.kwEvent)} `
  return `<tspan class="rb-kw">${esc(r.kw)}</tspan> `
}

function renderRows(rows: Row[], x0: number, y0: number, measure: Measure, mark?: (index: number) => Mark | null | undefined): string {
  const M = METRICS
  let out = ''
  rows.forEach((r, i) => {
    const top = y0 + M.rowsPadY + i * M.rowH
    const y = baseline(top, M.rowH, M.fontRow)
    const mk = r.transition !== undefined ? mark?.(r.transition) : null
    const cls = ['rb-row', r.cls ? `rb-k-${r.cls}` : '', mk?.cls ?? ''].filter(Boolean).join(' ')
    const data = r.transition !== undefined ? ` data-transition="${r.transition}"` : ''
    const kw = rowKeyword(r)
    if (mk?.glyph && !r.cont) out += `<text class="rb-glyph ${esc(mk.cls)}"${data} x="${n(x0 + 2)}" y="${y}">${esc(mk.glyph)}${mk.title ? `<title>${esc(mk.title)}</title>` : ''}</text>`
    if (r.machine) {
      // A machine invoke: a pill the viewer makes clickable.
      const kwW = measure(r.kw + ' ', M.fontRow, false)
      const textW = measure(r.text, M.fontRow, false)
      const px = x0 + M.padX + kwW
      const pillW = textW + 2 * M.machinePillPad + 12
      const cx = px + pillW - 10
      const cy = top + M.rowH / 2
      out +=
        `<text class="${cls}" x="${n(x0 + M.padX)}" y="${y}">${kw}</text>` +
        `<g class="rb-machine-link" data-machine="${esc(r.machine)}"><title>Open machine ${esc(r.machine)}</title>` +
        `<rect class="rb-pill" x="${n(px)}" y="${n(top + 0.5)}" width="${n(pillW)}" height="${M.rowH - 1}" rx="${(M.rowH - 1) / 2}"/>` +
        `<text class="rb-row rb-pill-text" x="${n(px + M.machinePillPad)}" y="${y}" xml:space="preserve"><tspan class="rb-kw">machine</tspan>${pillRest(r)}</text>` +
        `<path class="rb-pill-chevron" d="M${n(cx - 2)},${n(cy - 3.5)} l3.5,3.5 l-3.5,3.5"/></g>`
    } else if (r.cls === 'collapsed') {
      out += `<text class="${cls}" x="${n(x0 + M.padX)}" y="${y}" xml:space="preserve">${esc(r.text)}</text>`
    } else {
      const text = r.event ? messageText(r.text, r.event) : esc(r.text)
      out += `<text class="${cls}"${data} x="${n(x0 + M.padX)}" y="${y}" xml:space="preserve">${kw}${text}</text>`
    }
  })
  return out
}

/** The pill text after `machine`: the call, then `as name` with `as` as a keyword. */
function pillRest(r: Row): string {
  const rest = r.text.slice('machine'.length)
  const tail = r.alias ? ` as ${r.alias}` : ''
  if (!tail || !rest.endsWith(tail)) return esc(rest)
  return `${esc(rest.slice(0, -tail.length))} <tspan class="rb-kw">as</tspan> <tspan class="rb-alias">${esc(r.alias!)}</tspan>`
}

function renderState(state: State, parent: State | null, collapsed: boolean, g: Box, measure: Measure, interactive: boolean, annotate?: Annotations): string {
  const M = METRICS
  const box = stateBox(state, measure, collapsed)
  const isContainer = state.states.length > 0 && !collapsed
  const inset = state.kind === 'final' ? M.finalInset : 0
  const mark = annotate?.state?.(state.id)
  const cls = [
    'rb-state',
    `rb-kind-${state.kind}`,
    isContainer ? 'rb-container' : 'rb-leaf',
    collapsed ? 'rb-collapsed' : '',
    parent && parent.kind === 'parallel' ? 'rb-region' : '',
    mark?.cls ?? '',
  ]
    .filter(Boolean)
    .join(' ')
  const w = g.width
  const h = g.height
  let s = `<g class="${cls}" data-state="${esc(state.id)}" transform="translate(${n(g.x)},${n(g.y)})">`
  s += `<title>${esc(state.id)}${state.doc ? ' - ' + esc(state.doc) : ''}${mark?.title ? '\n' + esc(mark.title) : ''}</title>`
  s += `<rect class="rb-box" width="${n(w)}" height="${n(h)}" rx="${isContainer ? 11 : 8}"/>`
  if (collapsed) s += `<rect class="rb-box-stack" x="3" y="${n(h)}" width="${n(w - 6)}" height="3" rx="1.5"/>`
  if (state.kind === 'final') {
    s += `<rect class="rb-box-inner" x="${inset}" y="${inset}" width="${n(w - 2 * inset)}" height="${n(h - 2 * inset)}" rx="5"/>`
  }
  const titleY = baseline(inset, box.titleH, M.fontTitle)
  const toggle = interactive && state.states.length > 0
  if (isContainer) {
    const tx = toggle ? M.padX + M.toggle + 6 : M.padX
    s += `<text class="rb-title" x="${tx}" y="${titleY}">${esc(state.name)}</text>`
    if (state.kind === 'parallel') s += `<text class="rb-tag" x="${n(w - M.padX)}" y="${titleY}" text-anchor="end">parallel</text>`
  } else {
    const shift = toggle ? (M.toggle + 6) / 2 : 0
    s += `<text class="rb-title" x="${n(w / 2 + shift)}" y="${titleY}" text-anchor="middle">${esc(state.name)}</text>`
  }
  if (toggle) {
    // Collapse / expand. The sign says which, so it reads without colour.
    const ty = n(inset + (box.titleH - M.toggle) / 2)
    const tx = M.padX - 2
    const c = M.toggle / 2
    const sign = collapsed ? `M${tx + 3.5},${n(+ty + c)} h${M.toggle - 7} M${tx + c},${n(+ty + 3.5)} v${M.toggle - 7}` : `M${tx + 3.5},${n(+ty + c)} h${M.toggle - 7}`
    s +=
      `<g class="rb-toggle" data-toggle="${esc(state.id)}"><title>${collapsed ? 'Expand' : 'Collapse'} ${esc(state.name)}</title>` +
      `<rect x="${tx}" y="${ty}" width="${M.toggle}" height="${M.toggle}" rx="3"/><path d="${sign}"/></g>`
  }
  if (box.rows.length) {
    const sepY = inset + box.titleH
    if (!isContainer) s += `<line class="rb-sep" x1="${inset}" x2="${n(w - inset)}" y1="${sepY}" y2="${sepY}"/>`
    const tr = annotate?.transition
    s += renderRows(box.rows, inset, sepY - (isContainer ? M.rowsPadY : 0), measure, tr ? (i) => tr(state.id, i) : undefined)
  }
  if (mark?.badge) s += badge(mark.badge, w - 6, 0)
  return s + '</g>'
}

function renderLabel(state: State, index: number, kind: EdgeKind, label: Box, mark?: Mark | null): string {
  const M = METRICS
  const lines = transitionLines(state, index)
  const cx = label.x + label.width / 2
  let s = `<g class="rb-label rb-k-${kind}${mark ? ' ' + esc(mark.cls) : ''}" data-state="${esc(state.id)}" data-transition="${index}">`
  if (mark?.title) s += `<title>${esc(mark.title)}</title>`
  s += `<rect class="rb-label-bg" x="${n(label.x)}" y="${n(label.y)}" width="${n(label.width)}" height="${n(label.height)}" rx="5"/>`
  let top = label.y + M.labelPadY
  for (const l of lines) {
    const text = l.event ? messageText(l.text, l.event) : esc(l.text)
    if (l.cls === 'trigger') {
      const guard = l.guard ? `${l.text ? ' ' : ''}<tspan class="rb-guard">${esc(l.guard)}</tspan>` : ''
      s += `<text class="rb-label-trigger" x="${n(cx)}" y="${baseline(top, M.labelLineH, M.fontLabel)}" text-anchor="middle" xml:space="preserve">${text}${guard}</text>`
      top += M.labelLineH
    } else {
      s += `<text class="rb-label-effect" x="${n(cx)}" y="${baseline(top, M.effectLineH, M.fontEffect)}" text-anchor="middle" xml:space="preserve">${text}</text>`
      top += M.effectLineH
    }
  }
  if (mark?.badge) s += badge(mark.badge, label.x + label.width, label.y)
  return s + '</g>'
}

function markers(prefix: string): string {
  return KINDS.map(
    (k) =>
      `<marker id="${prefix}-arrow-${k}" class="rb-arrow rb-k-${k}" viewBox="0 0 10 10" refX="9.2" refY="5" markerWidth="9" markerHeight="9" markerUnits="userSpaceOnUse" orient="auto"><path d="M0.5,1 L9.5,5 L0.5,9 z"/></marker>`,
  ).join('')
}

export interface RenderOptions {
  /** The text measurer the layout used. Default: the character-count estimate. */
  measure?: Measure
  /** Prefix for marker ids, unique per document. Default `rb`. */
  idPrefix?: string
  /** Draw the collapse / expand buttons. Default false. */
  interactive?: boolean
  /** Put the stylesheet inside the SVG, for a standalone `.svg` file. Default false. */
  standalone?: boolean
  /** With `standalone`: `light` or `dark`. Default: follow the system. */
  theme?: 'light' | 'dark'
  /** What a mode marks on states and transitions (diff, coverage). */
  annotate?: Annotations | undefined
}

/**
 * The SVG of one machine, given its layout (`layoutMachine` with the same collapsed states).
 * The `<g>` of every state carries `data-state`, of every arrow and label `data-state` and
 * `data-transition` (its index in the state's `transitions`), so the viewer can hook them up.
 */
export function renderSvg(ir: Ir, machineName: string, layout: Layout, opts: RenderOptions & { collapsed?: ReadonlySet<string> } = {}): string {
  const measure = opts.measure ?? estimateWidth
  const prefix = opts.idPrefix ?? 'rb'
  const machine = own(ir.machines, machineName)
  if (!machine) throw new Error(`no machine ${machineName}`)
  const view = machineView(machine, opts.collapsed)
  const drawn = new Map(view.states.map((d) => [d.state.id, d]))
  const W = layout.width
  const H = layout.height

  let containers = ''
  let leaves = ''
  let edgePaths = ''
  let labels = ''
  let initials = ''
  for (const d of view.states) {
    const g = layout.nodes[d.state.id]
    if (!g) continue
    const svg = renderState(d.state, d.parent, d.collapsed, g, measure, opts.interactive ?? false, opts.annotate)
    if (d.state.states.length && !d.collapsed) containers += svg
    else leaves += svg
  }

  const edge = (id: string, kind: EdgeKind, attrs: string, port: boolean, inner: boolean, mk?: Mark | null) => {
    const e = layout.edges[id]
    if (!e || e.points.length < 2) return ''
    const d = roundedPath(e.points)
    // A transition written on a compound state leaves from a dot on the container's border;
    // one from a state hidden inside a collapsed one from a hollow square.
    const p0 = e.points[0]!
    const mark = inner
      ? `<rect class="rb-port rb-port-inner" x="${n(p0.x - 3.5)}" y="${n(p0.y - 3.5)}" width="7" height="7"/>`
      : port
        ? `<circle class="rb-port" cx="${n(p0.x)}" cy="${n(p0.y)}" r="3.5"/>`
        : ''
    return (
      `<g class="rb-edge rb-k-${kind}${inner ? ' rb-from-inside' : ''}${mk ? ' ' + esc(mk.cls) : ''}"${attrs}>` +
      (mk?.title ? `<title>${esc(mk.title)}</title>` : '') +
      `<path class="rb-edge-hit" d="${d}"/>` +
      `<path class="rb-edge-line" d="${d}" marker-end="url(#${prefix}-arrow-${kind})"/>${mark}</g>`
    )
  }

  walkStates(machine.root, (state) => {
    const here = drawn.get(state.id)
    if (state.initial && (here ? !here.collapsed : state.id === machine.root.id)) {
      const m = layout.nodes[`${state.id}/initial`]
      if (m) {
        initials += `<circle class="rb-initial" cx="${n(m.x + m.width / 2)}" cy="${n(m.y + m.height / 2)}" r="${METRICS.initialR}"><title>initial state of ${esc(state.id)}</title></circle>`
        edgePaths += edge(`${state.id}/initial/edge`, 'initial', '', false, false)
      }
    }
    state.transitions.forEach((t, index) => {
      if (t.target === null) return
      const id = transitionId(state.id, index)
      const kind = triggerKind(t.trigger)
      const inner = view.visible(state.id) !== state.id
      const isContainer = state.states.length > 0 && !(here?.collapsed ?? false)
      const mark = opts.annotate?.transition?.(state.id, index)
      edgePaths += edge(id, kind, ` data-state="${esc(state.id)}" data-transition="${index}"`, isContainer, inner, mark)
      const e = layout.edges[id]
      if (e && e.label) labels += renderLabel(state, index, kind, e.label, mark)
    })
  })

  const style = opts.standalone ? `<style>${standaloneStyle(opts.theme)}</style>` : ''
  return (
    `<svg xmlns="http://www.w3.org/2000/svg" class="rb-svg" viewBox="0 0 ${W} ${H}" width="${W}" height="${H}" role="img" aria-label="State diagram of machine ${esc(machineName)}">` +
    style +
    `<defs>${markers(prefix)}</defs>` +
    (opts.standalone ? `<rect class="rb-backdrop" width="${W}" height="${H}"/>` : '') +
    `<g class="rb-layer-containers">${containers}</g>` +
    `<g class="rb-layer-edges">${edgePaths}</g>` +
    `<g class="rb-layer-states">${leaves}${initials}</g>` +
    `<g class="rb-layer-labels">${labels}</g>` +
    `</svg>`
  )
}

/**
 * The diagram's stylesheet for a standalone SVG file, with the custom properties replaced by
 * their values, since not every SVG renderer supports `var()`.
 */
function standaloneStyle(theme?: 'light' | 'dark'): string {
  const resolve = (vars: string) => {
    const values = new Map<string, string>()
    for (const m of (vars + FONTS).matchAll(/(--rb-[a-z-]+):\s*([^;]+);/g)) values.set(m[1]!, m[2]!.trim())
    return (DIAGRAM_CSS + '.rb-backdrop { fill: var(--rb-canvas); }').replace(/var\((--rb-[a-z-]+)\)/g, (_, name: string) => values.get(name) ?? 'currentColor')
  }
  if (theme) return resolve(theme === 'dark' ? DARK : LIGHT)
  return resolve(LIGHT) + `@media (prefers-color-scheme: dark) { ${resolve(DARK)} }`
}

/** Legend of the edge styles and node shapes. HTML; uses the diagram's classes. */
export function renderLegend(opts: { interactive?: boolean } = {}): string {
  const line = (kind: EdgeKind) =>
    `<svg class="rb-legend-swatch" viewBox="0 0 46 14" width="46" height="14" aria-hidden="true">` +
    `<g class="rb-edge rb-k-${kind}"><path class="rb-edge-line" d="M1,7 L37,7"/></g>` +
    `<g class="rb-arrow rb-k-${kind}"><path d="M36,3 L45,7 L36,11 z"/></g></svg>`
  const item = (swatch: string, label: { text: string; cls?: string }, text: string) =>
    `<li>${swatch}<span><code class="${label.cls ?? ''}">${esc(label.text)}</code> ${esc(text)}</span></li>`
  const shape = (inner: string) => `<svg class="rb-legend-swatch" viewBox="0 0 46 18" width="46" height="18" aria-hidden="true">${inner}</svg>`
  return (
    `<ul class="rb-legend-list">` +
    item(line('event'), { text: 'Cancel' }, 'event') +
    item(line('message'), { text: 'upload.Progress', cls: 'rb-t-msg' }, opts.interactive ? 'event emitted by upload, another machine; click the name to open it' : 'event emitted by upload, another machine') +
    item(line('done'), { text: 'done', cls: 'rb-t-done' }, 'invoke or nested states finished') +
    item(line('fail'), { text: 'fail X', cls: 'rb-t-fail' }, 'invoke failed with error X') +
    item(line('after'), { text: 'after 30s' }, 'time in the state (dashed)') +
    item(line('always'), { text: 'always' }, 'no event needed (dotted)') +
    item(
      shape('<g class="rb-label"><rect class="rb-label-bg" x="5" y="2" width="36" height="14" rx="4"/><text class="rb-label-trigger rb-legend-guard" x="23" y="12.5" text-anchor="middle"><tspan class="rb-guard">[x&lt;3]</tspan></text></g>'),
      { text: '[guard]' },
      'condition; branches are tried in order',
    ) +
    item(
      shape('<g class="rb-edge rb-k-initial"><path class="rb-edge-line" d="M12,9 L37,9"/></g><g class="rb-arrow rb-k-initial"><path d="M36,5 L45,9 L36,13 z"/></g><circle class="rb-initial" cx="7" cy="9" r="5.5"/>'),
      { text: 'initial' },
      'state',
    ) +
    item(shape('<g class="rb-state rb-kind-final"><rect class="rb-box" x="4" y="1" width="38" height="16" rx="5"/><rect class="rb-box-inner" x="7" y="4" width="32" height="10" rx="2.5"/></g>'), { text: 'final' }, 'state (double border)') +
    item(
      shape('<g class="rb-label"><rect class="rb-label-bg" x="2" y="2" width="42" height="14" rx="4"/><text class="rb-label-effect rb-legend-guard" x="23" y="12.5" text-anchor="middle"><tspan class="rb-msg"><tspan class="rb-msg-kw">send</tspan></tspan></text></g>'),
      { text: 'send x.E', cls: 'rb-t-msg' },
      'queues E for machine x; emit E tells whoever listens',
    ) +
    item(
      shape('<g class="rb-machine-link"><rect class="rb-pill" x="2" y="2" width="42" height="14" rx="7"/><path class="rb-pill-chevron" d="M33,5.5 l3.5,3.5 l-3.5,3.5"/></g>'),
      { text: 'machine' },
      opts.interactive ? 'invokes another machine; click to open it' : 'invokes another machine',
    ) +
    (opts.interactive
      ? item(
          shape('<g class="rb-state rb-collapsed"><rect class="rb-box" x="4" y="1" width="38" height="13" rx="4"/><rect class="rb-box-stack" x="7" y="14" width="32" height="3" rx="1.5"/></g><g class="rb-toggle"><rect x="8" y="3.5" width="9" height="9" rx="2"/><path d="M10,8 h5 M12.5,5.5 v5"/></g>'),
          { text: '+' },
          'collapsed state; click + to expand',
        )
      : '') +
    `</ul>`
  )
}

/**
 * A list of parameters or fields as HTML: `name: Type`, a `ref Machine` type (or an alias of
 * one) a button that opens that machine.
 */
export function paramsHtml(ir: Ir, params: { name: string; type: string }[]): string {
  return params.map((p) => `${esc(p.name)}: ${typeHtml(ir, p.type)}`).join(', ')
}

/** A type reference as HTML; a `ref` to a machine of the IR links to it. */
export function typeHtml(ir: Ir, type: string): string {
  const to = refMachine(ir, type)
  if (!to || !own(ir.machines, to)) return esc(type)
  return `<button class="rb-link rb-mono rb-ref" type="button" data-open-machine="${esc(to)}" title="Open machine ${esc(to)}">${esc(type)}</button>`
}

/** `machine Order(id: string, inventory: ref Inventory) -> Receipt ! E` as HTML, refs linked. */
export function signatureHtml(ir: Ir, kind: string, name: string, decl: { params: { name: string; type: string }[]; returns?: string | null; errors?: string[]; strict?: boolean }): string {
  let s = `${decl.strict ? 'strict ' : ''}${esc(kind)} ${esc(name)}(${paramsHtml(ir, decl.params)})`
  if (decl.returns) s += ` -&gt; ${esc(decl.returns)}`
  if (decl.errors && decl.errors.length) s += ` ! ${esc(decl.errors.join(' | '))}`
  return s
}

/** The signature and doc comment of a machine. HTML. */
export function renderMachineHeader(ir: Ir, machineName: string): string {
  const m = own(ir.machines, machineName)
  if (!m) return ''
  const doc = m.doc ? `<p class="rb-doc">${esc(m.doc).replace(/\n/g, '<br>')}</p>` : ''
  return `<code class="rb-signature">${signatureHtml(ir, 'machine', machineName, m)}</code>${doc}`
}
