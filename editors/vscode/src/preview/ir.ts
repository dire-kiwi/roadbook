// The few parts of the IR (spec/ir.md) the extension reads itself: sources and the spans of
// states and transitions, to reveal what was clicked in the preview. The viewer reads the rest.

export interface Span {
  file: number
  line: number
  col: number
  endLine: number
  endCol: number
}

export interface State {
  id: string
  span: Span
  states: State[]
  transitions: { span: Span }[]
}

export interface Ir {
  irVersion: string
  sources: { path: string; package: string | null }[]
  machines: Record<string, { span: Span; root: State }>
}

/** What was clicked in the preview: a state, or a state's transition by its index. */
export type Target = { kind: 'state'; id: string } | { kind: 'transition'; state: string; index: number }

const own = <T>(map: Record<string, T>, key: string): T | undefined =>
  Object.prototype.hasOwnProperty.call(map, key) ? map[key] : undefined

function findState(s: State, id: string): State | undefined {
  if (s.id === id) return s
  for (const c of s.states) {
    const found = findState(c, id)
    if (found) return found
  }
  return undefined
}

/** The span of a state or transition of `machine`; the machine's own span for its root state. */
export function spanOf(ir: Ir, machine: string, target: Target): Span | undefined {
  const m = own(ir.machines, machine)
  if (!m) return undefined
  const state = findState(m.root, target.kind === 'state' ? target.id : target.state)
  if (!state) return undefined
  if (target.kind === 'transition') return state.transitions[target.index]?.span
  return state === m.root ? m.span : state.span
}

/**
 * Where a span's file is, as a path relative to the project root (`/`-separated), or
 * undefined for a file of another package, which the extension does not look up.
 */
export function sourcePath(ir: Ir, span: Span): string | undefined {
  const src = ir.sources[span.file]
  if (!src || src.package !== null) return undefined
  return src.path
}
