// Keyed spawn (IR 0.3): collections of children, `spawn`, `stop`, sends to one child or to all,
// and `on name.Event`, `on name.done`, `on name.fail E`, in the model text, the diagram, the
// details panel, search, the project index, simulate and replay, for test/charts/spawn.road and
// the conformance chart hub.road.

import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'
import { coverage } from '../src/coverage.ts'
import type { Ir, Machine, State } from '../src/ir.ts'
import { layoutMachine } from '../src/layout.ts'
import type { Box } from '../src/layout.ts'
import { effectText, findState, stateRows, talks, transitionId, transitionLines, triggerKind, triggerSource, triggerText } from '../src/model.ts'
import { panelHtml } from '../src/panel.ts'
import { indexEdges, layoutIndex, projectIndex, renderIndexLegend, renderIndexSvg, renderIndexTable } from '../src/project.ts'
import { renderLegend, renderMachineHeader, renderSvg } from '../src/render.ts'
import { Replay, recordText } from '../src/replay.ts'
import { buildSearchIndex, search } from '../src/search.ts'
import { Simulation } from '../src/sim.ts'
import { chart, loadIr, newElk } from './charts.ts'

const here = path.dirname(fileURLToPath(import.meta.url))
const conformance = path.resolve(here, '../../../spec/conformance')
const spawn = chart('charts-spawn')
const hub = loadIr(path.join(conformance, 'charts/hub.ir.json'))
const trace = (name: string) => fs.readFileSync(path.join(conformance, 'cases', name, 'trace.jsonl'), 'utf8')

const state = (m: Machine, id: string): State => {
  const s = findState(m, id)
  if (!s) throw new Error(`no state ${id}`)
  return s
}
const rows = (s: State) => stateRows(s).map((r) => `${r.kw}|${r.text.trim()}`)
const serving = state(spawn.machines.Hub!, 'Hub.Serving')
const ctx = (ir: Ir, machine: string) => ({ ir, machine, collapsed: new Set<string>() })

describe('model text', () => {
  it('labels spawn triggers through the collection, as the CLI does', () => {
    const t = (i: number) => serving.transitions[i]!.trigger
    expect([4, 5, 6, 7].map((i) => triggerText(t(i)))).toEqual(['sessions.done', 'sessions.fail Lost', 'sessions.fail *', 'jobs.done'])
    expect([4, 5, 6].map((i) => triggerSource(t(i)))).toEqual(['on sessions.done', 'on sessions.fail Lost', 'on sessions.fail *'])
    expect([4, 5, 6].map((i) => triggerKind(t(i)))).toEqual(['done', 'fail', 'fail'])
    expect(triggerText(t(3))).toBe('sessions.Typing')
  })

  it('writes spawn, stop and sends to children as written', () => {
    const connected = serving.transitions[0]!.effects
    expect(connected.map(effectText)).toEqual(['set open = open + 1', 'spawn sessions[event.id] = Session(event.id, "hi")', 'send sessions[event.id].Notify { text: "welcome" }'])
    expect(effectText(serving.transitions[1]!.effects[0]!)).toBe('stop sessions[event.id]')
    expect(effectText(serving.transitions[2]!.effects[0]!)).toBe('send sessions.*.Notify { text: event.text }')
    expect(effectText(spawn.machines.Hub!.root.exit[0]!)).toBe('send sessions.*.Leave')
  })

  it('shows them as rows, a spawn linked to its machine and a send to its event', () => {
    expect(rows(serving).slice(0, 7)).toEqual([
      'entry|spawn jobs[Lane.Slow] = Job(Lane.Slow)',
      'on Connected|set open = open + 1',
      '|spawn sessions[event.id] =',
      '|Session(event.id, "hi")',
      '|send sessions[event.id].Notify {',
      '|text: "welcome" }',
      'on Disconnected|stop sessions[event.id]',
    ])
    const r = stateRows(serving)
    expect(r[0]!.spawn).toBe('Job')
    expect(r[2]!.spawn).toBe('Session')
    expect(r[3]!.spawn).toBe('Session')
    expect(r[4]!.event).toEqual({ machine: 'Session', event: 'Notify' })
    expect(r[6]!.spawn).toBeUndefined()
    expect(r.find((x) => x.kw === 'on sessions.Typing')).toMatchObject({ cls: 'message', kwEvent: { machine: 'Session', event: 'Typing' } })
    expect(r.find((x) => x.kw === 'on sessions.done')).toMatchObject({ cls: 'done', transition: 4 })
    expect(r.find((x) => x.kw === 'on sessions.fail Lost')).toMatchObject({ cls: 'fail', transition: 5, text: '[error.reason == "idle"]' })
    const idle = state(spawn.machines.Hub!, 'Hub.Idle')
    expect(transitionLines(idle, 0)).toEqual([
      { cls: 'trigger', text: 'Open', guard: '' },
      { cls: 'effect', text: 'spawn jobs[Lane.Fast] = Job(Lane.Fast)', guard: '', spawn: 'Job' },
    ])
  })

  it('says which machines a machine spawns, and counts what it sends to and handles from them', () => {
    expect(talks(spawn, 'Hub')).toEqual({
      invokes: [],
      spawns: [
        { machine: 'Session', names: ['sessions'] },
        { machine: 'Job', names: ['jobs'] },
      ],
      refs: [],
      sends: [{ machine: 'Session', events: ['Leave', 'Notify'] }],
      receives: [{ machine: 'Session', events: ['Typing'] }],
    })
    expect(talks(spawn, 'Session').spawns).toEqual([])
  })
})

