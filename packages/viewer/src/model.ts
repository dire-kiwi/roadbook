// View model: the lines of text the diagram and the details panel show, taken from the IR.
// Pure and DOM-free. Layout (to size boxes) and render (to draw them) both call it, so the
// two always agree on what goes where.

import type { Effect, Field, Invoke, Ir, Machine, Span, State, Transition, Trigger, TypeDecl } from './ir.ts'
import { own } from './ir.ts'

export const METRICS = {
  charW: 0.62, // estimated advance of a monospace glyph, in em (the browser measures instead)
  fontTitle: 13,
  fontRow: 11.5,
  fontLabel: 12,
  fontEffect: 11,
  rowH: 17,
  titleH: 30, // title band of a state box
  padX: 12,
  rowsPadY: 7, // space above and below the rows of a state box
  finalInset: 4, // room for the double border of a final state
  labelPadX: 7,
  labelPadY: 4,
  labelLineH: 16,
  effectLineH: 15,
  initialR: 5.5,
  wrapAt: 46, // wrap code in a state box beyond this many characters
  finalWrapAt: 30, // narrower in final states, which ELK tends to put side by side
  labelWrapAt: 30, // put the guard of an edge label on its own line beyond this
  machinePillPad: 6,
  toggle: 14, // the collapse / expand button of a container
} as const

/** Width of `text` at `fontSize` px. The browser passes a real measurer; Node estimates. */
export type Measure = (text: string, fontSize: number, bold: boolean) => number

export const estimateWidth: Measure = (text, fontSize) => [...text].length * fontSize * METRICS.charW

/**
 * Breaks a long piece of code at ", " or " " so it fits in `max` characters. Continuation lines
 * are indented. Never inside a string literal, whose spaces are shown as written: a line with
 * a long literal is longer than `max`.
 */
export function wrapCode(text: string, max: number = METRICS.wrapAt): string[] {
  if (text.length <= max) return [text]
  const out: string[] = []
  let rest = text
  let first = true
  while (rest.length > (first ? max : max - 2)) {
    const limit = first ? max : max - 2
    const spaces = breakable(rest)
    // The last ", " that fits (cut after the comma), else the last " " that fits.
    let cut = -1
    for (const i of spaces) if (i <= limit && i > 9 && rest[i - 1] === ',') cut = i
    if (cut < 0) for (const i of spaces) if (i <= limit && i > 8) cut = i
    // Only spaces inside a literal fit: break after it, the line longer than `max`.
    if (cut < 0 && rest.lastIndexOf(' ', limit) > 8) cut = spaces.find((i) => i > limit) ?? -1
    if (cut < 0) break
    out.push((first ? '' : '  ') + rest.slice(0, cut).trimEnd())
    rest = rest.slice(cut).trimStart()
    first = false
  }
  out.push((first ? '' : '  ') + rest)
  return out
}

/** Indices of the spaces in `code` outside string literals. */
function breakable(code: string): number[] {
  const out: number[] = []
  let quoted = false
  for (let i = 0; i < code.length; i++) {
    const c = code[i]
    if (quoted && c === '\\') i++
    else if (c === '"') quoted = !quoted
    else if (c === ' ' && !quoted) out.push(i)
  }
  return out
}

export function effectText(e: Effect): string {
  return e.kind === 'set' ? `set ${e.var} = ${e.value.text}` : `do ${e.action}(${e.args.map((a) => a.text).join(', ')})`
}

export function invokeText(inv: Invoke): string {
  return `${inv.src}(${inv.args.map((a) => a.text).join(', ')})`
}

/** How the diagram encodes a trigger: line style, colour and wording all follow it. */
export type EdgeKind = 'event' | 'done' | 'fail' | 'after' | 'always' | 'initial'

export function triggerKind(trigger: Trigger): EdgeKind {
  switch (trigger.kind) {
    case 'event':
    case 'any':
      return 'event'
    case 'after':
      return 'after'
    case 'always':
      return 'always'
    case 'invoke.fail':
      return 'fail'
    case 'done':
    case 'invoke.done':
      return 'done'
  }
}

