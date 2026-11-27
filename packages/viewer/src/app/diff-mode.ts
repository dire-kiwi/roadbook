// Diff mode's panel: what changed in the chart between the two revisions `roadbook viz --diff`
// compared, the machine on screen's changes in a list to step through, and (in the details) the
// before and after of what is selected. The marks on the diagram come from diffAnnotations().
// The model is src/diff.ts.

import type { Ir } from '../ir.ts'
import { own } from '../ir.ts'
import { DIFF_SIGN, changesTable, countsText, diffAnnotations, historyCountsText, diffDetailsHtml, diffEntries, machineDiff, statusChip } from '../diff.ts'
import type { ChartDiff } from '../diff.ts'
import { findState, isHistory, kindText, triggerText } from '../model.ts'
import { esc } from '../render.ts'
import type { Controller, Host } from './modes.ts'

export function diffMode(host: Host, el: HTMLElement, diff: ChartDiff): Controller {
  const ir: Ir = host.ir
  let at = -1

  const entries = () => diffEntries(ir, diff, host.current())

  function entryLabel(e: ReturnType<typeof entries>[number]): string {
    const m = host.current()
    const short = (id: string) => (id === m ? m : id.slice(m.length + 1))
    const machine = own(ir.machines, m)
    if (e.type === 'state') {
      const st = machine ? findState(machine, e.state) : null
      return `${st && isHistory(st) ? kindText(st) + ' ' : ''}state ${short(e.state)}`
    }
    const st = machine ? findState(machine, e.state) : null
    const t = st?.transitions[e.index]
    if (!t) return `${short(e.state)} #${e.index}`
    return `${short(e.state)}: ${triggerText(t.trigger)}${t.guard ? ` [${t.guard.text}]` : ''} → ${t.target ? short(t.target) : '(internal)'}`
  }

  function render() {
    const name = host.current()
    const d = machineDiff(diff, name)
    let h = `<div class="rb-mode-head"><h2>Changes <span class="rb-cnote">${esc(diff.from)} → ${esc(diff.to)}</span></h2></div>`
    h += `<p>This chart: ${statusChip(diff.status)}</p>`
    h += `<p class="rb-mode-note">Marks: <strong>+</strong> added (heavy, solid), <strong>−</strong> removed (dashed, faded, struck through), <strong>~</strong> changed. Click one for its before and after; <kbd>→</kbd> and <kbd>←</kbd> step through the changes.</p>`
    const names = Object.keys(ir.machines)
    if (names.length > 1) {
      h += `<h3>Machines</h3><ul class="rb-list">${names
        .map((n) => {
          const md = machineDiff(diff, n)
          const counts = md.status === 'unchanged' ? '' : ` <span class="rb-cnote">states ${esc(countsText(md.counts.states) + historyCountsText(md))}, transitions ${esc(countsText(md.counts.transitions))}</span>`
          return `<li><button class="rb-link rb-mono" type="button" data-diff-machine="${esc(n)}"${n === name ? ' aria-current="true"' : ''}>${esc(n)}</button> ${statusChip(md.status)}${counts}</li>`
        })
        .join('')}</ul>`
    }
    h += `<h3>${esc(name)} ${statusChip(d.status)}</h3>`
    if (d.status !== 'unchanged') h += `<p class="rb-cnote">States ${esc(countsText(d.counts.states) + historyCountsText(d))} · transitions ${esc(countsText(d.counts.transitions))}</p>`
    if (d.changes.length) h += `<h3>The machine itself</h3>${changesTable(diff, d.changes)}`
    const list = entries()
    if (d.status === 'added' || d.status === 'removed') {
      h += `<p class="rb-empty">The whole machine was ${d.status}.</p>`
    } else if (list.length) {
      h += `<h3>Changes in the diagram</h3><ol class="rb-list rb-diff-list">${list
        .map((e, i) => {
          const data = e.type === 'state' ? `data-m-state="${esc(e.state)}"` : `data-m-transition="${esc(JSON.stringify([e.state, e.index]))}"`
          return `<li${i === at ? ' class="rb-current"' : ''}><span class="rb-diff-sign rb-diff-sign-${e.status}" aria-hidden="true">${DIFF_SIGN[e.status]}</span> <button class="rb-link rb-mono" type="button" ${data} data-diff-at="${i}">${esc(entryLabel(e))}</button> <span class="rb-cnote">${e.status}${e.changes.length ? `: ${esc(e.changes.map((c) => c.what).join(', '))}` : ''}</span></li>`
        })
        .join('')}</ol>`
    } else if (!d.changes.length) {
      h += `<p class="rb-empty">No change in this machine.</p>`
    }
    if (diff.decls.length) {
      h += `<h3>Declarations</h3><ul class="rb-list">${diff.decls
        .map((x) => `<li><span class="rb-mono">${esc(x.name)}</span> ${statusChip(x.status)}${x.changes.length ? changesTable(diff, x.changes) : ''}</li>`)
        .join('')}</ul>`
    }
    el.innerHTML = h
  }

  function go(i: number) {
    const list = entries()
    if (!list.length) return
    at = ((i % list.length) + list.length) % list.length
    const e = list[at]!
    host.select(e.type === 'state' ? { type: 'state', id: e.state } : { type: 'transition', state: e.state, index: e.index })
    render()
  }

  el.addEventListener('click', (ev) => {
    const b = (ev.target as Element).closest<HTMLElement>('button')
    if (!b) return
    if (b.dataset.diffMachine) {
      at = -1
      void host.open(b.dataset.diffMachine)
    } else if (b.dataset.diffAt !== undefined) at = Number(b.dataset.diffAt)
  })

  return {
    render() {
      at = -1
      render()
    },
    highlight: () => null,
    status() {
      const d = machineDiff(diff, host.current())
      return `Diff · ${diff.from} → ${diff.to} · ${host.current()}: ${d.status}${d.status === 'changed' ? ` · states ${countsText(d.counts.states)} · transitions ${countsText(d.counts.transitions)}` : ''}`
    },
    annotate: (machine, collapsed) => diffAnnotations(diff, machine, collapsed),
    details(machine, sel) {
      if (!sel || sel.type === 'event') return ''
      return diffDetailsHtml(diff, machine, sel)
    },
    forward: () => go(at + 1),
    back: () => go(at < 0 ? -1 : at - 1),
  }
}

/** The page of a chart that does not compile at one side: the diagnostics, as the CLI printed them. */
export function diffErrorsHtml(entry: string, diff: ChartDiff): string {
  return `<main class="rb-index-main"><section><h2>${esc(entry)} ${statusChip('error')}</h2><p>The chart could not be compared: it does not compile at ${diff.errors
    .map((e) => `<code>${esc(e.label)}</code>`)
    .join(' and ')}.</p>${diff.errors.map((e) => `<h3>At ${esc(e.label)} (${e.side === 'before' ? 'before' : 'after'})</h3><pre class="rb-code rb-diagnostics">${esc(e.text)}</pre>`).join('')}</section></main>`
}