describe('the diagram', () => {
  const elk = newElk()

  it('links a spawn to the child machine, marks sends and emitted events, colours outcomes', async () => {
    const svg = renderSvg(spawn, 'Hub', await layoutMachine(spawn, 'Hub', elk), { interactive: true })
    // The spawn on an arrow's label, and in a state's rows.
    expect(svg).toMatch(/class="rb-label-effect"[^>]*><tspan class="rb-spawn" data-machine="Job"><title>Open machine Job<\/title><tspan class="rb-spawn-kw">spawn<\/tspan> jobs\[Lane\.Fast\]/)
    expect(svg).toMatch(/<tspan class="rb-kw">entry<\/tspan> <tspan class="rb-spawn" data-machine="Job">/)
    expect(svg).toContain('<tspan class="rb-spawn" data-machine="Session"><title>Open machine Session</title><tspan class="rb-spawn-kw">spawn</tspan> sessions[event.id] =</tspan>')
    expect(svg).toMatch(/<tspan class="rb-msg" data-event="Session\.Notify"><title>Open event Session\.Notify<\/title><tspan class="rb-msg-kw">send<\/tspan> sessions\[event\.id\]\.Notify/)
    expect(svg).toMatch(/<tspan class="rb-msg-kw">send<\/tspan> sessions\.\*\.Leave/)
    expect(svg).toMatch(/<tspan class="rb-kw">on<\/tspan> <tspan class="rb-msg" data-event="Session\.Typing">/)
    expect(svg).toMatch(/class="rb-row rb-k-done" data-transition="4"[^>]*><tspan class="rb-kw">on sessions\.done<\/tspan>/)
    expect(svg).toMatch(/class="rb-row rb-k-fail" data-transition="6"[^>]*><tspan class="rb-kw">on sessions\.fail \*<\/tspan>/)
    expect(svg).not.toMatch(/NaN|undefined/)
  })

  it('lists the collections of children under the signature, their machines linked', () => {
    const head = renderMachineHeader(spawn, 'Hub')
    expect(head).toContain(
      '<code class="rb-signature rb-children">children sessions: <button class="rb-link rb-mono rb-ref" type="button" data-open-machine="Session" data-via="spawn" title="Open machine Session">Session</button> by SessionId</code>',
    )
    expect(head).toContain('children jobs: <button')
    expect(renderMachineHeader(spawn, 'Session')).not.toContain('rb-children')
  })

  it('explains spawn in the legend', () => {
    expect(renderLegend({ interactive: true })).toContain('<code class="rb-t-spawn">spawn xs[k] = M()</code> starts a child machine M under key k; click to open M')
  })
})