/** The trigger as the diagram labels it: `Cancel`, `on *`, `after 30s`, `always`, `done`, `fail CardDeclined`. */
export function triggerText(trigger: Trigger): string {
  switch (trigger.kind) {
    case 'event':
      return trigger.event
    case 'any':
      return 'on *'
    case 'after':
      return `after ${trigger.delay.text}`
    case 'always':
      return 'always'
    case 'invoke.fail':
      return `fail ${trigger.error}`
    case 'done':
    case 'invoke.done':
      return 'done'
  }
}

/** The trigger as written in source: `on Cancel`, `on *`, `after 30s`, `done`, `fail E`. */
export function triggerSource(trigger: Trigger): string {
  return trigger.kind === 'event' ? `on ${trigger.event}` : triggerText(trigger)
}

/** `[guard]` for a guarded branch, `[else]` for the unguarded last branch of a block, `""` otherwise. */
export function guardText(state: State, index: number): string {
  const t = state.transitions[index]
  if (!t) return ''
  if (t.guard) return `[${t.guard.text}]`
  const first = state.transitions.findIndex((o) => o.group === t.group)
  return first !== index ? '[else]' : ''
}

export interface LabelLine {
  cls: 'trigger' | 'effect'
  text: string
  guard: string
}

/**
 * Lines of an edge label: the trigger and guard first, then one line per effect. A long
 * trigger and guard is split over two lines for every branch of the block alike, so labels
 * stay narrow and the branches of one trigger look the same.
 */
export function transitionLines(state: State, index: number): LabelLine[] {
  const t = state.transitions[index]
  if (!t) return []
  const head = triggerText(t.trigger)
  const guard = guardText(state, index)
  const split = state.transitions.some((o, i) => {
    const g = o.group === t.group ? guardText(state, i) : ''
    return g !== '' && (head + ' ' + g).length > METRICS.labelWrapAt
  })
  const lines: LabelLine[] = []
  if (guard && split) {
    lines.push({ cls: 'trigger', text: head, guard: '' })
    for (const l of wrapCode(guard, METRICS.labelWrapAt + 8)) lines.push({ cls: 'trigger', text: '', guard: l })
  } else {
    lines.push({ cls: 'trigger', text: head, guard })
  }
  for (const e of t.effects) for (const l of wrapCode(effectText(e), METRICS.labelWrapAt + 8)) lines.push({ cls: 'effect', text: l, guard: '' })
  return lines
}

/** A row inside a state box. `kw` is drawn muted, `text` in the code colour. */
export interface Row {
  kw: string
  text: string
  /** A continuation line of the row above. */
  cont: boolean
  cls?: EdgeKind | 'collapsed'
  /** The row is a machine invoke: clicking it opens this machine. */
  machine?: string
  /** The row is an internal transition: its index in the state's `transitions`. */
  transition?: number
}

/** Number of states nested inside `state`, at any depth. */
export function descendantCount(state: State): number {
  return state.states.reduce((n, c) => n + 1 + descendantCount(c), 0)
}

/** The rows shown inside a state box. `collapsed`: the state's children are hidden. */
export function stateRows(state: State, collapsed = false): Row[] {
  const rows: Row[] = []
  const wrap = state.kind === 'final' ? METRICS.finalWrapAt : METRICS.wrapAt
  const add = (kw: string, text: string, extra: Partial<Row> = {}) => {
    wrapCode(text, Math.max(wrap - kw.length - 1, 16)).forEach((l, i) => {
      rows.push({ kw: i === 0 ? kw : '', text: i === 0 ? l : ' '.repeat(kw.length + 1) + l.trimStart(), cont: i > 0, ...extra })
    })
  }
  if (state.invoke) {
    if (state.invoke.kind === 'machine') add('invoke', `machine ${invokeText(state.invoke)}`, { machine: state.invoke.src })
    else add('invoke', invokeText(state.invoke))
  }
  for (const e of state.entry) add('entry', effectText(e))
  for (const e of state.exit) add('exit', effectText(e))
  state.transitions.forEach((t, index) => {
    if (t.target !== null) return
    const guard = guardText(state, index)
    const body = [guard, t.effects.map(effectText).join(', ')].filter(Boolean).join(' ')
    add(triggerSource(t.trigger), body, { transition: index, cls: triggerKind(t.trigger) })
  })
  if (state.output) add('output', state.output.text)
  if (state.fail) add('fail', state.fail.text, { cls: 'fail' })
  if (collapsed && state.states.length) {
    const n = descendantCount(state)
    rows.push({ kw: '+', text: `${n} state${n === 1 ? '' : 's'} inside (collapsed)`, cont: false, cls: 'collapsed' })
  }
  return rows
}

