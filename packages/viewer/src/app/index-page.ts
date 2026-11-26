// The project index page: the graph of which machine invokes which, and a table of machines.

import type { ElkLike } from '../layout.ts'
import type { Measure } from '../model.ts'
import type { ProjectIndex } from '../project.ts'
import { layoutIndex, renderIndexSvg, renderIndexTable } from '../project.ts'
import { esc } from '../render.ts'
import { $, setupTheme } from './dom.ts'

export async function mountIndex(index: ProjectIndex, elk: ElkLike, measure: Measure) {
  document.body.className = 'rb-index'
  document.title = `${index.title} · Roadbook`
  const pages = index.pages.map((p) => `<a class="rb-tab" href="${esc(p.href)}">${esc(p.entry)}</a>`).join('')
  document.body.innerHTML = `
<header class="rb-top">
  <span class="rb-brand">Roadbook<small>${esc(index.title)}</small></span>
  <nav class="rb-tabs" aria-label="Charts"><span class="rb-tabs-label">Charts</span>${pages}</nav>
  <div class="rb-tools"><button class="rb-btn" id="rb-theme" type="button"></button></div>
</header>
<main class="rb-index-main">
  <section><h2>Which machine invokes which</h2><div class="rb-graph" id="rb-graph"><p class="rb-empty">Laying out…</p></div></section>
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
