// Drawing: IR + layout -> SVG string. Pure and DOM-free, so the browser viewer and the
// static page generated in Node draw exactly the same picture. All colours come from the
// CSS custom properties in viewer.css; nothing here sets a colour.

import { METRICS, estimateWidth, triggerKind, transitionLines, signatureText } from './model.js'
import { stateBox, drawnStates } from './layout.js'

const KINDS = ['event', 'done', 'fail', 'after', 'always', 'initial']

export function esc(s) {
  return String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;')
}
const n = (v) => Math.round(v * 10) / 10

// Orthogonal polyline with rounded corners.
function roundedPath(all, r = 7) {
  const points = all.filter((p, i) => i === 0 || Math.hypot(p.x - all[i - 1].x, p.y - all[i - 1].y) > 0.01)
  if (points.length < 2) return ''
  let d = `M${n(points[0].x)},${n(points[0].y)}`
  for (let i = 1; i < points.length - 1; i++) {
    const p = points[i - 1], c = points[i], q = points[i + 1]
    const inLen = Math.hypot(c.x - p.x, c.y - p.y)
    const outLen = Math.hypot(q.x - c.x, q.y - c.y)
    const k = Math.min(r, inLen / 2, outLen / 2)
    if (k < 0.5) { d += ` L${n(c.x)},${n(c.y)}`; continue }
    const a = { x: c.x - ((c.x - p.x) / inLen) * k, y: c.y - ((c.y - p.y) / inLen) * k }
    const b = { x: c.x + ((q.x - c.x) / outLen) * k, y: c.y + ((q.y - c.y) / outLen) * k }
    d += ` L${n(a.x)},${n(a.y)} Q${n(c.x)},${n(c.y)} ${n(b.x)},${n(b.y)}`
  }
  const last = points[points.length - 1]
  return `${d} L${n(last.x)},${n(last.y)}`
}

const baseline = (top, lineH, fontSize) => n(top + lineH / 2 + fontSize * 0.34)

function renderRows(box, x0, y0, width, measure) {
  const M = METRICS
  let out = ''
  box.rows.forEach((r, i) => {
    const top = y0 + M.rowsPadY + i * M.rowH
    const y = baseline(top, M.rowH, M.fontRow)
    const cls = ['rb-row', r.cls ? `rb-k-${r.cls}` : ''].filter(Boolean).join(' ')
    const data = r.transition !== undefined ? ` data-transition="${r.transition}"` : ''
    const kw = r.kw ? `<tspan class="rb-kw">${esc(r.kw)}</tspan> ` : ''
    if (r.machine) {
      // Machine invoke: a pill the viewer makes clickable.
      const kwW = measure(r.kw + ' ', M.fontRow, false)
      const textW = measure(r.text, M.fontRow, false)
      const px = x0 + M.padX + kwW
      const pillW = textW + 2 * M.machinePillPad + 12
      const cx = px + pillW - 10, cy = top + M.rowH / 2
      out += `<text class="${cls}" x="${n(x0 + M.padX)}" y="${y}">${kw}</text>`
        + `<g class="rb-machine-link" data-machine="${esc(r.machine)}"><title>Open machine ${esc(r.machine)}</title>`
        + `<rect class="rb-pill" x="${n(px)}" y="${n(top + 0.5)}" width="${n(pillW)}" height="${M.rowH - 1}" rx="${(M.rowH - 1) / 2}"/>`
        + `<text class="rb-row rb-pill-text" x="${n(px + M.machinePillPad)}" y="${y}"><tspan class="rb-kw">machine</tspan>${esc(r.text.slice('machine'.length))}</text>`
        + `<path class="rb-pill-chevron" d="M${n(cx - 2)},${n(cy - 3.5)} l3.5,3.5 l-3.5,3.5"/></g>`
    } else {
      out += `<text class="${cls}"${data} x="${n(x0 + M.padX)}" y="${y}" xml:space="preserve">${kw}${esc(r.text)}</text>`
    }
  })
  return out
}

