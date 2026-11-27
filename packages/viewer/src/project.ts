// The project index: every machine of the charts given to `roadbook viz`, which machine invokes
// or spawns which, and which talk to which: refs held, events sent, emitted events handled, calls made, peers' ends heard. `roadbook viz`
// writes the data (it compiles each entry chart and keeps one entry per machine); this module
// lays it out and draws it. Pure and DOM-free.

import type { ElkNode, ElkExtendedEdge } from 'elkjs/lib/elk-api.js'
import type { ChartDiff, IndexDiff } from './diff.ts'
import type { Ir } from './ir.ts'
import { own } from './ir.ts'
import type { ElkLike, Box, Point } from './layout.ts'
import { METRICS, descendantCount, estimateWidth, invokedMachines, signatureText, sourcePath, sourceText, talks, wrapCode } from './model.ts'
import type { Measure } from './model.ts'
import { esc } from './render.ts'

/** The data of a page `roadbook viz` writes, in `<script type="application/json" id="rb-data">`. */
export type PageData =
  | {
      kind: 'chart'
      /**
       * The mode the page opens in: `explore`; `replay` for a page given a trace; `coverage`
       * for one given traces to count; `diff` for a page of `roadbook viz --diff`, whose IR is
       * the merged chart.
       */
      mode: 'explore' | 'simulate' | 'replay' | 'coverage' | 'diff'
      /** Project path of the entry chart. */
      entry: string
      /** Href of the project index (relative, `./index.html`), or null if there is none. */
      index: string | null
      ir: Ir
      /** A trace to replay (`roadbook viz --trace`): the file's name and its JSON Lines text. */
      trace?: { name: string; text: string } | null
      /** Traces to count (`roadbook viz --coverage`): each file's name and JSON Lines text. */
      coverage?: { name: string; text: string }[]
      /** What changed (`roadbook viz --diff`). */
      diff?: ChartDiff
    }
  | { kind: 'index'; index: ProjectIndex }

/**
 * A link to a file next to the page: `./` and the name, each path segment percent-encoded,
 * so no file name (`javascript:alert(1).html`) can make it anything but a relative path. A
 * href that already starts with `./` (as `roadbook viz` writes them) is kept as it is.
 */
export function localHref(href: string): string {
  if (href.startsWith('./')) return href
  return './' + href.split('/').map(encodeURIComponent).join('/')
}

/** The link to the machine `name` on the page `href`. */
export function machineHref(href: string, name: string): string {
  return `${localHref(href)}#${encodeURIComponent(name)}`
}

export interface ProjectIndex {
  title: string
  /** One page per entry chart. `href`: relative, percent-encoded (`./shop-place_order.html`). */
  pages: { href: string; entry: string }[]
  /** Every machine once, in the order of the pages and then of the IR. */
  machines: IndexMachine[]
  /** For the index of `roadbook viz --diff`: the summary, and how each machine and chart changed. */
  diff?: IndexDiff
}

export interface IndexMachine {
  name: string
  /** `file:line` of the declaration. */
  source: string
  doc: string
  /** `machine Name(params) -> T ! E | F` */
  signature: string
  /** Number of states, not counting the machine itself. */
  states: number
  /** Index in `pages` of the page that draws it. */
  page: number
  /** Indices in `machines` of the machines it invokes. */
  invokes: number[]
  /** The machines it spawns, with its collections of children (`children name: Machine by Key`) that hold them. */
  spawns: { to: number; names: string[] }[]
  /** The machines it holds a `ref` to, with the parameters and variables that hold one. */
  refs: { to: number; names: string[] }[]
  /** The events it sends, by receiving machine (to an invoked or spawned child, a ref). */
  sends: { to: number; events: string[] }[]
  /** The events of other machines' `emits` it handles (`on name.Event`), by sending machine. */
  receives: { from: number; events: string[] }[]
  /** The calls it makes (`invoke inventory.reserve(…)`), by called machine. */
  calls: { to: number; calls: string[] }[]
  /** The machines whose end it hears (`on inventory.ended`), with the parameters and variables it watches them through. */
  ends: { to: number; names: string[] }[]
}

