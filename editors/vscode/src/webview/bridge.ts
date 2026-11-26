// Runs in the preview's webview, after the viewer script (crates/roadbook-cli/assets/
// roadbook-viewer.js), which mounts the page on DOMContentLoaded. It connects the unmodified
// viewer to the extension from the outside, through the DOM the viewer draws:
//
// - follows VS Code's theme, by keeping `<html data-theme>` on the theme the extension sends;
// - restores the machine and the selection the reader had before the page was replaced with a
//   newer chart, by clicking the machine's tab and the selected state or transition;
// - reports the machine and selection, so the next page can restore them;
// - reports a click on a state or a transition, so the editor can reveal its source;
// - shows the problems (compile errors) the extension sends, over the last diagram drawn.

import type { BridgeConfig, FromWebview, Problems, Theme, ToWebview, ViewState } from '../preview/protocol.ts'
import type { Target } from '../preview/ir.ts'

declare function acquireVsCodeApi(): {
  postMessage(message: FromWebview): void
  getState(): unknown
  setState(state: unknown): void
}

const vscode = acquireVsCodeApi()
const config = JSON.parse(document.getElementById('rbx-config')?.textContent ?? '{}') as BridgeConfig
const post = (m: FromWebview) => vscode.postMessage(m)
// Read now: the viewer replaces the body, data included, when it mounts.
const hasChart = document.getElementById('rb-data') !== null

// ───────────── Content-Security-Policy violations, reported for the tests ─────────────
document.addEventListener('securitypolicyviolation', (e) => post({ type: 'csp', directive: e.violatedDirective, blocked: e.blockedURI }))
window.addEventListener('error', (e) => post({ type: 'error', message: `${e.message} (${e.filename}:${e.lineno})` }))
window.addEventListener('unhandledrejection', (e) => post({ type: 'error', message: String(e.reason) }))

// ───────────── theme ─────────────
let theme: Theme = config.theme
const html = document.documentElement
function applyTheme() {
  if (html.dataset.theme !== theme) html.dataset.theme = theme
}
// The viewer sets or removes `data-theme` when it mounts (its own toggle, hidden here).
new MutationObserver(applyTheme).observe(html, { attributes: true, attributeFilter: ['data-theme'] })
applyTheme()

