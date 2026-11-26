// The details panel: HTML for a machine, a state, a transition or an event. Pure (strings in,
// string out), so it is tested in Node. Buttons carry data attributes the browser app acts on:
//   data-select-state="<id>", data-select-transition='["<state id>", index]',
//   data-open-machine="<name>" (data-via="invoke" when it drills into an invoke), data-open-event="<Machine>.<Event>",
//   data-goto='["<machine>", selection]' (a state or transition of any machine),
//   data-toggle="<id>" (collapse or expand), data-clear.

import type { Address, Effect, EventDecl, Ir, State, TaskDecl, Machine } from './ir.ts'
import { own } from './ir.ts'
import {
  baseType,
  effectEvent,
  effectText,
  eventDecl,
  eventDeclText,
  eventKey,
  findState,
  guardText,
  invokeText,
  messageSites,
  paramsText,
  signatureText,
  sourceText,
  talks,
  triggerKind,
  triggerSource,
  triggerText,
  typeDeclText,
  walkStates,
} from './model.ts'
import type { EventRef, MessageSite } from './model.ts'
import { esc, paramsHtml, typeHtml } from './render.ts'

export type Selection =
  | { type: 'state'; id: string }
  | { type: 'transition'; state: string; index: number }
  /** An event the machine accepts or emits. */
  | { type: 'event'; name: string }
  | null

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

function machineLink(name: string): string {
  return `<button class="rb-link rb-mono" type="button" data-open-machine="${esc(name)}">${esc(name)}</button>`
}

function eventLink(ref: EventRef, label?: string): string {
  return `<button class="rb-link rb-mono" type="button" data-open-event="${esc(eventKey(ref))}" title="Event ${esc(eventKey(ref))}">${esc(label ?? ref.event)}</button>`
}

/** A button that selects a state or transition in any machine of the IR. */
function gotoLink(machine: string, sel: Exclude<Selection, null>, label: string): string {
  return `<button class="rb-link rb-mono" type="button" data-goto="${esc(JSON.stringify([machine, sel]))}">${esc(label)}</button>`
}

/** `the child upload (invoked in Uploading)`, `the ref inventory` */
function addressText(ctx: PanelContext, a: Address): string {
  return a.kind === 'child' ? `the child ${a.name}${a.state ? `, invoked in ${shortId(ctx, a.state)}` : ''}` : `the ref ${a.name}`
}

