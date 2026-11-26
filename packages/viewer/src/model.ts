// View model: the lines of text the diagram and the details panel show, taken from the IR.
// Pure and DOM-free. Layout (to size boxes) and render (to draw them) both call it, so the
// two always agree on what goes where.

import type { Address, Effect, EventDecl, Field, Invoke, Ir, Machine, Span, State, Transition, Trigger, TypeDecl } from './ir.ts'
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

/**
 * `set a = 1`, `do f(x)`, `send upload.Pause`, `send inventory.Reserve { orderId: id }`,
 * `send sessions[id].Notify { text: t }`, `send sessions.*.Leave`, `emit Progress { percent: p }`,
 * `spawn sessions[id] = Session(id)`, `stop sessions[id]`
 */
export function effectText(e: Effect): string {
  switch (e.kind) {
    case 'set':
      return `set ${e.var} = ${e.value.text}`
    case 'do':
      return `do ${e.action}(${e.args.map((a) => a.text).join(', ')})`
    case 'send':
      // A send's or emit's value text is the event as written after `send name.` or `emit`.
      return `send ${e.to.name}${sendKey(e.to, e.key)}.${e.value.text}`
    case 'emit':
      return `emit ${e.value.text}`
    case 'spawn':
      return `spawn ${e.children}[${e.key.text}] = ${e.machine}(${e.args.map((a) => a.text).join(', ')})`
    case 'stop':
      return `stop ${e.children}[${e.key.text}]`
  }
}

/** Which child of a collection a send goes to: `[key]`, or `.*` for every one; `""` for another address. */
function sendKey(to: Address, key: { text: string } | null): string {
  if (to.kind !== 'children') return ''
  return key ? `[${key.text}]` : '.*'
}

/** An event of a machine, as the viewer links to it: `Upload.Progress`. */
export interface EventRef {
  machine: string
  event: string
}

export const eventKey = (r: EventRef): string => `${r.machine}.${r.event}`

/** `Upload.Progress` back to its parts; null for anything else. */
export function parseEventKey(key: string): EventRef | null {
  const m = /^([A-Z][A-Za-z0-9]*)\.([A-Z][A-Za-z0-9]*)$/.exec(key)
  return m ? { machine: m[1]!, event: m[2]! } : null
}

/** The machine a state id belongs to: its first segment. */
export function machineOf(stateId: string): string {
  return stateId.split('.')[0]!
}

/**
 * The event a messaging effect carries, in the machine that declares it: the receiver's for
 * a `send`, the machine's own (`machine`) for an `emit`. Null for `set` and `do`.
 */
export function effectEvent(machine: string, e: Effect): EventRef | null {
  if (e.kind === 'send') return { machine: e.to.machine, event: e.event }
  if (e.kind === 'emit') return { machine, event: e.event }
  return null
}

/** The event an `on name.Event` trigger takes, in the sender's machine. Null for other triggers. */
export function triggerEvent(t: Trigger): EventRef | null {
  return t.kind === 'emitted' ? { machine: t.from.machine, event: t.event } : null
}

/** `Upload(draft.file)`, `Upload(draft.file) as upload` */
export function invokeText(inv: Invoke): string {
  return `${inv.src}(${inv.args.map((a) => a.text).join(', ')})${inv.as ? ` as ${inv.as}` : ''}`
}

/**
 * How the diagram encodes a trigger: line style, colour and wording all follow it. `message`
 * is an event another machine emitted (`on upload.Progress`), labelled through its sender.
 */
export type EdgeKind = 'event' | 'message' | 'done' | 'fail' | 'after' | 'always' | 'initial'

export function triggerKind(trigger: Trigger): EdgeKind {
  switch (trigger.kind) {
    case 'event':
    case 'any':
      return 'event'
    case 'emitted':
      return 'message'
    case 'after':
      return 'after'
    case 'always':
      return 'always'
    case 'invoke.fail':
    case 'spawn.fail':
      return 'fail'
    case 'done':
    case 'invoke.done':
    case 'spawn.done':
      return 'done'
  }
}

/**
 * The trigger as the diagram labels it: `Cancel`, `upload.Progress`, `on *`, `after 30s`,
 * `always`, `done`, `fail CardDeclined`, `sessions.done`, `sessions.fail Lost`.
 */