describe('the details panel', () => {
  it('lists a machine’s collections with where they are spawned and stopped, and whom it spawns', () => {
    const html = panelHtml(ctx(spawn, 'Hub'), null)
    expect(html).toContain('<h3>Children it spawns</h3>')
    expect(html).toContain('children sessions: <button class="rb-link rb-mono rb-ref" type="button" data-open-machine="Session" data-via="spawn"')
    expect(html).toContain('<span class="rb-sub rb-note">The open sessions.</span>')
    expect(html).toContain('spawned</span> <button class="rb-link rb-mono" type="button" data-select-transition="[&quot;Hub.Serving&quot;,0]">Serving: on Connected</button>')
    expect(html).toMatch(/stopped<\/span> <button[^>]*>Serving: on Disconnected<\/button>/)
    expect(html).toContain('spawned</span> <button class="rb-link rb-mono" type="button" data-select-state="Hub.Serving">Serving (entry)</button>')
    expect(html).toMatch(/spawns<\/span> <button[^>]*data-open-machine="Session"[^>]*>Session<\/button> <span class="rb-rel">into<\/span> sessions/)
    expect(html).toMatch(/sends<\/span> <button[^>]*data-open-event="Session\.Leave"/)
    expect(html).toMatch(/handles<\/span> <button[^>]*data-open-event="Session\.Typing"/)
    const session = panelHtml(ctx(spawn, 'Session'), null)
    expect(session).toMatch(/<h3>Spawned by<\/h3><ul class="rb-list"><li><button[^>]*data-open-machine="Hub"[^>]*>Hub<\/button> <span class="rb-rel">into<\/span> sessions/)
    expect(session).not.toContain('Children it spawns')
  })

  it('shows a spawn, a stop and sends to children in a transition, each said in words', () => {
    const connected = panelHtml(ctx(spawn, 'Hub'), { type: 'transition', state: 'Hub.Serving', index: 0 })
    expect(connected).toContain(
      '<span class="rb-spawn-kw">spawn</span> <button class="rb-link rb-mono" type="button" data-open-machine="Session" data-via="spawn" title="Open machine Session">sessions[event.id] = Session(event.id, &quot;hi&quot;)</button>',
    )
    expect(connected).toContain('to Session, the child of sessions with key event.id')
    expect(panelHtml(ctx(spawn, 'Hub'), { type: 'transition', state: 'Hub.Serving', index: 2 })).toContain('to Session, every running child of sessions')
    expect(panelHtml(ctx(spawn, 'Hub'), { type: 'transition', state: 'Hub.Serving', index: 1 })).toContain('<span class="rb-spawn-kw">stop</span> sessions[event.id]')
    expect(panelHtml(ctx(spawn, 'Hub'), { type: 'transition', state: 'Hub.Serving', index: 3 })).toContain("Session emitted Typing: a child of sessions. The child's key is available as key.")
  })

  it('explains a child’s outcome, with the error and the spawned machine', () => {
    const done = panelHtml(ctx(spawn, 'Hub'), { type: 'transition', state: 'Hub.Serving', index: 4 })
    expect(done).toContain('<pre class="rb-code">on sessions.done</pre>')
    expect(done).toContain('A child of sessions finished; its value is available as result, its key as key.')
    expect(done).toContain('<h3>Spawned machine</h3><pre class="rb-code">machine Session(id: SessionId, greeting: string) -&gt; int ! Lost | Kicked</pre>')
    const lost = panelHtml(ctx(spawn, 'Hub'), { type: 'transition', state: 'Hub.Serving', index: 5 })
    expect(lost).toContain("A child of sessions failed with Lost; the error is available as error, the child's key as key.")
    expect(lost).toContain('error Lost { reason: string }')
    expect(panelHtml(ctx(spawn, 'Hub'), { type: 'transition', state: 'Hub.Serving', index: 6 })).toContain('failed with a declared error no other on sessions.fail names')
  })

  it('lists sends to children among an event’s senders', () => {
    const html = panelHtml(ctx(spawn, 'Session'), { type: 'event', name: 'Notify' })
    expect(html).toContain('Hub · Serving: on Connected → (stays)')
    expect(html).toContain('Hub · Serving: on Broadcast → (stays)')
    expect(panelHtml(ctx(spawn, 'Session'), { type: 'event', name: 'Typing' })).toContain('Hub · Serving: on sessions.Typing → (stays)')
  })
})

