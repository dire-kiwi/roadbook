// Replay mode's panel: load a trace (embedded by `roadbook viz --trace`, picked, or dropped on
// the page), step through its records, and see at each one the states and transition it names,
// the active states and variables of its run, and the child runs to follow. The model is
// src/replay.ts; this file draws it.

import type { Ir } from '../ir.ts'
import { own } from '../ir.ts'
import { findState, triggerText } from '../model.ts'
import { Replay, recordText, stateName } from '../replay.ts'
import { esc } from '../render.ts'
import { activeHtml, pretty, recordsHtml, scrollWithin, variablesHtml } from './modes.ts'
import type { Controller, Highlight, Host } from './modes.ts'

export function replayMode(host: Host, el: HTMLElement, embedded: { name: string; text: string } | null): Controller & { load(name: string, text: string): void } {
  const ir: Ir = host.ir
  let replay: Replay | null = null
  let name = ''
  let at = 0
  let loadError = ''

  el.innerHTML = `<div id="rb-rp-head"></div><div id="rb-rp-state"></div><div id="rb-rp-records"></div>`
  const head = el.querySelector<HTMLElement>('#rb-rp-head')!
  const state = el.querySelector<HTMLElement>('#rb-rp-state')!
  const list = el.querySelector<HTMLElement>('#rb-rp-records')!

  const runOf = (i: number) => replay?.records[i]?.run ?? replay?.runs[0]?.id ?? ''
  const info = (run: string) => replay?.runs.find((r) => r.id === run)

  /** The machines from the top-level run down to `run`, for the trail. */
  function trailFor(run: string) {
    const parts = run.split('.')
    const out: { name: string; via: string }[] = []
    for (let i = 1; i <= parts.length; i++) {
      const r = info(parts.slice(0, i).join('.'))
      if (r && own(ir.machines, r.machine)) out.push({ name: r.machine, via: `invokes (${r.id}) ›` })
    }
    return out
  }

  function load(n: string, text: string) {
    name = n
    loadError = ''
    replay = new Replay(ir, text)
    if (!replay.records.length) loadError = 'The file has no trace records.'
    at = 0
    renderHead()
    renderList()
    void go(0)
  }

  async function go(i: number) {
    if (!replay || !replay.records.length) return renderState()
    at = Math.max(0, Math.min(replay.records.length - 1, i))
    const run = runOf(at)
    const r = info(run)
    // Follow the run of the record into its machine's diagram.
    if (r && own(ir.machines, r.machine) && r.machine !== host.current()) await host.open(r.machine, trailFor(run))
    renderState()
    markList()
    host.refresh()
  }

  function renderHead() {
    let h = `<div class="rb-mode-head"><h2>Replay${name ? ` <span class="rb-mono">${esc(name)}</span>` : ''}</h2></div>`
    h += `<div class="rb-row-btns"><label class="rb-btn rb-file">Open a trace…<input type="file" accept=".jsonl,.json,.ndjson,application/x-ndjson,application/json,text/plain" data-m-file hidden></label><span class="rb-cnote">or drop a <code>.jsonl</code> file on the page</span></div>`
    if (!replay) {
      h += `<p class="rb-mode-note">A trace is the JSON Lines file a run writes (spec/trace.md), from either runtime. <code>roadbook viz --trace run.jsonl</code> puts one in the page. Nothing is uploaded: the file is read in the browser.</p>`
    }
    if (loadError) h += `<p class="rb-alert" role="alert">${esc(loadError)}</p>`
    if (replay) {
      for (const w of replay.warnings) h += `<div class="rb-refusal" role="alert"><strong>Warning:</strong> ${esc(w)}</div>`
      if (replay.problems.length)
        h += `<details class="rb-problems"><summary>${replay.problems.length} record${replay.problems.length === 1 ? '' : 's'} that do not fit this chart</summary><ul class="rb-list">${replay.problems
          .slice(0, 200)
          .map((p) => `<li>${esc(p)}</li>`)
          .join('')}</ul></details>`
      h += `<h3>Runs</h3><ul class="rb-list">${replay.runs
        .map((r) => {
          const view = replay!.viewAt(replay!.records.length - 1, r.id)
          const end = view.suspended ? 'suspended' : view.status
          return `<li><button class="rb-link rb-mono" type="button" data-m-record="${r.first}">${esc(r.id)} ${esc(r.machine)}</button> <span class="rb-cnote">${r.parent ? `child of ${esc(r.parent)}, ` : ''}${r.last - r.first + 1} records, ${esc(end)}</span></li>`
        })
        .join('')}</ul>`
    }
    head.innerHTML = h
  }

  function renderState() {
    if (!replay || !replay.records.length) {
      state.innerHTML = ''
      return
    }
    const rec = replay.records[at]!
    const run = rec.run
    const r = info(run)
    const m = r && own(ir.machines, r.machine)
    const view = replay.viewAt(at, run)
    let h = `<div class="rb-steps"><button class="rb-btn" type="button" data-m-step="first" title="First record (Home)">⇤</button><button class="rb-btn" type="button" data-m-step="back" title="Step back (←)">← Back</button><span class="rb-step-text">record ${at + 1} of ${replay.records.length}</span><button class="rb-btn" type="button" data-m-step="fwd" title="Step forward (→)">Forward →</button><button class="rb-btn" type="button" data-m-step="last" title="Last record (End)">⇥</button></div>`
    h += `<div class="rb-now"><p class="rb-path">run ${esc(run)}${r ? ` · ${esc(r.machine)}` : ''} · t = ${esc(String(rec.t))}</p><p class="rb-now-text"><strong>${esc(recordText(rec))}</strong></p></div>`
    if (!m) h += `<p class="rb-empty">This chart has no machine ${esc(r?.machine ?? '')}: nothing to draw for this run.</p>`
    else {
      h += `<h3>Active states of ${esc(run)}</h3>${activeHtml(view.active.filter((id) => id !== r!.machine))}`
      if (view.taken) {
        const st = findState(m, view.taken.state)
        const tr = st?.transitions[view.taken.index]
        h += `<h3>Last transition</h3><p><button class="rb-link rb-mono" type="button" data-m-transition="${esc(JSON.stringify([view.taken.state, view.taken.index]))}">${esc(stateName(view.taken.state) || view.taken.state)}: ${esc(tr ? triggerText(tr.trigger) : `#${view.taken.index}`)}${tr?.target ? ` → ${esc(stateName(tr.target))}` : ''}</button></p>`
      }
      if (view.invokes.length || view.timers.length) {
        h += `<h3>Running</h3><ul class="rb-list">${view.invokes.map((i) => `<li>invoke ${i.id}: ${esc(i.src)} <span class="rb-cnote">in ${esc(stateName(i.state))}${replay!.runs.some((x) => x.id === `${run}.${i.id}`) ? `, child run ${esc(`${run}.${i.id}`)}` : ''}</span></li>`).join('')}${view.timers
          .map((t) => `<li>timer ${t.id}: ${t.delay} ms <span class="rb-cnote">in ${esc(stateName(t.state))}</span></li>`)
          .join('')}</ul>`
      }
      if (view.calls.length) {
        h += `<h3>Pending calls</h3><ul class="rb-list">${view.calls
          .map((c) => `<li><code>${esc(c.call)}</code> from ${esc(c.from)} <span class="rb-cnote">its invoke ${c.invoke}; taken, not answered yet</span></li>`)
          .join('')}</ul>`
      }
      const children = replay.runs.filter((x) => x.parent === run)
      if (children.length)
        h += `<h3>Child runs</h3><ul class="rb-list">${children.map((c) => `<li><button class="rb-link rb-mono" type="button" data-m-record="${c.first}">${esc(c.id)} ${esc(c.machine)}</button></li>`).join('')}</ul>`
      if (r!.parent) h += `<p><button class="rb-link" type="button" data-m-parent="${esc(r!.parent)}">← back to the parent run ${esc(r!.parent)}</button></p>`
      h += `<h3>Variables of ${esc(run)}</h3>${variablesHtml(m, view.input, view.vars, true)}`
      if (view.status !== 'running') h += `<h3>Result</h3><pre class="rb-code">${esc(view.status)} ${esc(pretty(view.result))}</pre>`
    }
    state.innerHTML = h
  }

  function renderList() {
    list.innerHTML = replay ? `<h3>Records</h3>${recordsHtml(replay.records, { jump: true, showRun: replay.runs.length > 1 })}` : ''
  }

  function markList() {
    list.querySelectorAll('li.rb-current').forEach((li) => {
      li.classList.remove('rb-current')
      li.removeAttribute('aria-current')
    })
    const li = list.querySelector(`[data-m-record="${at}"]`)?.closest('li')
    if (li) {
      li.classList.add('rb-current')
      li.setAttribute('aria-current', 'step')
      scrollWithin(li)
    }
  }

  function readFile(file: File) {
    file.text().then(
      (text) => load(file.name, text),
      (e: unknown) => {
        loadError = `Could not read ${file.name}: ${e instanceof Error ? e.message : String(e)}`
        renderHead()
      },
    )
  }

  el.addEventListener('change', (ev) => {
    const input = ev.target as HTMLInputElement
    if (input.dataset.mFile !== undefined && input.files?.[0]) readFile(input.files[0])
  })
  el.addEventListener('click', (ev) => {
    const b = (ev.target as Element).closest<HTMLElement>('button')
    if (!b) return
    if (b.dataset.mRecord !== undefined) void go(Number(b.dataset.mRecord))
    else if (b.dataset.mStep === 'first') void go(0)
    else if (b.dataset.mStep === 'back') void go(at - 1)
    else if (b.dataset.mStep === 'fwd') void go(at + 1)
    else if (b.dataset.mStep === 'last') void go(Infinity)
    else if (b.dataset.mParent !== undefined) {
      // The parent's record that started this child, or its last before it.
      const parent = b.dataset.mParent
      const child = runOf(at)
      const inv = Number(child.slice(child.lastIndexOf('.') + 1))
      const i = replay?.records.findIndex((r) => r.run === parent && r.kind === 'invoke.started' && r.invoke === inv) ?? -1
      void go(i >= 0 ? i : (info(parent)?.first ?? 0))
    }
  })
  // Dropping a trace anywhere on the page opens it here.
  document.addEventListener('dragover', (ev) => {
    if (ev.dataTransfer?.types.includes('Files')) ev.preventDefault()
  })
  document.addEventListener('drop', (ev) => {
    const file = ev.dataTransfer?.files[0]
    if (!file || document.body.dataset.mode === 'coverage' || document.body.dataset.mode === 'diff') return
    ev.preventDefault()
    host.setMode('replay')
    readFile(file)
  })

  renderHead()
  if (embedded) load(embedded.name, embedded.text)

  return {
    load,
    render() {
      renderHead()
      renderState()
      markList()
    },
    highlight(machine): Highlight | null {
      if (!replay || !replay.records.length) return null
      let run = runOf(at)
      if (info(run)?.machine !== machine) {
        // Another machine is drawn: its latest run started so far, if any.
        const other = replay.runs.filter((r) => r.machine === machine && r.first <= at).pop()
        if (!other) return null
        run = other.id
      }
      const view = replay.viewAt(at, run)
      const named = run === runOf(at) ? replay.named(at) : { states: [], transition: null }
      const taken = named.transition && replay.records[at]?.kind === 'transition' ? [named.transition] : view.taken ? [view.taken] : []
      return { active: view.active, taken, named: named.states }
    },
    status() {
      if (!replay || !replay.records.length) return 'Replay: open a trace'
      const rec = replay.records[at]!
      return `Replay · record ${at + 1}/${replay.records.length} · run ${rec.run} · t = ${rec.t} · ${recordText(rec)}`
    },
    forward: () => void go(at + 1),
    back: () => void go(at - 1),
    first: () => void go(0),
    last: () => void go(Infinity),
  }
}