export function triggerText(trigger: Trigger): string {
  switch (trigger.kind) {
    case 'event':
      return trigger.event
    case 'emitted':
      return `${trigger.from.name}.${trigger.event}`
    case 'any':
      return 'on *'
    case 'after':
      return `after ${trigger.delay.text}`
    case 'always':
      return 'always'
    case 'invoke.fail':
      return `fail ${trigger.error}`
    case 'spawn.done':
      return `${trigger.children}.done`
    case 'spawn.fail':
      return `${trigger.children}.fail ${trigger.error}`
    case 'done':
    case 'invoke.done':
      return 'done'
  }
}

/**
 * The trigger as written in source: `on Cancel`, `on upload.Progress`, `on *`, `after 30s`,
 * `done`, `fail E`, `on sessions.done`, `on sessions.fail E`.
 */
export function triggerSource(trigger: Trigger): string {
  const on = trigger.kind === 'event' || trigger.kind === 'emitted' || trigger.kind === 'spawn.done' || trigger.kind === 'spawn.fail'
  return on ? `on ${triggerText(trigger)}` : triggerText(trigger)
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
  /**
   * The line names another machine's event: the trigger of `on upload.Progress`, or a `send`
   * or `emit` effect. Clicking it opens that event.
   */
  event?: EventRef
  /** The line is (part of) a `spawn` effect: clicking it opens this machine, the child's. */
  spawn?: string
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
  const from = triggerEvent(t.trigger)
  const trig = (text: string, g: string): LabelLine => (from && text ? { cls: 'trigger', text, guard: g, event: from } : { cls: 'trigger', text, guard: g })
  if (guard && split) {
    lines.push(trig(head, ''))
    for (const l of wrapCode(guard, METRICS.labelWrapAt + 8)) lines.push({ cls: 'trigger', text: '', guard: l })
  } else {
    lines.push(trig(head, guard))
  }
  const machine = machineOf(state.id)
  for (const e of t.effects) {
    const link = effectLink(machine, e)
    for (const l of wrapCode(effectText(e), METRICS.labelWrapAt + 8)) lines.push({ cls: 'effect', text: l, guard: '', ...link })
  }
  return lines
}

/** What the lines of an effect link to: the event of a `send` or `emit`, the machine of a `spawn`. */
function effectLink(machine: string, e: Effect): { event?: EventRef; spawn?: string } {
  const event = effectEvent(machine, e)
  if (event) return { event }
  return e.kind === 'spawn' ? { spawn: e.machine } : {}
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
  /** The `as` name of that invoke, at the end of `text`. */
  alias?: string
  /** The row is an internal transition: its index in the state's `transitions`. */
  transition?: number
  /** `kw` ends with another machine's event (`on upload.Progress`): clicking it opens the event. */
  kwEvent?: EventRef
  /** `text` is (part of) a `send` or `emit` effect: clicking it opens the event. */
  event?: EventRef
  /** `text` is (part of) a `spawn` effect: clicking it opens this machine, the child's. */
  spawn?: string
}

/** Number of states nested inside `state`, at any depth. */
export function descendantCount(state: State): number {
  return state.states.reduce((n, c) => n + 1 + descendantCount(c), 0)
}

