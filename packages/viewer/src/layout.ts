// Layout: an IR machine -> an ELK graph -> boxes and edge routes in absolute coordinates.
// DOM-free. The caller passes the ELK instance (elkjs works the same in Node and in the browser),
// so tests can give one that fails on purpose.

import type { ElkExtendedEdge, ElkNode, LayoutOptions as ElkOptions } from 'elkjs/lib/elk-api.js'
import type { Machine, State, Ir } from './ir.ts'
import { own } from './ir.ts'
import { METRICS, ancestorIds, descendantCount, estimateWidth, isCopy, isHistory, rootIsVisible, stateRows, targetEdgeId, transitionId, transitionLines, walkStates } from './model.ts'
import type { Measure, Row, LabelLine } from './model.ts'

/** What `layoutMachine` needs of elkjs. */
export interface ElkLike {
  layout(graph: ElkNode): Promise<ElkNode>
}

export interface LayoutOptions {
  /** Text measurer. Default: a character-count estimate. */
  measure?: Measure
  /** Ids of compound and parallel states drawn collapsed, their children hidden. */
  collapsed?: ReadonlySet<string>
}

export interface Point {
  x: number
  y: number
}

export interface Box {
  x: number
  y: number
  width: number
  height: number
}

export interface EdgeRoute {
  points: Point[]
  label: Box | null
}

export interface Layout {
  machine: string
  width: number
  height: number
  /** Drawn states and `<id>/initial` markers. */
  nodes: Record<string, Box>
  /** Transitions (`<state id>#<index>`) and `<id>/initial/edge` arrows. */
  edges: Record<string, EdgeRoute>
  /** Which entry of `ATTEMPTS` succeeded: 0 unless ELK refused the preferred options. */
  attempt: number
  /** The messages of the attempts that failed. */
  errors: string[]
}

// Options for the root and for every compound state (ELK does not inherit them).
export const ELK_OPTIONS: ElkOptions = {
  'elk.algorithm': 'layered',
  'elk.direction': 'DOWN',
  'elk.hierarchyHandling': 'INCLUDE_CHILDREN',
  'elk.edgeRouting': 'ORTHOGONAL',
  'elk.layered.spacing.nodeNodeBetweenLayers': '22',
  'elk.layered.spacing.edgeNodeBetweenLayers': '18',
  'elk.layered.spacing.edgeEdgeBetweenLayers': '16',
  'elk.spacing.nodeNode': '36',
  'elk.spacing.edgeNode': '26',
  'elk.spacing.edgeEdge': '18',
  'elk.spacing.edgeLabel': '6',
  'elk.spacing.labelLabel': '8',
  'elk.spacing.portPort': '28',
  'elk.layered.cycleBreaking.strategy': 'DEPTH_FIRST',
  'elk.layered.nodePlacement.strategy': 'NETWORK_SIMPLEX',
  'elk.layered.nodePlacement.favorStraightEdges': 'true',
  'elk.layered.mergeEdges': 'false',
  // A long edge's label goes in the layer next to its source, so the label of a return edge
  // (`after retryDelay(attempts)`) sits by the state it leaves, not halfway along the loop.
  'elk.layered.edgeLabels.centerLabelPlacementStrategy': 'TAIL_LAYER',
  'elk.json.shapeCoords': 'ROOT',
  'elk.json.edgeCoords': 'ROOT',
}

/**
 * Tried in order until ELK succeeds. Model order keeps states and branches in source order,
 * which is what a reader expects. ELK accepts it only on the root graph (elkjs 0.11 crashed on
 * it in nested graphs, 0.12 refuses it there), and it may refuse or fail on some graphs, so
 * the layout falls back to plain layered, then to ELK's defaults for the layered algorithm.
 */
export const ATTEMPTS: { name: string; root: ElkOptions; unset: string[] }[] = [
  { name: 'model order', root: { 'elk.layered.considerModelOrder.strategy': 'NODES_AND_EDGES' }, unset: [] },
  { name: 'layered', root: {}, unset: [] },
  {
    name: 'layered defaults',
    root: {},
    unset: [
      'elk.layered.cycleBreaking.strategy',
      'elk.layered.nodePlacement.strategy',
      'elk.layered.nodePlacement.favorStraightEdges',
      'elk.layered.feedbackEdges',
      'elk.layered.edgeLabels.centerLabelPlacementStrategy',
    ],
  },
]

