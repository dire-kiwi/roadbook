#!/usr/bin/env node
// Roadbook spike compiler: .road source -> IR JSON.
// Throwaway. Hand-written lexer + recursive-descent parser for the grammar in
// docs/design/language.md, import resolution, name/target resolution, a few checks.
//
//   node spike/compile.mjs examples/shop/place_order.road > out.json
//
// Diagnostics go to stderr as `file:line:col: error[CODE]: message`; exit code 1 on errors.

import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

export const IR_VERSION = '0.1'

// Diagnostic codes used by the spike (RB0204 is the one the design doc shows).
export const CODES = {
  RB0001: 'syntax error',
  RB0002: 'import cannot be resolved',
  RB0101: 'unknown type',
  RB0102: 'unknown task or machine',
  RB0103: 'unknown function',
  RB0104: 'unknown action',
  RB0105: 'unknown variable',
  RB0106: 'unknown event',
  RB0107: 'duplicate or reserved name',
  RB0108: 'assignment to something that is not a machine variable',
  RB0201: 'unknown transition target',
  RB0202: 'ambiguous transition target',
  RB0203: 'missing or invalid `initial`',
  RB0204: 'invoke does not handle a declared error',
  RB0205: 'same trigger written twice in one state',
  RB0206: 'branch shadowed by an earlier unguarded branch',
  RB0207: 'invoke outcome names an error that is not declared',
  RB0208: 'item not allowed here',
}

const KEYWORDS = new Set([
  'import', 'type', 'enum', 'error', 'extern', 'fn', 'action', 'task', 'machine',
  'var', 'event', 'initial', 'state', 'final', 'parallel', 'entry', 'exit', 'on', 'after',
  'always', 'done', 'invoke', 'output', 'fail', 'if', 'else', 'set', 'do',
  'and', 'or', 'not', 'true', 'false',
])
const BUILTIN_TYPES = new Set(['string', 'int', 'float', 'bool', 'duration', 'json'])
const DURATION_MS = { ms: 1, s: 1000, m: 60_000, h: 3_600_000 }
const PUNCT = ['->', '==', '!=', '<=', '>=', '{', '}', '(', ')', '[', ']', ',', ':', '.', '=', '<', '>', '+', '-', '*', '!', '|', '?']

class SyntaxError_ extends Error {
  constructor(message, line, col) { super(message); this.line = line; this.col = col }
}

// ───────────────────────────── lexer ─────────────────────────────
// Token: { t: 'upper'|'lower'|'kw'|'int'|'float'|'duration'|'string'|'punct'|'eof', v, text, line, col, doc }
// `doc` holds the `///` lines that immediately precede the token.
function lex(src) {
  const toks = []
  let i = 0, line = 1, col = 1
  let doc = []
  const isIdent = (c) => /[A-Za-z0-9]/.test(c)
  const isDigit = (c) => c >= '0' && c <= '9'
  const push = (t, v, text, l, c) => {
    toks.push({ t, v, text, line: l, col: c, doc: doc.length ? doc.join('\n') : '' })
    doc = []
  }
  while (i < src.length) {
    const c = src[i]
    if (c === '\n') { i++; line++; col = 1; continue }
    if (c === ' ' || c === '\t' || c === '\r') { i++; col++; continue }
    if (c === '/' && src[i + 1] === '/') {
      let j = i
      while (j < src.length && src[j] !== '\n') j++
      const text = src.slice(i, j)
      if (text.startsWith('///') && !text.startsWith('////')) doc.push(text.slice(3).replace(/^ /, '').trimEnd())
      col += j - i; i = j
      continue
    }
    const l0 = line, c0 = col
    if (isDigit(c)) {
      let j = i
      while (isDigit(src[j] ?? '')) j++
      const unit = /^(ms|s|m|h)(?![A-Za-z0-9])/.exec(src.slice(j, j + 3))
      if (unit) {
        const text = src.slice(i, j + unit[1].length)
        push('duration', Number(src.slice(i, j)) * DURATION_MS[unit[1]], text, l0, c0)
        j += unit[1].length
      } else if (src[j] === '.' && isDigit(src[j + 1] ?? '')) {
        j++
        while (isDigit(src[j] ?? '')) j++
        push('float', Number(src.slice(i, j)), src.slice(i, j), l0, c0)
      } else {
        const text = src.slice(i, j)
        const n = Number(text)
        // ints are 64-bit in the language; JSON numbers are not. Keep big ones as strings.
        push('int', Number.isSafeInteger(n) ? n : text, text, l0, c0)
      }
      if (isIdent(src[j] ?? '')) throw new SyntaxError_(`malformed number or duration \`${src.slice(i, j + 1)}…\``, l0, c0)
      col += j - i; i = j
      continue
    }
    if (/[A-Za-z]/.test(c)) {
      let j = i
      while (j < src.length && isIdent(src[j])) j++
      const text = src.slice(i, j)
      const t = /[A-Z]/.test(c) ? 'upper' : KEYWORDS.has(text) ? 'kw' : 'lower'
      push(t, text, text, l0, c0)
      col += j - i; i = j
      continue
    }
    if (c === '"') {
      let j = i + 1, v = ''
      for (;;) {
        const d = src[j]
        if (d === undefined || d === '\n') throw new SyntaxError_('unterminated string', l0, c0)
        if (d === '"') { j++; break }
        if (d === '\\') {
          const e = src[j + 1]
          const map = { n: '\n', t: '\t', '"': '"', '\\': '\\' }
          if (!(e in map)) throw new SyntaxError_(`unknown escape \`\\${e}\` in string`, line, col + (j - i))
          v += map[e]; j += 2
        } else { v += d; j++ }
      }
      push('string', v, src.slice(i, j), l0, c0)
      col += j - i; i = j
      continue
    }
    const p = PUNCT.find((p) => src.startsWith(p, i))
    if (!p) throw new SyntaxError_(`unexpected character \`${c}\``, l0, c0)
    push('punct', p, p, l0, c0)
    i += p.length; col += p.length
  }
  push('eof', null, '<end of file>', line, col)
  return toks
}