describe('search', () => {
  it('finds spawns by machine and the errors of children by name', () => {
    const index = buildSearchIndex(spawn)
    expect(search(index, 'Session').map((e) => `${e.kind} ${e.label}`)).toContain('machine spawn sessions[event.id] = Session(event.id, "hi"): Hub.Serving → (stays)')
    expect(search(index, 'Lost').map((e) => `${e.kind} ${e.label}`)).toContain('error sessions.fail Lost: Hub.Serving → (stays)')
  })
})

describe('the project index', () => {
  const index = projectIndex('spawn', [
    { href: 'spawn.html', entry: 'spawn', ir: spawn },
    { href: 'hub.html', entry: 'hub', ir: hub },
  ])
  const at = (n: string, page = 0) => index.machines.findIndex((m) => m.name === n && m.page === page)

  it('records spawns as a relation, and sends to and receives from spawned children', () => {
    const h = index.machines[at('Hub')]!
    expect(h.invokes).toEqual([])
    expect(h.spawns).toEqual([
      { to: at('Session'), names: ['sessions'] },
      { to: at('Job'), names: ['jobs'] },
    ])
    expect(h.sends).toEqual([{ to: at('Session'), events: ['Leave', 'Notify'] }])
    expect(h.receives).toEqual([{ from: at('Session'), events: ['Typing'] }])
    expect(index.machines[at('Pool', 1)]!.spawns).toEqual([{ to: at('Session', 1), names: ['slots'] }])
  })

  it('draws a spawn edge, labelled with the collection, and its children are not top level', async () => {
    const edges = indexEdges(index).map((e) => `${e.kind} ${index.machines[e.from]!.name} -> ${index.machines[e.to]!.name}: ${e.lines.join(' / ')}`)
    expect(edges.slice(0, 2)).toEqual(['spawn Hub -> Session: spawns sessions', 'spawn Hub -> Job: spawns jobs'])
    expect(edges).toContain('send Hub -> Session: sends Leave, Notify')
    expect(edges).toContain('emit Session -> Hub: emits Typing')
    const layout = await layoutIndex(index, newElk())
    const overlap = (a: Box, b: Box) => a.x + 0.5 < b.x + b.width && b.x + 0.5 < a.x + a.width && a.y + 0.5 < b.y + b.height && b.y + 0.5 < a.y + a.height
    const boxes = [...layout.nodes, ...layout.edges.flatMap((e) => (e.label ? [e.label] : []))]
    const found: string[] = []
    for (let i = 0; i < boxes.length; i++) for (let j = i + 1; j < boxes.length; j++) if (overlap(boxes[i]!, boxes[j]!)) found.push(`${i} overlaps ${j}`)
    expect(found).toEqual([])
    const svg = renderIndexSvg(index, layout)
    expect(svg).toContain('class="rb-edge rb-iedge rb-i-spawn"><title>Hub spawns Session</title>')
    expect(svg).toContain('marker-end="url(#rbi-arrow-spawn)"')
    expect(svg).toContain('<tspan class="rb-ikw">spawns</tspan> sessions')
    // Only machines nobody invokes or spawns are top level.
    expect(svg).toMatch(/Hub<\/text>(?:(?!<\/a>).)*top level/)
    expect(svg).not.toMatch(/Session<\/text>(?:(?!<\/a>).)*top level/)
    expect(renderIndexLegend()).toContain('A spawns children B into xs')
    expect(renderIndexTable(index)).toContain('<code>spawns Session (sessions)</code><br><code>spawns Job (jobs)</code><br><code>sends Leave, Notify to Session</code>')
  })
})