const LABEL_OPTIONS: ElkOptions = {
  'elk.edgeLabels.inline': 'true',
  'elk.edgeLabels.placement': 'CENTER',
}

/** At most this many final states of one container side by side; more wrap onto further rows. */
export const FINALS_PER_ROW = 3

// ───────────── which states are drawn ─────────────

export interface DrawnState {
  state: State
  parent: State | null
  depth: number
  /** Drawn as a box whose children are hidden. */
  collapsed: boolean
}

/** The part of a machine the diagram draws, given the collapsed states. */
export interface View {
  machine: Machine
  states: DrawnState[]
  /** The id of the drawn box that stands for a state: itself, or its outermost collapsed ancestor. */
  visible(id: string): string
}

export function machineView(machine: Machine, collapsed: ReadonlySet<string> = new Set()): View {
  const states: DrawnState[] = []
  const showRoot = rootIsVisible(machine)
  const visit = (s: State, parent: State | null, depth: number) => {
    const isCollapsed = s.states.length > 0 && collapsed.has(s.id) && depth > 0
    if (depth > 0 || showRoot) states.push({ state: s, parent, depth, collapsed: isCollapsed })
    if (!isCollapsed) for (const c of s.states) visit(c, s, depth + 1)
  }
  visit(machine.root, null, 0)
  const collapsedIds = new Set(states.filter((d) => d.collapsed).map((d) => d.state.id))
  return {
    machine,
    states,
    visible(id) {
      for (const a of ancestorIds(id)) if (collapsedIds.has(a)) return a
      return id
    },
  }
}

/**
 * The states a large machine opens with collapsed: every container at the shallowest depth
 * that brings the number of drawn states to at most `budget`. Small machines open expanded.
 */
export function defaultCollapsed(machine: Machine, budget = 32): Set<string> {
  const containers: { id: string; depth: number }[] = []
  walkStates(machine.root, (s, _p, depth) => {
    if (depth > 0 && s.states.length > 0) containers.push({ id: s.id, depth })
  })
  if (descendantCount(machine.root) <= budget) return new Set()
  const maxDepth = Math.max(0, ...containers.map((c) => c.depth))
  let best = new Set<string>()
  for (let d = maxDepth; d >= 1; d--) {
    const set = new Set(containers.filter((c) => c.depth >= d).map((c) => c.id))
    best = set
    const drawn = machineView(machine, set).states.filter((s) => s.depth > 0).length
    if (drawn <= budget) break
  }
  return best
}

// ───────────── sizes ─────────────

export interface StateBox {
  rows: Row[]
  titleH: number
  width: number
  height: number
}

/** Size of a state's own content: the title and the rows under it. */
export function stateBox(state: State, measure: Measure, collapsed = false): StateBox {
  const M = METRICS
  // A history state is a small circle marked `H` or `H*`, with no rows.
  if (isHistory(state)) return { rows: [], titleH: M.historyD, width: M.historyD, height: M.historyD }
  const rows = stateRows(state, collapsed)
  const isContainer = state.states.length > 0 && !collapsed
  const inset = state.kind === 'final' ? M.finalInset : 0
  const hasToggle = state.states.length > 0
  let w = measure(state.name, M.fontTitle, true) + 2 * M.padX + (hasToggle ? M.toggle + 8 : 0)
  if (state.kind === 'parallel') w += measure('parallel', M.fontEffect, false) + M.padX
  for (const r of rows) {
    const text = (r.kw ? r.kw + ' ' : '') + r.text
    w = Math.max(w, measure(text, M.fontRow, false) + 2 * M.padX + (r.machine ? 2 * M.machinePillPad + 12 : 0))
  }
  const rowsH = rows.length ? rows.length * M.rowH + 2 * M.rowsPadY : 0
  const titleH = !isContainer && !rows.length ? 36 : M.titleH
  return {
    rows,
    titleH,
    width: Math.ceil(Math.max(w, 92) + 2 * inset),
    height: Math.ceil(titleH + rowsH + 2 * inset),
  }
}

export interface LabelBox {
  lines: LabelLine[]
  width: number
  height: number
}