// ───────────────────────────── expressions ─────────────────────────────
// Precedence levels, used both for parsing and for printing canonical text.
const PREC = { or: 1, and: 2, not: 3, cmp: 4, add: 5, mul: 6, neg: 7, atom: 8 }
const BIN_PREC = { or: 1, and: 2, '==': 4, '!=': 4, '<': 4, '<=': 4, '>': 4, '>=': 4, '+': 5, '-': 5, '*': 6 }

function precOf(e) {
  if (e.kind !== 'op') return PREC.atom
  if (e.args.length === 1) return e.op === 'not' ? PREC.not : PREC.neg
  return BIN_PREC[e.op]
}
function wrap(e, min) { return precOf(e) < min ? `(${e.text})` : e.text }

function mkBin(op, a, b, span) {
  const p = BIN_PREC[op]
  // cmp is non-associative, the rest are left-associative.
  const text = `${wrap(a, p === PREC.cmp ? p + 1 : p)} ${op} ${wrap(b, p + 1)}`
  return { kind: 'op', op, args: [a, b], text, span }
}
function mkUnary(op, a, span) {
  const text = op === 'not' ? `not ${wrap(a, PREC.cmp)}` : `-${wrap(a, PREC.atom)}`
  return { kind: 'op', op, args: [a], text, span }
}

// ───────────────────────────── parser ─────────────────────────────
class Parser {
  constructor(toks, fileId) { this.toks = toks; this.p = 0; this.fileId = fileId }
  get tok() { return this.toks[this.p] }
  peek(n = 1) { return this.toks[Math.min(this.p + n, this.toks.length - 1)] }
  span(t = this.tok) { return { file: this.fileId, line: t.line, col: t.col } }
  fail(msg, t = this.tok) { throw new SyntaxError_(msg, t.line, t.col) }
  is(t, v) { const k = this.tok; return k.t === t && (v === undefined || k.v === v) }
  isP(v) { return this.is('punct', v) }
  isKw(v) { return this.is('kw', v) }
  next() { return this.toks[this.p++] }
  eat(t, v) { if (this.is(t, v)) return this.next(); return null }
  expectP(v) { return this.eat('punct', v) ?? this.fail(`expected \`${v}\`, found \`${this.tok.text}\``) }
  expectKw(v) { return this.eat('kw', v) ?? this.fail(`expected \`${v}\`, found \`${this.tok.text}\``) }
  upper(what) {
    if (this.is('upper')) return this.next()
    this.fail(`expected ${what} (a name starting with an upper-case letter), found \`${this.tok.text}\``)
  }
  lower(what) {
    if (this.is('lower')) return this.next()
    if (this.is('kw')) this.fail(`\`${this.tok.text}\` is a keyword and cannot be used as ${what}`)
    this.fail(`expected ${what} (a name starting with a lower-case letter), found \`${this.tok.text}\``)
  }

  // file = { import | decl }
  file() {
    const imports = [], decls = []
    while (!this.is('eof')) {
      if (this.isKw('import')) imports.push(this.importDecl())
      else decls.push(this.decl())
    }
    return { imports, decls }
  }

  // import = "import" "{" name { "," name } [ "," ] "}" "from" string
  importDecl() {
    const span = this.span()
    this.expectKw('import')
    this.expectP('{')
    const names = []
    for (;;) {
      const t = this.tok
      if (!this.is('upper') && !this.is('lower')) this.fail(`expected a name to import, found \`${t.text}\``)
      this.next()
      names.push({ name: t.v, span: this.span(t) })
      if (!this.eat('punct', ',')) break
      if (this.isP('}')) break
    }
    this.expectP('}')
    if (!this.is('lower', 'from')) this.fail(`expected \`from\`, found \`${this.tok.text}\``)
    this.next()
    if (!this.is('string')) this.fail(`expected the path of the file to import as a string, found \`${this.tok.text}\``)
    const from = this.next()
    return { names, path: from.v, span, pathSpan: this.span(from) }
  }

  decl() {
    const t = this.tok
    const doc = t.doc, span = this.span()
    if (t.t !== 'kw') this.fail(`expected a declaration (type, enum, error, extern, fn, action, task, machine), found \`${t.text}\``)
    switch (t.v) {
      case 'extern': {
        this.next(); this.expectKw('type')
        const name = this.upper('a type name')
        return { decl: 'type', name: name.v, kind: 'extern', doc, span }
      }
      case 'type': {
        this.next()
        const name = this.upper('a type name')
        if (this.eat('punct', '=')) return { decl: 'type', name: name.v, kind: 'alias', target: this.type(), doc, span }
        if (!this.isP('{')) this.fail(`expected \`=\` or \`{\` after \`type ${name.v}\`, found \`${this.tok.text}\``)
        return { decl: 'type', name: name.v, kind: 'record', fields: this.fields(), doc, span }
      }
      case 'enum': {
        this.next()
        const name = this.upper('an enum name')
        this.expectP('{')
        const variants = [this.upper('an enum variant').v]
        while (this.eat('punct', ',')) {
          if (this.isP('}')) break
          variants.push(this.upper('an enum variant').v)
        }
        this.expectP('}')
        return { decl: 'type', name: name.v, kind: 'enum', variants, doc, span }
      }
      case 'error': {
        this.next()
        const name = this.upper('an error name')
        const fields = this.isP('{') ? this.fields() : []
        return { decl: 'type', name: name.v, kind: 'error', fields, doc, span }
      }
      case 'fn': {
        this.next()
        const name = this.lower('a function name')
        const params = this.params()
        this.expectP('->')
        return { decl: 'fn', name: name.v, params, returns: this.type(), doc, span }
      }
      case 'action': {
        this.next()
        const name = this.lower('an action name')
        return { decl: 'action', name: name.v, params: this.params(), doc, span }
      }
      case 'task': {
        this.next()
        const name = this.lower('a task name')
        const params = this.params()
        this.expectP('->')
        const returns = this.type()
        return { decl: 'task', name: name.v, params, returns, errors: this.errors(), doc, span }
      }
      case 'machine': return this.machine()
      default:
        this.fail(`expected a declaration (type, enum, error, extern, fn, action, task, machine), found \`${t.text}\``)
    }
  }