/**
 * The index of a set of pages, each the IR of one entry chart. A machine is identified by its
 * name and source file, so one imported by several entries is listed once, with the first
 * page that draws it. `roadbook viz` builds the same data in Rust; this is the reference.
 */
export function projectIndex(title: string, pages: { href: string; entry: string; ir: Ir }[]): ProjectIndex {
  const machines: IndexMachine[] = []
  const keys = new Map<string, number>()
  const key = (ir: Ir, name: string) => `${sourcePath(ir.sources[own(ir.machines, name)!.span.file])}\u0000${name}`
  pages.forEach((p, page) => {
    for (const [name, m] of Object.entries(p.ir.machines)) {
      const k = key(p.ir, name)
      if (keys.has(k)) continue
      keys.set(k, machines.length)
      machines.push({
        name,
        source: sourceText(p.ir, m.span),
        doc: m.doc,
        signature: signatureText('machine', name, m),
        states: descendantCount(m.root),
        page,
        invokes: [],
        spawns: [],
        refs: [],
        sends: [],
        receives: [],
        calls: [],
        ends: [],
      })
    }
  })
  pages.forEach((p, page) => {
    for (const name of Object.keys(p.ir.machines)) {
      const from = machines[keys.get(key(p.ir, name))!]!
      for (const child of invokedMachines(own(p.ir.machines, name)!)) {
        const to = keys.get(key(p.ir, child))
        if (to !== undefined && !from.invokes.includes(to)) from.invokes.push(to)
      }
      // Each machine is listed once, from the first page that has it: its relations too.
      if (from.page !== page) continue
      const t = talks(p.ir, name)
      const at = (machine: string) => (own(p.ir.machines, machine) ? keys.get(key(p.ir, machine)) : undefined)
      for (const r of t.spawns) {
        const to = at(r.machine)
        if (to !== undefined) from.spawns.push({ to, names: r.names })
      }
      for (const r of t.refs) {
        const to = at(r.machine)
        if (to !== undefined) from.refs.push({ to, names: r.names })
      }
      for (const r of t.sends) {
        const to = at(r.machine)
        if (to !== undefined) from.sends.push({ to, events: r.events })
      }
      for (const r of t.receives) {
        const sender = at(r.machine)
        if (sender !== undefined) from.receives.push({ from: sender, events: r.events })
      }
      for (const r of t.calls) {
        const to = at(r.machine)
        if (to !== undefined) from.calls.push({ to, calls: r.calls })
      }
      for (const r of t.ends) {
        const to = at(r.machine)
        if (to !== undefined) from.ends.push({ to, names: r.names })
      }
    }
  })
  return { title, pages: pages.map(({ href, entry }) => ({ href, entry })), machines }
}

/**
 * The kinds of edge the index draws. `invoke`: A invokes B. `spawn`: A spawns B into a
 * collection. `ref`: A holds a `ref B`. `send`: A sends B events. `call`: A calls B's calls and
 * waits for the answers. `emit`: B emits events A handles, drawn from B to A. `ended`: A hears
 * when B, which it holds a `ref` to, ends (`on name.ended`), drawn from A to B beside the ref.
 */
export type IndexEdgeKind = 'invoke' | 'spawn' | 'ref' | 'send' | 'call' | 'emit' | 'ended'

export interface IndexEdge {
  kind: IndexEdgeKind
  /** Indices in `machines`: the edge runs from `from` to `to`. */
  from: number
  to: number
  /** The label's lines, the first starting with its keyword (`spawns`, `sends`, `calls`, `emits`, `ref`, `on`); none for an invoke. */
  lines: string[]
}

/** Wraps `kw items, …` at `,` so labels stay narrow. */
function labelLines(kw: string, items: string[]): string[] {
  return wrapCode(`${kw} ${items.join(', ')}`, 28)
}