describe('simulate', () => {
  it('treats a spawned child as a task: its outcome and its emitted events are given by hand', () => {
    const sim = new Simulation(hub, 'Hub')
    expect(sim.start({})).toEqual({ kind: 'ok' })
    expect(sim.send({ _tag: 'Join', id: 'a', name: 'Ann' })).toEqual({ kind: 'ok' })
    expect(sim.send({ _tag: 'Join', id: 'b', name: 'Bob' })).toEqual({ kind: 'ok' })
    const spawned = sim.choices().spawned
    expect(spawned.map((c) => [c.id, c.children, c.key, c.src, c.returns])).toEqual([
      [1, 'sessions', 'a', 'Session', 'int'],
      [2, 'sessions', 'b', 'Session', 'int'],
    ])
    expect(spawned[0]!.errors).toEqual([{ name: 'Lost', fields: [{ name: 'reason', type: 'string', doc: '' }] }])
    expect(spawned[0]!.emits.map((e) => e.name)).toEqual(['Said'])
    // A message to a child is a record only.
    expect(sim.send({ _tag: 'Tell', id: 'a', text: 'hi' })).toEqual({ kind: 'ok' })
    expect(sim.current!.records.map((r) => r.kind)).toEqual(['event', 'transition', 'sent'])
    expect(sim.emitFromChild(1, { _tag: 'Said', text: 'hi' })).toEqual({ kind: 'ok' })
    expect(sim.current!.label).toBe('sessions["a"] emits Said')
    expect(sim.current!.taken).toEqual([{ state: 'Hub.Serving', index: 9 }])
    expect(sim.complete(2, { done: 3 })).toEqual({ kind: 'ok' })
    expect(sim.current!.label).toBe('sessions["b"] done')
    expect(sim.current!.taken).toEqual([{ state: 'Hub.Serving', index: 10 }])
    expect(sim.choices().spawned.map((c) => c.id)).toEqual([1])
    expect(sim.complete(1, { fail: { _tag: 'Lost', reason: 'gone' } })).toEqual({ kind: 'ok' })
    expect(sim.current!.taken).toEqual([{ state: 'Hub.Serving', index: 11 }])
    // Stopped and replaced children leave the choices.
    expect(sim.send({ _tag: 'Join', id: 'c', name: 'Cy' })).toEqual({ kind: 'ok' })
    expect(sim.send({ _tag: 'Kick', id: 'c' })).toEqual({ kind: 'ok' })
    expect(sim.current!.records.map((r) => r.kind)).toContain('spawn.stopped')
    expect(sim.choices().spawned).toEqual([])
    expect(sim.complete(3, { done: 1 })).toEqual({ kind: 'error', message: 'invoke 3 is not running' })
  })
})

describe('replay', () => {
  it('follows spawned children: started, running, stopped, and their outcomes', () => {
    const replay = new Replay(hub, trace('spawn-basic'))
    expect(replay.warnings).toEqual([])
    expect(replay.problems).toEqual([])
    const started = replay.records.findIndex((r) => r.kind === 'spawn.started' && r.spawn === 2)
    expect(recordText(replay.records[started]!)).toBe('spawned child 2 started: sessions["b"] = Session(name: "Bob")')
    expect(replay.viewAt(started, 'r1').spawned).toEqual([
      { id: 1, children: 'sessions', key: 'a', src: 'Session' },
      { id: 2, children: 'sessions', key: 'b', src: 'Session' },
    ])
    const done = replay.records.findIndex((r) => r.kind === 'event' && (r.event as { kind: string }).kind === 'spawn.done')
    expect(recordText(replay.records[done]!)).toBe('event spawned child 2 done 1')
    expect(replay.viewAt(done, 'r1').spawned.map((c) => c.id)).toEqual([1])
    const stopped = replay.records.findIndex((r) => r.kind === 'spawn.stopped')
    expect(recordText(replay.records[stopped]!)).toBe('spawned child 1 stopped: sessions["a"]')
    expect(replay.viewAt(stopped, 'r1').spawned).toEqual([])
    expect(replay.spawned('r1.1')).toBe(true)
    expect(replay.spawned('r1')).toBe(false)
    expect(replay.runs.map((r) => `${r.id} ${r.machine}`)).toEqual(['r1 Hub', 'r1.1 Session', 'r1.2 Session', 'r1.3 Session'])
  })

  it('reports a stop of a child that is not running', () => {
    const text = trace('spawn-basic').replace('"kind":"spawn.stopped","run":"r1","spawn":1', '"kind":"spawn.stopped","run":"r1","spawn":7')
    expect(new Replay(hub, text).problems).toEqual([expect.stringMatching(/spawn\.stopped, run r1\): spawned child 7 is not running$/)])
  })
})

describe('coverage', () => {
  it('counts transitions on children’s outcomes like any other', () => {
    const c = coverage(hub, [{ name: 'spawn-basic', text: trace('spawn-basic') }])
    expect(c.problems).toEqual([])
    const t = c.machines.Hub!.transitions
    expect(t[transitionId('Hub.Serving', 10)]).toBe(1) // on sessions.done
    expect(t[transitionId('Hub.Serving', 9)]).toBeGreaterThan(0) // on sessions.Said
  })
})