export function labelBox(state: State, index: number, measure: Measure): LabelBox {
  const M = METRICS
  const lines = transitionLines(state, index)
  let w = 0
  let h = 2 * M.labelPadY
  for (const l of lines) {
    const size = l.cls === 'trigger' ? M.fontLabel : M.fontEffect
    const text = l.cls === 'trigger' ? [l.text, l.guard].filter(Boolean).join(' ') : l.text
    w = Math.max(w, measure(text, size, l.cls === 'trigger'))
    h += l.cls === 'trigger' ? M.labelLineH : M.effectLineH
  }
  return { lines, width: Math.ceil(w + 2 * M.labelPadX), height: Math.ceil(h) }
}

// ───────────── the ELK graph ─────────────

const isInvisible = (id: string) => id.endsWith('/wrap')

export function buildElkGraph(ir: Ir, machineName: string, opts: LayoutOptions = {}): ElkNode {
  const measure = opts.measure ?? estimateWidth
  const machine = own(ir.machines, machineName)
  if (!machine) throw new Error(`no machine ${machineName}`)
  const view = machineView(machine, opts.collapsed)
  const drawn = new Map(view.states.map((d) => [d.state.id, d]))
  const M = METRICS
  const edges: ElkExtendedEdge[] = []
  // Label nodes of self-loops, by the id of the state whose children they join.
  const loops = new Map<string, ElkNode[]>()

  const initialMarker = (state: State): ElkNode[] => {
    if (!state.initial) return []
    const id = `${state.id}/initial`
    edges.push({ id: `${id}/edge`, sources: [id], targets: [view.visible(state.initial)] })
    return [{ id, width: 2 * M.initialR, height: 2 * M.initialR }]
  }

  // Final states of one container side by side make the chart wide (PlaceOrder has six).
  // Invisible edges from each final to the one FINALS_PER_ROW further on stack them in rows.
  const wrapFinals = (container: State) => {
    const finals = container.states.filter((c) => c.kind === 'final' && drawn.has(c.id))
    for (let i = FINALS_PER_ROW; i < finals.length; i++) {
      const a = finals[i - FINALS_PER_ROW]!
      const b = finals[i]!
      edges.push({ id: `${a.id}->${b.id}/wrap`, sources: [a.id], targets: [b.id] })
    }
  }

  // Every transition with a target, from the box that stands for its source to the one for each
  // of its targets: the first arrow carries the label, the others none. The copies of an
  // `on A | B` are one transition, drawn as the first.
  walkStates(machine.root, (state) => {
    state.transitions.forEach((t, index) => !isCopy(state, index) && t.targets.forEach((target, k) => {
      const src = view.visible(state.id)
      const tgt = view.visible(target)
      if (!drawn.has(src) || !drawn.has(tgt)) return
      // Inside a collapsed state, between two of its hidden states: not drawn.
      if (src !== state.id && src === tgt) return
      const id = targetEdgeId(state.id, index, k)
      if (k > 0) {
        if (src !== tgt) edges.push({ id, sources: [src], targets: [tgt] })
        return
      }
      const lb = labelBox(state, index, measure)
      if (src === tgt) {
        // ELK puts the label of a self-loop beside the loop, where it is hard to attribute.
        // Model the label as a node the loop passes through instead: state -> label -> state.
        const parent = drawn.get(src)?.parent
        const parentId = parent && drawn.has(parent.id) ? parent.id : '$root'
        const list = loops.get(parentId) ?? []
        list.push({ id: `${id}/loop`, width: lb.width, height: lb.height })
        loops.set(parentId, list)
        edges.push({ id: `${id}/out`, sources: [src], targets: [`${id}/loop`] })
        edges.push({ id: `${id}/back`, sources: [`${id}/loop`], targets: [src] })
        return
      }
      edges.push({
        id,
        sources: [src],
        targets: [tgt],
        labels: [{ id: `${id}/label`, text: lb.lines[0]?.text || '-', width: lb.width, height: lb.height, layoutOptions: LABEL_OPTIONS }],
      })
    }))
  })

  const node = (d: DrawnState): ElkNode => {
    const { state } = d
    const box = stateBox(state, measure, d.collapsed)
    if (!state.states.length || d.collapsed) return { id: state.id, width: box.width, height: box.height }
    wrapFinals(state)
    const children = [...initialMarker(state), ...childNodes(state)]
    return {
      id: state.id,
      layoutOptions: {
        ...ELK_OPTIONS,
        'elk.padding': `[top=${box.height + 8},left=16,bottom=16,right=16]`,
        // At least as wide as its own rows. ELK applies a compound node's minimum size before
        // it rotates the graph for direction DOWN, so it is given as (height, width).
        // test/layout.test.ts checks every box holds its content.
        'elk.nodeSize.constraints': 'MINIMUM_SIZE',
        'elk.nodeSize.minimum': `(${box.height + 30},${box.width})`,
      },
      children,
    }
  }
  const childNodes = (state: State): ElkNode[] => {
    const out: ElkNode[] = []
    for (const c of state.states) {
      const d = drawn.get(c.id)
      if (d) out.push(node(d))
    }
    return out
  }

  const root = machine.root
  let children: ElkNode[]
  if (rootIsVisible(machine)) {
    children = [node(drawn.get(root.id)!)]
  } else {
    wrapFinals(root)
    children = [...initialMarker(root), ...childNodes(root)]
  }
  // Attach the loop label nodes to their containers.
  const attach = (n: ElkNode) => {
    const extra = loops.get(n.id)
    if (extra) n.children = [...(n.children ?? []), ...extra]
    for (const c of n.children ?? []) attach(c)
  }
  const graph: ElkNode = {
    id: '$root',
    layoutOptions: { ...ELK_OPTIONS, 'elk.padding': '[top=16,left=16,bottom=16,right=16]' },
    children,
    edges,
  }
  attach(graph)
  return graph
}