  // fields = "{" [ field { "," field } [ "," ] ] "}"
  fields() {
    this.expectP('{')
    const out = []
    while (!this.isP('}')) {
      out.push(this.field())
      if (!this.eat('punct', ',')) break
    }
    this.expectP('}')
    return out
  }
  field() {
    const name = this.lower('a field name')
    this.expectP(':')
    return { name: name.v, type: this.type(), span: this.span(name) }
  }
  // params = "(" [ field { "," field } ] ")"
  params() {
    this.expectP('(')
    const out = []
    if (!this.isP(')')) {
      out.push(this.field())
      while (this.eat('punct', ',')) out.push(this.field())
    }
    this.expectP(')')
    return out
  }
  // type = ( Upper | builtin ) { "[]" | "?" }      -> { text, base, span }
  type() {
    const t = this.tok
    if (t.t !== 'upper' && !(t.t === 'lower' && BUILTIN_TYPES.has(t.v))) {
      this.fail(`expected a type, found \`${t.text}\``)
    }
    this.next()
    let text = t.v
    for (;;) {
      if (this.isP('[')) { this.next(); this.expectP(']'); text += '[]' }
      else if (this.eat('punct', '?')) text += '?'
      else break
    }
    return { text, base: t.v, span: this.span(t) }
  }
  // errors = "!" Upper { "|" Upper }
  errors() {
    const out = []
    if (!this.eat('punct', '!')) return out
    do {
      const t = this.upper('an error name')
      out.push({ name: t.v, span: this.span(t) })
    } while (this.eat('punct', '|'))
    return out
  }

  // machineDecl = "machine" Upper params [ "->" type ] [ errors ] "{" { machineItem } "}"
  machine() {
    const doc = this.tok.doc, span = this.span()
    this.expectKw('machine')
    const name = this.upper('a machine name')
    const params = this.params()
    const returns = this.eat('punct', '->') ? this.type() : null
    const errors = this.errors()
    const root = this.newState(name.v, 'state', doc, span)
    const vars = [], events = []
    this.expectP('{')
    while (!this.isP('}')) {
      if (this.is('eof')) this.fail(`machine \`${name.v}\` is not closed: expected \`}\``)
      const t = this.tok
      if (this.isKw('var')) {
        this.next()
        const n = this.lower('a variable name')
        this.expectP(':')
        const type = this.type()
        const init = this.eat('punct', '=') ? this.expr() : null
        vars.push({ name: n.v, type, init, doc: t.doc, span: this.span(n) })
      } else if (this.isKw('event')) {
        this.next()
        const n = this.upper('an event name')
        const fields = this.isP('{') ? this.fields() : []
        events.push({ name: n.v, fields, doc: t.doc, span: this.span(n) })
      } else {
        this.stateItem(root)
      }
    }
    this.expectP('}')
    return { decl: 'machine', name: name.v, params, returns, errors, vars, events, root, doc, span }
  }

  newState(name, modifier, doc, span) {
    return {
      name, modifier, doc, span,
      initial: null, states: [], entry: [], exit: [], invokes: [], blocks: [],
      output: null, fail: null,
    }
  }

  // stateDecl = [ "final" | "parallel" ] "state" Upper "{" { stateItem } "}"
  stateDecl() {
    const doc = this.tok.doc, span = this.span()
    const modifier = this.eat('kw', 'final')?.v ?? this.eat('kw', 'parallel')?.v ?? 'state'
    this.expectKw('state')
    const name = this.upper('a state name')
    const st = this.newState(name.v, modifier, doc, span)
    st.nameSpan = this.span(name)
    this.expectP('{')
    while (!this.isP('}')) {
      if (this.is('eof')) this.fail(`state \`${name.v}\` is not closed: expected \`}\``)
      this.stateItem(st)
    }
    this.expectP('}')
    return st
  }

  stateItem(st) {
    const t = this.tok
    const span = this.span()
    if (t.t !== 'kw') this.fail(`expected a state item (state, initial, entry, exit, on, after, always, done, invoke, output, fail), found \`${t.text}\``)
    switch (t.v) {
      case 'initial': {
        this.next()
        const n = this.upper('the name of the initial state')
        st.initials = st.initials ?? []
        st.initials.push({ name: n.v, span: this.span(n) })
        return
      }
      case 'final': case 'parallel': case 'state':
        st.states.push(this.stateDecl()); return
      case 'entry': this.next(); st.entry.push(...this.effects(true)); return
      case 'exit': this.next(); st.exit.push(...this.effects(true)); return
      case 'on': {
        this.next()
        let event
        if (this.eat('punct', '*')) event = '*'
        else event = this.upper('an event name or `*`').v
        const eventSpan = this.span(this.toks[this.p - 1])
        st.blocks.push({ trigger: { kind: 'event', event }, key: `on ${event}`, eventSpan, span, branches: this.branches() })
        return
      }
      case 'after': {
        this.next()
        // Like Rust's `if`: no bare record literal here, or `after X { … }` would be ambiguous.
        const delay = this.expr({ noRecord: true })
        st.blocks.push({ trigger: { kind: 'after', delay }, key: `after ${delay.text}`, span, branches: this.branches() })
        return
      }
      case 'always':
        this.next()
        st.blocks.push({ trigger: { kind: 'always' }, key: 'always', span, branches: this.branches() })
        return
      case 'done':
        this.next()
        st.blocks.push({ trigger: { kind: 'done' }, key: 'done', span, branches: this.branches() })
        return
      case 'invoke': {
        this.next()
        const n = this.tok
        if (!this.is('upper') && !this.is('lower')) this.fail(`expected the task or machine to invoke, found \`${n.text}\``)
        this.next()
        const args = this.args()
        const inv = { kind: n.t === 'upper' ? 'machine' : 'task', src: n.v, args, span, nameSpan: this.span(n), outcomes: [] }
        this.expectP('{')
        while (!this.isP('}')) {
          const o = this.tok, ospan = this.span()
          if (this.eat('kw', 'done')) {
            const b = { trigger: { kind: 'invoke.done' }, key: 'done', span: ospan, branches: this.branches() }
            inv.outcomes.push(b); st.blocks.push(b)
          } else if (this.eat('kw', 'fail')) {
            let error
            if (this.eat('punct', '*')) error = '*'
            else error = this.upper('an error name or `*`').v
            const errorSpan = this.span(this.toks[this.p - 1])
            const b = { trigger: { kind: 'invoke.fail', error }, key: `fail ${error}`, errorSpan, span: ospan, branches: this.branches() }
            inv.outcomes.push(b); st.blocks.push(b)
          } else {
            this.fail(`expected \`done\`, \`fail\` or \`}\` inside \`invoke\`, found \`${o.text}\``)
          }
        }
        this.expectP('}')
        st.invokes.push(inv)
        return
      }
      case 'output': {
        this.next()
        if (st.output || st.fail) this.fail('a final state has one `output` or `fail`', t)
        st.output = this.expr(); st.resultSpan = span
        return
      }
      case 'fail': {
        this.next()
        if (st.output || st.fail) this.fail('a final state has one `output` or `fail`', t)
        st.fail = this.expr(); st.resultSpan = span
        return
      }
      default:
        this.fail(`expected a state item (state, initial, entry, exit, on, after, always, done, invoke, output, fail), found \`${t.text}\``)
    }
  }

