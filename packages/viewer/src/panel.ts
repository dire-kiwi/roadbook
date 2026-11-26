// The details panel: HTML for a machine, a state or a transition. Pure (strings in, string out),
// so it is tested in Node. Buttons carry data attributes the browser app acts on:
//   data-select-state="<id>", data-select-transition='["<state id>", index]',
//   data-open-machine="<name>", data-toggle="<id>" (collapse or expand), data-clear.

import type { Ir, State, TaskDecl, Machine } from './ir.ts'
import { own } from './ir.ts'
import {
  baseType,
  effectText,
  findState,
  guardText,
  invokeText,
  paramsText,
  signatureText,
  sourceText,
  triggerKind,
  triggerSource,
  triggerText,
  typeDeclText,
  walkStates,
} from './model.ts'
import { esc } from './render.ts'

export type Selection = { type: 'state'; id: string } | { type: 'transition'; state: string; index: number } | null

export interface PanelContext {
  ir: Ir
  machine: string
  /** Ids of the states drawn collapsed. */
  collapsed: ReadonlySet<string>
}

const code = (s: string) => `<pre class="rb-code">${esc(s)}</pre>`
const section = (title: string, body: string) => (body ? `<h3>${esc(title)}</h3>${body}` : '')
const list = (items: string[]) => (items.length ? `<ul class="rb-list">${items.map((i) => `<li>${i}</li>`).join('')}</ul>` : '')
const docHtml = (doc: string) => (doc ? `<p class="rb-doc">${esc(doc).replace(/\n/g, '<br>')}</p>` : '')

function shortId(ctx: PanelContext, id: string): string {
  return id.slice(ctx.machine.length + 1) || id
}

function stateLink(ctx: PanelContext, id: string, label?: string): string {
  return `<button class="rb-link rb-mono" type="button" data-select-state="${esc(id)}">${esc(label ?? shortId(ctx, id))}</button>`
}

function head(title: string, chip: string, closable: boolean): string {
  return (
    `<div class="rb-panel-head"><h2>${title}</h2><span class="rb-chip">${esc(chip)}</span>` +
    (closable ? `<button class="rb-btn" type="button" data-clear title="Back to the machine overview (Esc)" aria-label="Close">×</button>` : '') +
    `</div>`
  )
}

/** Declarations of the user types a signature mentions, one per line. */
function typesOf(ir: Ir, decl: { params: { type: string }[]; returns?: string | null; errors?: string[] }): string {
  const names = [...decl.params.map((p) => p.type), decl.returns ?? null, ...(decl.errors ?? [])].filter((t): t is string => !!t).map(baseType)
  const lines = [...new Set(names)].flatMap((n) => {
    const t = own(ir.types, n)
    return t ? [typeDeclText(n, t)] : []
  })
  return lines.length ? code(lines.join('\n')) : ''
}

function signatureKv(decl: { params: { name: string; type: string; doc: string }[]; returns?: string | null; errors?: string[] }): string {
  return (
    `<dl class="rb-kv">` +
    `<dt>takes</dt><dd>${esc(paramsText(decl.params) || 'nothing')}</dd>` +
    (decl.returns !== undefined ? `<dt>returns</dt><dd>${esc(decl.returns ?? 'nothing')}</dd>` : '') +
    (decl.errors ? `<dt>fails with</dt><dd>${esc(decl.errors.join(' | ') || 'never fails')}</dd>` : '') +
    `</dl>`
  )
}

function transitionSummary(ctx: PanelContext, state: State, index: number, withSource: boolean): string {
  const t = state.transitions[index]!
  const g = guardText(state, index)
  const kind = triggerKind(t.trigger)
  const cls = kind === 'done' ? 'rb-t-done' : kind === 'fail' ? 'rb-t-fail' : ''
  const target = t.target ? shortId(ctx, t.target) : 'stays (internal)'
  return (
    `<button class="rb-tlink" type="button" data-select-transition="${esc(JSON.stringify([state.id, index]))}">` +
    (withSource ? `<span class="rb-arrow-text">${esc(shortId(ctx, state.id))}: </span>` : '') +
    `<b class="${cls}">${esc(triggerText(t.trigger))}</b>${g ? ' ' + esc(g) : ''} <span class="rb-arrow-text">→</span> ${esc(target)}` +
    t.effects.map((e) => `<span class="rb-sub">${esc(effectText(e))}</span>`).join('') +
    `</button>`
  )
}

