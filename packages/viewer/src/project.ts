// The project index: every machine of the charts given to `roadbook viz`, and which machine
// invokes which. `roadbook viz` writes the data (it compiles each entry chart and keeps one
// entry per machine); this module lays it out and draws it. Pure and DOM-free.

import type { ElkNode, ElkExtendedEdge } from 'elkjs/lib/elk-api.js'
import type { Ir } from './ir.ts'
import type { ElkLike, Box, Point } from './layout.ts'
import { METRICS, descendantCount, estimateWidth, invokedMachines, signatureText, sourceText, wrapCode } from './model.ts'
import type { Measure } from './model.ts'
import { esc } from './render.ts'

/** The data of a page `roadbook viz` writes, in `<script type="application/json" id="rb-data">`. */
export type PageData =
  | {
      kind: 'chart'
      /** The viewer mode. Only `explore` exists yet; simulate, replay, diff and coverage come later. */
      mode: 'explore'
      /** Project path of the entry chart. */
      entry: string
      /** Href of the project index, or null if there is none. */
      index: string | null
      ir: Ir
    }
  | { kind: 'index'; index: ProjectIndex }

export interface ProjectIndex {
  title: string
  /** One page per entry chart. */
  pages: { href: string; entry: string }[]
  /** Every machine once, in the order of the pages and then of the IR. */
  machines: IndexMachine[]
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
}

/**
 * The index of a set of pages, each the IR of one entry chart. A machine is identified by its
 * name and source file, so one imported by several entries is listed once, with the first
 * page that draws it. `roadbook viz` builds the same data in Rust; this is the reference.
 */
export function projectIndex(title: string, pages: { href: string; entry: string; ir: Ir }[]): ProjectIndex {
  const machines: IndexMachine[] = []
  const keys = new Map<string, number>()
  const key = (ir: Ir, name: string) => `${ir.sources[ir.machines[name]!.span.file]?.path}\u0000${name}`
  pages.forEach((p, page) => {
    for (const [name, m] of Object.entries(p.ir.machines)) {
      const k = key(p.ir, name)
      if (keys.has(k)) continue
      keys.set(k, machines.length)
      machines.push({ name, source: sourceText(p.ir, m.span), doc: m.doc, signature: signatureText('machine', name, m), states: descendantCount(m.root), page, invokes: [] })
    }
  })
  for (const p of pages) {
    for (const name of Object.keys(p.ir.machines)) {
      const from = machines[keys.get(key(p.ir, name))!]!
      for (const child of invokedMachines(p.ir.machines[name]!)) {
        const to = keys.get(key(p.ir, child))
        if (to !== undefined && !from.invokes.includes(to)) from.invokes.push(to)
      }
    }
  }
  return { title, pages: pages.map(({ href, entry }) => ({ href, entry })), machines }
}

export interface IndexLayout {
  width: number
  height: number
  nodes: Box[]
  edges: { from: number; to: number; points: Point[] }[]
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

export async function layoutIndex(index: ProjectIndex, elk: ElkLike, measure: Measure = estimateWidth): Promise<IndexLayout> {
  const edges: ElkExtendedEdge[] = []
  const invoked = new Set(index.machines.flatMap((m) => m.invokes))
  index.machines.forEach((m, i) => m.invokes.forEach((j) => edges.push({ id: `${i}->${j}`, sources: [`m${i}`], targets: [`m${j}`] })))
  const graph: ElkNode = {
    id: 'root',
    layoutOptions: {
      'elk.algorithm': 'layered',
      'elk.direction': 'DOWN',
      'elk.edgeRouting': 'ORTHOGONAL',
      'elk.spacing.nodeNode': '30',
      'elk.layered.spacing.nodeNodeBetweenLayers': '44',
      'elk.padding': '[top=16,left=16,bottom=16,right=16]',
      'elk.layered.considerModelOrder.strategy': 'NODES_AND_EDGES',
    },
    children: index.machines.map((m, i) => ({ id: `m${i}`, ...nodeSize(m, measure, !invoked.has(i)) })),
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
    const [from, to] = e.id.split('->').map(Number) as [number, number]
    const points: Point[] = []
    for (const s of e.sections ?? []) points.push(s.startPoint, ...(s.bendPoints ?? []), s.endPoint)
    return { from, to, points }
  })
  return { width: Math.ceil(out.width ?? 0), height: Math.ceil(out.height ?? 0), nodes, edges: routes }
}