/** Every edge of the index: invokes, spawns and refs first, then sends, then calls, then emitted events, then peers' ends heard. */
export function indexEdges(index: ProjectIndex): IndexEdge[] {
  const out: IndexEdge[] = []
  index.machines.forEach((m, i) => {
    for (const j of m.invokes) out.push({ kind: 'invoke', from: i, to: j, lines: [] })
    for (const r of m.spawns) out.push({ kind: 'spawn', from: i, to: r.to, lines: labelLines('spawns', r.names) })
    for (const r of m.refs) out.push({ kind: 'ref', from: i, to: r.to, lines: labelLines('ref', r.names) })
  })
  index.machines.forEach((m, i) => {
    for (const r of m.sends) out.push({ kind: 'send', from: i, to: r.to, lines: labelLines('sends', r.events) })
  })
  index.machines.forEach((m, i) => {
    for (const r of m.calls) out.push({ kind: 'call', from: i, to: r.to, lines: labelLines('calls', r.calls) })
  })
  index.machines.forEach((m, i) => {
    for (const r of m.receives) out.push({ kind: 'emit', from: r.from, to: i, lines: labelLines('emits', r.events) })
  })
  index.machines.forEach((m, i) => {
    // `ends` is absent from an index written before IR 0.8.
    for (const r of m.ends ?? []) out.push({ kind: 'ended', from: i, to: r.to, lines: labelLines('on', r.names.map((n) => `${n}.ended`)) })
  })
  return out
}

export interface IndexLayout {
  width: number
  height: number
  nodes: Box[]
  edges: (IndexEdge & { points: Point[]; label: Box | null })[]
}

function nodeLines(m: IndexMachine): string[] {
  return wrapCode(m.signature, 44)
}

const LINE = 16

function metaLine(m: IndexMachine, topLevel: boolean): string {
  return `${m.states} states · ${m.source}${topLevel ? ' · top level' : ''}`
}

function nodeSize(m: IndexMachine, measure: Measure, topLevel: boolean): { width: number; height: number } {
  const lines = nodeLines(m)
  const meta = metaLine(m, topLevel)
  const w = Math.max(measure(m.name, METRICS.fontTitle, true), ...lines.map((l) => measure(l, METRICS.fontRow, false)), measure(meta, METRICS.fontEffect, false))
  return { width: Math.ceil(w + 2 * METRICS.padX), height: METRICS.titleH + lines.length * LINE + LINE + 12 }
}

function labelSize(lines: string[], measure: Measure): { width: number; height: number } {
  const M = METRICS
  const w = Math.max(...lines.map((l) => measure(l, M.fontEffect, false)))
  return { width: Math.ceil(w + 2 * M.labelPadX), height: Math.ceil(lines.length * M.effectLineH + 2 * M.labelPadY) }
}

/** A machine is top level when no machine invokes or spawns it: a peer held by `ref` is started by host code, so it is one. */
function topLevel(index: ProjectIndex): Set<number> {
  const invoked = new Set(index.machines.flatMap((m) => [...m.invokes, ...m.spawns.map((r) => r.to)]))
  return new Set(index.machines.map((_m, i) => i).filter((i) => !invoked.has(i)))
}