  // branches = branch | "{" { "if" expr branch } [ "else" branch ] "}"
  // The block form is parsed leniently (any order of `if` / `else` branches) so that a branch
  // after `else` gets the "shadowed" diagnostic instead of a bare syntax error.
  branches() {
    if (this.eat('punct', '{')) {
      const out = []
      while (!this.isP('}')) {
        const t = this.tok
        if (this.isKw('if')) out.push(this.branch())
        else if (this.eat('kw', 'else')) out.push({ ...this.branchRest(t), isElse: true })
        else this.fail(`expected \`if\`, \`else\` or \`}\` in a branch block, found \`${t.text}\``)
      }
      this.expectP('}')
      if (!out.length) this.fail('a branch block needs at least one branch', this.toks[this.p - 1])
      return out
    }
    return [{ ...this.branch(), single: true }]
  }
  // branch = [ "if" expr ] [ "->" target ] [ effects ]
  branch() {
    const t = this.tok
    const guard = this.eat('kw', 'if') ? this.expr() : null
    return { ...this.branchRest(t), guard }
  }
  branchRest(first) {
    const b = { guard: null, target: null, effects: [], span: this.span(first) }
    if (this.eat('punct', '->')) {
      const segs = [this.upper('a target state')]
      while (this.eat('punct', '.')) segs.push(this.upper('a state name'))
      b.target = { path: segs.map((s) => s.v), span: this.span(segs[0]) }
    }
    if (this.isKw('set') || this.isKw('do')) b.effects = this.effects(false)
    if (!b.target && !b.effects.length) {
      this.fail(`a transition must do something: expected \`-> Target\`, \`set\` or \`do\`, found \`${this.tok.text}\``)
    }
    return b
  }

  // effects = effect { effect } ; effect = "set" assign { "," assign } | "do" call { "," call }
  // In a transition, `set` assignments run before `do` actions, so they are emitted in that order.
  effects(keepOrder) {
    const out = []
    if (!this.isKw('set') && !this.isKw('do')) this.fail(`expected \`set\` or \`do\`, found \`${this.tok.text}\``)
    for (;;) {
      if (this.eat('kw', 'set')) {
        do {
          const n = this.lower('a variable name')
          this.expectP('=')
          out.push({ kind: 'set', var: n.v, value: this.expr(), span: this.span(n) })
        } while (this.eat('punct', ','))
      } else if (this.eat('kw', 'do')) {
        do {
          const n = this.lower('an action name')
          if (!this.isP('(')) this.fail(`expected \`(\` after action name \`${n.v}\`, found \`${this.tok.text}\``)
          out.push({ kind: 'do', action: n.v, args: this.args(), span: this.span(n) })
        } while (this.eat('punct', ','))
      } else break
    }
    if (keepOrder) return out
    return [...out.filter((e) => e.kind === 'set'), ...out.filter((e) => e.kind === 'do')]
  }

  args() {
    this.expectP('(')
    const out = []
    if (!this.isP(')')) {
      out.push(this.expr())
      while (this.eat('punct', ',')) out.push(this.expr())
    }
    this.expectP(')')
    return out
  }

