// View model: turns IR states and transitions into the lines of text the diagram shows.
// Pure and DOM-free. Shared by layout.js (to size boxes) and render.js (to draw them),
// so the two always agree on what goes where.

export const METRICS = {
  charW: 0.62,        // estimated advance of a monospace glyph, in em (the browser measures instead)
  fontTitle: 13,
  fontRow: 11.5,
  fontLabel: 12,
  fontEffect: 11,
  rowH: 17,
  titleH: 30,         // title band of a state box
  padX: 12,
  rowsPadY: 7,        // space above and below the rows of a state box
  finalInset: 4,      // room for the double border of a final state
  labelPadX: 7,
  labelPadY: 4,
  labelLineH: 16,
  effectLineH: 15,
  initialR: 5.5,
  wrapAt: 46,         // wrap code longer than this many characters
  labelWrapAt: 30,    // put the guard of an edge label on its own line beyond this
  machinePillPad: 6,
}

export function estimateWidth(text, fontSize) {
  return text.length * fontSize * METRICS.charW
}

// Break a long piece of code at ", " or " " so it fits in `max` characters; continuation lines are indented.
export function wrapCode(text, max = METRICS.wrapAt) {
  if (text.length <= max) return [text]
  const out = []
  let rest = text
  let first = true
  while (rest.length > (first ? max : max - 2)) {
    const limit = first ? max : max - 2
    let cut = rest.lastIndexOf(', ', limit - 1)
    if (cut > 8) cut += 1
    else cut = rest.lastIndexOf(' ', limit)
    if (cut <= 8) break
    out.push((first ? '' : '  ') + rest.slice(0, cut).trimEnd())
    rest = rest.slice(cut).trimStart()
    first = false
  }
  out.push((first ? '' : '  ') + rest)
  return out
}

export function effectText(e) {
  return e.kind === 'set'
    ? `set ${e.var} = ${e.value.text}`
    : `do ${e.action}(${e.args.map((a) => a.text).join(', ')})`
}

export function invokeText(inv) {
  return `${inv.src}(${inv.args.map((a) => a.text).join(', ')})`
}

// The kind of a trigger as the diagram encodes it: line style, colour and wording.
export function triggerKind(trigger) {
  switch (trigger.kind) {
    case 'event': return 'event'
    case 'after': return 'after'
    case 'always': return 'always'
    case 'invoke.fail': return 'fail'
    default: return 'done' // done, invoke.done
  }
}

export function triggerText(trigger) {
  switch (trigger.kind) {
    case 'event': return trigger.event
    case 'after': return `after ${trigger.delay.text}`
    case 'always': return 'always'
    case 'invoke.fail': return `fail ${trigger.error}`
    default: return 'done'
  }
}

// "[guard]" for a transition, "[else]" for the unguarded last branch of a block, "" otherwise.
export function guardText(state, index) {
  const t = state.transitions[index]
  if (t.guard) return `[${t.guard.text}]`
  const first = state.transitions.findIndex((o) => o.group === t.group)
  return first !== index ? '[else]' : ''
}

// Lines of an edge label: the trigger (+ guard) first, then one line per effect.
// A long trigger + guard is split over two lines, for every branch of the block alike,
// so labels stay narrow and the branches of one trigger look the same.
export function transitionLines(state, index) {
  const t = state.transitions[index]
  const head = triggerText(t.trigger)
  const guard = guardText(state, index)
  const split = state.transitions.some((o, i) => {
    const g = o.group === t.group ? guardText(state, i) : ''
    return g && (head + ' ' + g).length > METRICS.labelWrapAt
  })
  const lines = []
  if (guard && split) {
    lines.push({ cls: 'trigger', text: head, guard: '' })
    for (const l of wrapCode(guard)) lines.push({ cls: 'trigger', text: '', guard: l })
  } else {
    lines.push({ cls: 'trigger', text: head, guard })
  }
  for (const e of t.effects) for (const l of wrapCode(effectText(e))) lines.push({ cls: 'effect', text: l })
  return lines
}

// Rows shown inside a state box. `kw` is drawn muted, `text` in the code colour.
//   { kw, text, cls?, machine?, transition? }
export function stateRows(state, depth) {
  const rows = []
  const add = (kw, text, extra = {}) => {
    wrapCode(text, METRICS.wrapAt - kw.length - 1).forEach((l, i) => {
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
    const kind = triggerKind(t.trigger)
    const head = (kind === 'event' ? 'on ' : '') + triggerText(t.trigger)
    const guard = guardText(state, index)
    const body = [guard, t.effects.map(effectText).join(', ')].filter(Boolean).join(' ')
    add(head, body, { transition: index, cls: kind })
  })
  if (state.output) add('output', state.output.text)
  if (state.fail) add('fail', state.fail.text, { cls: 'fail' })
  return rows
}

export function walkStates(state, fn, parent = null, depth = 0) {
  fn(state, parent, depth)
  for (const c of state.states) walkStates(c, fn, state, depth + 1)
}

export function findState(machine, id) {
  let found = null
  walkStates(machine.root, (s) => { if (s.id === id) found = s })
  return found
}

// The machine root is drawn as a frame only when it carries something of its own.
export function rootIsVisible(machine) {
  const r = machine.root
  return r.transitions.length > 0 || r.entry.length > 0 || r.exit.length > 0 || r.invoke !== null
}

export function paramsText(params) {
  return params.map((p) => `${p.name}: ${p.type}`).join(', ')
}

// "machine Checkout(order: Order, user: User) -> Receipt ! CardDeclined | GatewayUnavailable"
export function signatureText(kind, name, decl) {
  let s = `${kind} ${name}(${paramsText(decl.params)})`
  if (decl.returns) s += ` -> ${decl.returns}`
  if (decl.errors && decl.errors.length) s += ` ! ${decl.errors.join(' | ')}`
  return s
}

export function sourceText(ir, span) {
  const src = ir.sources[span.file]
  return `${src ? src.path : '?'}:${span.line}`
}
