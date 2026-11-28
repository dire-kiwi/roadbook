// The TextMate grammar against snapshots: every token of the shop example's charts, of the
// upload example (messaging) and of a file of constructs the examples do not show, with its
// scopes. `vitest run -u` rewrites the snapshots; review the diff.

import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'
import { INITIAL, Registry, parseRawGrammar } from 'vscode-textmate'
import type { IGrammar } from 'vscode-textmate'
import { createOnigScanner, createOnigString, loadWASM } from 'vscode-oniguruma'

const here = path.dirname(fileURLToPath(import.meta.url))
const repo = path.resolve(here, '../../..')
const grammarPath = path.resolve(here, '../syntaxes/roadbook.tmLanguage.json')

async function loadGrammar(): Promise<IGrammar> {
  const wasm = fs.readFileSync(path.join(path.dirname(fileURLToPath(import.meta.resolve('vscode-oniguruma'))), 'onig.wasm'))
  await loadWASM(wasm.buffer.slice(wasm.byteOffset, wasm.byteOffset + wasm.byteLength))
  const registry = new Registry({
    onigLib: Promise.resolve({ createOnigScanner, createOnigString }),
    loadGrammar: async (scope) => (scope === 'source.roadbook' ? parseRawGrammar(fs.readFileSync(grammarPath, 'utf8'), grammarPath) : null),
  })
  const g = await registry.loadGrammar('source.roadbook')
  if (!g) throw new Error('the grammar did not load')
  return g
}

const grammar = loadGrammar()

/** Each line, then a line under each token that is not only whitespace, with its scopes below the root. */
export async function snapshot(text: string): Promise<string> {
  const g = await grammar
  let stack = INITIAL
  const out: string[] = []
  for (const line of text.replace(/\n$/, '').split('\n')) {
    const r = g.tokenizeLine(line, stack)
    stack = r.ruleStack
    out.push(`>${line}`)
    for (const t of r.tokens) {
      const s = line.slice(t.startIndex, t.endIndex)
      if (!s.trim()) continue
      out.push(`#${' '.repeat(t.startIndex)}${'^'.repeat(t.endIndex - t.startIndex)} ${t.scopes.slice(1).join(' ')}`)
    }
  }
  return out.join('\n') + '\n'
}

/** The scopes of the token at the first occurrence of `needle` in a one-line text. */
async function scopesAt(line: string, needle: string): Promise<string[]> {
  const g = await grammar
  const at = line.indexOf(needle)
  const t = g.tokenizeLine(line, INITIAL).tokens.find((t) => t.startIndex <= at && at < t.endIndex)
  return t?.scopes ?? []
}

describe('snapshots', () => {
  const files = [
    'examples/shop/checkout.road',
    'examples/shop/place_order.road',
    'examples/shop/types.road',
    'examples/upload/upload.road',
    'editors/vscode/test/grammar/edge-cases.road.txt',
  ]
  for (const f of files) {
    it(f, async () => {
      const name = path.basename(f).replace(/\.txt$/, '')
      await expect(await snapshot(fs.readFileSync(path.join(repo, f), 'utf8'))).toMatchFileSnapshot(`snapshots/${name}.snap`)
    })
  }
})