/** The machine graph: a box per machine (a link to its page), an arrow per invoke. */
export function renderIndexSvg(index: ProjectIndex, layout: IndexLayout): string {
  const n = (v: number) => Math.round(v * 10) / 10
  const invoked = new Set(index.machines.flatMap((m) => m.invokes))
  let nodes = ''
  index.machines.forEach((m, i) => {
    const b = layout.nodes[i]
    if (!b) return
    const page = index.pages[m.page]
    const href = page ? `${page.href}#${encodeURIComponent(m.name)}` : '#'
    const lines = nodeLines(m)
    let t = `<text class="rb-title" x="${n(b.width / 2)}" y="20" text-anchor="middle">${esc(m.name)}</text>`
    lines.forEach((l, k) => {
      t += `<text class="rb-row" x="${METRICS.padX}" y="${METRICS.titleH + 12 + k * LINE}" xml:space="preserve">${esc(l)}</text>`
    })
    t += `<text class="rb-row rb-entry-mark" x="${METRICS.padX}" y="${METRICS.titleH + 12 + lines.length * LINE}">${esc(metaLine(m, !invoked.has(i)))}</text>`
    nodes +=
      `<a class="rb-mnode" href="${esc(href)}" data-machine="${i}"><g class="rb-state rb-kind-atomic" transform="translate(${n(b.x)},${n(b.y)})">` +
      `<title>Open ${esc(m.name)}</title><rect class="rb-box" width="${n(b.width)}" height="${n(b.height)}" rx="8"/>` +
      `<line class="rb-sep" x1="0" x2="${n(b.width)}" y1="${METRICS.titleH}" y2="${METRICS.titleH}"/>${t}</g></a>`
  })
  let edges = ''
  for (const e of layout.edges) {
    if (e.points.length < 2) continue
    const d = e.points.map((p, k) => `${k ? 'L' : 'M'}${n(p.x)},${n(p.y)}`).join(' ')
    edges += `<g class="rb-edge rb-k-event"><path class="rb-edge-line" d="${d}" marker-end="url(#rbi-arrow)"/></g>`
  }
  return (
    `<svg xmlns="http://www.w3.org/2000/svg" class="rb-svg" viewBox="0 0 ${layout.width} ${layout.height}" width="${layout.width}" height="${layout.height}" role="img" aria-label="Which machine invokes which">` +
    `<defs><marker id="rbi-arrow" class="rb-arrow rb-k-event" viewBox="0 0 10 10" refX="9.2" refY="5" markerWidth="9" markerHeight="9" markerUnits="userSpaceOnUse" orient="auto"><path d="M0.5,1 L9.5,5 L0.5,9 z"/></marker></defs>` +
    `<g class="rb-layer-edges">${edges}</g><g class="rb-layer-states">${nodes}</g></svg>`
  )
}

/** The table under the graph: every machine with its signature, doc and page. HTML. */
export function renderIndexTable(index: ProjectIndex): string {
  const rows = index.machines.map((m) => {
    const page = index.pages[m.page]
    const href = page ? `${page.href}#${encodeURIComponent(m.name)}` : '#'
    const invokes = m.invokes.map((j) => index.machines[j]?.name ?? '?').join(', ')
    return (
      `<tr><td><a href="${esc(href)}">${esc(m.name)}</a></td>` +
      `<td><code>${esc(m.signature)}</code>${m.doc ? `<p class="rb-doc">${esc(m.doc).replace(/\n/g, '<br>')}</p>` : ''}</td>` +
      `<td>${m.states}</td><td>${invokes ? `<code>${esc(invokes)}</code>` : ''}</td>` +
      `<td><code>${esc(m.source)}</code><br><span class="rb-doc">${esc(page ? page.entry : '')}</span></td></tr>`
    )
  })
  return `<table class="rb-table"><thead><tr><th>Machine</th><th>Signature</th><th>States</th><th>Invokes</th><th>Source, page</th></tr></thead><tbody>${rows.join('')}</tbody></table>`
}
