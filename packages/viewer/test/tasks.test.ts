// Tasks that emit (IR 0.10): `on up.E` through a task's `as` name and `on uploads.E` from a
// collection of tasks, a stream task whose invoke has no outcomes, in the model text, the
// diagram, the details panel, search, the project index, simulate, replay and coverage, for
// test/charts/tasks.road and the conformance chart task-emits.road.

import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'
import { coverage } from '../src/coverage.ts'
import type { Ir, Machine, State } from '../src/ir.ts'
import { layoutMachine } from '../src/layout.ts'
import { eventDecl, findState, parseEventKey, stateRows, talks, transitionLines, triggerKind, triggerText } from '../src/model.ts'
import { panelHtml } from '../src/panel.ts'
import { projectIndex, renderIndexTable } from '../src/project.ts'
import { renderMachineHeader, renderSvg } from '../src/render.ts'
import { Replay, recordText } from '../src/replay.ts'
import { buildSearchIndex, search } from '../src/search.ts'
import { Simulation } from '../src/sim.ts'
import { chart, conformanceChart, newElk } from './charts.ts'

const here = path.dirname(fileURLToPath(import.meta.url))
const conformance = path.resolve(here, '../../../spec/conformance')
const tasks = chart('charts-tasks')
const emits = conformanceChart('task-emits')
const trace = (name: string) => fs.readFileSync(path.join(conformance, 'cases', name, 'trace.jsonl'), 'utf8')
const machine = tasks.machines.Uploads!
const state = (m: Machine, id: string): State => {
  const s = findState(m, id)
  if (!s) throw new Error(`no state ${id}`)
  return s
}
const ctx = (ir: Ir, m: string) => ({ ir, machine: m, collapsed: new Set<string>() })

describe('model text', () => {
  it('labels what a task emits through its as name or its collection, as a message', () => {
    const single = state(machine, 'Uploads.Single')
    expect(triggerText(single.transitions[2]!.trigger)).toBe('up.Progress')
    expect(triggerKind(single.transitions[2]!.trigger)).toBe('message')
    // The arrow's label links to the task's event, `upload.Progress`.
    const lines = transitionLines(single, 2)
    expect(lines[0]).toEqual({ cls: 'trigger', text: 'up.Progress', guard: '', event: { machine: 'upload', event: 'Progress' } })
    expect(lines[1]).toEqual({ cls: 'trigger', text: '', guard: '[event.percent >= 100]' })
    expect(parseEventKey('upload.Progress')).toEqual({ machine: 'upload', event: 'Progress' })
    expect(eventDecl(tasks, { machine: 'upload', event: 'Stage' })?.decl.fields.map((f) => f.name)).toEqual(['name'])
    expect(eventDecl(tasks, { machine: 'upload', event: 'Nope' })).toBeNull()
  })

  it('shows a stream task as an invoke row, and a spawn of a task with no machine to open', () => {
    const rows = stateRows(state(machine, 'Uploads.Waiting'))
    expect(rows[0]).toEqual({ kw: 'invoke', text: 'ticks(1s) as clock', cont: false })
    expect(rows[1]).toMatchObject({ kw: 'on clock.Tick', cls: 'message', kwEvent: { machine: 'ticks', event: 'Tick' } })
    const many = stateRows(state(machine, 'Uploads.Many'))
    expect(many[0]).toMatchObject({ kw: 'on Add', text: 'spawn uploads[event.name] =' })
    expect(many.some((r) => r.spawn !== undefined)).toBe(false)
    expect(many.find((r) => r.kw === 'on uploads.Progress')).toMatchObject({ cls: 'message', kwEvent: { machine: 'upload', event: 'Progress' } })
  })

  it('says which tasks a machine spawns and hears from', () => {
    const t = talks(tasks, 'Uploads')
    expect(t.spawns).toEqual([])
    expect(t.spawnsTasks).toEqual([{ task: 'upload', names: ['uploads'] }])
    expect(t.receives).toEqual([])
    expect(t.receivesTasks).toEqual([
      { task: 'upload', events: ['Progress', 'Stage'] },
      { task: 'ticks', events: ['Tick'] },
    ])
  })
})

describe('the diagram', () => {
  it('draws on up.Progress as a message arrow and the self-loops as message rows, linked to the task event', async () => {
    const layout = await layoutMachine(tasks, 'Uploads', newElk())
    const svg = renderSvg(tasks, 'Uploads', layout, { interactive: true })
    expect(svg).toContain('<g class="rb-edge rb-k-message" data-state="Uploads.Single" data-transition="2">')
    expect(svg).toContain('<tspan class="rb-msg" data-event="upload.Progress"><title>Open event upload.Progress</title>')
    expect(svg).toContain('data-event="ticks.Tick"')
    expect(svg).toContain('data-event="upload.Stage"')
    expect(svg).not.toContain('class="rb-spawn"')
  })

  it('writes a collection of tasks in the header without a link', () => {
    const head = renderMachineHeader(tasks, 'Uploads')
    expect(head).toContain('children uploads: upload by string')
    expect(head).not.toContain('data-open-machine="upload"')
  })
})

