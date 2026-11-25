// Layout: IR machine -> ELK graph -> positions in absolute coordinates.
// DOM-free. The caller passes an ELK instance (elkjs works the same in Node and in the browser).

import { METRICS, estimateWidth, stateRows, transitionLines, rootIsVisible, walkStates } from './model.js'

// Options that apply to the root and to every compound state (ELK does not inherit them).
export const ELK_OPTIONS = {
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
  'elk.layered.feedbackEdges': 'true',
  'elk.json.shapeCoords': 'ROOT',
  'elk.json.edgeCoords': 'ROOT',
}

// Tried in order until ELK succeeds. Model order keeps states and branches in source order, which
// is what a reader expects, but it crashes elkjs 0.11.1 ("Cannot read properties of undefined") on
// some larger hierarchical graphs, so the layout falls back to plain layered, then to ELK's defaults.
const ATTEMPTS = [
  { set: { 'elk.layered.considerModelOrder.strategy': 'NODES_AND_EDGES' }, unset: [] },
  { set: {}, unset: [] },
  { set: {}, unset: ['elk.layered.cycleBreaking.strategy', 'elk.layered.nodePlacement.strategy', 'elk.layered.nodePlacement.favorStraightEdges', 'elk.layered.feedbackEdges'] },
]

const LABEL_OPTIONS = {
  'elk.edgeLabels.inline': 'true',
  'elk.edgeLabels.placement': 'CENTER',
}

// Size of a state's own content: the title and the rows under it.
export function stateBox(state, depth, measure) {
  const M = METRICS
  const rows = stateRows(state, depth)
  const isContainer = state.states.length > 0
  const inset = state.kind === 'final' ? M.finalInset : 0
  let w = measure(state.name, M.fontTitle, true) + 2 * M.padX
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

export function labelBox(state, index, measure) {
  const M = METRICS
  const lines = transitionLines(state, index)
  let w = 0, h = 2 * M.labelPadY
  for (const l of lines) {
    const size = l.cls === 'trigger' ? M.fontLabel : M.fontEffect
    const text = l.cls === 'trigger' ? [l.text, l.guard].filter(Boolean).join(' ') : l.text
    w = Math.max(w, measure(text, size, l.cls === 'trigger'))
    h += l.cls === 'trigger' ? M.labelLineH : M.effectLineH
  }
  return { lines, width: Math.ceil(w + 2 * M.labelPadX), height: Math.ceil(h) }
}

export function buildElkGraph(ir, machineName, opts = {}) {
  const measure = opts.measure ?? ((text, size) => estimateWidth(text, size))
  const machine = ir.machines[machineName]
  const M = METRICS
  const edges = []

  const initialMarker = (state) => {
    if (!state.initial) return []
    const id = `${state.id}/initial`
    edges.push({ id: `${id}/edge`, sources: [id], targets: [state.initial] })
    return [{ id, width: 2 * M.initialR, height: 2 * M.initialR }]
  }

  // Returns the ELK node of a state, followed by the label nodes of its self-transitions.
  const node = (state, depth) => {
    const box = stateBox(state, depth, measure)
    const loops = []
    state.transitions.forEach((t, index) => {
      if (t.target === null) return
      const lb = labelBox(state, index, measure)
      if (t.target === state.id) {
        // ELK puts the label of a self-loop beside the loop, where it is hard to attribute.
        // Model the label as a node the loop passes through instead: state -> label -> state.
        const id = `${state.id}#${index}`
        loops.push({ id: `${id}/loop`, width: lb.width, height: lb.height })
        edges.push({ id: `${id}/out`, sources: [state.id], targets: [`${id}/loop`] })
        edges.push({ id: `${id}/back`, sources: [`${id}/loop`], targets: [state.id] })
        return
      }
      edges.push({
        id: `${state.id}#${index}`,
        sources: [state.id],
        targets: [t.target],
        labels: [{ id: `${state.id}#${index}/label`, text: lb.lines[0].text || '-', width: lb.width, height: lb.height, layoutOptions: LABEL_OPTIONS }],
      })
    })
    if (!state.states.length) return [{ id: state.id, width: box.width, height: box.height }, ...loops]
    return [{
      id: state.id,
      layoutOptions: {
        ...ELK_OPTIONS,
        'elk.padding': `[top=${box.height + 8},left=16,bottom=16,right=16]`,
        'elk.nodeSize.constraints': 'MINIMUM_SIZE',
        'elk.nodeSize.minimum': `(${box.width},${box.height + 30})`,
      },
      children: [...initialMarker(state), ...state.states.flatMap((c) => node(c, depth + 1))],
    }, ...loops]
  }

  const root = machine.root
  const graph = {
    id: '$root',
    layoutOptions: { ...ELK_OPTIONS, 'elk.padding': '[top=16,left=16,bottom=16,right=16]' },
    children: rootIsVisible(machine)
      ? node(root, 0)
      : [...initialMarker(root), ...root.states.flatMap((c) => node(c, 1))],
    edges,
  }
  return graph
}

// Run ELK and flatten the result:
//   { machine, width, height, fallback,                    // fallback: 0 unless ELK needed a retry
//     nodes: { [id]: { x, y, width, height } },            // states and "<id>/initial" markers
//     edges: { [id]: { points: [{x,y}], label: { x, y, width, height } | null } } }
export async function layoutMachine(ir, machineName, elk, opts = {}) {
  let out = null, attempt = 0, error = null
  for (; attempt < ATTEMPTS.length && !out; attempt++) {
    const graph = buildElkGraph(ir, machineName, opts)
    const patch = (n) => {
      if (n.layoutOptions) {
        Object.assign(n.layoutOptions, ATTEMPTS[attempt].set)
        for (const k of ATTEMPTS[attempt].unset) delete n.layoutOptions[k]
      }
      for (const c of n.children ?? []) patch(c)
    }
    patch(graph)
    try { out = await elk.layout(graph) } catch (e) { error = e }
  }
  if (!out) throw error
  const nodes = {}
  const edges = {}
  const visit = (n) => {
    for (const c of n.children ?? []) {
      nodes[c.id] = { x: c.x, y: c.y, width: c.width, height: c.height }
      visit(c)
    }
    for (const e of n.edges ?? []) {
      const points = []
      for (const s of e.sections ?? []) points.push(s.startPoint, ...(s.bendPoints ?? []), s.endPoint)
      const l = e.labels?.[0]
      edges[e.id] = { points, label: l ? { x: l.x, y: l.y, width: l.width, height: l.height } : null }
    }
  }
  visit(out)
  // Join the two halves of each self-loop into one edge whose label is the label node.
  for (const id of Object.keys(nodes)) {
    if (!id.endsWith('/loop')) continue
    const base = id.slice(0, -'/loop'.length)
    const a = edges[`${base}/out`], b = edges[`${base}/back`]
    if (a && b) edges[base] = { points: [...a.points, ...b.points], label: nodes[id] }
    delete edges[`${base}/out`]
    delete edges[`${base}/back`]
    delete nodes[id]
  }
  return { machine: machineName, width: Math.ceil(out.width), height: Math.ceil(out.height), nodes, edges, fallback: attempt - 1 }
}

// Every state of a machine that the diagram draws, outermost first.
export function drawnStates(machine) {
  const out = []
  walkStates(machine.root, (s, parent, depth) => {
    if (depth === 0 && !rootIsVisible(machine)) return
    out.push({ state: s, parent, depth })
  })
  return out
}