function renderState(entry, layout, measure) {
  const M = METRICS
  const { state, parent, depth } = entry
  const g = layout.nodes[state.id]
  if (!g) return ''
  const box = stateBox(state, depth, measure)
  const isContainer = state.states.length > 0
  const inset = state.kind === 'final' ? M.finalInset : 0
  const cls = ['rb-state', `rb-kind-${state.kind}`, isContainer ? 'rb-container' : 'rb-leaf',
    parent && parent.kind === 'parallel' ? 'rb-region' : ''].filter(Boolean).join(' ')
  const w = g.width, h = g.height
  let s = `<g class="${cls}" data-state="${esc(state.id)}" transform="translate(${n(g.x)},${n(g.y)})">`
  s += `<title>${esc(state.id)}${state.doc ? ' — ' + esc(state.doc) : ''}</title>`
  s += `<rect class="rb-box" width="${n(w)}" height="${n(h)}" rx="${isContainer ? 11 : 8}"/>`
  if (state.kind === 'final') {
    s += `<rect class="rb-box-inner" x="${inset}" y="${inset}" width="${n(w - 2 * inset)}" height="${n(h - 2 * inset)}" rx="5"/>`
  }
  const titleY = baseline(inset, box.titleH, M.fontTitle)
  if (isContainer) {
    s += `<text class="rb-title" x="${M.padX}" y="${titleY}">${esc(state.name)}</text>`
    if (state.kind === 'parallel') s += `<text class="rb-tag" x="${n(w - M.padX)}" y="${titleY}" text-anchor="end">parallel</text>`
  } else {
    s += `<text class="rb-title" x="${n(w / 2)}" y="${titleY}" text-anchor="middle">${esc(state.name)}</text>`
  }
  if (box.rows.length) {
    const sepY = inset + box.titleH
    if (!isContainer) s += `<line class="rb-sep" x1="${inset}" x2="${n(w - inset)}" y1="${sepY}" y2="${sepY}"/>`
    s += renderRows(box, inset, sepY - (isContainer ? M.rowsPadY : 0), w, measure)
  }
  return s + '</g>'
}

function renderLabel(state, index, kind, label, measure) {
  const M = METRICS
  const lines = transitionLines(state, index)
  const cx = label.x + label.width / 2
  let s = `<g class="rb-label rb-k-${kind}" data-state="${esc(state.id)}" data-transition="${index}">`
  s += `<rect class="rb-label-bg" x="${n(label.x)}" y="${n(label.y)}" width="${n(label.width)}" height="${n(label.height)}" rx="5"/>`
  let top = label.y + M.labelPadY
  for (const l of lines) {
    if (l.cls === 'trigger') {
      const guard = l.guard ? `${l.text ? ' ' : ''}<tspan class="rb-guard">${esc(l.guard)}</tspan>` : ''
      s += `<text class="rb-label-trigger" x="${n(cx)}" y="${baseline(top, M.labelLineH, M.fontLabel)}" text-anchor="middle" xml:space="preserve">${esc(l.text)}${guard}</text>`
      top += M.labelLineH
    } else {
      s += `<text class="rb-label-effect" x="${n(cx)}" y="${baseline(top, M.effectLineH, M.fontEffect)}" text-anchor="middle" xml:space="preserve">${esc(l.text)}</text>`
      top += M.effectLineH
    }
  }
  return s + '</g>'
}

function markers(prefix) {
  return KINDS.map((k) =>
    `<marker id="${prefix}-arrow-${k}" class="rb-arrow rb-k-${k}" viewBox="0 0 10 10" refX="9.2" refY="5" markerWidth="9" markerHeight="9" markerUnits="userSpaceOnUse" orient="auto"><path d="M0.5,1 L9.5,5 L0.5,9 z"/></marker>`,
  ).join('')
}

// renderSvg(ir, machineName, layout, opts) -> string
//   layout: the result of layoutMachine()
//   opts.measure: the same text measurer the layout used (default: character-count estimate)
//   opts.idPrefix: prefix for marker ids, unique per document (default "rb")
export function renderSvg(ir, machineName, layout, opts = {}) {
  const measure = opts.measure ?? ((text, size) => estimateWidth(text, size))
  const prefix = opts.idPrefix ?? 'rb'
  const machine = ir.machines[machineName]
  const states = drawnStates(machine)
  const W = layout.width, H = layout.height

  let containers = '', leaves = '', edgePaths = '', labels = '', initials = ''
  for (const entry of states) {
    const svg = renderState(entry, layout, measure)
    if (entry.state.states.length) containers += svg
    else leaves += svg
  }

  const edge = (id, kind, attrs, fromContainer) => {
    const e = layout.edges[id]
    if (!e || e.points.length < 2) return ''
    const d = roundedPath(e.points)
    // A transition defined on a compound state starts with a dot on the container's border.
    const port = fromContainer ? `<circle class="rb-port" cx="${n(e.points[0].x)}" cy="${n(e.points[0].y)}" r="3.5"/>` : ''
    return `<g class="rb-edge rb-k-${kind}"${attrs}>`
      + `<path class="rb-edge-hit" d="${d}"/>`
      + `<path class="rb-edge-line" d="${d}" marker-end="url(#${prefix}-arrow-${kind})"/>${port}</g>`
  }

  walk(machine.root, (state) => {
    if (state.initial) {
      const m = layout.nodes[`${state.id}/initial`]
      if (m) {
        initials += `<circle class="rb-initial" cx="${n(m.x + m.width / 2)}" cy="${n(m.y + m.height / 2)}" r="${METRICS.initialR}"><title>initial state of ${esc(state.id)}</title></circle>`
        edgePaths += edge(`${state.id}/initial/edge`, 'initial', '')
      }
    }
    state.transitions.forEach((t, index) => {
      if (t.target === null) return
      const id = `${state.id}#${index}`
      const kind = triggerKind(t.trigger)
      edgePaths += edge(id, kind, ` data-state="${esc(state.id)}" data-transition="${index}"`, state.states.length > 0)
      const e = layout.edges[id]
      if (e && e.label) labels += renderLabel(state, index, kind, e.label, measure)
    })
  })

  return `<svg xmlns="http://www.w3.org/2000/svg" class="rb-svg" viewBox="0 0 ${W} ${H}" width="${W}" height="${H}" role="img" aria-label="State diagram of machine ${esc(machineName)}">`
    + `<defs>${markers(prefix)}</defs>`
    + `<g class="rb-layer-containers">${containers}</g>`
    + `<g class="rb-layer-edges">${edgePaths}</g>`
    + `<g class="rb-layer-states">${leaves}${initials}</g>`
    + `<g class="rb-layer-labels">${labels}</g>`
    + `</svg>`
}