function patchOptions(n: ElkNode, attempt: (typeof ATTEMPTS)[number], isRoot: boolean) {
  if (n.layoutOptions) {
    if (isRoot) Object.assign(n.layoutOptions, attempt.root)
    for (const k of attempt.unset) delete n.layoutOptions[k]
  }
  for (const c of n.children ?? []) patchOptions(c, attempt, false)
}

/** Lays out a machine with ELK, falling back through `ATTEMPTS` if ELK refuses or fails. */
export async function layoutMachine(ir: Ir, machineName: string, elk: ElkLike, opts: LayoutOptions = {}): Promise<Layout> {
  const errors: string[] = []
  let out: ElkNode | null = null
  let attempt = 0
  for (; attempt < ATTEMPTS.length; attempt++) {
    const graph = buildElkGraph(ir, machineName, opts)
    patchOptions(graph, ATTEMPTS[attempt]!, true)
    try {
      out = await elk.layout(graph)
      break
    } catch (e) {
      errors.push(e instanceof Error ? e.message : String(e))
    }
  }
  if (!out) throw new Error(`ELK could not lay out ${machineName}: ${errors.join('; ')}`)
  return flatten(machineName, out, attempt, errors)
}

function flatten(machine: string, out: ElkNode, attempt: number, errors: string[]): Layout {
  const nodes: Record<string, Box> = {}
  const edges: Record<string, EdgeRoute> = {}
  const visit = (n: ElkNode) => {
    for (const c of n.children ?? []) {
      nodes[c.id] = { x: c.x ?? 0, y: c.y ?? 0, width: c.width ?? 0, height: c.height ?? 0 }
      visit(c)
    }
    for (const e of n.edges ?? []) {
      if (isInvisible(e.id)) continue
      const points: Point[] = []
      for (const s of e.sections ?? []) points.push(s.startPoint, ...(s.bendPoints ?? []), s.endPoint)
      const l = e.labels?.[0]
      edges[e.id] = { points, label: l ? { x: l.x ?? 0, y: l.y ?? 0, width: l.width ?? 0, height: l.height ?? 0 } : null }
    }
  }
  visit(out)
  // Join the two halves of each self-loop into one edge whose label is the label node.
  for (const id of Object.keys(nodes)) {
    if (!id.endsWith('/loop')) continue
    const base = id.slice(0, -'/loop'.length)
    const a = edges[`${base}/out`]
    const b = edges[`${base}/back`]
    if (a && b) edges[base] = { points: [...a.points, ...b.points], label: nodes[id]! }
    delete edges[`${base}/out`]
    delete edges[`${base}/back`]
    delete nodes[id]
  }
  return { machine, width: Math.ceil(out.width ?? 0), height: Math.ceil(out.height ?? 0), nodes, edges, attempt, errors }
}