function invokeSection(ctx: PanelContext, state: State): string {
  const { ir } = ctx
  const inv = state.invoke
  if (!inv) return ''
  const decl: Machine | TaskDecl | undefined = inv.kind === 'machine' ? own(ir.machines, inv.src) : own(ir.tasks, inv.src)
  let s = code(`invoke ${invokeText(inv)}`)
  if (!decl) return section('Invoke', s)
  s += `<p class="rb-path">${inv.kind} declared at ${esc(sourceText(ir, decl.span))}</p>`
  s += code(signatureText(inv.kind, inv.src, decl)) + docHtml(decl.doc) + signatureKv(decl)
  if (inv.kind === 'machine') s += `<p><button class="rb-btn" type="button" data-open-machine="${esc(inv.src)}">Open machine ${esc(inv.src)} →</button></p>`
  return section(`Invokes ${inv.kind}`, s) + section('Types involved', typesOf(ir, decl))
}

/** The machines of the IR that invoke `name`, as `[machine, state id]`. */
export function callersOf(ir: Ir, name: string): [string, string][] {
  const out: [string, string][] = []
  for (const [other, m] of Object.entries(ir.machines)) {
    walkStates(m.root, (s) => {
      if (s.invoke && s.invoke.kind === 'machine' && s.invoke.src === name) out.push([other, s.id])
    })
  }
  return out
}

export function machinePanel(ctx: PanelContext): string {
  const { ir } = ctx
  const m = own(ir.machines, ctx.machine)!
  const tree = (s: State): string =>
    `<li>${stateLink(ctx, s.id, s.name)} <span class="rb-chip">${s.kind}${ctx.collapsed.has(s.id) ? ', collapsed' : ''}</span>${s.states.length ? `<ul>${s.states.map(tree).join('')}</ul>` : ''}</li>`
  const callers = callersOf(ir, ctx.machine).map(([name, id]) => `<button class="rb-link rb-mono" type="button" data-open-machine="${esc(name)}">${esc(id)}</button>`)
  return (
    head(esc(ctx.machine), 'machine', false) +
    `<p class="rb-path">${esc(sourceText(ir, m.span))}</p>` +
    docHtml(m.doc) +
    section('Signature', code(signatureText('machine', ctx.machine, m)) + signatureKv(m)) +
    section(
      'Variables',
      list(m.vars.map((v) => `${esc(v.name)}: ${esc(v.type)}${v.init ? ` = ${esc(v.init.text)}` : ' <span class="rb-note">(assigned later)</span>'}${v.doc ? `<span class="rb-sub rb-note">${esc(v.doc)}</span>` : ''}`)),
    ) +
    section(
      'Events',
      list(Object.entries(m.events).map(([name, e]) => `${esc(name)}${e.fields.length ? ` { ${esc(paramsText(e.fields))} }` : ''}${e.doc ? `<span class="rb-sub rb-note">${esc(e.doc)}</span>` : ''}`)),
    ) +
    section('States', `<ul class="rb-tree">${m.root.states.map(tree).join('')}</ul>`) +
    section('Invoked by', list(callers)) +
    section('Types involved', typesOf(ir, m)) +
    `<h3>Tip</h3><p class="rb-empty">Click a state or an arrow for its details. <kbd>/</kbd> searches, <kbd>f</kbd> fits the diagram to the view.</p>`
  )
}

export function statePanel(ctx: PanelContext, state: State): string {
  const { ir } = ctx
  const m = own(ir.machines, ctx.machine)!
  const parents: State[] = []
  let incoming = ''
  walkStates(m.root, (s) => {
    if (state.id !== s.id && state.id.startsWith(s.id + '.')) parents.push(s)
    s.transitions.forEach((t, i) => {
      if (t.target === state.id) incoming += transitionSummary(ctx, s, i, true)
    })
  })
  const parent = parents[parents.length - 1]
  const isInitial = parent !== undefined && parent.initial === state.id
  const out = state.transitions.map((_t, i) => transitionSummary(ctx, state, i, false)).join('')
  let inherited = ''
  for (const p of parents.slice().reverse()) {
    if (!p.transitions.length) continue
    inherited += `<p class="rb-empty">from ${stateLink(ctx, p.id, p.id === m.root.id ? p.name : undefined)}</p>` + p.transitions.map((_t, i) => transitionSummary(ctx, p, i, false)).join('')
  }
  const result = state.output ? code(`output ${state.output.text}`) : state.fail ? code(`fail ${state.fail.text}`) : ''
  const collapsed = ctx.collapsed.has(state.id)
  const toggle = state.states.length
    ? `<p><button class="rb-btn" type="button" data-toggle="${esc(state.id)}">${collapsed ? 'Expand' : 'Collapse'} ${esc(state.name)}</button></p>`
    : ''
  const children = state.states.length ? list(state.states.map((c) => `${stateLink(ctx, c.id, c.name)} <span class="rb-chip">${c.kind}</span>`)) : ''
  return (
    head(esc(state.name), `${state.kind} state${isInitial ? ', initial' : ''}${collapsed ? ', collapsed' : ''}`, true) +
    `<p class="rb-path">${esc(state.id)} · ${esc(sourceText(ir, state.span))}</p>` +
    docHtml(state.doc) +
    toggle +
    section(state.output ? 'Machine returns' : 'Machine fails with', result) +
    invokeSection(ctx, state) +
    section('Starts in', state.initial ? stateLink(ctx, state.initial) : '') +
    section(state.kind === 'parallel' ? 'Regions' : 'States inside', children) +
    section('On entry', list(state.entry.map((e) => esc(effectText(e))))) +
    section('On exit', list(state.exit.map((e) => esc(effectText(e))))) +
    section('Transitions out', out || (state.kind === 'final' ? '<p class="rb-empty">None: this state ends its parent.</p>' : '<p class="rb-empty">None.</p>')) +
    section('Also applies here', inherited) +
    section('Transitions in', incoming || (isInitial ? '<p class="rb-empty">Entered first (initial state).</p>' : ''))
  )
}