export async function layoutIndex(index: ProjectIndex, elk: ElkLike, measure: Measure = estimateWidth): Promise<IndexLayout> {
  const all = indexEdges(index)
  const top = topLevel(index)
  const edges: ElkExtendedEdge[] = all.map((e, k) => ({
    id: `e${k}`,
    sources: [`m${e.from}`],
    targets: [`m${e.to}`],
    ...(e.lines.length ? { labels: [{ id: `e${k}/label`, text: e.lines[0]!, ...labelSize(e.lines, measure), layoutOptions: { 'elk.edgeLabels.inline': 'true', 'elk.edgeLabels.placement': 'CENTER' } }] } : {}),
  }))
  const graph: ElkNode = {
    id: 'root',
    layoutOptions: {
      'elk.algorithm': 'layered',
      'elk.direction': 'DOWN',
      'elk.edgeRouting': 'ORTHOGONAL',
      'elk.spacing.nodeNode': '30',
      'elk.spacing.edgeNode': '24',
      'elk.spacing.edgeEdge': '16',
      'elk.spacing.edgeLabel': '6',
      'elk.spacing.labelLabel': '8',
      'elk.layered.spacing.nodeNodeBetweenLayers': '44',
      'elk.layered.spacing.edgeNodeBetweenLayers': '18',
      'elk.layered.spacing.edgeEdgeBetweenLayers': '14',
      'elk.layered.mergeEdges': 'false',
      'elk.padding': '[top=16,left=16,bottom=16,right=16]',
      'elk.layered.considerModelOrder.strategy': 'NODES_AND_EDGES',
    },
    children: index.machines.map((m, i) => ({ id: `m${i}`, ...nodeSize(m, measure, top.has(i)) })),
    edges,
  }
  let out: ElkNode
  try {
    out = await elk.layout(graph)
  } catch {
    delete graph.layoutOptions!['elk.layered.considerModelOrder.strategy']
    out = await elk.layout(graph)
  }
  const nodes = (out.children ?? []).map((c) => ({ x: c.x ?? 0, y: c.y ?? 0, width: c.width ?? 0, height: c.height ?? 0 }))
  const routes = (out.edges ?? []).map((e) => {
    const edge = all[Number(e.id.slice(1))]!
    const points: Point[] = []
    for (const s of e.sections ?? []) points.push(s.startPoint, ...(s.bendPoints ?? []), s.endPoint)
    const l = e.labels?.[0]
    return { ...edge, points, label: l ? { x: l.x ?? 0, y: l.y ?? 0, width: l.width ?? 0, height: l.height ?? 0 } : null }
  })
  return { width: Math.ceil(out.width ?? 0), height: Math.ceil(out.height ?? 0), nodes, edges: routes }
}

const ARROWS: Record<IndexEdgeKind, string> = {
  invoke: '<path d="M0.5,1 L9.5,5 L0.5,9 z"/>',
  // Many children: a double head.
  spawn: '<path d="M0.5,1 L5,5 L0.5,9 z M5,1 L9.5,5 L5,9 z"/>',
  ref: '<path d="M0.5,1 L9.5,5 L0.5,9 z"/>',
  // Messages: an open arrowhead.
  send: '<path d="M1,1 L9.5,5 L1,9"/>',
  // A call waits for its answer: a filled arrowhead, in the message colour.
  call: '<path d="M0.5,1 L9.5,5 L0.5,9 z"/>',
  emit: '<path d="M1,1 L9.5,5 L1,9"/>',
  // A peer's end: an open arrowhead with a bar across the line, the run closing.
  ended: '<path d="M1,1 L9.5,5 L1,9 M1,0.5 L1,9.5"/>',
}

function indexMarkers(): string {
  return (Object.keys(ARROWS) as IndexEdgeKind[])
    .map(
      (k) =>
        `<marker id="rbi-arrow-${k}" class="rb-arrow rb-i-${k}" viewBox="0 0 10 10" refX="9.2" refY="5" markerWidth="9" markerHeight="9" markerUnits="userSpaceOnUse" orient="auto">${ARROWS[k]}</marker>`,
    )
    .join('')
}