  // expr = orExpr ; see the grammar. `opts.noRecord` forbids a bare `Upper { … }` at this level.
  expr(opts = {}) { return this.orExpr(opts) }
  orExpr(o) {
    let a = this.andExpr(o)
    while (this.isKw('or')) { const t = this.next(); a = mkBin('or', a, this.andExpr(o), this.span(t)) }
    return a
  }
  andExpr(o) {
    let a = this.notExpr(o)
    while (this.isKw('and')) { const t = this.next(); a = mkBin('and', a, this.notExpr(o), this.span(t)) }
    return a
  }
  notExpr(o) {
    const t = this.eat('kw', 'not')
    const a = this.cmpExpr(o)
    return t ? mkUnary('not', a, this.span(t)) : a
  }
  cmpExpr(o) {
    const a = this.addExpr(o)
    const t = this.tok
    if (t.t === 'punct' && ['==', '!=', '<', '<=', '>', '>='].includes(t.v)) {
      this.next()
      return mkBin(t.v, a, this.addExpr(o), this.span(t))
    }
    return a
  }
  addExpr(o) {
    let a = this.mulExpr(o)
    while (this.isP('+') || this.isP('-')) { const t = this.next(); a = mkBin(t.v, a, this.mulExpr(o), this.span(t)) }
    return a
  }
  mulExpr(o) {
    let a = this.unary(o)
    while (this.isP('*')) { const t = this.next(); a = mkBin('*', a, this.unary(o), this.span(t)) }
    return a
  }
  unary(o) {
    const t = this.eat('punct', '-')
    const a = this.postfix(o)
    return t ? mkUnary('-', a, this.span(t)) : a
  }
  postfix(o) {
    let a = this.primary(o)
    while (this.isP('.')) {
      this.next()
      const f = this.lower('a field name')
      if (a.kind === 'ref') a = { kind: 'ref', path: [...a.path, f.v], text: `${a.text}.${f.v}`, span: a.span }
      // IR extension: field access on something that is not a variable path.
      else a = { kind: 'field', of: a, name: f.v, text: `${wrap(a, PREC.atom)}.${f.v}`, span: a.span }
    }
    return a
  }
  primary(o) {
    const t = this.tok, span = this.span()
    switch (t.t) {
      case 'int': case 'float': case 'duration': case 'string':
        this.next()
        return { kind: 'lit', type: t.t, value: t.v, text: t.t === 'string' ? JSON.stringify(t.v) : t.text, span }
      case 'kw':
        if (t.v === 'true' || t.v === 'false') {
          this.next()
          return { kind: 'lit', type: 'bool', value: t.v === 'true', text: t.v, span }
        }
        // `event` and `error` are declaration keywords and also the implicit names in transitions.
        if (t.v === 'event' || t.v === 'error') { this.next(); return { kind: 'ref', path: [t.v], text: t.v, span } }
        this.fail(`expected an expression, found keyword \`${t.text}\``)
        break
      case 'lower': {
        this.next()
        if (this.isP('(')) {
          const args = this.args()
          return { kind: 'call', fn: t.v, args, text: `${t.v}(${args.map((a) => a.text).join(', ')})`, span }
        }
        return { kind: 'ref', path: [t.v], text: t.v, span }
      }
      case 'upper': {
        this.next()
        if (this.isP('.') && this.peek().t === 'upper') {
          this.next()
          const v = this.next()
          return { kind: 'enum', type: t.v, variant: v.v, text: `${t.v}.${v.v}`, span }
        }
        const fields = {}
        const parts = []
        if (this.isP('{') && !o.noRecord) {
          this.next()
          while (!this.isP('}')) {
            const f = this.lower('a field name')
            this.expectP(':')
            if (f.v in fields) this.fail(`field \`${f.v}\` is given twice`, f)
            fields[f.v] = this.expr()
            parts.push(`${f.v}: ${fields[f.v].text}`)
            if (!this.eat('punct', ',')) break
          }
          this.expectP('}')
        }
        return { kind: 'record', type: t.v, fields, text: parts.length ? `${t.v} { ${parts.join(', ')} }` : t.v, span }
      }
      case 'punct':
        if (t.v === '(') {
          this.next()
          const e = this.expr() // parentheses reset the no-record restriction
          this.expectP(')')
          return e
        }
    }
    this.fail(`expected an expression, found \`${t.text}\``)
  }
}

// ───────────────────────────── loading and imports ─────────────────────────────
function load(entry, cwd, diags) {
  const sources = []           // [{ path, abs }], index = file id
  const files = new Map()      // abs -> { id, ast, scope }
  const rel = (abs) => path.relative(cwd, abs).split(path.sep).join('/')

  function loadFile(abs, from) {
    if (files.has(abs)) return files.get(abs)
    let src
    try { src = fs.readFileSync(abs, 'utf8') } catch {
      if (from) diags.push({ ...from, code: 'RB0002', message: `cannot read \`${rel(abs)}\`` })
      else diags.push({ file: -1, path: rel(abs), line: 1, col: 1, code: 'RB0002', message: 'cannot read file' })
      return null
    }
    const id = sources.length
    sources.push({ path: rel(abs) })
    const f = { id, abs, ast: { imports: [], decls: [] }, scope: new Map(), own: new Map() }
    files.set(abs, f) // registered before parsing imports, so import cycles terminate
    try {
      f.ast = new Parser(lex(src), id).file()
    } catch (e) {
      if (!(e instanceof SyntaxError_)) throw e
      diags.push({ file: id, line: e.line, col: e.col, code: 'RB0001', message: e.message })
      f.broken = true
    }
    for (const d of f.ast.decls) {
      if (f.own.has(d.name)) {
        diags.push({ ...d.span, code: 'RB0107', message: `\`${d.name}\` is declared twice in this file` })
        continue
      }
      f.own.set(d.name, d)
      f.scope.set(d.name, d)
    }
    for (const imp of f.ast.imports) {
      const target = loadFile(path.resolve(path.dirname(abs), imp.path), imp.pathSpan)
      if (!target || target.broken) {
        // Do not pile "unknown name" errors on top of the import error.
        for (const n of imp.names) if (!f.scope.has(n.name)) f.scope.set(n.name, { poisoned: true, name: n.name })
        continue
      }
      for (const n of imp.names) {
        const d = target.own.get(n.name)
        if (!d) {
          diags.push({ ...n.span, code: 'RB0002', message: `\`${rel(target.abs)}\` does not declare \`${n.name}\`` })
        } else if (f.scope.has(n.name) && f.scope.get(n.name) !== d) {
          diags.push({ ...n.span, code: 'RB0107', message: `\`${n.name}\` is already declared in this file` })
        } else f.scope.set(n.name, d)
      }
    }
    return f
  }

  loadFile(path.resolve(cwd, entry), null)
  return { sources, files: [...files.values()] }
}

// ───────────────────────────── checks and IR emission ─────────────────────────────
const stripSpan = (e) => {
  if (e === null) return null
  const { span, ...rest } = e
  if (rest.args) rest.args = rest.args.map(stripSpan)
  if (rest.of) rest.of = stripSpan(rest.of)
  if (rest.fields) rest.fields = Object.fromEntries(Object.entries(rest.fields).map(([k, v]) => [k, stripSpan(v)]))
  return rest
}
const irFields = (fs_) => fs_.map((f) => ({ name: f.name, type: f.type.text }))
const sig = (d) => `${d.params.map((p) => `${p.name}: ${p.type.text}`).join(', ')}`
const declSig = (d) =>
  `${d.returns ? `-> ${d.returns.text}` : ''}${d.errors?.length ? ` ! ${d.errors.map((e) => e.name).join(' | ')}` : ''}`.trim()