export function transitionPanel(ctx: PanelContext, state: State, index: number): string {
  const { ir } = ctx
  const t = state.transitions[index]!
  const m = own(ir.machines, ctx.machine)!
  const group = state.transitions.map((o, i) => [o, i] as const).filter(([o]) => o.group === t.group)
  const pos = group.findIndex(([, i]) => i === index) + 1
  const inv = state.invoke
  const decl = inv ? (inv.kind === 'machine' ? own(ir.machines, inv.src) : own(ir.tasks, inv.src)) : undefined
  let what = ''
  let extra = ''
  const short = shortId(ctx, state.id)
  switch (t.trigger.kind) {
    case 'event': {
      const e = own(m.events, t.trigger.event)
      what = `Event ${t.trigger.event} arrives.`
      if (e) extra = section('Event', code(`event ${t.trigger.event}${e.fields.length ? ` { ${paramsText(e.fields)} }` : ''}`) + docHtml(e.doc))
      break
    }
    case 'any':
      what = `Any event arrives for which ${short} has no on.`
      break
    case 'after':
      what = `The machine has been in ${short} for ${t.trigger.delay.text}, measured from entering it.`
      break
    case 'always':
      what = t.guard ? 'Taken as soon as the guard holds; no event needed.' : 'Taken immediately on entering the state.'
      break
    case 'done':
      what = state.kind === 'parallel' ? `Every region of ${short} reached a final state.` : `${short} reached a final child state.`
      break
    case 'invoke.done':
      what = `${inv ? inv.src : 'The invoke'} finished; its value is available as result.`
      break
    case 'invoke.fail': {
      const err = t.trigger.error
      what = `${inv ? inv.src : 'The invoke'} failed with ${err === '*' ? 'a declared error not listed' : err}; the error is available as error.`
      const td = own(ir.types, err)
      if (td) extra = section('Error', code(typeDeclText(err, td)) + docHtml(td.doc))
      break
    }
  }
  if (decl && inv && t.trigger.kind.startsWith('invoke.')) {
    extra += section(`Invoked ${inv.kind}`, code(signatureText(inv.kind, inv.src, decl)) + `<p class="rb-path">${esc(sourceText(ir, decl.span))}</p>` + docHtml(decl.doc))
  }
  const guard = t.guard ? code(t.guard.text) : group.length > 1 ? code('else') : ''
  const branchNote =
    group.length > 1 ? `<p class="rb-empty">Branch ${pos} of ${group.length} for <code>${esc(triggerSource(t.trigger))}</code>; branches are tried in order.</p>` : ''
  const title = `${esc(short)} <span class="rb-arrow-text">→</span> ${t.target ? esc(shortId(ctx, t.target)) : '(stays)'}`
  return (
    head(title, 'transition', true) +
    `<p class="rb-path">${esc(sourceText(ir, t.span))}</p>` +
    section('Trigger', code(triggerSource(t.trigger)) + `<p class="rb-empty">${esc(what)}</p>`) +
    section('Guard', guard + branchNote) +
    section('Effects', list(t.effects.map((e) => esc(effectText(e)))) || '<p class="rb-empty">None.</p>') +
    section('From', stateLink(ctx, state.id)) +
    section(
      'To',
      t.target
        ? stateLink(ctx, t.target) + (t.target === state.id ? '<p class="rb-empty">The state is left and entered again: its exit and entry effects run.</p>' : '')
        : '<p class="rb-empty">Internal: the state is not left, so no exit or entry effects run.</p>',
    ) +
    extra
  )
}

/** The panel for a selection; the machine overview when nothing (or nothing valid) is selected. */
export function panelHtml(ctx: PanelContext, selection: Selection): string {
  const m = own(ctx.ir.machines, ctx.machine)
  if (!m) return ''
  const state = selection && findState(m, selection.type === 'state' ? selection.id : selection.state)
  if (!selection || !state) return machinePanel(ctx)
  if (selection.type === 'state') return statePanel(ctx, state)
  if (!state.transitions[selection.index]) return machinePanel(ctx)
  return transitionPanel(ctx, state, selection.index)
}
