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
  })

  it('marks states, events, calls and values', async () => {
    const line = '    after retryDelay(attempts) -> Charging set n = n + 1'
    expect(await scopesAt(line, 'retryDelay')).toContain('entity.name.function.call.roadbook')
    expect(await scopesAt(line, 'attempts')).toContain('variable.other.roadbook')
    expect(await scopesAt(line, 'Charging')).toContain('entity.name.type.state.roadbook')
    expect(await scopesAt('on upload.Progress -> A', 'Progress')).toContain('entity.name.type.event.roadbook')
    expect(await scopesAt('on upload.Progress -> A', 'upload')).toContain('variable.other.roadbook')
  })

  it('marks literals', async () => {
    expect(await scopesAt('after 30s -> A', '30s')).toContain('constant.numeric.duration.roadbook')
    expect(await scopesAt('set r = 1.5', '1.5')).toContain('constant.numeric.float.roadbook')
    expect(await scopesAt('set s = "a\\"b"', '\\"')).toContain('constant.character.escape.roadbook')
    expect(await scopesAt('set s = "a\\qb"', '\\q')).toContain('invalid.illegal.unknown-escape.roadbook')
    expect(await scopesAt('if ok == true -> A', '==')).toContain('keyword.operator.comparison.roadbook')
  })
})