function emit(loaded, diags) {
  const ir = { irVersion: IR_VERSION, sources: loaded.sources, types: {}, functions: {}, actions: {}, tasks: {}, machines: {} }
  const err = (span, code, message) => diags.push({ ...span, code, message })
  const seen = new Map()

  for (const f of loaded.files) {
    const scope = f.scope
    const lookup = (name, ...kinds) => {
      const d = scope.get(name)
      if (d?.poisoned) return { poisoned: true, decl: kinds[0], name, params: [], errors: [], variants: [] }
      return d && kinds.includes(d.decl) ? d : null
    }
    const hint = (name) => {
      for (const g of loaded.files) {
        if (g !== f && g.own.has(name)) return ` (declared in \`${loaded.sources[g.id].path}\`; add it to an \`import\`)`
      }
      return ''
    }
    const checkType = (t) => {
      if (!t || BUILTIN_TYPES.has(t.base)) return
      if (!lookup(t.base, 'type')) err(t.span, 'RB0101', `unknown type \`${t.base}\`${hint(t.base)}`)
    }
    const checkErrors = (errors) => {
      for (const e of errors) {
        const d = lookup(e.name, 'type')
        if (!d) err(e.span, 'RB0101', `unknown error \`${e.name}\`${hint(e.name)}`)
        else if (d.kind !== 'error' && !d.poisoned) err(e.span, 'RB0101', `\`${e.name}\` is a ${d.kind} type, not an \`error\``)
      }
    }
    const checkParams = (params) => {
      const names = new Set()
      for (const p of params) {
        if (names.has(p.name)) err(p.span, 'RB0107', `parameter \`${p.name}\` is declared twice`)
        names.add(p.name)
        checkType(p.type)
      }
    }

    for (const d of f.ast.decls) {
      if (f.own.get(d.name) !== d) continue
      // The IR keys declarations by bare name, so names must be unique across the whole program.
      if (seen.has(d.name)) {
        const other = seen.get(d.name)
        err(d.span, 'RB0107', `\`${d.name}\` is also declared at ${loaded.sources[other.span.file].path}:${other.span.line}; names must be unique across imported files`)
        continue
      }
      seen.set(d.name, d)

      if (d.decl === 'type') {
        const t = { kind: d.kind }
        if (d.kind === 'record' || d.kind === 'error') {
          const names = new Set()
          for (const fl of d.fields) {
            if (names.has(fl.name)) err(fl.span, 'RB0107', `field \`${fl.name}\` is declared twice`)
            names.add(fl.name)
            checkType(fl.type)
          }
          t.fields = irFields(d.fields)
        }
        if (d.kind === 'enum') t.variants = d.variants
        if (d.kind === 'alias') { checkType(d.target); t.target = d.target.text }
        ir.types[d.name] = { ...t, doc: d.doc, span: d.span }
      } else if (d.decl === 'fn') {
        checkParams(d.params); checkType(d.returns)
        ir.functions[d.name] = { params: irFields(d.params), returns: d.returns.text, doc: d.doc, span: d.span }
      } else if (d.decl === 'action') {
        checkParams(d.params)
        ir.actions[d.name] = { params: irFields(d.params), doc: d.doc, span: d.span }
      } else if (d.decl === 'task') {
        checkParams(d.params); checkType(d.returns); checkErrors(d.errors)
        ir.tasks[d.name] = {
          params: irFields(d.params), returns: d.returns.text, errors: d.errors.map((e) => e.name), doc: d.doc, span: d.span,
        }
      } else if (d.decl === 'machine') {
        checkParams(d.params); checkType(d.returns); checkErrors(d.errors)
        ir.machines[d.name] = emitMachine(d, { lookup, hint, checkType, err })
      }
    }
  }
  return ir
}

