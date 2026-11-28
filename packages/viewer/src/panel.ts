// The details panel: HTML for a machine, a state, a transition, an event or a call. Pure (strings in,
// string out), so it is tested in Node. Buttons carry data attributes the browser app acts on:
//   data-select-state="<id>", data-select-transition='["<state id>", index]',
//   data-open-machine="<name>" (data-via="invoke" when it drills into an invoke),
//   data-open-event="<Machine>.<Event>" or "<Machine>.<call>",
//   data-goto='["<machine>", selection]' (a state or transition of any machine),
//   data-toggle="<id>" (collapse or expand), data-clear.

import type { Address, CallDecl, Effect, EventDecl, Invoke, Ir, State, TaskDecl, Machine } from './ir.ts'
import { own } from './ir.ts'
import {
  callSignatureText,
  childStates,
  effectEvent,
  effectText,
  eventDecl,
  eventDeclText,
  eventKey,
  findState,
  guardText,
  historyText,
  invokeCall,
  invokeText,
  isHistory,
  isTaskEvent,
  kindText,
  messageSites,
  paramsText,
  parentState,
  peer,
  signatureText,
  sourceText,
  spawnSites,
  talks,
  targetsText,
  triggerKind,
  triggerSource,
  triggerText,
  typeDeclText,
  typeNamesOf,
  walkStates,
} from './model.ts'
import type { EventRef, MessageSite, SpawnSite } from './model.ts'
import { childrenHtml, esc, paramsHtml, typeHtml } from './render.ts'

export type Selection =
  | { type: 'state'; id: string }
  | { type: 'transition'; state: string; index: number }
  /**
   * An event the machine accepts or emits, or (a lower-case name) one of its calls, or (a
   * dotted name, `upload.Progress`) an event a task emits, shown on the current machine's page.
   */
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

/** A transition's target as the panel names it: `Processing.Paying`, `Working.Previous (history)`. */
function targetText(ctx: PanelContext, id: string): string {
  const m = own(ctx.ir.machines, ctx.machine)
  const s = m && findState(m, id)
  return shortId(ctx, id) + (s && isHistory(s) ? ` (${kindText(s)})` : '')
}

function stateLink(ctx: PanelContext, id: string, label?: string): string {
  return `<button class="rb-link rb-mono" type="button" data-select-state="${esc(id)}">${esc(label ?? shortId(ctx, id))}</button>`
}

function machineLink(name: string): string {
  return `<button class="rb-link rb-mono" type="button" data-open-machine="${esc(name)}">${esc(name)}</button>`
}

/** Whether an event reference names a call: call names are lower case, event names upper case. */
const isCall = (ref: EventRef) => /^[a-z]/.test(ref.event)

function eventLink(ref: EventRef, label?: string): string {
  return `<button class="rb-link rb-mono" type="button" data-open-event="${esc(eventKey(ref))}" title="${isCall(ref) ? 'Call' : 'Event'} ${esc(eventKey(ref))}">${esc(label ?? ref.event)}</button>`
}

/** A button that selects a state or transition in any machine of the IR. */
function gotoLink(machine: string, sel: Exclude<Selection, null>, label: string): string {
  return `<button class="rb-link rb-mono" type="button" data-goto="${esc(JSON.stringify([machine, sel]))}">${esc(label)}</button>`
}

/**
 * `the child upload, invoked in Uploading`, `the task up, invoked in Uploading`, `the ref
 * inventory`, for a collection of spawned children `the child of sessions with key event.id` (a
 * send's `key`), `every running child of sessions` (a send without one), `a child of sessions`
 * or `a task of uploads` (a sender).
 */
function addressText(ctx: PanelContext, a: Address, key?: { text: string } | null): string {
  switch (a.kind) {
    case 'child':
    case 'task':
      return `the ${a.kind} ${a.name}${a.state ? `, invoked in ${shortId(ctx, a.state)}` : ''}`
    case 'ref':
      return `the ref ${a.name}`
    case 'children':
      if (key === undefined) return `a ${a.task !== undefined ? 'task' : 'child'} of ${a.name}`
      return key ? `the child of ${a.name} with key ${key.text}` : `every running child of ${a.name}`
  }
}

/**
 * An effect as HTML: a `send` or `emit` links to its event and says where it goes, a `reply`
 * to its call, a `spawn` links to the child's machine.
 */