describe('the details panel', () => {
  it('lists a task’s emits on its invoke, with what its as name does', () => {
    const html = panelHtml(ctx(tasks, 'Uploads'), { type: 'state', id: 'Uploads.Single' })
    expect(html).toContain('It emits, while it runs:')
    expect(html).toContain('data-open-event="upload.Progress"')
    expect(html).toContain('data-open-event="upload.Stage"')
    expect(html).toContain('<code>up</code> names the task')
    expect(html).not.toContain('No outcomes are written')
  })

  it('shows an invoke with no outcomes as running until its state is left', () => {
    const html = panelHtml(ctx(tasks, 'Uploads'), { type: 'state', id: 'Uploads.Waiting' })
    expect(html).toContain('No outcomes are written: it runs until Waiting is left, which cancels it.')
    expect(html).toContain('data-open-event="ticks.Tick"')
  })

  it('says an invoke without as delivers nothing', () => {
    const html = panelHtml(ctx(emits, 'Quiet'), { type: 'state', id: 'Quiet.Unnamed' })
    expect(html).toContain('Invoked without <code>as</code>, so what it emits reaches no one.')
  })

  it('explains a transition on what a task emits, and opens the task’s event', () => {
    const html = panelHtml(ctx(tasks, 'Uploads'), { type: 'transition', state: 'Uploads.Single', index: 2 })
    expect(html).toContain('The task upload emitted Progress while it ran: the task up, invoked in Single.')
    expect(html).toContain('Event of task upload')
    expect(html).toContain('emits Progress { percent: int }')
    expect(html).toContain('data-open-event="upload.Progress">Open upload.Progress →')
    const spawned = panelHtml(ctx(tasks, 'Uploads'), { type: 'transition', state: 'Uploads.Many', index: 2 })
    expect(spawned).toContain('a task of uploads.')
    expect(spawned).toContain("The task's key is available as key.")
  })

  it('shows a task event: its declaration, who runs the task and who handles it', () => {
    const html = panelHtml(ctx(tasks, 'Uploads'), { type: 'event', name: 'upload.Progress' })
    expect(html).toContain('<span class="rb-chip">task event</span>')
    expect(html).toContain('How much of the file is stored, in percent.')
    expect(html).toContain('invokes it as up')
    expect(html).toContain('spawns it into uploads')
    expect(html).toContain('Uploads · Single: on up.Progress → Waiting')
    expect(html).toContain('Uploads · Many: on uploads.Progress → (stays)')
    // An unknown task or event falls back to the machine.
    expect(panelHtml(ctx(tasks, 'Uploads'), { type: 'event', name: 'upload.Nope' })).toContain('<span class="rb-chip">machine</span>')
  })

  it('lists a collection of tasks, its spawns and the task it spawns', () => {
    const html = panelHtml(ctx(tasks, 'Uploads'), null)
    expect(html).toContain('children uploads: upload by string')
    expect(html).toContain('The uploads running side by side, by file name.')
    expect(html).toContain('spawns task</span> <code>upload</code>')
    expect(html).toContain('emitted by task</span> <code>ticks</code>')
    const done = panelHtml(ctx(tasks, 'Uploads'), { type: 'transition', state: 'Uploads.Many', index: 3 })
    expect(done).toContain('A task of uploads finished')
    expect(done).toContain('Spawned task')
    expect(done).toContain('task upload(name: string) -&gt; int ! UploadFailed')
    const add = panelHtml(ctx(tasks, 'Uploads'), { type: 'transition', state: 'Uploads.Many', index: 0 })
    expect(add).toContain('starts a task of uploads')
    expect(add).not.toContain('data-open-machine="upload"')
  })
})

describe('search and the project index', () => {
  it('finds the events a task emits, opening them on the page of the machine that runs it', () => {
    const index = buildSearchIndex(tasks)
    const found = search(index, 'Progress')
    expect(found[0]).toMatchObject({ kind: 'event', label: 'emits Progress of task upload, run by Uploads', machine: 'Uploads', target: { type: 'event', name: 'upload.Progress' } })
    expect(found.some((e) => e.label === 'up.Progress: Uploads.Single → Waiting')).toBe(true)
    expect(search(index, 'Tick').map((e) => e.label)).toContain('emits Tick of task ticks, run by Uploads')
    expect(search(index, 'upload').some((e) => e.kind === 'task' && e.label === 'children uploads: upload by string in Uploads')).toBe(true)
  })

  it('lists the tasks a machine spawns in the index', () => {
    const index = projectIndex('t', [{ href: './tasks.html', entry: 'tasks.road', ir: tasks }])
    expect(index.machines[0]!.spawnsTasks).toEqual([{ task: 'upload', names: ['uploads'] }])
    expect(renderIndexTable(index)).toContain('<code>spawns task upload (uploads)</code>')
  })
})