// ───────────── what the reader is looking at ─────────────
const q = (s: string) => s.replace(/["\\]/g, '\\$&')

function currentMachine(): string | null {
  return document.querySelector<HTMLElement>('#rb-tabs [aria-selected="true"]')?.dataset.machine ?? null
}

function currentSelection(): Target | null {
  const svg = document.querySelector('svg.rb-svg')
  if (!svg) return null
  const st = svg.querySelector<SVGElement>('.rb-state.rb-selected[data-state]')
  if (st) return { kind: 'state', id: st.dataset.state! }
  const tr = svg.querySelector<SVGElement>('.rb-selected[data-transition]')
  const owner = tr?.closest<SVGElement>('[data-state]')
  if (tr && owner) return { kind: 'transition', state: owner.dataset.state!, index: Number(tr.dataset.transition) }
  return null
}

function saveView(): ViewState {
  const view: ViewState = { machine: currentMachine(), selection: currentSelection() }
  vscode.setState({ uri: config.uri, view })
  post({ type: 'view', view })
  return view
}

/** The target a click in the diagram selects, as the viewer reads it; null for toggles and links. */
function clickTarget(el: Element): Target | null {
  if (el.closest('[data-toggle], [data-event], .rb-machine-link')) return null
  const tr = el.closest<SVGElement>('[data-transition]')
  const owner = tr?.closest<SVGElement>('[data-state]')
  if (tr && owner) return { kind: 'transition', state: owner.dataset.state!, index: Number(tr.dataset.transition) }
  const st = el.closest<SVGElement>('[data-state]')
  return st ? { kind: 'state', id: st.dataset.state! } : null
}

let restoring = false
let down: { x: number; y: number } | null = null
document.addEventListener('pointerdown', (e) => (down = { x: e.clientX, y: e.clientY }), true)
document.addEventListener('click', (e) => {
  const moved = down !== null && Math.hypot(e.clientX - down.x, e.clientY - down.y) >= 4
  const target = e.target instanceof Element ? e.target : null
  // After the viewer's own handlers have run.
  setTimeout(() => {
    if (restoring) return
    saveView()
    if (moved || !target?.closest('#rb-canvas')) return
    const t = clickTarget(target)
    const machine = currentMachine()
    if (t && machine) post({ type: 'reveal', machine, target: t })
  }, 0)
})
document.addEventListener('keyup', () => setTimeout(() => !restoring && saveView(), 0))

// ───────────── restoring the view once the viewer has drawn ─────────────
/** Resolves with the diagram's SVG once one other than `previous` is in the page. */
function nextDiagram(previous: Element | null, timeoutMs = 20_000): Promise<Element | null> {
  return new Promise((resolve) => {
    const found = () => {
      const svg = document.querySelector('svg.rb-svg')
      return svg && svg !== previous ? svg : null
    }
    const now = found()
    if (now) return resolve(now)
    const obs = new MutationObserver(() => {
      const svg = found()
      if (svg) {
        obs.disconnect()
        clearTimeout(timer)
        resolve(svg)
      }
    })
    const timer = setTimeout(() => {
      obs.disconnect()
      resolve(null)
    }, timeoutMs)
    obs.observe(document.body, { childList: true, subtree: true })
  })
}

function click(el: Element | null | undefined) {
  el?.dispatchEvent(new MouseEvent('click', { bubbles: true }))
}

async function restore(view: ViewState | null) {
  let svg = await nextDiagram(null)
  if (!svg) {
    const shown = document.querySelector('.rb-error, .rb-empty')?.textContent ?? document.body.textContent?.trim().slice(0, 300)
    return post({ type: 'error', message: `the viewer drew no diagram${shown ? `: ${shown}` : ''}` })
  }
  restoring = true
  try {
    if (view?.machine && view.machine !== currentMachine()) {
      const tab = document.querySelector(`#rb-tabs [data-machine="${q(view.machine)}"]`)
      if (tab) {
        click(tab)
        svg = (await nextDiagram(svg)) ?? svg
      }
    }
    const sel = view?.selection
    if (sel?.kind === 'state') click(svg.querySelector(`.rb-state[data-state="${q(sel.id)}"]`))
    else if (sel?.kind === 'transition') {
      const owner = `[data-state="${q(sel.state)}"]`
      click(svg.querySelector(`${owner}[data-transition="${sel.index}"], ${owner} .rb-row[data-transition="${sel.index}"]`))
    }
  } finally {
    // The viewer's handlers have run by the next task.
    setTimeout(() => {
      restoring = false
      post({ type: 'rendered', view: saveView() })
    }, 0)
  }
}

// ───────────── problems ─────────────
let problems: Problems | null = null

function esc(s: string): string {
  return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;')
}

function renderProblems() {
  let box = document.getElementById('rbx-problems')
  if (!problems) {
    if (box) box.hidden = true
    return
  }
  if (!box || !document.body.contains(box)) {
    box = document.createElement('section')
    box.id = 'rbx-problems'
    box.setAttribute('role', 'alert')
    box.addEventListener('click', (e) => {
      const b = (e.target as Element).closest<HTMLElement>('button[data-at]')
      if (!b) return
      const [file, line, col] = JSON.parse(b.dataset.at!) as [string, number, number]
      post({ type: 'open', file, line, col })
    })
    document.body.appendChild(box)
  }
  const p = problems
  const n = p.diagnostics.length
  const head = n
    ? `${esc(p.entry)} does not compile: ${n} error${n === 1 ? '' : 's'}`
    : `The preview of ${esc(p.entry)} could not be updated`
  const items = p.diagnostics
    .map(
      (d) =>
        `<li><button type="button" data-at="${esc(JSON.stringify([d.file, d.line, d.col]))}">` +
        `<span class="rbx-code">${esc(d.code)}</span> ${esc(d.message)} <span class="rbx-where">${esc(d.file)}:${d.line}:${d.col}</span>` +
        (d.help ? `<span class="rbx-help">help: ${esc(d.help)}</span>` : '') +
        `</button></li>`,
    )
    .join('')
  box.innerHTML =
    `<h2>${head}${p.stale ? '. The diagram below is the last version that compiled.' : ''}</h2>` +
    (items ? `<ul>${items}</ul>` : '') +
    (p.text ? `<pre>${esc(p.text)}</pre>` : '')
  box.classList.toggle('rbx-alone', !p.stale)
  box.hidden = false
}

// ───────────── messages from the extension ─────────────
window.addEventListener('message', (e: MessageEvent<ToWebview>) => {
  const m = e.data
  if (m?.type === 'theme') {
    theme = m.theme === 'dark' ? 'dark' : 'light'
    applyTheme()
  } else if (m?.type === 'problems') {
    problems = m.problems
    renderProblems()
  }
})

function start() {
  // The webview's state is the newest when VS Code reloads a hidden preview; the page's own
  // when the extension has just replaced it (the two are then the same, if VS Code keeps it).
  const saved = vscode.getState() as { view?: ViewState } | undefined
  if (hasChart) void restore(saved?.view ?? config.view ?? null)
  // The viewer replaces the body when it mounts, so the problems box is added after it.
  renderProblems()
  post({ type: 'ready' })
}

// Registered after the viewer's own listener, so this runs once the viewer has mounted.
if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', start)
else start()