describe('scopes', () => {
  it('tells doc comments from comments', async () => {
    expect(await scopesAt('/// A doc', 'A')).toContain('comment.line.documentation.roadbook')
    expect(await scopesAt('// A comment', 'A')).toContain('comment.line.double-slash.roadbook')
    expect(await scopesAt('//// not a doc', 'not')).toContain('comment.line.double-slash.roadbook')
  })

  it('marks declarations and their names', async () => {
    expect(await scopesAt('machine Checkout(order: Order) -> Receipt {', 'Checkout')).toContain('entity.name.type.machine.roadbook')
    expect(await scopesAt('machine Checkout(order: Order) -> Receipt {', 'order')).toContain('variable.parameter.roadbook')
    expect(await scopesAt('machine Checkout(order: Order) -> Receipt {', 'Receipt')).toContain('entity.name.type.roadbook')
    expect(await scopesAt('task chargeCard(order: Order) -> Receipt ! CardDeclined', 'chargeCard')).toContain('entity.name.function.roadbook')
    expect(await scopesAt('  final state Charged { output receipt }', 'Charged')).toContain('entity.name.type.state.roadbook')
    expect(await scopesAt('  event Cancel', 'Cancel')).toContain('entity.name.type.event.roadbook')
    expect(await scopesAt('strict machine Lock() {', 'strict')).toContain('storage.modifier.strict.roadbook')
    expect(await scopesAt('strict machine Lock() {', 'Lock')).toContain('entity.name.type.machine.roadbook')
    expect(await scopesAt('    on Knock if event.loud ignore', 'ignore')).toContain('keyword.control.ignore.roadbook')
    expect(await scopesAt('type Page<T> { items: map<string, T> }', 'map')).toContain('support.type.primitive.roadbook')
    expect(await scopesAt('type Page<T> { items: map<string, T> }', 'Page')).toContain('entity.name.type.roadbook')
  })

  it('marks history states, and history and deep as names elsewhere', async () => {
    expect(await scopesAt('    history Last', 'history')).toContain('storage.type.history.roadbook')
    expect(await scopesAt('    history Last', 'Last')).toContain('entity.name.type.state.roadbook')
    expect(await scopesAt('    deep history All', 'deep')).toContain('storage.modifier.deep.roadbook')
    expect(await scopesAt('    deep history All', 'history')).toContain('storage.type.history.roadbook')
    expect(await scopesAt('    deep history All', 'All')).toContain('entity.name.type.state.roadbook')
    expect(await scopesAt('  var history: int', 'history')).toContain('variable.other.definition.roadbook')
    expect(await scopesAt('    always set deep = history + 1', 'history')).toContain('variable.other.roadbook')
    expect(await scopesAt('    always set deep = history + 1', 'deep')).not.toContain('storage.modifier.deep.roadbook')
  })

  it('marks internal targets and several targets as states', async () => {
    const line = '    on Sleep -> .Playback.Paused, .Volume.Muted  do log()'
    expect(await scopesAt(line, 'Playback')).toContain('entity.name.type.state.roadbook')
    expect(await scopesAt(line, 'Muted')).toContain('entity.name.type.state.roadbook')
    expect(await scopesAt(line, ',')).toContain('punctuation.separator.comma.roadbook')
    expect(await scopesAt(line, 'do')).not.toContain('entity.name.type.state.roadbook')
  })

  it('marks states, events, calls and values', async () => {
    const line = '    after retryDelay(attempts) -> Charging set n = n + 1'
    expect(await scopesAt(line, 'retryDelay')).toContain('entity.name.function.call.roadbook')
    expect(await scopesAt(line, 'attempts')).toContain('variable.other.roadbook')
    expect(await scopesAt(line, 'Charging')).toContain('entity.name.type.state.roadbook')
    expect(await scopesAt('on upload.Progress -> A', 'Progress')).toContain('entity.name.type.event.roadbook')
    expect(await scopesAt('on upload.Progress -> A', 'upload')).toContain('variable.other.roadbook')
  })

  it('marks a peer’s end, and ended as a name elsewhere', async () => {
    const line = '    on inventory.ended if event.status == Ended.Defect -> Alarm'
    expect(await scopesAt(line, 'inventory')).toContain('variable.other.roadbook')
    expect(await scopesAt(line, 'ended')).toContain('keyword.control.ended.roadbook')
    expect(await scopesAt(line, 'Alarm')).toContain('entity.name.type.state.roadbook')
    expect(await scopesAt('    on current.ended -> Gone', 'ended')).toContain('keyword.control.ended.roadbook')
    expect(await scopesAt('    always set ended = ended + 1', 'ended')).not.toContain('keyword.control.ended.roadbook')
  })

  it('marks calls, the triggers that take them and replies', async () => {
    expect(await scopesAt('  call reserve(orderId: string) -> Reservation', 'call')).toContain('storage.type.call.roadbook')
    expect(await scopesAt('  call reserve(orderId: string) -> Reservation', 'reserve')).toContain('entity.name.function.roadbook')
    expect(await scopesAt('  call reserve(orderId: string) -> Reservation', 'orderId')).toContain('variable.parameter.roadbook')
    expect(await scopesAt('  var pending: call reserve', 'call')).toContain('storage.modifier.call.roadbook')
    expect(await scopesAt('  var pending: call reserve', 'reserve')).toContain('entity.name.function.roadbook')
    expect(await scopesAt('on reserve -> A  set pending = call', 'reserve')).toContain('entity.name.function.roadbook')
    expect(await scopesAt('on reserve -> A  set pending = call', '= call')).toContain('keyword.operator.assignment.roadbook')
    expect(await scopesAt('set pending = call', 'call')).toContain('variable.language.roadbook')
    expect(await scopesAt('on count reply call done held', 'reply')).toContain('keyword.other.effect.roadbook')
    expect(await scopesAt('on count reply call done held', 'call')).toContain('variable.language.roadbook')
    expect(await scopesAt('on count reply call done held', 'done')).toContain('keyword.control.done.roadbook')
    expect(await scopesAt('reply pending fail error', 'pending')).toContain('variable.other.roadbook')
    expect(await scopesAt('reply pending fail error', 'fail')).toContain('keyword.control.fail.roadbook')
    expect(await scopesAt('invoke inventory.reserve(id) {', 'inventory')).toContain('variable.other.roadbook')
    expect(await scopesAt('invoke inventory.reserve(id) {', 'reserve')).toContain('entity.name.function.call.roadbook')
    expect(await scopesAt('fail Unanswered -> A', 'Unanswered')).toContain('support.type.builtin.error.roadbook')
    expect(await scopesAt('fail OutOfStock -> A', 'OutOfStock')).toContain('entity.name.type.error.roadbook')
  })

  it('marks keyed spawn only where it is keyed spawn', async () => {
    const decl = '  children sessions: Session by SessionId'
    expect(await scopesAt(decl, 'children')).toContain('storage.type.children.roadbook')
    expect(await scopesAt(decl, 'sessions')).toContain('variable.other.definition.roadbook')
    expect(await scopesAt(decl, 'Session')).toContain('entity.name.type.machine.roadbook')
    expect(await scopesAt(decl, 'by')).toContain('keyword.other.by.roadbook')
    expect(await scopesAt('type T { children: int, by: string }', 'children')).toContain('variable.other.property.roadbook')
    const spawn = 'on J spawn sessions[event.id] = Session(key)  stop sessions[key]'
    expect(await scopesAt(spawn, 'spawn')).toContain('keyword.other.effect.roadbook')
    expect(await scopesAt(spawn, 'stop')).toContain('keyword.other.effect.roadbook')
    expect(await scopesAt(spawn, 'sessions')).toContain('variable.other.roadbook')
    expect(await scopesAt(spawn, 'key')).toContain('variable.language.roadbook')
    expect(await scopesAt('action stop()', 'stop')).toContain('entity.name.function.roadbook')
    expect(await scopesAt('on X do stop()', 'stop')).toContain('entity.name.function.call.roadbook')
    expect(await scopesAt('send sessions[key].Notify', 'Notify')).toContain('entity.name.type.event.roadbook')
    expect(await scopesAt('send sessions.*.Notify', '*')).toContain('keyword.operator.wildcard.roadbook')
    expect(await scopesAt('send sessions.*.Notify', 'Notify')).toContain('entity.name.type.event.roadbook')
    expect(await scopesAt('on sessions.done do f(key)', 'done')).toContain('keyword.control.done.roadbook')
    expect(await scopesAt('on sessions.fail Lost -> A', 'fail')).toContain('keyword.control.fail.roadbook')
    expect(await scopesAt('on sessions.fail Lost -> A', 'Lost')).toContain('entity.name.type.error.roadbook')
    expect(await scopesAt('emit E { key: event.text }', 'key')).toContain('variable.other.property.roadbook')
    expect(await scopesAt('emit E { k: event.key }', 'key')).toContain('variable.other.property.roadbook')
  })

  it('marks the events a task emits, and collections of tasks', async () => {
    const task = 'task ticks(every: duration) ! Lost emits Tick { n: int }'
    expect(await scopesAt(task, 'ticks')).toContain('entity.name.function.roadbook')
    expect(await scopesAt(task, 'emits')).toContain('storage.type.emits.roadbook')
    expect(await scopesAt(task, 'Tick')).toContain('entity.name.type.event.roadbook')
    expect(await scopesAt('  emits Progress { percent: int }', 'Progress')).toContain('entity.name.type.event.roadbook')
    const decl = '  children uploads: upload by string'
    expect(await scopesAt(decl, 'uploads')).toContain('variable.other.definition.roadbook')
    expect(await scopesAt(decl, 'upload ')).toContain('entity.name.function.call.roadbook')
    expect(await scopesAt(decl, 'by')).toContain('keyword.other.by.roadbook')
    expect(await scopesAt('on up.Progress set p = event.percent', 'up')).toContain('variable.other.roadbook')
    expect(await scopesAt('on up.Progress set p = event.percent', 'Progress')).toContain('entity.name.type.event.roadbook')
    expect(await scopesAt('invoke ticks(1s) as clock', 'clock')).toContain('variable.other.definition.roadbook')
  })

  it('marks internal events, raise and cancel, and the same words as names', async () => {
    const decl = '  internal event Remind { attempt: int }'
    expect(await scopesAt(decl, 'internal')).toContain('storage.modifier.internal.roadbook')
    expect(await scopesAt(decl, 'event')).toContain('storage.type.event.roadbook')
    expect(await scopesAt(decl, 'Remind')).toContain('entity.name.type.event.roadbook')
    const raise = 'on Go raise Ping  raise Remind { attempt: 1 } after 24h as reminder'
    expect(await scopesAt(raise, 'raise')).toContain('keyword.other.effect.roadbook')
    expect(await scopesAt(raise, 'Ping')).toContain('entity.name.type.event.roadbook')
    expect(await scopesAt(raise, 'Remind')).toContain('entity.name.type.event.roadbook')
    expect(await scopesAt(raise, 'after')).toContain('keyword.control.after.roadbook')
    expect(await scopesAt(raise, '24h')).toContain('constant.numeric.duration.roadbook')
    expect(await scopesAt(raise, 'as')).toContain('keyword.control.as.roadbook')
    expect(await scopesAt(raise, 'reminder')).toContain('variable.other.definition.roadbook')
    const cancel = 'exit cancel reminder  do log()'
    expect(await scopesAt(cancel, 'cancel')).toContain('keyword.other.effect.roadbook')
    expect(await scopesAt(cancel, 'reminder')).toContain('variable.other.roadbook')
    expect(await scopesAt('  var raise: int = 0', 'raise')).toContain('variable.other.definition.roadbook')
    expect(await scopesAt('set raise = raise + 1', 'raise')).toContain('variable.other.roadbook')
    expect(await scopesAt('set n = cancel and ok', 'cancel')).toContain('variable.other.roadbook')
    expect(await scopesAt('set n = cancel and ok', 'and')).toContain('keyword.operator.logical.roadbook')
    expect(await scopesAt('on X do cancel()', 'cancel')).toContain('entity.name.function.call.roadbook')
    expect(await scopesAt('type T { internal: bool }', 'internal')).toContain('variable.other.property.roadbook')
  })

  it('marks literals', async () => {
    expect(await scopesAt('after 30s -> A', '30s')).toContain('constant.numeric.duration.roadbook')
    expect(await scopesAt('set r = 1.5', '1.5')).toContain('constant.numeric.float.roadbook')
    expect(await scopesAt('set s = "a\\"b"', '\\"')).toContain('constant.character.escape.roadbook')
    expect(await scopesAt('set s = "a\\qb"', '\\q')).toContain('invalid.illegal.unknown-escape.roadbook')
    expect(await scopesAt('if ok == true -> A', '==')).toContain('keyword.operator.comparison.roadbook')
  })
})