describe('simulate', () => {
  it('makes a running task emit what the machine hears, and keeps it running', () => {
    const sim = new Simulation(tasks, 'Uploads')
    expect(sim.start({ file: 'a.txt' })).toEqual({ kind: 'ok' })
    const [inv] = sim.choices().invokes
    expect(inv).toMatchObject({ src: 'upload', srcKind: 'task', as: 'up', open: false })
    expect(inv!.emits.map((e) => e.name)).toEqual(['Progress', 'Stage'])
    expect(sim.emitFromChild(inv!.id, { _tag: 'Progress', percent: 40 })).toEqual({ kind: 'ok' })
    expect(sim.current!.label).toBe('upload emits Progress')
    expect(sim.current!.records[0]).toMatchObject({ kind: 'event', event: { kind: 'task.emitted', invoke: inv!.id, value: { _tag: 'Progress', percent: 40 } } })
    expect(sim.current!.snapshot.vars.percent).toBe(40)
    expect(sim.choices().invokes.map((i) => i.id)).toEqual([inv!.id])
    // A guarded branch leaves the state, which cancels the task.
    sim.emitFromChild(inv!.id, { _tag: 'Progress', percent: 100 })
    expect(sim.current!.snapshot.active).toContain('Uploads.Waiting')
    expect(sim.current!.records.map((r) => r.kind)).toContain('invoke.cancelled')
    // The stream task: no outcomes, and its ticks counted.
    const [clock] = sim.choices().invokes
    expect(clock).toMatchObject({ src: 'ticks', as: 'clock', open: true, returns: null })
    sim.emitFromChild(clock!.id, { _tag: 'Tick' })
    sim.emitFromChild(clock!.id, { _tag: 'Tick' })
    expect(sim.current!.snapshot.vars.seconds).toBe(2)
    // A spawned task, by key.
    sim.send({ _tag: 'Next' })
    sim.send({ _tag: 'Add', name: 'b.txt' })
    const [child] = sim.choices().spawned
    expect(child).toMatchObject({ srcKind: 'task', src: 'upload', key: 'b.txt', returns: 'int' })
    // The machine hears Stage from upload too (in Single): what it hears is by task, in any state.
    expect(child!.emits.map((e) => e.name)).toEqual(['Progress', 'Stage'])
    expect(sim.emitFromChild(child!.id, { _tag: 'Progress', percent: 7 })).toEqual({ kind: 'ok' })
    expect(sim.current!.records.find((r) => r.kind === 'action')).toMatchObject({ action: 'showProgress', args: { name: 'b.txt', percent: 7 } })
  })

  it('delivers nothing the machine does not hear, nor from an invoke without as', () => {
    const sim = new Simulation(emits, 'Quiet')
    sim.start({ size: 10 })
    const [heard] = sim.choices().invokes
    // Quiet hears only Progress from upload.
    expect(heard!.emits.map((e) => e.name)).toEqual(['Progress'])
    expect(sim.emitFromChild(heard!.id, { _tag: 'Stage', name: 'x' })).toEqual({ kind: 'ok' })
    expect(sim.current!.records).toEqual([])
    sim.complete(heard!.id, { done: 5 })
    const [unnamed] = sim.choices().invokes
    expect(unnamed!.emits).toEqual([])
    sim.emitFromChild(unnamed!.id, { _tag: 'Progress', percent: 1 })
    expect(sim.current!.records).toEqual([])
    expect(sim.current!.label).toBe('upload emits Progress, which nothing hears')
  })
})

describe('replay and coverage', () => {
  it('replay a trace with task.emitted inputs, taken, dropped and stale', () => {
    for (const name of ['task-emit-progress', 'task-emit-stale', 'task-emit-dropped', 'task-emit-nested']) {
      expect(new Replay(emits, trace(name)).problems, name).toEqual([])
    }
    const replay = new Replay(emits, trace('task-emit-progress'))
    const at = replay.records.findIndex((r) => r.kind === 'event' && (r.event as { kind?: string }).kind === 'task.emitted')
    expect(recordText(replay.records[at]!)).toBe('event Progress emitted by the task of invoke 1 {"percent":50}')
    // Taking what a task emits does not end its invoke.
    expect(replay.viewAt(at, 'r1').invokes.map((i) => i.src)).toEqual(['upload'])
  })

  it('count the transitions what a task emits takes', () => {
    const cov = coverage(emits, [{ name: 'task-emit-progress', text: trace('task-emit-progress') }])
    expect(cov.problems).toEqual([])
    expect(cov.machines.Publish!.transitions['Publish.Uploading#1']).toBe(2)
  })
})
