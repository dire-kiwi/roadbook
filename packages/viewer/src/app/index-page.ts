// The project index page: the graph of which machine invokes, spawns and talks to which, and a table of machines.

import type { ElkLike } from '../layout.ts'
import type { Measure } from '../model.ts'
import type { ProjectIndex } from '../project.ts'
import { layoutIndex, localHref, machineHref, renderIndexLegend, renderIndexSvg, renderIndexTable } from '../project.ts'
import { countsText, countsWords, statusChip } from '../diff.ts'
import { esc } from '../render.ts'
import { $, setupTheme } from './dom.ts'

export async function mountIndex(index: ProjectIndex, elk: ElkLike, measure: Measure) {
  document.body.className = 'rb-index'
  document.title = `${index.title} · Roadbook`
  const pages = index.pages.map((p) => `<a class="rb-tab" href="${esc(localHref(p.href))}">${esc(p.entry)}</a>`).join('')
  document.body.innerHTML = `
<header class="rb-top">
  <span class="rb-brand">Roadbook<small>${esc(index.title)}</small></span>
  <nav class="rb-tabs" aria-label="Charts"><span class="rb-tabs-label">Charts</span>${pages}</nav>
  <div class="rb-tools"><button class="rb-btn" id="rb-theme" type="button"></button></div>
</header>
<main class="rb-index-main">
  ${index.diff ? diffSummaryHtml(index) : ''}
  <section><h2>Which machine invokes, spawns and talks to which</h2><div class="rb-graph" id="rb-graph"><p class="rb-empty">Laying out…</p></div>${renderIndexLegend()}</section>
  <section><h2>Machines</h2>${renderIndexTable(index)}</section>
</main>`
  setupTheme($(document.body, '#rb-theme'))
  const graph = $(document.body, '#rb-graph')
  if (!index.machines.length) {
    graph.innerHTML = '<p class="rb-empty">No machines.</p>'
    return
  }
  try {
    graph.innerHTML = renderIndexSvg(index, await layoutIndex(index, elk, measure))
  } catch (e) {
    graph.innerHTML = `<p class="rb-error">${esc(e instanceof Error ? e.message : String(e))}</p>`
  }
}

/**
 * The top of the index of `roadbook viz --diff`: the revisions, the counts, and the machines that
 * changed (first, and linked), then the charts that do not compile at a side.
 */
export function diffSummaryHtml(index: ProjectIndex): string {
  const d = index.diff!
  const s = d.summary
  let h = `<section class="rb-diff-summary"><h2>Changes <span class="rb-cnote">${esc(d.from)} → ${esc(d.to)}</span></h2>`
  // Machines that only moved to another file are counted apart (`roadbook viz --diff` matches
  // machines across files).
  const machines = [countsWords(s.machines), s.machines.moved ? `${s.machines.moved} moved` : ''].filter((x) => x && !(x === 'none' && s.machines.moved)).join(', ')
  h += `<p>Machines: ${esc(machines)}. States: ${esc(countsWords(s.states))}. Transitions: ${esc(countsWords(s.transitions))}.</p>`
  const changed = index.machines.map((m, i) => ({ m, d: d.machines[i] })).filter((x) => x.d && x.d.status !== 'unchanged')
  if (changed.length) {
    h += `<table class="rb-table"><thead><tr><th>Machine</th><th>Change</th><th>States</th><th>Transitions</th><th>Chart</th></tr></thead><tbody>${changed
      .map(({ m, d: md }) => {
        const page = index.pages[m.page]
        const href = page ? machineHref(page.href, m.name) : '#'
        return `<tr><td><a class="rb-mono" href="${esc(href)}">${esc(m.name)}</a></td><td>${statusChip(md!.status)}</td><td>${esc(countsText(md!.states))}</td><td>${esc(countsText(md!.transitions))}</td><td class="rb-mono">${esc(page?.entry ?? '')}</td></tr>`
      })
      .join('')}</tbody></table>`
  } else h += `<p class="rb-empty">No machine changed.</p>`
  const broken = d.charts.map((c, i) => ({ c, page: index.pages[i] })).filter((x) => x.c.errors.length)
  if (broken.length)
    h += `<h3>Charts that do not compile</h3><ul class="rb-list">${broken
      .map(({ c, page }) => `<li><a class="rb-mono" href="${esc(page ? localHref(page.href) : '#')}">${esc(c.entry)}</a> ${statusChip('error')} at ${c.errors.map((side) => `<code>${esc(side === 'before' ? d.from : d.to)}</code>`).join(' and ')}</li>`)
      .join('')}</ul>`
  return h + '</section>'
}