function walk(state, fn) {
  fn(state)
  for (const c of state.states) walk(c, fn)
}

// Legend for the edge styles and the node shapes. HTML string; uses the same classes as the diagram.
export function renderLegend(opts = {}) {
  const line = (kind) =>
    `<svg class="rb-legend-swatch" viewBox="0 0 46 14" width="46" height="14" aria-hidden="true">`
    + `<g class="rb-edge rb-k-${kind}"><path class="rb-edge-line" d="M1,7 L37,7"/></g>`
    + `<g class="rb-arrow rb-k-${kind}"><path d="M36,3 L45,7 L36,11 z"/></g></svg>`
  const item = (swatch, label, text) =>
    `<li>${swatch}<span><code class="${label.cls ?? ''}">${esc(label.text)}</code> ${esc(text)}</span></li>`
  const shape = (inner) => `<svg class="rb-legend-swatch" viewBox="0 0 46 18" width="46" height="18" aria-hidden="true">${inner}</svg>`
  return `<ul class="rb-legend-list">`
    + item(line('event'), { text: 'Cancel' }, 'event')
    + item(line('done'), { text: 'done', cls: 'rb-t-done' }, 'invoke or nested states finished')
    + item(line('fail'), { text: 'fail X', cls: 'rb-t-fail' }, 'invoke failed with error X')
    + item(line('after'), { text: 'after 30s' }, 'time in the state (dashed)')
    + item(line('always'), { text: 'always' }, 'no event needed (dotted)')
    + item(shape('<g class="rb-label"><rect class="rb-label-bg" x="5" y="2" width="36" height="14" rx="4"/><text class="rb-label-trigger rb-legend-guard" x="23" y="12.5" text-anchor="middle"><tspan class="rb-guard">[x&lt;3]</tspan></text></g>'), { text: '[guard]' }, 'condition; branches are tried in order')
    + item(shape('<g class="rb-edge rb-k-initial"><path class="rb-edge-line" d="M12,9 L37,9"/></g><g class="rb-arrow rb-k-initial"><path d="M36,5 L45,9 L36,13 z"/></g><circle class="rb-initial" cx="7" cy="9" r="5.5"/>'), { text: 'initial' }, 'state')
    + item(shape('<g class="rb-state rb-kind-final"><rect class="rb-box" x="4" y="1" width="38" height="16" rx="5"/><rect class="rb-box-inner" x="7" y="4" width="32" height="10" rx="2.5"/></g>'), { text: 'final' }, 'state (double border)')
    + item(shape('<g class="rb-machine-link"><rect class="rb-pill" x="2" y="2" width="42" height="14" rx="7"/><path class="rb-pill-chevron" d="M33,5.5 l3.5,3.5 l-3.5,3.5"/></g>'), { text: 'machine' }, opts.interactive ? 'invokes another machine; click to open it' : 'invokes another machine')
    + `</ul>`
}

// Header block for a machine: signature and doc comment. HTML string.
export function renderMachineHeader(ir, machineName) {
  const m = ir.machines[machineName]
  const doc = m.doc ? `<p class="rb-doc">${esc(m.doc).replace(/\n/g, '<br>')}</p>` : ''
  return `<code class="rb-signature">${esc(signatureText('machine', machineName, m))}</code>${doc}`
}