export function walkStates(state: State, fn: (s: State, parent: State | null, depth: number) => void, parent: State | null = null, depth = 0): void {
  fn(state, parent, depth)
  for (const c of state.states) walkStates(c, fn, state, depth + 1)
}

export function allStates(machine: Machine): State[] {
  const out: State[] = []
  walkStates(machine.root, (s) => out.push(s))
  return out
}

export function findState(machine: Machine, id: string): State | null {
  let found: State | null = null
  walkStates(machine.root, (s) => {
    if (s.id === id) found = s
  })
  return found
}

/** The ids of the states containing `id`, outermost first, not including the machine root. */
export function ancestorIds(id: string): string[] {
  const parts = id.split('.')
  const out: string[] = []
  for (let i = 2; i < parts.length; i++) out.push(parts.slice(0, i).join('.'))
  return out
}

/** The machine root is drawn as a frame only when it carries something of its own. */
export function rootIsVisible(machine: Machine): boolean {
  const r = machine.root
  return r.transitions.length > 0 || r.entry.length > 0 || r.exit.length > 0 || r.invoke !== null
}

export function paramsText(params: Field[]): string {
  return params.map((p) => `${p.name}: ${p.type}`).join(', ')
}

/** `machine Checkout(order: Order, user: User) -> Receipt ! CardDeclined | GatewayUnavailable` */
export function signatureText(kind: string, name: string, decl: { params: Field[]; returns?: string | null; errors?: string[] }): string {
  let s = `${kind} ${name}(${paramsText(decl.params)})`
  if (decl.returns) s += ` -> ${decl.returns}`
  if (decl.errors && decl.errors.length) s += ` ! ${decl.errors.join(' | ')}`
  return s
}

/** `examples/shop/checkout.road:12` */
export function sourceText(ir: Ir, span: Span): string {
  const src = ir.sources[span.file]
  return `${src ? src.path : '?'}:${span.line}`
}

/** A type declaration in source form, on one line. */
export function typeDeclText(name: string, t: TypeDecl): string {
  const fields = (fs: Field[]) => (fs.length ? ` { ${fs.map((f) => `${f.name}: ${f.type}`).join(', ')} }` : '')
  switch (t.kind) {
    case 'record':
      return `type ${name}${fields(t.fields)}`
    case 'error':
      return `error ${name}${fields(t.fields)}`
    case 'enum':
      return `enum ${name} { ${t.variants.join(', ')} }`
    case 'alias':
      return `type ${name} = ${t.type}`
    case 'extern':
      return `extern type ${name}`
  }
}

/** The base name of a type reference: `LineItem[]?` is `LineItem`. */
export function baseType(ref: string): string {
  return ref.replace(/(\[\]|\?)+$/, '')
}

/** The machines a machine invokes, directly, in the order their states are written. */
export function invokedMachines(machine: Machine): string[] {
  const out: string[] = []
  walkStates(machine.root, (s) => {
    if (s.invoke && s.invoke.kind === 'machine' && !out.includes(s.invoke.src)) out.push(s.invoke.src)
  })
  return out
}

/** The id the layout gives a transition: the owning state's id and its index. */
export function transitionId(stateId: string, index: number): string {
  return `${stateId}#${index}`
}

/** Whether the transition `t` of `state` is drawn as an arrow (it has a target). */
export function isExternal(t: Transition): boolean {
  return t.target !== null
}

/**
 * The machine a chart page opens with: `wanted` (from the URL's `#`) if the chart has it,
 * else the entry file's first machine, not one it imports.
 */
export function openingMachine(ir: Ir, wanted: string): string | undefined {
  if (own(ir.machines, wanted)) return wanted
  const names = Object.keys(ir.machines)
  return names.find((m) => own(ir.machines, m)!.span.file === 0) ?? names[0]
}
