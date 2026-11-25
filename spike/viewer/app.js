// Browser side of the viewer: machine switcher, pan and zoom, selection, side panel.
// Everything it draws comes from renderSvg(); this file only wires up interaction.

import { effectText, invokeText, triggerKind, triggerText, guardText, walkStates, findState, paramsText, signatureText, sourceText } from './model.js'
import { layoutMachine } from './layout.js'
import { renderSvg, renderLegend, renderMachineHeader, esc } from './render.js'

function startViewer() {
  const ir = JSON.parse(document.getElementById('wm-ir').textContent)
  const $ = (id) => document.getElementById(id)
  const canvas = $('wm-canvas')
  const panel = $('wm-panel')
  const machineNames = Object.keys(ir.machines)
  const elk = new ELK()
  const layouts = new Map()

  let current = null          // machine name
  let selection = null        // null | { type: 'state', id } | { type: 'transition', state, index }
  let navStack = []           // [{ machine, selection }] for the Back button
  let svg = null
  let view = null             // visible part of the diagram, in diagram units: { x, y, w, h }
  let fitted = true           // true until the user pans or zooms

  // Measure text with the real monospace font, so boxes fit their text exactly.
  const ctx = document.createElement('canvas').getContext('2d')
  const mono = getComputedStyle(document.documentElement).getPropertyValue('--wm-mono').trim() || 'monospace'
  const measure = (text, size, bold) => {
    ctx.font = `${bold ? '650' : '400'} ${size}px ${mono}`
    return ctx.measureText(text).width + 1
  }

  // ───────────── showing a machine ─────────────
  async function show(name, select = null) {
    current = name
    selection = select
    if (!layouts.has(name)) layouts.set(name, await layoutMachine(ir, name, elk, { measure }))
    if (current !== name) return
    if (svg) svg.remove()
    canvas.insertAdjacentHTML('afterbegin', renderSvg(ir, name, layouts.get(name), { measure }))
    svg = canvas.querySelector('svg.wm-svg')
    svg.removeAttribute('width')
    svg.removeAttribute('height')
    try { history.replaceState(null, '', '#' + name) } catch { /* some file:// setups refuse; the hash is only a convenience */ }
    document.title = `${name} · Waymark`
    fit()
    renderChrome()
    applySelection()
    renderPanel()
  }

  function renderChrome() {
    $('wm-machine-head').innerHTML = renderMachineHeader(ir, current)
    $('wm-tabs').innerHTML = `<span class="wm-tabs-label">${machineNames.length === 1 ? 'Machine' : 'Machines'}</span>`
      + machineNames.map((m) =>
        `<button class="wm-tab" type="button" role="tab" aria-selected="${m === current}" data-machine="${esc(m)}">${esc(m)}</button>`).join('')
    const back = $('wm-back')
    const prev = navStack[navStack.length - 1]
    back.hidden = !prev
    if (prev) back.textContent = `← Back to ${prev.machine}`
  }

  function openMachine(name, viaInvoke) {
    if (!ir.machines[name]) return
    if (viaInvoke) navStack.push({ machine: current, selection })
    else navStack = []
    show(name)
  }

  // ───────────── pan and zoom ─────────────
  const size = () => ({ w: Math.max(canvas.clientWidth, 50), h: Math.max(canvas.clientHeight, 50) })
  function applyView() {
    if (svg) svg.setAttribute('viewBox', `${view.x} ${view.y} ${view.w} ${view.h}`)
  }
  function fit() {
    const l = layouts.get(current)
    if (!l) return
    const { w, h } = size()
    const margin = 28
    const scale = Math.min((w - 2 * margin) / l.width, (h - 2 * margin) / l.height, 1.25)
    view = { w: w / scale, h: h / scale }
    view.x = (l.width - view.w) / 2
    view.y = (l.height - view.h) / 2
    fitted = true
    applyView()
  }
  function zoomBy(factor, cx, cy) {
    const { w, h } = size()
    const scale = Math.min(Math.max((w / view.w) * factor, 0.1), 5)
    const px = view.x + (cx / w) * view.w
    const py = view.y + (cy / h) * view.h
    view.w = w / scale
    view.h = h / scale
    view.x = px - (cx / w) * view.w
    view.y = py - (cy / h) * view.h
    fitted = false
    applyView()
  }
  function reveal(box) {
    if (!box) return
    const pad = 20
    const inside = box.x >= view.x + pad && box.y >= view.y + pad
      && box.x + box.width <= view.x + view.w - pad && box.y + box.height <= view.y + view.h - pad
    if (inside || (box.width > view.w - 2 * pad || box.height > view.h - 2 * pad)) return
    view.x = box.x + box.width / 2 - view.w / 2
    view.y = box.y + box.height / 2 - view.h / 2
    fitted = false
    applyView()
  }

  canvas.addEventListener('wheel', (ev) => {
    ev.preventDefault()
    const r = canvas.getBoundingClientRect()
    zoomBy(Math.exp(-ev.deltaY * (ev.deltaMode === 1 ? 0.05 : 0.0015)), ev.clientX - r.left, ev.clientY - r.top)
  }, { passive: false })

  let drag = null
  let suppressClick = false
  canvas.addEventListener('pointerdown', (ev) => {
    if (ev.button !== 0) return
    drag = { x: ev.clientX, y: ev.clientY, vx: view.x, vy: view.y, moved: false }
  })
  window.addEventListener('pointermove', (ev) => {
    if (!drag) return
    const dx = ev.clientX - drag.x, dy = ev.clientY - drag.y
    if (!drag.moved && Math.hypot(dx, dy) < 4) return
    drag.moved = true
    canvas.classList.add('wm-dragging')
    const k = view.w / size().w
    view.x = drag.vx - dx * k
    view.y = drag.vy - dy * k
    fitted = false
    applyView()
  })
  window.addEventListener('pointerup', () => {
    if (drag && drag.moved) {
      suppressClick = true
      setTimeout(() => { suppressClick = false }, 0)
    }
    drag = null
    canvas.classList.remove('wm-dragging')
  })
  new ResizeObserver(() => {
    if (!view) return
    if (fitted) return fit()
    const { w, h } = size()
    const scale = w / view.w
    view.h = h / scale
    applyView()
  }).observe(canvas)

  $('wm-fit').addEventListener('click', fit)
  $('wm-zoom-in').addEventListener('click', () => zoomBy(1.25, size().w / 2, size().h / 2))
  $('wm-zoom-out').addEventListener('click', () => zoomBy(0.8, size().w / 2, size().h / 2))
  document.addEventListener('keydown', (ev) => {
    if (ev.target.closest('input, textarea, select') || ev.metaKey || ev.ctrlKey || ev.altKey) return
    if (ev.key === 'f') fit()
    else if (ev.key === 'Escape') select(null)
  })

  // Colour scheme: follow the system, or force one (handy for checking both).
  const themes = ['auto', 'light', 'dark']
  $('wm-theme').addEventListener('click', (ev) => {
    const next = themes[(themes.indexOf(document.documentElement.dataset.theme || 'auto') + 1) % themes.length]
    if (next === 'auto') delete document.documentElement.dataset.theme
    else document.documentElement.dataset.theme = next
    ev.currentTarget.textContent = `Theme: ${next}`
  })

  // ───────────── selection ─────────────
  const q = (s) => s.replace(/["\\]/g, '\\$&')
  function transitionEls(stateId, index, root = svg) {
    return root ? [...root.querySelectorAll(`[data-state="${q(stateId)}"][data-transition="${index}"], [data-state="${q(stateId)}"] .wm-row[data-transition="${index}"]`)] : []
  }
  function applySelection() {
    if (!svg) return
    svg.querySelectorAll('.wm-selected').forEach((el) => el.classList.remove('wm-selected'))
    if (!selection) return
    if (selection.type === 'state') {
      const el = svg.querySelector(`.wm-state[data-state="${q(selection.id)}"]`)
      if (el) el.classList.add('wm-selected')
    } else {
      transitionEls(selection.state, selection.index).forEach((el) => el.classList.add('wm-selected'))
    }
  }
  function select(sel, opts = {}) {
    selection = sel
    applySelection()
    renderPanel()
    if (sel && opts.reveal) {
      const l = layouts.get(current)
      if (sel.type === 'state') reveal(l.nodes[sel.id])
      else {
        const e = l.edges[`${sel.state}#${sel.index}`]
        reveal((e && e.label) || l.nodes[sel.state])
      }
    }
  }

  canvas.addEventListener('click', (ev) => {
    if (suppressClick) return
    const link = ev.target.closest('.wm-machine-link')
    if (link) return openMachine(link.dataset.machine, true)
    const tr = ev.target.closest('[data-transition]')
    if (tr) {
      const owner = tr.closest('[data-state]')
      return select({ type: 'transition', state: owner.dataset.state, index: Number(tr.dataset.transition) })
    }
    const st = ev.target.closest('[data-state]')
    select(st ? { type: 'state', id: st.dataset.state } : null)
  })
  // Hovering an arrow lights up its label and the other way round.
  const hover = (ev, on) => {
    const el = ev.target.closest && ev.target.closest('.wm-edge[data-transition], .wm-label[data-transition]')
    if (el) transitionEls(el.dataset.state, el.dataset.transition).forEach((e) => e.classList.toggle('wm-hover', on))
  }
  canvas.addEventListener('pointerover', (ev) => hover(ev, true))
  canvas.addEventListener('pointerout', (ev) => hover(ev, false))

  $('wm-tabs').addEventListener('click', (ev) => {
    const b = ev.target.closest('[data-machine]')
    if (b && b.dataset.machine !== current) openMachine(b.dataset.machine, false)
  })
  $('wm-back').addEventListener('click', () => {
    const prev = navStack.pop()
    if (prev) show(prev.machine, prev.selection)
  })
  panel.addEventListener('click', (ev) => {
    const b = ev.target.closest('button')
    if (!b) return
    if (b.dataset.openMachine) openMachine(b.dataset.openMachine, true)
    else if (b.dataset.selectState) select({ type: 'state', id: b.dataset.selectState }, { reveal: true })
    else if (b.dataset.selectTransition) {
      const [state, index] = JSON.parse(b.dataset.selectTransition)
      select({ type: 'transition', state, index }, { reveal: true })
    } else if (b.dataset.clear !== undefined) select(null)
  })

  // ───────────── side panel ─────────────
  const code = (s) => `<pre class="wm-code">${esc(s)}</pre>`
  const section = (title, body) => (body ? `<h3>${esc(title)}</h3>${body}` : '')
  const list = (items) => (items.length ? `<ul class="wm-list">${items.map((i) => `<li>${i}</li>`).join('')}</ul>` : '')
  const stateLink = (id, label) => `<button class="wm-link wm-mono" type="button" data-select-state="${esc(id)}">${esc(label ?? shortId(id))}</button>`
  const shortId = (id) => id.slice(current.length + 1) || id
  const docHtml = (doc) => (doc ? `<p class="wm-doc">${esc(doc).replace(/\n/g, '<br>')}</p>` : '')
  const head = (title, chip, onClose) =>
    `<div class="wm-panel-head"><h2>${title}</h2><span class="wm-chip">${esc(chip)}</span>`
    + (onClose ? `<button class="wm-btn" type="button" data-clear title="Back to the machine overview (Esc)" aria-label="Close">×</button>` : '') + `</div>`

  function typeDecl(name) {
    const t = ir.types[name]
    if (!t) return null
    const fields = (fs) => (fs.length ? ` { ${fs.map((f) => `${f.name}: ${f.type}`).join(', ')} }` : '')
    switch (t.kind) {
      case 'record': return `type ${name}${fields(t.fields)}`
      case 'error': return `error ${name}${fields(t.fields)}`
      case 'enum': return `enum ${name} { ${t.variants.join(', ')} }`
      case 'alias': return `type ${name} = ${t.target}`
      default: return `extern type ${name}`
    }
  }
  // Declarations of the user types a signature mentions, one per line.
  function typesOf(decl) {
    const names = [...decl.params.map((p) => p.type), decl.returns, ...(decl.errors ?? [])]
      .filter(Boolean).map((t) => t.replace(/(\[\]|\?)+$/, ''))
    const lines = [...new Set(names)].map(typeDecl).filter(Boolean)
    return lines.length ? code(lines.join('\n')) : ''
  }
  const signatureKv = (decl) => `<dl class="wm-kv">`
    + `<dt>takes</dt><dd>${esc(paramsText(decl.params) || 'nothing')}</dd>`
    + (decl.returns !== undefined ? `<dt>returns</dt><dd>${esc(decl.returns ?? 'nothing')}</dd>` : '')
    + (decl.errors ? `<dt>fails with</dt><dd>${esc(decl.errors.join(' | ') || 'never fails')}</dd>` : '')
    + `</dl>`

  function transitionSummary(state, index, withSource) {
    const t = state.transitions[index]
    const g = guardText(state, index)
    const kind = triggerKind(t.trigger)
    const cls = kind === 'done' ? 'wm-t-done' : kind === 'fail' ? 'wm-t-fail' : ''
    const target = t.target ? shortId(t.target) : 'stays (internal)'
    return `<button class="wm-tlink" type="button" data-select-transition="${esc(JSON.stringify([state.id, index]))}">`
      + (withSource ? `<span class="wm-arrow-text">${esc(shortId(state.id))}: </span>` : '')
      + `<b class="${cls}">${esc(triggerText(t.trigger))}</b>${g ? ' ' + esc(g) : ''} <span class="wm-arrow-text">→</span> ${esc(target)}`
      + t.effects.map((e) => `<span class="wm-sub">${esc(effectText(e))}</span>`).join('')
      + `</button>`
  }

  function invokeSection(state) {
    const inv = state.invoke
    if (!inv) return ''
    const decl = inv.kind === 'machine' ? ir.machines[inv.src] : ir.tasks[inv.src]
    let s = code(`invoke ${invokeText(inv)}`)
    if (!decl) return section('Invoke', s)
    s += `<p class="wm-path">${inv.kind} declared at ${esc(sourceText(ir, decl.span))}</p>`
    s += code(signatureText(inv.kind, inv.src, decl)) + docHtml(decl.doc) + signatureKv(decl)
    if (inv.kind === 'machine') s += `<p><button class="wm-btn" type="button" data-open-machine="${esc(inv.src)}">Open machine ${esc(inv.src)} →</button></p>`
    return section(`Invokes ${inv.kind}`, s) + section('Types involved', typesOf(decl))
  }

  function machinePanel() {
    const m = ir.machines[current]
    const tree = (s) => `<li>${stateLink(s.id, s.name)} <span class="wm-chip">${s.kind}</span>${s.states.length ? `<ul>${s.states.map(tree).join('')}</ul>` : ''}</li>`
    const callers = []
    for (const [name, other] of Object.entries(ir.machines)) {
      walkStates(other.root, (s) => {
        if (s.invoke && s.invoke.kind === 'machine' && s.invoke.src === current) {
          callers.push(`<button class="wm-link wm-mono" type="button" data-open-machine="${esc(name)}">${esc(s.id)}</button>`)
        }
      })
    }
    return head(esc(current), 'machine', false)
      + `<p class="wm-path">${esc(sourceText(ir, m.span))}</p>`
      + docHtml(m.doc)
      + section('Signature', code(signatureText('machine', current, m)) + signatureKv(m))
      + section('Variables', list(m.vars.map((v) => `${esc(v.name)}: ${esc(v.type)}${v.init ? ` = ${esc(v.init.text)}` : ' <span class="wm-note">(assigned later)</span>'}`)))
      + section('Events', list(Object.entries(m.events).map(([name, e]) =>
        `${esc(name)}${e.fields.length ? ` { ${esc(paramsText(e.fields))} }` : ''}${e.doc ? `<span class="wm-sub wm-note">${esc(e.doc)}</span>` : ''}`)))
      + section('States', `<ul class="wm-tree">${m.root.states.map(tree).join('')}</ul>`)
      + section('Invoked by', list(callers))
      + section('Types involved', typesOf(m))
      + `<h3>Tip</h3><p class="wm-empty">Click a state or an arrow in the diagram for its details.</p>`
  }

  function statePanel(state) {
    const m = ir.machines[current]
    const parents = []
    let incoming = ''
    walkStates(m.root, (s) => {
      if (state.id !== s.id && state.id.startsWith(s.id + '.')) parents.push(s)
      s.transitions.forEach((t, i) => { if (t.target === state.id) incoming += transitionSummary(s, i, true) })
    })
    const parent = parents[parents.length - 1]
    const isInitial = parent && parent.initial === state.id
    const out = state.transitions.map((t, i) => transitionSummary(state, i, false)).join('')
    let inherited = ''
    for (const p of parents.slice().reverse()) {
      if (!p.transitions.length) continue
      inherited += `<p class="wm-empty">from ${stateLink(p.id, p.id === m.root.id ? p.name : undefined)}</p>`
        + p.transitions.map((t, i) => transitionSummary(p, i, false)).join('')
    }
    const result = state.output ? code(`output ${state.output.text}`) : state.fail ? code(`fail ${state.fail.text}`) : ''
    return head(esc(state.name), `${state.kind} state${isInitial ? ' · initial' : ''}`, true)
      + `<p class="wm-path">${esc(state.id)} · ${esc(sourceText(ir, state.span))}</p>`
      + docHtml(state.doc)
      + section(state.output ? 'Machine returns' : 'Machine fails with', result)
      + invokeSection(state)
      + section('Starts in', state.initial ? stateLink(state.initial) : '')
      + section('On entry', list(state.entry.map((e) => esc(effectText(e)))))
      + section('On exit', list(state.exit.map((e) => esc(effectText(e)))))
      + section('Transitions out', out || (state.kind === 'final' ? '<p class="wm-empty">None: this state ends the machine.</p>' : '<p class="wm-empty">None.</p>'))
      + section('Also applies here', inherited)
      + section('Transitions in', incoming || (isInitial ? '<p class="wm-empty">Entered first (initial state).</p>' : ''))
  }

  function transitionPanel(state, index) {
    const t = state.transitions[index]
    const m = ir.machines[current]
    const kind = triggerKind(t.trigger)
    const group = state.transitions.map((o, i) => [o, i]).filter(([o]) => o.group === t.group)
    const pos = group.findIndex(([, i]) => i === index) + 1
    const inv = state.invoke
    const decl = inv && (inv.kind === 'machine' ? ir.machines[inv.src] : ir.tasks[inv.src])
    let what = ''
    let extra = ''
    switch (t.trigger.kind) {
      case 'event': {
        const e = m.events[t.trigger.event]
        what = t.trigger.event === '*' ? 'Any event not handled otherwise arrives.' : `Event ${t.trigger.event} arrives.`
        if (e) extra = section('Event', code(`event ${t.trigger.event}${e.fields.length ? ` { ${paramsText(e.fields)} }` : ''}`) + docHtml(e.doc))
        break
      }
      case 'after': what = `The machine has been in ${shortId(state.id)} for ${t.trigger.delay.text}.`; break
      case 'always': what = t.guard ? 'Taken as soon as the guard holds; no event needed.' : 'Taken immediately on entering the state.'; break
      case 'done': what = `${shortId(state.id)} reached a final child state.`; break
      case 'invoke.done': what = `${inv ? inv.src : 'The invoke'} finished; its value is available as \`result\`.`; break
      case 'invoke.fail':
        what = `${inv ? inv.src : 'The invoke'} failed with ${t.trigger.error === '*' ? 'any error not listed' : t.trigger.error}; the error is available as \`error\`.`
        if (typeDecl(t.trigger.error)) extra = section('Error', code(typeDecl(t.trigger.error)))
        break
    }
    if (decl && t.trigger.kind.startsWith('invoke.')) {
      extra += section(`Invoked ${inv.kind}`, code(signatureText(inv.kind, inv.src, decl)) + docHtml(decl.doc))
    }
    const guard = t.guard ? code(t.guard.text) : group.length > 1 ? code('else') : ''
    const branchNote = group.length > 1
      ? `<p class="wm-empty">Branch ${pos} of ${group.length} for <code>${esc(triggerText(t.trigger))}</code>; branches are tried in order.</p>` : ''
    const title = `${esc(shortId(state.id))} <span class="wm-arrow-text">→</span> ${t.target ? esc(shortId(t.target)) : '(stays)'}`
    return head(title, 'transition', true)
      + `<p class="wm-path">${esc(sourceText(ir, t.span))}</p>`
      + section('Trigger', code(`${kind === 'event' ? 'on ' : ''}${triggerText(t.trigger)}`) + `<p class="wm-empty">${esc(what)}</p>`)
      + section('Guard', guard + branchNote)
      + section('Effects', list(t.effects.map((e) => esc(effectText(e)))) || '<p class="wm-empty">None.</p>')
      + section('From', stateLink(state.id))
      + section('To', t.target ? stateLink(t.target) : '<p class="wm-empty">Internal: the state is not left and re-entered.</p>')
      + extra
  }

  function renderPanel() {
    const m = ir.machines[current]
    const state = selection && findState(m, selection.type === 'state' ? selection.id : selection.state)
    if (!state) panel.innerHTML = machinePanel()
    else if (selection.type === 'state') panel.innerHTML = statePanel(state)
    else panel.innerHTML = transitionPanel(state, selection.index)
    panel.scrollTop = 0
  }

  // ───────────── start ─────────────
  $('wm-source').textContent = ir.sources[0] ? ir.sources[0].path : ''
  $('wm-legend').insertAdjacentHTML('beforeend', renderLegend({ interactive: true }))
  const wanted = decodeURIComponent(location.hash.slice(1)) || document.body.dataset.initialMachine
  show(ir.machines[wanted] ? wanted : machineNames[0])
  window.addEventListener('hashchange', () => {
    const name = decodeURIComponent(location.hash.slice(1))
    if (ir.machines[name] && name !== current) openMachine(name, false)
  })
}

if (typeof document !== 'undefined') startViewer()
