// Coverage mode's panel: load traces (embedded by `roadbook viz --coverage`, picked, or dropped
// on the page while in this mode), and see how often each transition of the machine on screen
// was taken and each state entered, across every run; the transitions never taken are listed,
// and `→` and `←` step through them. The marks on the diagram come from coverageAnnotations().
// The model is src/coverage.ts.

import type { Ir } from '../ir.ts'
import { own } from '../ir.ts'
import { coverage, coverageAnnotations, stateCounts, transitionCounts } from '../coverage.ts'
import type { Coverage } from '../coverage.ts'
import { findState, transitionId, triggerText } from '../model.ts'
import { esc } from '../render.ts'
import type { Controller, Host } from './modes.ts'

export function coverageMode(host: Host, el: HTMLElement, embedded: { name: string; text: string }[]): Controller {
  const ir: Ir = host.ir
  let traces: { name: string; text: string }[] = [...embedded]
  let cov: Coverage | null = traces.length ? coverage(ir, traces) : null
  let at = -1
  let loadError = ''

  const short = (id: string) => {
    const m = host.current()
    return id === m ? m : id.slice(m.length + 1)
  }
  const plural = (n: number, w: string) => `${n} ${w}${n === 1 ? '' : 's'}`

  function transitionLabel(state: string, index: number): string {
    const m = own(ir.machines, host.current())
    const st = m ? findState(m, state) : null
    const t = st?.transitions[index]
    if (!t) return `${short(state)} #${index}`
    return `${short(state)}: ${triggerText(t.trigger)}${t.guard ? ` [${t.guard.text}]` : ''} → ${t.target ? short(t.target) : '(internal)'}`
  }

  const never = () => {
    const m = own(ir.machines, host.current())
    if (!m || !cov) return []
    return transitionCounts(m, own(cov.machines, host.current())).filter((t) => t.count === 0)
  }

  function render() {
    const name = host.current()
    let h = `<div class="rb-mode-head"><h2>Coverage</h2></div>`
    h += `<div class="rb-row-btns"><label class="rb-btn rb-file">Add traces…<input type="file" multiple accept=".jsonl,.json,.ndjson,application/x-ndjson,application/json,text/plain" data-cov-file hidden></label>${traces.length ? '<button class="rb-btn" type="button" data-cov-clear>Clear</button>' : ''}<span class="rb-cnote">or drop <code>.jsonl</code> files on the page</span></div>`
    if (loadError) h += `<p class="rb-alert" role="alert">${esc(loadError)}</p>`
    if (!cov) {
      h += `<p class="rb-mode-note">Load the traces of many runs, from a test run or from production (spec/trace.md, either runtime), to see how often each transition was taken. <code>roadbook viz --coverage traces/</code> puts them in the page. Nothing is uploaded: the files are read in the browser.</p>`
      el.innerHTML = h
      return
    }
    h += `<p>${plural(cov.files.length, 'trace file')} · ${plural(cov.runs, 'run')} of this chart's machines · ${plural(cov.records, 'record')}</p>`
    if (cov.stale.runs)
      h += `<div class="rb-refusal" role="alert"><strong>Warning:</strong> ${plural(cov.stale.runs, 'run')} (${plural(cov.stale.records, 'record')}) of ${esc(cov.stale.machines.join(', '))} were recorded from another version of the machine (another IR hash) and are left out.</div>`
    if (cov.other.runs) h += `<p class="rb-cnote">${plural(cov.other.runs, 'run')} of machines this chart does not have (${esc(cov.other.machines.join(', '))}) are not counted.</p>`
    if (cov.problems.length)
      h += `<details class="rb-problems"><summary>${plural(cov.problems.length, 'line')} that are not trace records</summary><ul class="rb-list">${cov.problems
        .slice(0, 200)
        .map((p) => `<li>${esc(p)}</li>`)
        .join('')}</ul></details>`
    const m = own(ir.machines, name)
    const mc = own(cov.machines, name)
    h += `<h3>${esc(name)}: ${plural(mc?.runs ?? 0, 'run')}</h3>`
    if (!m || !mc || !mc.runs) {
      h += `<p class="rb-empty">No run of ${esc(name)} in these traces.</p>`
      el.innerHTML = h
      return
    }
    const ts = transitionCounts(m, mc)
    const ss = stateCounts(m, mc)
    const untaken = ts.filter((t) => t.count === 0)
    const unentered = ss.filter((s) => s.count === 0)
    h += `<p>Transitions taken: <strong>${ts.length - untaken.length} of ${ts.length}</strong> · states entered: <strong>${ss.length - unentered.length} of ${ss.length}</strong></p>`
    h += `<p class="rb-mode-note">Marks: <strong>×N</strong> taken or entered N times, heavier and darker the more often; <strong>never</strong> (dotted, in the error colour) never taken or entered. Hover for the count.</p>`
    if (untaken.length)
      h += `<h3>Never taken (${untaken.length})</h3><ol class="rb-list rb-cov-list">${untaken
        .map((t, i) => `<li${i === at ? ' class="rb-current"' : ''}><button class="rb-link rb-mono" type="button" data-m-transition="${esc(JSON.stringify([t.state, t.index]))}" data-cov-at="${i}">${esc(transitionLabel(t.state, t.index))}</button></li>`)
        .join('')}</ol>`
    else h += `<p>Every transition was taken at least once.</p>`
    if (unentered.length) h += `<h3>Never entered (${unentered.length})</h3><ul class="rb-list">${unentered.map((s) => `<li><button class="rb-link rb-mono" type="button" data-m-state="${esc(s.state)}">${esc(short(s.state))}</button></li>`).join('')}</ul>`
    h += `<details class="rb-cov-all"><summary>Every transition, with its count</summary><table class="rb-cov-table"><tbody>${ts
      .map((t) => `<tr><td class="rb-cov-n">${t.count ? `×${t.count}` : 'never'}</td><td><button class="rb-link rb-mono" type="button" data-m-transition="${esc(JSON.stringify([t.state, t.index]))}">${esc(transitionLabel(t.state, t.index))}</button></td></tr>`)
      .join('')}</tbody></table></details>`
    el.innerHTML = h
  }

  function add(files: { name: string; text: string }[]) {
    traces = [...traces, ...files]
    cov = coverage(ir, traces)
    at = -1
    loadError = cov.runs || !files.length ? '' : `None of the runs is of a machine of this chart${cov.stale.runs ? ' as it is now' : ''}.`
    render()
    host.redraw()
  }

  function readFiles(list: FileList | File[]) {
    const files = [...list]
    Promise.all(files.map((f) => f.text().then((text) => ({ name: f.name, text })))).then(add, (e: unknown) => {
      loadError = `Could not read the files: ${e instanceof Error ? e.message : String(e)}`
      render()
    })
  }

  function go(i: number) {
    const list = never()
    if (!list.length) return
    at = ((i % list.length) + list.length) % list.length
    const t = list[at]!
    host.select({ type: 'transition', state: t.state, index: t.index })
    render()
  }

  el.addEventListener('change', (ev) => {
    const input = ev.target as HTMLInputElement
    if (input.dataset.covFile !== undefined && input.files?.length) readFiles(input.files)
  })
  el.addEventListener('click', (ev) => {
    const b = (ev.target as Element).closest<HTMLElement>('button')
    if (!b) return
    if (b.dataset.covClear !== undefined) {
      traces = []
      cov = null
      at = -1
      render()
      host.redraw()
    } else if (b.dataset.covAt !== undefined) at = Number(b.dataset.covAt)
  })
  // Dropping traces on the page while in coverage mode adds them here (replay takes them otherwise).
  document.addEventListener('drop', (ev) => {
    if (document.body.dataset.mode !== 'coverage' || !ev.dataTransfer?.files.length) return
    ev.preventDefault()
    readFiles(ev.dataTransfer.files)
  })

  return {
    render() {
      at = -1
      render()
    },
    highlight: () => null,
    status() {
      if (!cov) return 'Coverage: add traces'
      const m = own(ir.machines, host.current())
      const mc = own(cov.machines, host.current())
      if (!m || !mc?.runs) return `Coverage · no run of ${host.current()} in ${plural(cov.files.length, 'trace file')}`
      const ts = transitionCounts(m, mc)
      const taken = ts.filter((t) => t.count > 0).length
      return `Coverage · ${host.current()} · ${plural(mc.runs, 'run')} · ${taken} of ${ts.length} transitions taken · ${ts.length - taken} never`
    },
    annotate: (machine) => (cov ? coverageAnnotations(ir, cov, machine) : null),
    details(machine, sel) {
      if (!cov || !sel || sel.type === 'event') return ''
      const mc = own(cov.machines, machine)
      if (!mc?.runs) return ''
      const runs = plural(mc.runs, 'run')
      if (sel.type === 'state') {
        const n = mc.states[sel.id] ?? 0
        return `<div class="rb-cov-details"><p>${n ? `Entered <strong>${plural(n, 'time')}</strong> in ${runs}.` : `<strong>Never entered</strong> in ${runs}.`}</p></div>`
      }
      const n = mc.transitions[transitionId(sel.state, sel.index)] ?? 0
      return `<div class="rb-cov-details"><p>${n ? `Taken <strong>${plural(n, 'time')}</strong> in ${runs}.` : `<strong>Never taken</strong> in ${runs}.`}</p></div>`
    },
    forward: () => go(at + 1),
    back: () => go(at < 0 ? -1 : at - 1),
  }
}