function emitMachine(m, { lookup, hint, checkType, err }) {
  // variables: parameters (read-only) and vars
  const vars = new Map()
  for (const p of m.params) vars.set(p.name, { param: true })
  for (const v of m.vars) {
    if (['result', 'event', 'error'].includes(v.name)) err(v.span, 'RB0107', `\`${v.name}\` is reserved: it names the invoke result inside \`done\``)
    else if (vars.has(v.name)) err(v.span, 'RB0107', `\`${v.name}\` is already declared as a ${vars.get(v.name).param ? 'parameter' : 'variable'}`)
    else vars.set(v.name, { param: false })
    checkType(v.type)
  }
  const events = {}
  for (const e of m.events) {
    if (e.name in events) err(e.span, 'RB0107', `event \`${e.name}\` is declared twice`)
    for (const fl of e.fields) checkType(fl.type)
    events[e.name] = { fields: irFields(e.fields), doc: e.doc }
  }

  // ctx: which implicit names are in scope ({ event, result, error })
  const checkExpr = (e, ctx) => {
    if (!e) return
    switch (e.kind) {
      case 'ref': {
        const head = e.path[0]
        if (vars.has(head)) return
        if (head === 'event' || head === 'result' || head === 'error') {
          if (ctx[head]) return
          const where = { event: 'an `on` transition', result: "an invoke's `done`", error: "an invoke's `fail`" }[head]
          err(e.span, 'RB0105', `\`${head}\` is only available in ${where}`)
          return
        }
        const other = lookup(head, 'fn', 'action', 'task')
        err(e.span, 'RB0105', other
          ? `\`${head}\` is ${other.decl === 'fn' ? 'a function' : `a${other.decl === 'action' ? 'n' : ''} ${other.decl}`}, not a variable`
          : `unknown variable \`${head}\` in machine \`${m.name}\``)
        return
      }
      case 'call': {
        const d = lookup(e.fn, 'fn')
        if (!d) {
          const other = lookup(e.fn, 'action', 'task')
          err(e.span, 'RB0103', other
            ? `\`${e.fn}\` is a${other.decl === 'action' ? 'n' : ''} ${other.decl}; only a \`fn\` can be called in an expression`
            : `unknown function \`${e.fn}\`${hint(e.fn)}`)
        }
        e.args.forEach((a) => checkExpr(a, ctx))
        return
      }
      case 'op': e.args.forEach((a) => checkExpr(a, ctx)); return
      case 'field': checkExpr(e.of, ctx); return
      case 'record': {
        const d = lookup(e.type, 'type')
        if (!d) err(e.span, 'RB0101', `unknown type \`${e.type}\`${hint(e.type)}`)
        else if (d.kind !== 'record' && d.kind !== 'error' && !d.poisoned) err(e.span, 'RB0101', `\`${e.type}\` is ${d.kind === 'enum' ? 'an enum' : `an ${d.kind} type`}; it has no record value`)
        Object.values(e.fields).forEach((a) => checkExpr(a, ctx))
        return
      }
      case 'enum': {
        const d = lookup(e.type, 'type')
        if (!d) err(e.span, 'RB0101', `unknown type \`${e.type}\`${hint(e.type)}`)
        else if (d.poisoned) return
        else if (d.kind !== 'enum') err(e.span, 'RB0101', `\`${e.type}\` is not an enum`)
        else if (!d.variants.includes(e.variant)) err(e.span, 'RB0101', `enum \`${e.type}\` has no variant \`${e.variant}\` (it has ${d.variants.join(', ')})`)
      }
    }
  }
  const emitEffects = (effects, ctx) => effects.map((ef) => {
    if (ef.kind === 'set') {
      const v = vars.get(ef.var)
      if (!v) err(ef.span, 'RB0105', `unknown variable \`${ef.var}\` in machine \`${m.name}\``)
      else if (v.param) err(ef.span, 'RB0108', `\`${ef.var}\` is a parameter of \`${m.name}\`; parameters are read-only`)
      checkExpr(ef.value, ctx)
      return { kind: 'set', var: ef.var, value: stripSpan(ef.value) }
    }
    if (!lookup(ef.action, 'action')) {
      const other = lookup(ef.action, 'fn', 'task')
      err(ef.span, 'RB0104', other
        ? `\`${ef.action}\` is a ${other.decl === 'fn' ? 'function' : 'task'}, not an action; \`do\` runs actions`
        : `unknown action \`${ef.action}\`${hint(ef.action)}`)
    }
    ef.args.forEach((a) => checkExpr(a, ctx))
    return { kind: 'do', action: ef.action, args: ef.args.map(stripSpan) }
  })

  // Target resolution. For a transition written in state S, the first segment is looked up among
  // the children of S's parent, then of each further ancestor up to the machine root. A transition
  // written directly in the machine body resolves among the root's children. Further segments descend.
  const resolveTarget = (st, target) => {
    const [head, ...rest] = target.path
    const written = target.path.join('.')
    let scope = st.parent ?? st
    let found = null
    for (; scope; scope = scope.parent) {
      const hits = scope.states.filter((c) => c.name === head)
      if (hits.length > 1) {
        err(target.span, 'RB0202', `ambiguous target \`${written}\`: \`${scope.id}\` has ${hits.length} states named \`${head}\``)
        return null
      }
      if (hits.length === 1) { found = hits[0]; break }
    }
    if (!found) {
      const child = st.states.find((c) => c.name === head)
      const help = child
        ? `; \`${head}\` is a child of \`${st.name}\`, write \`${st.name}.${written}\``
        : `; states in scope: ${scopeNames(st).join(', ') || 'none'}`
      err(target.span, 'RB0201', `unknown target state \`${written}\`${help}`)
      return null
    }
    for (const seg of rest) {
      const next = found.states.find((c) => c.name === seg)
      if (!next) {
        err(target.span, 'RB0201', `unknown target state \`${written}\`: \`${found.id}\` has no child \`${seg}\``)
        return null
      }
      found = next
    }
    return found.id
  }
  const scopeNames = (st) => {
    const out = []
    for (let s = st.parent ?? st; s; s = s.parent) for (const c of s.states) if (!out.includes(c.name)) out.push(c.name)
    return out
  }

  const emitState = (st, parent, depth) => {
    st.parent = parent
    st.id = parent ? `${parent.id}.${st.name}` : st.name
    const kind = st.modifier === 'final' ? 'final' : st.modifier === 'parallel' ? 'parallel' : st.states.length ? 'compound' : 'atomic'

    // children: unique names, ids assigned before transitions are resolved
    const names = new Map()
    for (const c of st.states) {
      if (names.has(c.name)) err(c.nameSpan, 'RB0107', `state \`${c.name}\` is declared twice in \`${st.id}\` (first at line ${names.get(c.name).span.line})`)
      else names.set(c.name, c)
      c.parent = st
      c.id = `${st.id}.${c.name}`
    }

    // initial
    let initial = null
    const inits = st.initials ?? []
    if (inits.length > 1) err(inits[1].span, 'RB0203', `\`${st.id}\` has more than one \`initial\``)
    if (kind === 'compound') {
      if (!inits.length) {
        err(st.nameSpan ?? st.span, 'RB0203', `${parent ? 'state' : 'machine'} \`${st.id}\` has child states but no \`initial\`; add \`initial ${st.states[0].name}\``)
      } else if (!names.has(inits[0].name)) {
        err(inits[0].span, 'RB0203', `\`initial ${inits[0].name}\`: \`${inits[0].name}\` is not a child of \`${st.id}\` (children: ${st.states.map((c) => c.name).join(', ')})`)
      } else initial = `${st.id}.${inits[0].name}`
    } else if (inits.length) {
      const why = kind === 'parallel' ? 'is parallel: all of its children are entered' : kind === 'final' ? 'is final' : 'has no child states'
      err(inits[0].span, 'RB0203', `\`initial ${inits[0].name}\`: \`${st.id}\` ${why}`)
    }

    if ((st.output || st.fail) && !(kind === 'final' && depth === 1)) {
      err(st.resultSpan, 'RB0208', `\`${st.output ? 'output' : 'fail'}\` is only allowed in a final state at the top level of a machine`)
    }
    checkExpr(st.output, {}); checkExpr(st.fail, {})

    // invoke
    let invoke = null
    if (st.invokes.length > 1) err(st.invokes[1].span, 'RB0208', `\`${st.id}\` already has an \`invoke\`; a state runs at most one`)
    const inv = st.invokes[0]
    if (inv) {
      const d = lookup(inv.src, inv.kind)
      if (!d) {
        const other = lookup(inv.src, 'fn', 'action')
        err(inv.nameSpan, 'RB0102', other
          ? `\`${inv.src}\` is ${other.decl === 'fn' ? 'a function' : 'an action'}; \`invoke\` runs a task or a machine`
          : `unknown ${inv.kind} \`${inv.src}\`${hint(inv.src)}`)
      } else if (!d.poisoned) {
        const declared = d.errors.map((e) => e.name)
        const handled = new Set(inv.outcomes.filter((o) => o.trigger.kind === 'invoke.fail').map((o) => o.trigger.error))
        for (const o of inv.outcomes) {
          const e = o.trigger.error
          if (o.trigger.kind === 'invoke.fail' && e !== '*' && !declared.includes(e)) {
            err(o.errorSpan, 'RB0207', `\`${inv.src}\` does not declare error \`${e}\` (declared as \`${declSig(d) || 'no result'}\`)`)
          }
        }
        if (!handled.has('*')) {
          for (const e of declared) {
            if (!handled.has(e)) {
              err(inv.span, 'RB0204', `\`invoke ${inv.src}\` does not handle \`${e}\` (declared as \`${declSig(d)}\`); add \`fail ${e} -> <State>\` or \`fail * -> <State>\``)
            }
          }
        }
      }
      inv.args.forEach((a) => checkExpr(a, {}))
      invoke = { kind: inv.kind, src: inv.src, args: inv.args.map(stripSpan) }
    }

    // transitions
    const transitions = []
    const keys = new Map()
    st.blocks.forEach((b, group) => {
      const what = b.trigger.kind.startsWith('invoke.') ? `outcome \`${b.key}\`` : `trigger \`${b.key}\``
      if (keys.has(b.key)) {
        err(b.span, 'RB0205', `${what} is written twice in \`${st.id}\` (first at line ${keys.get(b.key).span.line}); put the branches in one block: \`${b.key} { if … else … }\``)
      } else keys.set(b.key, b)

      if (b.trigger.kind === 'event' && b.trigger.event !== '*' && !(b.trigger.event in events)) {
        err(b.eventSpan, 'RB0106', `unknown event \`${b.trigger.event}\`; machine \`${m.name}\` declares ${Object.keys(events).map((e) => `\`${e}\``).join(', ') || 'no events'}`)
      }
      const ctx = { event: b.trigger.kind === 'event', result: b.trigger.kind === 'invoke.done', error: b.trigger.kind === 'invoke.fail' }
      let trigger = b.trigger
      if (trigger.kind === 'after') {
        checkExpr(trigger.delay, {})
        trigger = { kind: 'after', delay: stripSpan(trigger.delay) }
      }
      let unguarded = null
      for (const br of b.branches) {
        if (unguarded) {
          err(br.span, 'RB0206', `this branch can never be taken: the ${unguarded.isElse ? '`else`' : 'unguarded'} branch at line ${unguarded.span.line} always matches first`)
        } else if (!br.guard) unguarded = br
        checkExpr(br.guard, ctx)
        transitions.push({
          trigger, group,
          guard: stripSpan(br.guard),
          target: br.target ? resolveTarget(st, br.target) : null,
          effects: emitEffects(br.effects, ctx),
          span: br.single ? b.span : br.span,
        })
      }
    })

    return {
      id: st.id, name: st.name, kind, doc: st.doc, span: st.span,
      initial,
      states: st.states.map((c) => emitState(c, st, depth + 1)),
      entry: emitEffects(st.entry, {}), exit: emitEffects(st.exit, {}),
      invoke, transitions,
      output: stripSpan(st.output), fail: stripSpan(st.fail),
    }
  }

  for (const v of m.vars) checkExpr(v.init, {})
  if (!m.root.states.length) err(m.span, 'RB0203', `machine \`${m.name}\` has no states`)
  const root = emitState(m.root, null, 0)
  if (!m.root.states.length) root.kind = 'compound'

  return {
    doc: m.doc, span: m.span,
    params: irFields(m.params),
    returns: m.returns ? m.returns.text : null,
    errors: m.errors.map((e) => e.name),
    vars: m.vars.map((v) => ({ name: v.name, type: v.type.text, init: stripSpan(v.init) })),
    events,
    root,
  }
}

// ───────────────────────────── API and CLI ─────────────────────────────
export function compile(entry, { cwd = process.cwd() } = {}) {
  const diags = []
  const loaded = load(entry, cwd, diags)
  const ir = emit(loaded, diags)
  const diagnostics = diags
    .map((d) => ({ path: d.path ?? loaded.sources[d.file]?.path ?? entry, line: d.line, col: d.col, code: d.code, message: d.message, file: d.file }))
    .sort((a, b) => a.file - b.file || a.line - b.line || a.col - b.col)
  return { ir, diagnostics, ok: diagnostics.length === 0 }
}

export function formatDiagnostic(d) {
  return `${d.path}:${d.line}:${d.col}: error[${d.code}]: ${d.message}`
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const args = process.argv.slice(2)
  if (args.length !== 1 || args[0].startsWith('-')) {
    console.error('usage: node compile.mjs <file.road>   (IR JSON on stdout, diagnostics on stderr)')
    process.exit(2)
  }
  const { ir, diagnostics, ok } = compile(args[0])
  for (const d of diagnostics) console.error(formatDiagnostic(d))
  if (!ok) {
    console.error(`${diagnostics.length} error${diagnostics.length === 1 ? '' : 's'}`)
    process.exit(1)
  }
  process.stdout.write(JSON.stringify(ir, null, 2) + '\n')
}