/** The machine graph: a box per machine (a link to its page), an arrow per invoke, spawn, ref, kind of message, call and peer's end heard. */
export function renderIndexSvg(index: ProjectIndex, layout: IndexLayout): string {
  const n = (v: number) => Math.round(v * 10) / 10
  const top = topLevel(index)
  let nodes = ''
  index.machines.forEach((m, i) => {
    const b = layout.nodes[i]
    if (!b) return
    const page = index.pages[m.page]
    const href = page ? machineHref(page.href, m.name) : '#'
    const lines = nodeLines(m)
    let t = `<text class="rb-title" x="${n(b.width / 2)}" y="20" text-anchor="middle">${esc(m.name)}</text>`
    lines.forEach((l, k) => {
      t += `<text class="rb-row" x="${METRICS.padX}" y="${METRICS.titleH + 12 + k * LINE}" xml:space="preserve">${esc(l)}</text>`
    })
    t += `<text class="rb-row rb-entry-mark" x="${METRICS.padX}" y="${METRICS.titleH + 12 + lines.length * LINE}">${esc(metaLine(m, top.has(i)))}</text>`
    nodes +=
      `<a class="rb-mnode" href="${esc(href)}" data-machine="${i}"><g class="rb-state rb-kind-atomic" transform="translate(${n(b.x)},${n(b.y)})">` +
      `<title>Open ${esc(m.name)}</title><rect class="rb-box" width="${n(b.width)}" height="${n(b.height)}" rx="8"/>` +
      `<line class="rb-sep" x1="0" x2="${n(b.width)}" y1="${METRICS.titleH}" y2="${METRICS.titleH}"/>${t}</g></a>`
  })
  let edges = ''
  let labels = ''
  const M = METRICS
  for (const e of layout.edges) {
    if (e.points.length < 2) continue
    const d = e.points.map((p, k) => `${k ? 'L' : 'M'}${n(p.x)},${n(p.y)}`).join(' ')
    const a = index.machines[e.from]?.name ?? '?'
    const b = index.machines[e.to]?.name ?? '?'
    const what = {
      invoke: `${a} invokes ${b}`,
      spawn: `${a} spawns ${b}`,
      ref: `${a} holds a ref to ${b}`,
      send: `${a} sends ${b} events`,
      call: `${a} calls ${b} and waits for the answer`,
      emit: `${a} emits events ${b} handles`,
      ended: `${a} hears when ${b} ends`,
    }[e.kind]
    edges += `<g class="rb-edge rb-iedge rb-i-${e.kind}"><title>${esc(what)}</title><path class="rb-edge-line" d="${d}" marker-end="url(#rbi-arrow-${e.kind})"/></g>`
    if (!e.label) continue
    const l = e.label
    let s = `<g class="rb-label rb-ilabel rb-i-${e.kind}"><rect class="rb-label-bg" x="${n(l.x)}" y="${n(l.y)}" width="${n(l.width)}" height="${n(l.height)}" rx="5"/>`
    e.lines.forEach((line, k) => {
      const y = n(l.y + M.labelPadY + k * M.effectLineH + M.effectLineH / 2 + M.fontEffect * 0.34)
      const sp = line.indexOf(' ')
      const body = k === 0 && sp > 0 ? `<tspan class="rb-ikw">${esc(line.slice(0, sp))}</tspan>${esc(line.slice(sp))}` : esc(line)
      s += `<text class="rb-ilabel-text" x="${n(l.x + l.width / 2)}" y="${y}" text-anchor="middle" xml:space="preserve">${body}</text>`
    })
    labels += s + '</g>'
  }
  return (
    `<svg xmlns="http://www.w3.org/2000/svg" class="rb-svg" viewBox="0 0 ${layout.width} ${layout.height}" width="${layout.width}" height="${layout.height}" role="img" aria-label="Which machine invokes, spawns and talks to which">` +
    `<defs>${indexMarkers()}</defs>` +
    `<g class="rb-layer-edges">${edges}</g><g class="rb-layer-states">${nodes}</g><g class="rb-layer-labels">${labels}</g></svg>`
  )
}

/** The arrowheads of the legend's swatches, as `ARROWS` draws them. */
const LEGEND_HEADS: Record<IndexEdgeKind, string> = {
  invoke: '<path d="M36,3 L45,7 L36,11 z"/>',
  spawn: '<path d="M32,3 L38.5,7 L32,11 z M38.5,3 L45,7 L38.5,11 z"/>',
  ref: '<path d="M36,3 L45,7 L36,11 z"/>',
  send: '<path d="M37,3 L45,7 L37,11"/>',
  call: '<path d="M36,3 L45,7 L36,11 z"/>',
  emit: '<path d="M37,3 L45,7 L37,11"/>',
  ended: '<path d="M37,3 L45,7 L37,11 M37,2.5 L37,11.5"/>',
}