function effectHtml(ctx: PanelContext, e: Effect): string {
  if (e.kind === 'spawn') {
    const task = own(own(ctx.ir.machines, ctx.machine)?.children ?? {}, e.children)?.kind === 'task'
    const text = esc(effectText(e).slice('spawn '.length))
    return (
      `<span class="rb-spawn-kw">spawn</span> ` +
      (task ? text : `<button class="rb-link rb-mono" type="button" data-open-machine="${esc(e.src)}" data-via="spawn" title="Open machine ${esc(e.src)}">${text}</button>`) +
      `<span class="rb-sub rb-note">starts a ${task ? 'task' : 'child'} of ${esc(e.children)} under the key once the step has settled, stopping the one already under it</span>`
    )
  }
  if (e.kind === 'stop') return `<span class="rb-spawn-kw">stop</span> ${esc(effectText(e).slice('stop '.length))}<span class="rb-sub rb-note">stops the child of ${esc(e.children)} under the key, if there is one</span>`
  const ev = effectEvent(ctx.machine, e)
  if (!ev || (e.kind !== 'send' && e.kind !== 'emit' && e.kind !== 'reply')) return esc(effectText(e))
  const kw = e.kind
  const text = effectText(e).slice(kw.length + 1)
  const where =
    e.kind === 'send'
      ? `to ${esc(peer(e.to))}, ${esc(addressText(ctx, e.to, e.key))}`
      : e.kind === 'emit'
        ? `to whoever invoked or spawned ${esc(ctx.machine)} and every machine holding a ref to it`
        : e.call.text === 'call'
          ? `answers the ${esc(ev.event)} call being taken, to its caller alone`
          : `answers the ${esc(ev.event)} call ${esc(e.call.text)} holds, to its caller alone`
  return (
    `<span class="rb-msg-kw">${kw}</span> <button class="rb-link rb-mono" type="button" data-open-event="${esc(eventKey(ev))}" title="${isCall(ev) ? 'Call' : 'Event'} ${esc(eventKey(ev))}">${esc(text)}</button>` +
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
  const names = [...decl.params.map((p) => p.type), decl.returns ?? null, ...(decl.errors ?? [])].filter((t): t is string => !!t).flatMap(typeNamesOf)
  const lines = [...new Set(names)].flatMap((n) => {
    const t = own(ir.types, n)
    return t ? [typeDeclText(n, t)] : []
  })
  return lines.length ? code(lines.join('\n')) : ''
}

/** `takes`, `returns` and `fails with`; a call (`call`) fails with `Unanswered` too. */
function signatureKv(ir: Ir, decl: { params: { name: string; type: string; doc: string }[]; returns?: string | null; errors?: string[] }, call = false): string {
  const errors = [...(decl.errors ?? []), ...(call ? ['Unanswered'] : [])]
  return (
    `<dl class="rb-kv">` +
    `<dt>takes</dt><dd>${paramsHtml(ir, decl.params) || 'nothing'}</dd>` +
    (decl.returns !== undefined ? `<dt>returns</dt><dd>${esc(decl.returns ?? 'nothing')}</dd>` : '') +
    (decl.errors ? `<dt>fails with</dt><dd>${esc(errors.join(' | ') || 'never fails')}</dd>` : '') +
    `</dl>`
  )
}

function transitionSummary(ctx: PanelContext, state: State, index: number, withSource: boolean): string {
  const t = state.transitions[index]!
  const g = guardText(state, index)
  const kind = triggerKind(t.trigger)
  const cls = kind === 'done' ? 'rb-t-done' : kind === 'fail' ? 'rb-t-fail' : ''
  const target = t.targets.length
    ? t.targets.map((x) => targetText(ctx, x)).join(', ') + (t.internal ? ` (inside ${shortId(ctx, state.id)})` : '')
    : t.effects.length ? 'stays (no target)' : 'ignores it (no target)'
  return (
    `<button class="rb-tlink" type="button" data-select-transition="${esc(JSON.stringify([state.id, index]))}">` +
    (withSource ? `<span class="rb-arrow-text">${esc(shortId(ctx, state.id))}: </span>` : '') +
    `<b class="${cls}">${esc(triggerText(t.trigger))}</b>${g ? ' ' + esc(g) : ''} <span class="rb-arrow-text">→</span> ${esc(target)}` +
    t.effects.map((e) => `<span class="rb-sub">${esc(effectText(e))}</span>`).join('') +
    `</button>`
  )
}

/** What an invoke runs: the task, the machine, or the call of the callee's machine. */
function invokeDecl(ir: Ir, inv: Invoke): Machine | TaskDecl | CallDecl | undefined {
  if (inv.kind === 'machine') return own(ir.machines, inv.src)
  if (inv.kind === 'call') return inv.to ? own(own(ir.machines, peer(inv.to))?.calls ?? {}, inv.src) : undefined
  return own(ir.tasks, inv.src)
}

/** How an invoke names what it runs: `chargeCard`, `Checkout`, `inventory.reserve`. */
const invokeName = (inv: Invoke) => (inv.kind === 'call' && inv.to ? `${inv.to.name}.${inv.src}` : inv.src)

const UNANSWERED_NOTE =
  'Every call can also fail with the built-in <code>Unanswered</code>: the callee was not running, took no <code>on</code> for it (it is dropped), or ended before it replied.'

function invokeSection(ctx: PanelContext, state: State): string {
  const { ir } = ctx
  const inv = state.invoke
  if (!inv) return ''
  const decl = invokeDecl(ir, inv)
  let s = code(`invoke ${invokeText(inv)}`)
  if (!decl) return section('Invoke', s)
  const call = invokeCall(inv)
  if (call && inv.to) {
    s += `<p class="rb-path">call of ${esc(call.machine)} declared at ${esc(sourceText(ir, decl.span))}</p>`
    s += code(signatureText('call', inv.src, decl)) + docHtml(decl.doc) + signatureKv(ir, decl, true)
    s += `<p class="rb-empty">Asks the run <code>${esc(inv.to.name)}</code> holds and waits: its <code>reply</code> is the outcome of this invoke, and reaches this run alone. ${UNANSWERED_NOTE}</p>`
    s += `<p>${eventLink(call, `Open call ${eventKey(call)} →`)} ${machineLink(call.machine)}</p>`
    return section('Invokes call', s) + section('Types involved', typesOf(ir, decl))
  }
  s += `<p class="rb-path">${inv.kind} declared at ${esc(sourceText(ir, decl.span))}</p>`
  s += code(signatureText(inv.kind, inv.src, decl)) + docHtml(decl.doc) + signatureKv(ir, decl)
  const task = inv.kind === 'task' ? own(ir.tasks, inv.src) : undefined
  if (task && Object.keys(task.emits).length) {
    s +=
      `<p class="rb-cnote">It emits, while it runs:</p>` +
      eventItems(inv.src, task.emits) +
      (inv.as
        ? `<p class="rb-empty">While ${esc(state.name)} is active, <code>${esc(inv.as)}</code> names the task: <code>on ${esc(inv.as)}.Event</code> handles one it emits. A task takes no events, so nothing is sent to it.</p>`
        : `<p class="rb-empty">Invoked without <code>as</code>, so what it emits reaches no one.</p>`)
  }
  if (inv.kind === 'machine' && inv.as) {
    s += `<p class="rb-empty">While ${esc(state.name)} is active, <code>${esc(inv.as)}</code> names the child: <code>send ${esc(inv.as)}.Event</code> queues one of its events, <code>on ${esc(inv.as)}.Event</code> handles one it emits.</p>`
  }
  if (!state.transitions.some((t) => t.trigger.kind === 'invoke.done' || t.trigger.kind === 'invoke.fail')) {
    s += `<p class="rb-empty">No outcomes are written: it runs until ${esc(state.name)} is left, which cancels it. If it ends by itself first, its outcome is dropped.</p>`
  }
  if (inv.kind === 'machine') s += `<p><button class="rb-btn" type="button" data-open-machine="${esc(inv.src)}" data-via="invoke">Open machine ${esc(inv.src)} →</button></p>`
  return section(`Invokes ${inv.kind}`, s) + section('Types involved', typesOf(ir, decl))
}

/** The machines of the IR that spawn `name`, as `[machine, collection]`. */
export function spawnersOf(ir: Ir, name: string): [string, string][] {
  const out: [string, string][] = []
  for (const [other, m] of Object.entries(ir.machines)) {
    for (const [children, c] of Object.entries(m.children)) if (c.kind === 'machine' && c.src === name) out.push([other, children])
  }
  return out
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

/** Which machines this one invokes, spawns, holds a ref to, sends to, calls and receives from. */
function talksToSection(ctx: PanelContext): string {
  const t = talks(ctx.ir, ctx.machine)
  const items: string[] = []
  for (const i of t.invokes) items.push(`${rel('invokes')} ${machineLink(i.machine)}${i.as ? ` ${rel('as')} ${esc(i.as)}` : ''} ${rel('in')} ${stateLink(ctx, i.state)}`)
  for (const x of t.spawns) items.push(`${rel('spawns')} ${machineLink(x.machine)} ${rel('into')} ${esc(x.names.join(', '))}`)
  for (const x of t.spawnsTasks) items.push(`${rel('spawns task')} <code>${esc(x.task)}</code> ${rel('into')} ${esc(x.names.join(', '))}`)
  for (const r of t.refs) items.push(`${rel('holds a ref to')} ${machineLink(r.machine)} ${rel('in')} ${esc(r.names.join(', '))}`)
  for (const x of t.sends) items.push(`${rel('sends', true)} ${eventLinks(x.machine, x.events)} ${rel('to')} ${machineLink(x.machine)}`)
  for (const x of t.calls) items.push(`${rel('calls', true)} ${eventLinks(x.machine, x.calls)} ${rel('on')} ${machineLink(x.machine)}`)
  for (const x of t.receives) items.push(`${rel('handles', true)} ${eventLinks(x.machine, x.events)} ${rel('emitted by')} ${machineLink(x.machine)}`)
  for (const x of t.receivesTasks) items.push(`${rel('handles', true)} ${eventLinks(x.task, x.events)} ${rel('emitted by task')} <code>${esc(x.task)}</code>`)
  for (const x of t.ends) items.push(`${rel('hears when', true)} ${machineLink(x.machine)} ${rel('ends, through')} ${esc(x.names.join(', '))}`)
  return items.length ? `<ul class="rb-talks">${items.map((i) => `<li>${i}</li>`).join('')}</ul>` : ''
}

/** Which other machines of the IR hold a ref to this one, send to it, call it and handle what it emits. */
function talkedToBySection(ctx: PanelContext): string {
  const items: string[] = []
  for (const other of Object.keys(ctx.ir.machines)) {
    const t = talks(ctx.ir, other)
    const ref = t.refs.find((r) => r.machine === ctx.machine)
    if (ref) items.push(`${machineLink(other)} ${rel('holds a ref to it in')} ${esc(ref.names.join(', '))}`)
    const sends = t.sends.find((r) => r.machine === ctx.machine)
    if (sends) items.push(`${machineLink(other)} ${rel('sends', true)} ${eventLinks(ctx.machine, sends.events)}`)
    const calls = t.calls.find((r) => r.machine === ctx.machine)
    if (calls) items.push(`${machineLink(other)} ${rel('calls', true)} ${eventLinks(ctx.machine, calls.calls)}`)
    const receives = t.receives.find((r) => r.machine === ctx.machine)
    if (receives) items.push(`${machineLink(other)} ${rel('handles', true)} ${eventLinks(ctx.machine, receives.events)}`)
    const ends = t.ends.find((r) => r.machine === ctx.machine)
    if (ends) items.push(`${machineLink(other)} ${rel('hears when it ends, through', true)} ${esc(ends.names.join(', '))}`)
  }
  return items.length ? `<ul class="rb-talks">${items.map((i) => `<li>${i}</li>`).join('')}</ul>` : ''
}

/** `reserve(orderId: string, items: Item[]) ! OutOfStock`, the name a link to the call. */
function callItems(machine: string, calls: Record<string, CallDecl>): string {
  return list(
    Object.entries(calls).map(([name, c]) => {
      const sig = callSignatureText(name, c).slice(name.length)
      return `${eventLink({ machine, event: name })}${esc(sig)}${c.doc ? `<span class="rb-sub rb-note">${esc(c.doc)}</span>` : ''}`
    }),
  )
}

/** Where a machine spawns or stops a child, in it: `Serving: on Join`, `Serving (entry)`. */
function spawnSiteLink(ctx: PanelContext, m: Machine, site: SpawnSite): string {
  const short = shortId(ctx, site.state)
  if (typeof site.at !== 'number') return stateLink(ctx, site.state, `${short} (${site.at})`)
  const t = findState(m, site.state)?.transitions[site.at]
  const label = t ? `${short}: ${triggerSource(t.trigger)}` : short
  return `<button class="rb-link rb-mono" type="button" data-select-transition="${esc(JSON.stringify([site.state, site.at]))}">${esc(label)}</button>`
}

/** A machine's collections of spawned children, each with where it is spawned and stopped. */
function childrenSection(ctx: PanelContext, m: Machine): string {
  const sites = spawnSites(m)
  return list(
    Object.entries(m.children).map(([name, c]) => {
      const where = sites
        .filter((x) => x.children === name)
        .map((x) => `<span class="rb-sub">${rel(x.op === 'spawn' ? 'spawned' : 'stopped')} ${spawnSiteLink(ctx, m, x)}</span>`)
        .join('')
      return `${childrenHtml(ctx.ir, name, c)}${c.doc ? `<span class="rb-sub rb-note">${esc(c.doc)}</span>` : ''}${where}`
    }),
  )
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
    `<li>${stateLink(ctx, s.id, s.name)} <span class="rb-chip">${esc(kindText(s))}${ctx.collapsed.has(s.id) ? ', collapsed' : ''}</span>${s.states.length ? `<ul>${s.states.map(tree).join('')}</ul>` : ''}</li>`
  const callers = callersOf(ir, ctx.machine).map(([name, id]) => `<button class="rb-link rb-mono" type="button" data-open-machine="${esc(name)}">${esc(id)}</button>`)
  const spawners = spawnersOf(ir, ctx.machine).map(([name, children]) => `${machineLink(name)} ${rel('into')} ${esc(children)}`)
  return (
    head(esc(ctx.machine), 'machine', false) +
    `<p class="rb-path">${esc(sourceText(ir, m.span))}</p>` +
    docHtml(m.doc) +
    section('Signature', code(signatureText('machine', ctx.machine, m)) + signatureKv(ir, m)) +
    section(
      'Variables',
      list(m.vars.map((v) => `${v.sensitive ? 'sensitive ' : ''}${esc(v.name)}: ${typeHtml(ir, v.type)}${v.init ? ` = ${esc(v.init.text)}` : ' <span class="rb-note">(assigned later)</span>'}${v.doc ? `<span class="rb-sub rb-note">${esc(v.doc)}</span>` : ''}`)),
    ) +
    section(
      'Events it accepts',
      eventItems(ctx.machine, m.events) +
        (m.strict
          ? '<p class="rb-cnote">Strict: one of these events, a message or a call that no active state handles ends the run with a defect instead of being dropped.</p>'
          : ''),
    ) +
    section('Events it emits', eventItems(ctx.machine, m.emits)) +
    section('Calls it answers', callItems(ctx.machine, m.calls)) +
    section('Children it spawns', childrenSection(ctx, m)) +
    section('Talks to', talksToSection(ctx)) +
    section('States', `<ul class="rb-tree">${m.root.states.map(tree).join('')}</ul>`) +
    section('Invoked by', list(callers)) +
    section('Spawned by', list(spawners)) +
    section('Talked to by', talkedToBySection(ctx)) +
    section('Types involved', typesOf(ir, m)) +
    `<h3>Tip</h3><p class="rb-empty">Click a state or an arrow for its details. <kbd>/</kbd> searches, <kbd>f</kbd> fits the diagram to the view.</p>`
  )
}

/** A history state: what entering it does, its parent, and the transitions that target it. */
export function historyPanel(ctx: PanelContext, state: State): string {
  const { ir } = ctx
  const m = own(ir.machines, ctx.machine)!
  const parent = parentState(m, state.id)
  let incoming = ''
  walkStates(m.root, (s) => {
    s.transitions.forEach((t, i) => {
      if (t.targets.includes(state.id)) incoming += transitionSummary(ctx, s, i, true)
    })
  })
  return (
    head(esc(state.name), `${kindText(state)} state`, true) +
    `<p class="rb-path">${esc(state.id)} · ${esc(sourceText(ir, state.span))}</p>` +
    docHtml(state.doc) +
    section('What it does', parent ? `<p>${esc(historyText(state, parent))}</p><p class="rb-empty">A history state is never active: a transition to it enters those states instead.</p>` : '') +
    section('History of', parent ? stateLink(ctx, parent.id) : '') +
    section('Transitions in', incoming || '<p class="rb-empty">None: no transition targets it.</p>')
  )
}

export function statePanel(ctx: PanelContext, state: State): string {
  const { ir } = ctx
  const m = own(ir.machines, ctx.machine)!
  if (isHistory(state)) return historyPanel(ctx, state)
  const parents: State[] = []
  let incoming = ''
  walkStates(m.root, (s) => {
    if (state.id !== s.id && state.id.startsWith(s.id + '.')) parents.push(s)
    s.transitions.forEach((t, i) => {
      if (t.targets.includes(state.id)) incoming += transitionSummary(ctx, s, i, true)
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
  const chip = (c: State) => `${stateLink(ctx, c.id, c.name)} <span class="rb-chip">${esc(kindText(c))}</span>`
  const children = list(childStates(state).map(chip))
  const histories = list(state.states.filter(isHistory).map(chip))
  return (
    head(esc(state.name), `${state.kind} state${isInitial ? ', initial' : ''}${collapsed ? ', collapsed' : ''}`, true) +
    `<p class="rb-path">${esc(state.id)} · ${esc(sourceText(ir, state.span))}</p>` +
    docHtml(state.doc) +
    toggle +
    section(state.output ? 'Machine returns' : 'Machine fails with', result) +
    invokeSection(ctx, state) +
    section('Starts in', state.initial ? stateLink(ctx, state.initial) : '') +
    section(state.kind === 'parallel' ? 'Regions' : 'States inside', children) +
    section('History states', histories) +
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
  const decl = inv ? invokeDecl(ir, inv) : undefined
  const invName = inv ? invokeName(inv) : 'The invoke'
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
      const ref = { machine: peer(from), event: t.trigger.event }
      const task = isTaskEvent(ref)
      what = task
        ? `The task ${ref.machine} emitted ${t.trigger.event} while it ran: ${addressText(ctx, from)}.${from.kind === 'children' ? " The task's key is available as key." : ''} Taking it does not end the task.`
        : `${ref.machine} emitted ${t.trigger.event}: ${addressText(ctx, from)}.${from.kind === 'children' ? " The child's key is available as key." : ''}`
      const found = eventDecl(ir, ref)
      extra = section(
        `Event of ${task ? 'task ' : ''}${ref.machine}`,
        (found ? code(eventDeclText(t.trigger.event, found.decl, true)) + docHtml(found.decl.doc) + `<p class="rb-path">${esc(sourceText(ir, found.decl.span))}</p>` : '') +
          `<p><button class="rb-btn" type="button" data-open-event="${esc(eventKey(ref))}">Open ${esc(eventKey(ref))} →</button></p>`,
      )
      break
    }
    case 'call': {
      const name = t.trigger.call
      const c = own(m.calls, name)
      what = `Another run calls ${name} and waits for the answer. event holds the call's arguments, and call the pending call: reply call answers it now, or set p = call keeps it to answer later.`
      if (c)
        extra = section(
          'Call',
          code(signatureText('call', name, c)) + docHtml(c.doc) + `<p>${eventLink({ machine: ctx.machine, event: name }, `Who calls ${name} →`)}</p>`,
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
      what =
        inv?.kind === 'call'
          ? `${invName} was answered done; ${decl?.returns ? "the reply's value is available as result" : 'the call returns nothing'}.`
          : `${invName} finished; its value is available as result.`
      break
    case 'invoke.fail': {
      const err = t.trigger.error
      const isCallInvoke = inv?.kind === 'call'
      const which = err === '*' ? (isCallInvoke ? 'a declared error not listed, or Unanswered' : 'a declared error not listed') : err
      what = `${invName} ${isCallInvoke ? 'was answered fail' : 'failed'} with ${which}; the error is available as error.`
      if (isCallInvoke && err === 'Unanswered')
        what = `${invName} failed with the built-in Unanswered: the callee was not running, dropped the call (no on ${inv.src} where it was), or ended before it replied. error is {"_tag": "Unanswered"}.`
      const td = own(ir.types, err)
      if (td) extra = section('Error', code(typeDeclText(err, td)) + docHtml(td.doc))
      break
    }
    case 'spawn.done':
      what = `A ${own(m.children, t.trigger.children)?.kind === 'task' ? 'task' : 'child'} of ${t.trigger.children} finished; its value is available as result, its key as key.`
      break
    case 'ended': {
      const from = t.trigger.from
      const machine = peer(from)
      what = `The run of ${machine} that ${addressText(ctx, from)} holds closed, however it closed: it finished, failed, was cancelled, ended with a defect or was suspended. event.status says which. Taken at most once per run, after the last message it sent.`
      const td = own(ir.types, 'Ended')
      extra = section(
        `Peer ${machine}`,
        (td ? code(typeDeclText('Ended', td)) + docHtml(td.doc) : '') +
          (own(ir.machines, machine) ? `<p><button class="rb-btn" type="button" data-open-machine="${esc(machine)}">Open machine ${esc(machine)} →</button></p>` : ''),
      )
      break
    }
    case 'spawn.fail': {
      const err = t.trigger.error
      what = `A ${own(m.children, t.trigger.children)?.kind === 'task' ? 'task' : 'child'} of ${t.trigger.children} failed with ${err === '*' ? `a declared error no other on ${t.trigger.children}.fail names` : err}; the error is available as error, the child's key as key.`
      const td = own(ir.types, err)
      if (td) extra = section('Error', code(typeDeclText(err, td)) + docHtml(td.doc))
      break
    }
  }
  if (t.trigger.kind === 'spawn.done' || t.trigger.kind === 'spawn.fail') {
    const c = own(m.children, t.trigger.children)
    const child = c && c.kind === 'machine' ? own(ir.machines, c.src) : undefined
    const task = c && c.kind === 'task' ? own(ir.tasks, c.src) : undefined
    if (c && child) {
      extra += section(
        'Spawned machine',
        code(signatureText('machine', c.src, child)) +
          `<p class="rb-path">${esc(sourceText(ir, child.span))}</p>` +
          docHtml(child.doc) +
          `<p><button class="rb-btn" type="button" data-open-machine="${esc(c.src)}" data-via="spawn">Open machine ${esc(c.src)} →</button></p>`,
      )
    } else if (c && task) {
      extra += section('Spawned task', code(signatureText('task', c.src, task)) + `<p class="rb-path">${esc(sourceText(ir, task.span))}</p>` + docHtml(task.doc))
    }
  }
  if (decl && inv && t.trigger.kind.startsWith('invoke.')) {
    extra += section(`Invoked ${inv.kind}`, code(signatureText(inv.kind, inv.src, decl)) + `<p class="rb-path">${esc(sourceText(ir, decl.span))}</p>` + docHtml(decl.doc))
  }
  const guard = t.guard ? code(t.guard.text) : group.length > 1 ? code('else') : ''
  const branchNote =
    group.length > 1 ? `<p class="rb-empty">Branch ${pos} of ${group.length} for <code>${esc(triggerSource(t.trigger))}</code>; branches are tried in order.</p>` : ''
  const title = `${esc(short)} <span class="rb-arrow-text">→</span> ${esc(targetsText(t, (x) => shortId(ctx, x), '(stays)'))}`
  const toText = (id: string) => {
    const target = findState(m, id)
    const targetParent = target && isHistory(target) ? parentState(m, target.id) : null
    return (
      stateLink(ctx, id) +
      (id === state.id ? '<p class="rb-empty">The state is left and entered again: its exit and entry effects run.</p>' : '') +
      (target && targetParent ? `<p class="rb-empty">A ${esc(kindText(target))} state. ${esc(historyText(target, targetParent))}</p>` : '')
    )
  }
  const internalNote = t.internal
    ? `<p class="rb-empty">Internal (written with a leading dot): <code>${esc(shortId(ctx, state.id))}</code> is not left, so its own exit and entry effects do not run and its work keeps going; only the states below it${state.kind === 'parallel' ? ', in the regions the targets are in,' : ''} are exited and entered.</p>`
    : ''
  const severalNote = t.targets.length > 1 ? '<p class="rb-empty">Several targets: each region of the parallel state they are in is entered at its target, every other region by its initial state.</p>' : ''
  return (
    head(title, 'transition', true) +
    `<p class="rb-path">${esc(sourceText(ir, t.span))}</p>` +
    section('Trigger', code(triggerSource(t.trigger)) + `<p class="rb-empty">${esc(what)}</p>`) +
    section('Guard', guard + branchNote) +
    section('Effects', list(t.effects.map((e) => effectHtml(ctx, e))) || '<p class="rb-empty">None.</p>') +
    section('From', stateLink(ctx, state.id)) +
    section(
      'To',
      t.targets.length
        ? (t.targets.length > 1 ? t.targets.map((x) => `<div>${toText(x)}</div>`).join('') : toText(t.targets[0]!)) + severalNote + internalNote
        : '<p class="rb-empty">No target: the state is not left, so no exit or entry effects run.</p>',
    ) +
    extra
  )
}

/** `Order · Reserving (entry)`, `Order · Reserving (invoke)`, `Publish · Uploading: on PauseClicked → (stays)` */
function siteLink(ir: Ir, machine: string, site: { state: string; at: MessageSite['at'] }): string {
  const m = own(ir.machines, machine)
  const short = (id: string) => id.slice(machine.length + 1) || id
  if (typeof site.at !== 'number') return gotoLink(machine, { type: 'state', id: site.state }, `${machine} · ${short(site.state)} (${site.at})`)
  const t = m && findState(m, site.state)?.transitions[site.at]
  const label = t ? `${short(site.state)}: ${triggerSource(t.trigger)} → ${targetsText(t, short, '(stays)')}` : short(site.state)
  return gotoLink(machine, { type: 'transition', state: site.state, index: site.at }, `${machine} · ${label}`)
}

/** Every message site of the IR that names `ref`, with its machine. */
function sitesOf(ir: Ir, ref: EventRef): [string, MessageSite][] {
  const sites: [string, MessageSite][] = []
  for (const [other, om] of Object.entries(ir.machines)) {
    for (const site of messageSites(other, om)) if (site.event.machine === ref.machine && site.event.event === ref.event) sites.push([other, site])
  }
  return sites
}

/** The sites of one direction, each transition or entry or exit once (two replies in one transition are one place). */
function siteLinks(ir: Ir, sites: [string, MessageSite][], dir: MessageSite['dir']): string {
  return list([...new Set(sites.filter(([, s]) => s.dir === dir).map(([other, s]) => siteLink(ir, other, s)))])
}

/** A call the machine answers: its declaration, where it is taken and answered, and who calls it. */
export function callPanel(ctx: PanelContext, name: string): string {
  const { ir } = ctx
  const m = own(ir.machines, ctx.machine)!
  const c = own(m.calls, name)
  if (!c) return machinePanel(ctx)
  const sites = sitesOf(ir, { machine: ctx.machine, event: name })
  const links = (dir: MessageSite['dir']) => siteLinks(ir, sites, dir)
  const taken: string[] = []
  walkStates(m.root, (s) => {
    s.transitions.forEach((t, i) => {
      if (t.trigger.kind === 'call' && t.trigger.call === name) taken.push(transitionSummary(ctx, s, i, true))
    })
  })
  return (
    head(esc(name), 'call', true) +
    `<p class="rb-path">${esc(ctx.machine)}.${esc(name)} · ${esc(sourceText(ir, c.span))}</p>` +
    docHtml(c.doc) +
    section(
      'Declaration',
      code(signatureText('call', name, c)) +
        signatureKv(ir, c, true) +
        `<p class="rb-empty">A run holding a ref to ${esc(ctx.machine)} calls it with <code>invoke x.${esc(name)}(…)</code> and waits. ${esc(ctx.machine)} takes it with <code>on ${esc(name)}</code> and answers with <code>reply</code>; the answer reaches that caller alone. ${UNANSWERED_NOTE}</p>`,
    ) +
    section('Taken here', taken.join('') || `<p class="rb-empty">No transition takes it: every ${esc(name)} call is dropped and answered Unanswered.</p>`) +
    section('Answered', links('reply') || '<p class="rb-empty">No reply names it.</p>') +
    section('Called by', links('call') || '<p class="rb-empty">No machine of this chart calls it; another chart holding a ref may.</p>') +
    section('Types involved', typesOf(ir, c))
  )
}

/**
 * An event a task emits (`upload.Progress`): its declaration, the invokes and collections of
 * the task, and everywhere it is handled. A task has no page of its own, so it is shown on the
 * page of the machine it was opened from.
 */
export function taskEventPanel(ctx: PanelContext, task: string, name: string): string {
  const { ir } = ctx
  const t = own(ir.tasks, task)
  const decl = t && own(t.emits, name)
  if (!t || !decl) return machinePanel(ctx)
  const sites = sitesOf(ir, { machine: task, event: name })
  const runs: string[] = []
  for (const [other, om] of Object.entries(ir.machines)) {
    walkStates(om.root, (s) => {
      if (s.invoke?.kind === 'task' && s.invoke.src === task) {
        const note = s.invoke.as ? `as ${s.invoke.as}` : 'without as: what it emits reaches no one'
        runs.push(`${gotoLink(other, { type: 'state', id: s.id }, `${other} · ${s.id.slice(other.length + 1)}`)} <span class="rb-cnote">invokes it ${esc(note)}</span>`)
      }
    })
    for (const [children, c] of Object.entries(om.children)) {
      if (c.kind === 'task' && c.src === task) runs.push(`${machineLink(other)} <span class="rb-cnote">spawns it into ${esc(children)}</span>`)
    }
  }
  return (
    head(esc(name), 'task event', true) +
    `<p class="rb-path">${esc(task)}.${esc(name)} · ${esc(sourceText(ir, decl.span))}</p>` +
    docHtml(decl.doc) +
    section(
      'Declaration',
      code(`${signatureText('task', task, t)}\n  ${eventDeclText(name, decl, true)}`) +
        `<p class="rb-empty">The task ${esc(task)} emits it while it runs, when its host code says so, to the run that invoked it with <code>as</code> or spawned it, and to no one else.</p>`,
    ) +
    section('Fields', list(decl.fields.map((f) => `${f.sensitive ? 'sensitive ' : ''}${esc(f.name)}: ${esc(f.type)}${f.doc ? `<span class="rb-sub rb-note">${esc(f.doc)}</span>` : ''}`))) +
    section('Run by', list(runs) || '<p class="rb-empty">No machine of this chart invokes or spawns it.</p>') +
    section('Handled by', siteLinks(ir, sites, 'receive') || '<p class="rb-empty">No machine of this chart handles it: it is never delivered.</p>')
  )
}

/** An event the machine accepts or emits: its declaration and everywhere it is sent, emitted and handled. */
export function eventPanel(ctx: PanelContext, name: string): string {
  const { ir } = ctx
  const m = own(ir.machines, ctx.machine)!
  const dot = name.indexOf('.')
  if (dot > 0) return taskEventPanel(ctx, name.slice(0, dot), name.slice(dot + 1))
  if (own(m.calls, name)) return callPanel(ctx, name)
  const found = eventDecl(ir, { machine: ctx.machine, event: name })
  if (!found) return machinePanel(ctx)
  const { decl, emitted } = found
  // Every site in the IR that names this event of this machine.
  const sites = sitesOf(ir, { machine: ctx.machine, event: name })
  const links = (dir: MessageSite['dir']) => siteLinks(ir, sites, dir)
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
        ? `${esc(ctx.machine)} emits it to the machine that invoked or spawned it and to every machine holding a ref to it.`
        : `${esc(ctx.machine)} accepts it: another machine sends it with <code>send name.${esc(name)}</code>, or host code through the machine's handle.`
    }</p>`) +
    section('Fields', list(decl.fields.map((f) => `${f.sensitive ? 'sensitive ' : ''}${esc(f.name)}: ${esc(f.type)}${f.doc ? `<span class="rb-sub rb-note">${esc(f.doc)}</span>` : ''}`))) +
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