/** The rows shown inside a state box. `collapsed`: the state's children are hidden. */
export function stateRows(state: State, collapsed = false): Row[] {
  const rows: Row[] = []
  const wrap = state.kind === 'final' ? METRICS.finalWrapAt : METRICS.wrapAt
  const machine = machineOf(state.id)
  // `kw text`, wrapped. With `first` false the text continues the row above: no keyword, and
  // aligned under that row's text. Wrapped lines are indented two more.
  const add = (kw: string, text: string, extra: Partial<Row> = {}, first = true) => {
    const pad = ' '.repeat(kw.length + 1)
    wrapCode(text, Math.max(wrap - kw.length - 1, 16)).forEach((l, i) => {
      const head = first && i === 0
      const row: Row = { ...extra, kw: head ? kw : '', text: head ? l : pad + (i > 0 ? '  ' : '') + l.trimStart(), cont: !head }
      if (!head) delete row.kwEvent
      rows.push(row)
    })
  }
  const effectExtra = (e: Effect): Partial<Row> => effectLink(machine, e)
  if (state.invoke) {
    const inv = state.invoke
    if (inv.kind === 'machine') {
      const text = `machine ${invokeText(inv)}`
      // A machine invoke is a pill on its first line; its `as` name ends it when it fits there.
      const lines = wrapCode(text, Math.max(wrap - 'invoke'.length - 1, 16))
      add('invoke', text, {})
      const first = rows[rows.length - lines.length]!
      first.machine = inv.src
      if (inv.as && lines.length === 1) first.alias = inv.as
    } else add('invoke', invokeText(inv))
  }
  for (const e of state.entry) add('entry', effectText(e), effectExtra(e))
  for (const e of state.exit) add('exit', effectText(e), effectExtra(e))
  state.transitions.forEach((t, index) => {
    if (t.target !== null) return
    // The trigger, then the guard and each effect: one effect per line, so a `send` or `emit`
    // is a line of its own that can be clicked.
    const kw = triggerSource(t.trigger)
    const from = triggerEvent(t.trigger)
    const base: Partial<Row> = { transition: index, cls: triggerKind(t.trigger) }
    const guard = guardText(state, index)
    const items: [string, Partial<Row>][] = []
    if (guard) items.push([guard, {}])
    for (const e of t.effects) items.push([effectText(e), effectExtra(e)])
    if (!items.length) items.push(['', {}])
    items.forEach(([text, extra], i) => {
      add(kw, text, { ...base, ...extra, ...(i === 0 && from ? { kwEvent: from } : {}) }, i === 0)
    })
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

/** A source's path; a file of another package as imports write it: `inventory/inventory.road`. */
export function sourcePath(src: { path: string; package: string | null } | undefined): string {
  if (!src) return '?'
  return src.package ? `${src.package}/${src.path}` : src.path
}

/** `examples/shop/checkout.road:12` */
export function sourceText(ir: Ir, span: Span): string {
  return `${sourcePath(ir.sources[span.file])}:${span.line}`
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

// ───────────── messaging: who talks to whom ─────────────

/**
 * The machine a type is a handle to: `ref Inventory`, `ref Inventory[]?`, or an alias of one.
 * Null for any other type.
 */
export function refMachine(ir: Ir, type: string): string | null {
  let t = type
  for (let depth = 0; depth < 32; depth++) {
    const base = baseType(t)
    if (base.startsWith('ref ')) return base.slice(4)
    const decl = own(ir.types, base)
    if (!decl || decl.kind !== 'alias') return null
    t = decl.type
  }
  return null
}

/** Where a `send`, an `emit` or an `on name.Event` is written. */
export interface MessageSite {
  dir: 'send' | 'emit' | 'receive'
  /** The state it is written in. */
  state: string
  /** The index of its transition in the state's `transitions`, or the state's `entry` or `exit`. */
  at: number | 'entry' | 'exit'
  /** The receiver of a send, the sender of a receive; null for an emit. */
  address: Address | null
  /** The event, in the machine that declares it: the receiver's, the sender's, or the machine's own. */
  event: EventRef
}

/**
 * Every message a machine sends, emits and receives, in document order: state by state
 * (outermost first), its `entry`, its `exit`, then each transition's trigger and effects.
 */
export function messageSites(machineName: string, machine: Machine): MessageSite[] {
  const out: MessageSite[] = []
  const effects = (state: string, at: MessageSite['at'], list: Effect[]) => {
    for (const e of list) {
      if (e.kind === 'send') out.push({ dir: 'send', state, at, address: e.to, event: { machine: e.to.machine, event: e.event } })
      else if (e.kind === 'emit') out.push({ dir: 'emit', state, at, address: null, event: { machine: machineName, event: e.event } })
    }
  }
  walkStates(machine.root, (s) => {
    effects(s.id, 'entry', s.entry)
    effects(s.id, 'exit', s.exit)
    s.transitions.forEach((t, i) => {
      if (t.trigger.kind === 'emitted') out.push({ dir: 'receive', state: s.id, at: i, address: t.trigger.from, event: { machine: t.trigger.from.machine, event: t.trigger.event } })
      effects(s.id, i, t.effects)
    })
  })
  return out
}

/** Where a `spawn` or `stop` is written. */
export interface SpawnSite {
  op: 'spawn' | 'stop'
  /** The collection. */
  children: string
  /** The state it is written in. */
  state: string
  /** The index of its transition in the state's `transitions`, or the state's `entry` or `exit`. */
  at: number | 'entry' | 'exit'
}

/** Every `spawn` and `stop` of a machine, in the document order of `messageSites`. */
export function spawnSites(machine: Machine): SpawnSite[] {
  const out: SpawnSite[] = []
  const effects = (state: string, at: SpawnSite['at'], list: Effect[]) => {
    for (const e of list) if (e.kind === 'spawn' || e.kind === 'stop') out.push({ op: e.kind, children: e.children, state, at })
  }
  walkStates(machine.root, (s) => {
    effects(s.id, 'entry', s.entry)
    effects(s.id, 'exit', s.exit)
    s.transitions.forEach((t, i) => effects(s.id, i, t.effects))
  })
  return out
}

/** Which machines a machine talks to, from its chart alone. Each list in first-written order. */
export interface Talks {
  /** Machines invoked, with the `as` name if any. */
  invokes: { machine: string; as: string | null; state: string }[]
  /** Machines it spawns, with the collections (`children name: Machine by Key`) that hold them. */
  spawns: { machine: string; names: string[] }[]
  /** Machines it holds a `ref` to, with the parameters and variables that hold it. */
  refs: { machine: string; names: string[] }[]
  /** Events it sends, by receiving machine. */
  sends: { machine: string; events: string[] }[]
  /** Events of other machines' `emits` it handles, by sending machine. */
  receives: { machine: string; events: string[] }[]
}

/** Adds `value` to the list of `machine` in `list`, keeping first-seen order and no repeats. */
function group(list: { machine: string; values: string[] }[], machine: string, value: string) {
  let entry = list.find((e) => e.machine === machine)
  if (!entry) list.push((entry = { machine, values: [] }))
  if (!entry.values.includes(value)) entry.values.push(value)
}

export function talks(ir: Ir, machineName: string): Talks {
  const m = own(ir.machines, machineName)
  const invokes: Talks['invokes'] = []
  const spawns: { machine: string; values: string[] }[] = []
  const refs: { machine: string; values: string[] }[] = []
  const sends: { machine: string; values: string[] }[] = []
  const receives: { machine: string; values: string[] }[] = []
  if (m) {
    walkStates(m.root, (s) => {
      if (s.invoke && s.invoke.kind === 'machine') invokes.push({ machine: s.invoke.src, as: s.invoke.as, state: s.id })
    })
    for (const [name, c] of Object.entries(m.children)) group(spawns, c.machine, name)
    for (const v of [...m.params, ...m.vars]) {
      const to = refMachine(ir, v.type)
      if (to) group(refs, to, v.name)
    }
    for (const site of messageSites(machineName, m)) {
      if (site.dir === 'send') group(sends, site.event.machine, site.event.event)
      else if (site.dir === 'receive') group(receives, site.event.machine, site.event.event)
    }
  }
  return {
    invokes,
    spawns: spawns.map((r) => ({ machine: r.machine, names: r.values })),
    refs: refs.map((r) => ({ machine: r.machine, names: r.values })),
    sends: sends.map((r) => ({ machine: r.machine, events: r.values })),
    receives: receives.map((r) => ({ machine: r.machine, events: r.values })),
  }
}

/** A machine's event, accepted (`event`) or emitted (`emits`), with which it is. */
export function eventDecl(ir: Ir, ref: EventRef): { decl: EventDecl; emitted: boolean } | null {
  const m = own(ir.machines, ref.machine)
  if (!m) return null
  const accepted = own(m.events, ref.event)
  if (accepted) return { decl: accepted, emitted: false }
  const emitted = own(m.emits, ref.event)
  return emitted ? { decl: emitted, emitted: true } : null
}

/** `event Reserve { orderId: string, items: Item[] }`, `emits Progress { percent: int }` */
export function eventDeclText(name: string, decl: { fields: Field[] }, emitted: boolean): string {
  return `${emitted ? 'emits' : 'event'} ${name}${decl.fields.length ? ` { ${paramsText(decl.fields)} }` : ''}`
}