/** The legend of the index's edges. HTML. */
export function renderIndexLegend(): string {
  const line = (kind: IndexEdgeKind) =>
    `<svg class="rb-legend-swatch" viewBox="0 0 46 14" width="46" height="14" aria-hidden="true">` +
    `<g class="rb-edge rb-iedge rb-i-${kind}"><path class="rb-edge-line" d="M1,7 L37,7"/></g>` +
    `<g class="rb-arrow rb-i-${kind}">${LEGEND_HEADS[kind]}</g></svg>`
  const item = (kind: IndexEdgeKind, label: string, text: string) => `<li>${line(kind)}<span><code>${esc(label)}</code> ${esc(text)}</span></li>`
  return (
    `<ul class="rb-legend-list rb-index-legend">` +
    item('invoke', 'A → B', 'A invokes B (solid)') +
    item('spawn', 'spawns xs', 'A spawns children B into xs (solid, double head)') +
    item('ref', 'ref name', 'A holds a ref to B in name (dashed)') +
    item('send', 'sends E', 'A sends B the events E') +
    item('call', 'calls c', 'A calls c on B and waits for each answer') +
    item('emit', 'emits E', 'B emits the events E and A handles them (dotted)') +
    item('ended', 'on name.ended', 'A hears when B, which it holds a ref to in name, ends (dashed, barred head)') +
    `</ul>`
  )
}

/**
 * `spawns Session (sessions)`, `ref Inventory (inventory)`, `sends Reserve, Release to Inventory`,
 * `calls reserve on Inventory`, `handles Reserved from Inventory`, `hears when Inventory ends
 * (inventory)`, one per line.
 */
function talksText(index: ProjectIndex, m: IndexMachine): string[] {
  const name = (i: number) => index.machines[i]?.name ?? '?'
  return [
    ...m.spawns.map((r) => `spawns ${name(r.to)} (${r.names.join(', ')})`),
    ...m.refs.map((r) => `ref ${name(r.to)} (${r.names.join(', ')})`),
    ...m.sends.map((r) => `sends ${r.events.join(', ')} to ${name(r.to)}`),
    ...m.calls.map((r) => `calls ${r.calls.join(', ')} on ${name(r.to)}`),
    ...m.receives.map((r) => `handles ${r.events.join(', ')} from ${name(r.from)}`),
    ...(m.ends ?? []).map((r) => `hears when ${name(r.to)} ends (${r.names.join(', ')})`),
  ]
}

/** The table under the graph: every machine with its signature, doc and page. HTML. */
export function renderIndexTable(index: ProjectIndex): string {
  const rows = index.machines.map((m) => {
    const page = index.pages[m.page]
    const href = page ? machineHref(page.href, m.name) : '#'
    const invokes = m.invokes.map((j) => index.machines[j]?.name ?? '?').join(', ')
    const talk = talksText(index, m)
    return (
      `<tr><td><a href="${esc(href)}">${esc(m.name)}</a></td>` +
      `<td><code>${esc(m.signature)}</code>${m.doc ? `<p class="rb-doc">${esc(m.doc).replace(/\n/g, '<br>')}</p>` : ''}</td>` +
      `<td>${m.states}</td><td>${invokes ? `<code>${esc(invokes)}</code>` : ''}</td>` +
      `<td>${talk.map((t) => `<code>${esc(t)}</code>`).join('<br>')}</td>` +
      `<td><code>${esc(m.source)}</code><br><span class="rb-doc">${esc(page ? page.entry : '')}</span></td></tr>`
    )
  })
  return `<table class="rb-table"><thead><tr><th>Machine</th><th>Signature</th><th>States</th><th>Invokes</th><th>Talks to</th><th>Source, page</th></tr></thead><tbody>${rows.join('')}</tbody></table>`
}