/** An effect as HTML: a `send` or `emit` links to its event and says where it goes. */
function effectHtml(ctx: PanelContext, e: Effect): string {
  const ev = effectEvent(ctx.machine, e)
  if (!ev) return esc(effectText(e))
  const kw = e.kind === 'send' ? 'send' : 'emit'
  const text = effectText(e).slice(kw.length + 1)
  const where =
    e.kind === 'send'
      ? `to ${esc(e.to.machine)}, ${esc(addressText(ctx, e.to))}`
      : `to whoever invoked ${esc(ctx.machine)} and every machine holding a ref to it`
  return (
    `<span class="rb-msg-kw">${kw}</span> <button class="rb-link rb-mono" type="button" data-open-event="${esc(eventKey(ev))}" title="Event ${esc(eventKey(ev))}">${esc(text)}</button>` +
    `<span class="rb-sub rb-note">${where}</span>`
  )
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

function signatureKv(ir: Ir, decl: { params: { name: string; type: string; doc: string }[]; returns?: string | null; errors?: string[] }): string {
  return (
    `<dl class="rb-kv">` +
    `<dt>takes</dt><dd>${paramsHtml(ir, decl.params) || 'nothing'}</dd>` +
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
  const target = t.target ? shortId(ctx, t.target) : t.effects.length ? 'stays (internal)' : 'ignores it (internal)'
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
  s += code(signatureText(inv.kind, inv.src, decl)) + docHtml(decl.doc) + signatureKv(ir, decl)
  if (inv.kind === 'machine' && inv.as) {
    s += `<p class="rb-empty">While ${esc(state.name)} is active, <code>${esc(inv.as)}</code> names the child: <code>send ${esc(inv.as)}.Event</code> queues one of its events, <code>on ${esc(inv.as)}.Event</code> handles one it emits.</p>`
  }
  if (inv.kind === 'machine') s += `<p><button class="rb-btn" type="button" data-open-machine="${esc(inv.src)}" data-via="invoke">Open machine ${esc(inv.src)} →</button></p>`
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

/** Machines are named by buttons that open them; a list of events by buttons that open each. */
function eventLinks(machine: string, events: string[]): string {
  return events.map((e) => eventLink({ machine, event: e })).join(', ')
}

const rel = (text: string, msg = false) => `<span class="rb-rel${msg ? ' rb-t-msg' : ''}">${esc(text)}</span>`

/** Which machines this one invokes, holds a ref to, sends to and receives from. */
function talksToSection(ctx: PanelContext): string {
  const t = talks(ctx.ir, ctx.machine)
  const items: string[] = []
  for (const i of t.invokes) items.push(`${rel('invokes')} ${machineLink(i.machine)}${i.as ? ` ${rel('as')} ${esc(i.as)}` : ''} ${rel('in')} ${stateLink(ctx, i.state)}`)
  for (const r of t.refs) items.push(`${rel('holds a ref to')} ${machineLink(r.machine)} ${rel('in')} ${esc(r.names.join(', '))}`)
  for (const x of t.sends) items.push(`${rel('sends', true)} ${eventLinks(x.machine, x.events)} ${rel('to')} ${machineLink(x.machine)}`)
  for (const x of t.receives) items.push(`${rel('handles', true)} ${eventLinks(x.machine, x.events)} ${rel('emitted by')} ${machineLink(x.machine)}`)
  return items.length ? `<ul class="rb-talks">${items.map((i) => `<li>${i}</li>`).join('')}</ul>` : ''
}

/** Which other machines of the IR hold a ref to this one, send to it and handle what it emits. */
function talkedToBySection(ctx: PanelContext): string {
  const items: string[] = []
  for (const other of Object.keys(ctx.ir.machines)) {
    const t = talks(ctx.ir, other)
    const ref = t.refs.find((r) => r.machine === ctx.machine)
    if (ref) items.push(`${machineLink(other)} ${rel('holds a ref to it in')} ${esc(ref.names.join(', '))}`)
    const sends = t.sends.find((r) => r.machine === ctx.machine)
    if (sends) items.push(`${machineLink(other)} ${rel('sends', true)} ${eventLinks(ctx.machine, sends.events)}`)
    const receives = t.receives.find((r) => r.machine === ctx.machine)
    if (receives) items.push(`${machineLink(other)} ${rel('handles', true)} ${eventLinks(ctx.machine, receives.events)}`)
  }
  return items.length ? `<ul class="rb-talks">${items.map((i) => `<li>${i}</li>`).join('')}</ul>` : ''
}

function eventItems(machine: string, events: Record<string, EventDecl>): string {
  return list(
    Object.entries(events).map(
      ([name, e]) => `${eventLink({ machine, event: name })}${e.fields.length ? ` { ${esc(paramsText(e.fields))} }` : ''}${e.doc ? `<span class="rb-sub rb-note">${esc(e.doc)}</span>` : ''}`,
    ),
  )
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
    section('Signature', code(signatureText('machine', ctx.machine, m)) + signatureKv(ir, m)) +
    section(
      'Variables',
      list(m.vars.map((v) => `${esc(v.name)}: ${typeHtml(ir, v.type)}${v.init ? ` = ${esc(v.init.text)}` : ' <span class="rb-note">(assigned later)</span>'}${v.doc ? `<span class="rb-sub rb-note">${esc(v.doc)}</span>` : ''}`)),
    ) +
    section(
      'Events it accepts',
      eventItems(ctx.machine, m.events) +
        (m.strict
          ? '<p class="rb-cnote">Strict: one of these events, or a message, that no active state handles ends the run with a defect instead of being dropped.</p>'
          : ''),
    ) +
    section('Events it emits', eventItems(ctx.machine, m.emits)) +
    section('Talks to', talksToSection(ctx)) +
    section('States', `<ul class="rb-tree">${m.root.states.map(tree).join('')}</ul>`) +
    section('Invoked by', list(callers)) +
    section('Talked to by', talkedToBySection(ctx)) +
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
    section('On entry', list(state.entry.map((e) => effectHtml(ctx, e)))) +
    section('On exit', list(state.exit.map((e) => effectHtml(ctx, e)))) +
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
      if (e) extra = section('Event', code(eventDeclText(t.trigger.event, e, false)) + docHtml(e.doc) + `<p>${eventLink({ machine: ctx.machine, event: t.trigger.event }, `Who sends ${t.trigger.event} →`)}</p>`)
      break
    }
    case 'emitted': {
      const from = t.trigger.from
      const ref = { machine: from.machine, event: t.trigger.event }
      what = `${from.machine} emitted ${t.trigger.event}: ${addressText(ctx, from)}.`
      const found = eventDecl(ir, ref)
      extra = section(
        `Event of ${from.machine}`,
        (found ? code(eventDeclText(t.trigger.event, found.decl, true)) + docHtml(found.decl.doc) + `<p class="rb-path">${esc(sourceText(ir, found.decl.span))}</p>` : '') +
          `<p><button class="rb-btn" type="button" data-open-event="${esc(eventKey(ref))}">Open ${esc(eventKey(ref))} →</button></p>`,
      )
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
    section('Effects', list(t.effects.map((e) => effectHtml(ctx, e))) || '<p class="rb-empty">None.</p>') +
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

/** `Order · Reserving (entry)`, `Publish · Uploading: on PauseClicked → (stays)` */
function siteLink(ir: Ir, machine: string, site: MessageSite): string {
  const m = own(ir.machines, machine)
  const short = (id: string) => id.slice(machine.length + 1) || id
  if (typeof site.at !== 'number') return gotoLink(machine, { type: 'state', id: site.state }, `${machine} · ${short(site.state)} (${site.at})`)
  const t = m && findState(m, site.state)?.transitions[site.at]
  const label = t ? `${short(site.state)}: ${triggerSource(t.trigger)} → ${t.target ? short(t.target) : '(stays)'}` : short(site.state)
  return gotoLink(machine, { type: 'transition', state: site.state, index: site.at }, `${machine} · ${label}`)
}

/** An event the machine accepts or emits: its declaration and everywhere it is sent, emitted and handled. */
export function eventPanel(ctx: PanelContext, name: string): string {
  const { ir } = ctx
  const m = own(ir.machines, ctx.machine)!
  const found = eventDecl(ir, { machine: ctx.machine, event: name })
  if (!found) return machinePanel(ctx)
  const { decl, emitted } = found
  // Every site in the IR that names this event of this machine.
  const sites: [string, MessageSite][] = []
  for (const [other, om] of Object.entries(ir.machines)) {
    for (const site of messageSites(other, om)) if (site.event.machine === ctx.machine && site.event.event === name) sites.push([other, site])
  }
  const links = (dir: MessageSite['dir']) => list(sites.filter(([, s]) => s.dir === dir).map(([other, s]) => siteLink(ir, other, s)))
  let body = ''
  if (emitted) {
    body +=
      section('Emitted', links('emit') || '<p class="rb-empty">Nowhere: the machine never emits it.</p>') +
      section('Handled by', links('receive') || '<p class="rb-empty">No machine of this chart handles it; host code holding a handle may.</p>')
  } else {
    const handled: string[] = []
    walkStates(m.root, (s) => {
      s.transitions.forEach((t, i) => {
        if (t.trigger.kind === 'event' && t.trigger.event === name) handled.push(transitionSummary(ctx, s, i, true))
      })
    })
    body +=
      section('Handled here', handled.join('') || '<p class="rb-empty">No transition names it.</p>') +
      section('Sent by', links('send') || '<p class="rb-empty">No machine of this chart sends it; host code can.</p>')
  }
  return (
    head(esc(name), emitted ? 'emitted event' : 'event', true) +
    `<p class="rb-path">${esc(ctx.machine)}.${esc(name)} · ${esc(sourceText(ir, decl.span))}</p>` +
    docHtml(decl.doc) +
    section('Declaration', code(eventDeclText(name, decl, emitted)) + `<p class="rb-empty">${
      emitted
        ? `${esc(ctx.machine)} emits it to the machine that invoked it and to every machine holding a ref to it.`
        : `${esc(ctx.machine)} accepts it: another machine sends it with <code>send name.${esc(name)}</code>, or host code through the machine's handle.`
    }</p>`) +
    section('Fields', list(decl.fields.map((f) => `${esc(f.name)}: ${esc(f.type)}${f.doc ? `<span class="rb-sub rb-note">${esc(f.doc)}</span>` : ''}`))) +
    body
  )
}

/** The panel for a selection; the machine overview when nothing (or nothing valid) is selected. */
export function panelHtml(ctx: PanelContext, selection: Selection): string {
  const m = own(ctx.ir.machines, ctx.machine)
  if (!m) return ''
  if (selection && selection.type === 'event') return eventPanel(ctx, selection.name)
  const state = selection && findState(m, selection.type === 'state' ? selection.id : selection.state)
  if (!selection || !state) return machinePanel(ctx)
  if (selection.type === 'state') return statePanel(ctx, state)
  if (!state.transitions[selection.index]) return machinePanel(ctx)
  return transitionPanel(ctx, state, selection.index)
}
