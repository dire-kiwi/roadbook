// Simulate mode's panel: start the machine drawn with an input, then choose what happens next
// (an event, an invoke's outcome, a timer, the clock, a cancel), answer the functions the core
// calls, and step back and forward through the choices. The model is src/sim.ts; this file
// only draws it and wires up the forms.

import type { Json, Tagged } from '@dire-kiwi/roadbook-core'
import type { Field } from '../ir.ts'
import { own } from '../ir.ts'
import { findState, triggerText } from '../model.ts'
import { stateName } from '../replay.ts'
import { esc } from '../render.ts'
import { Simulation, refusal } from '../sim.ts'
import type { Outcome } from '../sim.ts'
import { checkFields, checkTagged, checkValue, parseJson, sampleFields, sampleTagged, sampleValue } from '../values.ts'
import { activeHtml, pretty, recordsHtml, scrollWithin, variablesHtml } from './modes.ts'
import type { Controller, Highlight, Host } from './modes.ts'

interface Form {
  /** What is wrong with the value typed. */
  check: (v: Json) => string[]
  submit: (v: Json) => Outcome
  initial: string
}

export function simulateMode(host: Host, el: HTMLElement): Controller {
  const { ir } = host
  const sims = new Map<string, Simulation>()
  /** The text typed in each form, by form key, kept across redraws. */
  const typed = new Map<string, string>()
  let forms = new Map<string, Form>()
  let buttons = new Map<string, () => Outcome | void>()
  let message = ''

  const sim = () => {
    const name = host.current()
    let s = sims.get(name)
    if (!s) {
      s = new Simulation(ir, name)
      sims.set(name, s)
    }
    return s
  }

  /** A JSON form: a textarea prefilled with `initial`, checked as it is typed, and a button. */
  function form(key: string, label: string, initial: Json, check: Form['check'], submit: Form['submit'], cls = ''): string {
    const text = pretty(initial)
    forms.set(key, { check, submit, initial: text })
    const value = typed.get(key) ?? text
    const rows = Math.min(12, value.split('\n').length)
    return (
      `<div class="rb-form ${cls}" data-form="${esc(key)}">` +
      `<textarea class="rb-json" rows="${rows}" spellcheck="false" aria-label="${esc(label)}: JSON" data-m-input="${esc(key)}">${esc(value)}</textarea>` +
      `<ul class="rb-form-errors" data-m-errors="${esc(key)}"></ul>` +
      `<button class="rb-btn rb-go" type="button" data-m-submit="${esc(key)}">${esc(label)}</button></div>`
    )
  }

  function button(key: string, label: string, act: () => Outcome | void, title = ''): string {
    buttons.set(key, act)
    return `<button class="rb-btn" type="button" data-m-do="${esc(key)}"${title ? ` title="${esc(title)}"` : ''}>${esc(label)}</button>`
  }

  /** A choice with fields: the form in a disclosure; without fields, a plain button. */
  function tagged(key: string, label: string, tag: string, fields: Field[], submit: (v: Tagged) => Outcome, note = ''): string {
    if (!fields.length) return `<div class="rb-choice">${button(key, label, () => submit({ _tag: tag }))}${note}</div>`
    return (
      `<details class="rb-choice"><summary>${esc(label)} …</summary>${note}` +
      form(key, label, sampleTagged(ir, tag, fields), (v) => checkTagged(ir, tag, fields, v), (v) => submit(v as Tagged)) +
      `</details>`
    )
  }

  function done(o: Outcome | void) {
    message = o && o.kind === 'error' ? o.message : ''
    render()
    host.refresh()
  }

  function render() {
    forms = new Map()
    buttons = new Map()
    const s = sim()
    const m = s.def
    const refused = refusal(ir, s.machine)
    let h = `<div class="rb-mode-head"><h2>Simulate <span class="rb-mono">${esc(s.machine)}</span></h2>${s.current ? `<span class="rb-chip rb-status-${esc(s.current.snapshot.status)}">${esc(s.current.snapshot.status)}</span>` : ''}</div>`
    h += `<p class="rb-mode-note">Runs <code>@dire-kiwi/roadbook-core</code> in this page, on a virtual clock. You are the host: you give the input, events, the outcome of each task and child machine, and the results of functions. Actions do nothing; <code>send</code> and <code>emit</code> show as records only.</p>`
    if (refused) {
      h += `<div class="rb-refusal" role="alert"><strong>Cannot simulate ${esc(s.machine)}.</strong> The core refused it: ${esc(refused)}. Explore and Replay still work for this chart.</div>`
      el.innerHTML = h
      return
    }
    if (message) h += `<p class="rb-alert" role="alert"><strong>Not done:</strong> ${esc(message)}</p>`

    if (s.asking) h += askHtml(s)

    if (!s.current) {
      h += `<h3>Input</h3><p class="rb-empty">The arguments of <code>${esc(s.machine)}</code>, by parameter name.</p>`
      h += form(`start:${s.machine}`, 'Start', sampleFields(ir, m.params), (v) => checkFields(ir, m.params, v, 'input'), (v) => s.start(v as { [k: string]: Json }), 'rb-start')
      el.innerHTML = h
      afterRender()
      return
    }

    const f = s.current
    const at = s.at
    h += `<div class="rb-steps">${button('back', '← Back', () => void s.back(), 'Step back (←)')}<span class="rb-step-text">step ${at + 1} of ${s.frames.length}: <strong>${esc(f.label)}</strong></span>${button('fwd', 'Forward →', () => void s.forward(), 'Step forward (→)')}${button(
      'restart',
      'New input',
      () => {
        sims.delete(s.machine)
        const next = new Simulation(ir, s.machine)
        for (const [k, a] of s.answers) next.answers.set(k, a)
        sims.set(s.machine, next)
      },
      'Start again with another input (the answers given are kept)',
    )}</div>`
    h += `<p class="rb-clock">clock <strong>t = ${f.now} ms</strong></p>`

    if (f.snapshot.status === 'running' && !s.asking) h += nextHtml(s, at)
    else if (f.snapshot.status !== 'running') h += `<h3>Result</h3><pre class="rb-code">${esc(f.snapshot.status)} ${esc(pretty(f.snapshot.result))}</pre>`

    h += `<h3>Active states</h3>${activeHtml(f.snapshot.active.filter((id) => id !== s.machine))}`
    if (f.taken.length) {
      h += `<h3>Transitions taken by this step</h3><ul class="rb-list">${f.taken
        .map((t) => {
          const st = findState(m, t.state)
          const tr = st?.transitions[t.index]
          return `<li><button class="rb-link rb-mono" type="button" data-m-transition="${esc(JSON.stringify([t.state, t.index]))}">${esc(stateName(t.state) || t.state)}: ${esc(tr ? triggerText(tr.trigger) : `#${t.index}`)}${tr?.target ? ` → ${esc(stateName(tr.target))}` : ''}</button></li>`
        })
        .join('')}</ul>`
    }
    h += `<h3>Variables</h3>${variablesHtml(m, f.snapshot.input, f.snapshot.vars, false)}`
    if (s.answers.size) {
      h += `<h3>Function results given</h3><ul class="rb-list">${[...s.answers.entries()]
        .map(([k, a]) => `<li>${esc(k)} → ${esc('value' in a.answer ? JSON.stringify(a.answer.value) : `defect "${a.answer.defect}"`)} ${button(`forget:${k}`, 'forget', () => s.forget(k))}</li>`)
        .join('')}</ul>`
    }
    const records = s.records()
    const fresh = records.length - f.records.length
    h += `<h3>Trace <span class="rb-h-note">(${records.length} records; this step's set apart)</span></h3>${recordsHtml(records, { mark: (i) => i >= fresh })}`
    el.innerHTML = h
    afterRender()
    const recent = el.querySelector<HTMLElement>('.rb-records li.rb-recent')
    if (recent) scrollWithin(recent)
  }

  function askHtml(s: Simulation): string {
    const ask = s.asking!
    const decl = ask.decl
    const sig = `${ask.fn}(${ask.args.map((a) => JSON.stringify(a)).join(', ')})`
    const returns = decl?.returns ?? 'json'
    let h = `<div class="rb-ask" role="alertdialog" aria-label="A function needs its result"><h3>The core calls a function</h3>`
    h += `<p><code>${esc(sig)}</code> → <code>${esc(returns)}</code>. What does it return? Your answer is kept for every call with these arguments.</p>`
    if (decl?.params.length) h += `<p class="rb-path">${esc(decl.params.map((p, i) => `${p.name} = ${JSON.stringify(ask.args[i])}`).join(', '))}</p>`
    h += form(`ask:${sig}`, 'Answer', sampleValue(ir, returns), (v) => checkValue(ir, returns, v, 'result'), (v) => {
      s.answer(ask.fn, ask.args, { value: v })
      return s.retry()
    })
    h += `<div class="rb-row-btns">${button('ask-defect', 'It throws (host defect)', () => {
      s.answer(ask.fn, ask.args, { defect: `${ask.fn} threw` })
      return s.retry()
    })}${button('ask-cancel', 'Give up this choice', () => s.abandon())}</div></div>`
    return h
  }

  function nextHtml(s: Simulation, at: number): string {
    const c = s.choices()
    const m = s.def
    let h = '<h3>What can happen next</h3>'
    const k = (x: string) => `${at}:${x}`
    if (c.events.length) {
      h += '<h4>Send an event</h4>'
      for (const e of c.events) {
        const note = e.handled ? '' : '<span class="rb-cnote">no <code>on</code> for it here: it would be dropped</span>'
        h += tagged(k(`event:${e.name}`), `send ${e.name}`, e.name, e.fields, (v) => s.send(v), note)
      }
    }
    for (const inv of c.invokes) {
      h += `<h4>invoke ${inv.id}: <code>${esc(inv.src)}</code> <span class="rb-h-note">in ${esc(stateName(inv.state))}</span></h4>`
      if (inv.srcKind === 'machine') {
        h += `<p class="rb-cnote rb-task-note">${esc(inv.src)} is a machine${own(ir.machines, inv.src) ? ' of this chart' : ''}. Here it is treated as a task: its run is not started, and you give its outcome by hand.${own(ir.machines, inv.src) ? ` To step through ${esc(inv.src)} itself, open its tab and simulate it there.` : ''}</p>`
      }
      const returns = inv.returns
      if (returns === null) h += `<div class="rb-choice">${button(k(`done:${inv.id}`), `${inv.src} done`, () => s.complete(inv.id, { done: null }))}</div>`
      else
        h +=
          `<details class="rb-choice"><summary>${esc(inv.src)} done …</summary>` +
          form(k(`done:${inv.id}`), `${inv.src} done`, sampleValue(ir, returns), (v) => checkValue(ir, returns, v, 'result'), (v) => s.complete(inv.id, { done: v })) +
          `</details>`
      for (const e of inv.errors) h += tagged(k(`fail:${inv.id}:${e.name}`), `${inv.src} fails ${e.name}`, e.name, e.fields, (v) => s.complete(inv.id, { fail: v }))
      h += `<div class="rb-choice">${button(k(`defect:${inv.id}`), `${inv.src} has a defect`, () => s.complete(inv.id, { defect: `${inv.src} failed` }), 'End it with a host defect: the run ends with a defect too')}</div>`
      for (const e of inv.emits) h += tagged(k(`emit:${inv.id}:${e.name}`), `${inv.as ?? inv.src} emits ${e.name}`, e.name, e.fields, (v) => s.emitFromChild(inv.id, v))
    }
    if (c.timers.length) {
      h += '<h4>Timers</h4>'
      for (const t of c.timers) {
        const st = findState(s.def, t.state)
        const tr = st?.transitions[t.index]
        h += `<div class="rb-choice">${button(k(`timer:${t.id}`), `fire timer ${t.id}`, () => s.fire(t.id))}<span class="rb-cnote"><code>${esc(tr ? triggerText(tr.trigger) : 'after')}</code> on ${esc(stateName(t.state) || t.state)}: ${t.delay} ms, due at t = ${t.due}</span></div>`
      }
    }
    h += '<h4>Clock and run</h4>'
    h += `<div class="rb-choice rb-advance"><label>advance <input class="rb-ms" type="number" min="0" step="1" value="${esc(typed.get('advance') ?? '1000')}" data-m-ms aria-label="milliseconds"> ms</label>${button(k('advance'), 'Advance', () => {
      const ms = Number(el.querySelector<HTMLInputElement>('[data-m-ms]')?.value)
      return s.advance(ms)
    }, 'Move the clock on, firing the timers due on the way')}</div>`
    h += `<div class="rb-choice">${button(k('cancel'), 'Cancel the run', () => s.cancel(), 'Cancel from outside: every state is exited')}</div>`
    if (!c.events.length && !c.invokes.length && !c.timers.length) h += `<p class="rb-empty">${esc(m.root.name)} waits for nothing: no event, invoke or timer can move it.</p>`
    return h
  }

  /** Checks each form as it is shown. */
  function afterRender() {
    for (const key of forms.keys()) validate(key)
  }

  function validate(key: string): Json | undefined {
    const f = forms.get(key)
    const ta = el.querySelector<HTMLTextAreaElement>(`[data-m-input="${CSS.escape(key)}"]`)
    const errs = el.querySelector<HTMLElement>(`[data-m-errors="${CSS.escape(key)}"]`)
    const btn = el.querySelector<HTMLButtonElement>(`[data-m-submit="${CSS.escape(key)}"]`)
    if (!f || !ta || !errs || !btn) return undefined
    const parsed = parseJson(ta.value)
    const problems = 'error' in parsed ? [parsed.error] : f.check(parsed.value)
    errs.innerHTML = problems.map((p) => `<li>${esc(p)}</li>`).join('')
    btn.disabled = problems.length > 0
    ta.setAttribute('aria-invalid', String(problems.length > 0))
    return problems.length || 'error' in parsed ? undefined : parsed.value
  }

  el.addEventListener('input', (ev) => {
    const t = ev.target as HTMLElement
    const key = t.dataset.mInput
    if (key !== undefined) {
      typed.set(key, (t as HTMLTextAreaElement).value)
      validate(key)
    } else if (t.dataset.mMs !== undefined) typed.set('advance', (t as HTMLInputElement).value)
  })
  el.addEventListener('click', (ev) => {
    const b = (ev.target as Element).closest<HTMLElement>('button')
    if (!b) return
    if (b.dataset.mSubmit !== undefined) {
      const key = b.dataset.mSubmit
      const v = validate(key)
      if (v === undefined) return
      typed.delete(key)
      done(forms.get(key)!.submit(v))
    } else if (b.dataset.mDo !== undefined) {
      done(buttons.get(b.dataset.mDo)?.())
    }
  })

  return {
    render,
    highlight(machine): Highlight | null {
      const s = sims.get(machine)
      const f = s?.current
      if (!f) return null
      return { active: [...f.snapshot.active], taken: f.taken, named: [] }
    },
    status() {
      const s = sims.get(host.current())
      const f = s?.current
      if (!s || !f) return 'Simulate: give the input and start'
      // The active leaves: one, or one per region of a parallel state.
      const active = f.snapshot.active
      const leaves = active.filter((id) => !active.some((other) => other.startsWith(`${id}.`)))
      const where = leaves.map((leaf) => stateName(leaf) || leaf).join(', ')
      return `Simulate · step ${s.at + 1}/${s.frames.length} · t = ${f.now} ms · ${f.snapshot.status}${where ? ` · in ${where}` : ''}`
    },
    forward() {
      if (sim().forward()) done()
    },
    back() {
      if (sim().back()) done()
    },
  }
}
