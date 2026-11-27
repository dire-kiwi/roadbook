// Compiling a chart for the preview: the helpers, and the real command line when one is built
// (ROADBOOK_BIN, or target/debug/roadbook under CARGO_TARGET_DIR or the repository).

import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'
import { projectFile, projectPath, projectRoot, resolveExecutable } from '../src/cli.ts'
import { compileChart, compileWithServer, fromServer, parseCheck } from '../src/preview/compile.ts'
import { Serial } from '../src/serial.ts'
import { sourcePath, spanOf } from '../src/preview/ir.ts'
import type { Ir } from '../src/preview/ir.ts'

const here = path.dirname(fileURLToPath(import.meta.url))
const repo = path.resolve(here, '../../..')
const checkout = path.join(repo, 'examples/shop/checkout.road')

function builtRoadbook(): string | undefined {
  const candidates = [
    process.env.ROADBOOK_BIN,
    process.env.CARGO_TARGET_DIR && path.join(process.env.CARGO_TARGET_DIR, 'debug/roadbook'),
    path.join(repo, 'target/debug/roadbook'),
  ]
  return candidates.find((c): c is string => !!c && fs.existsSync(c))
}

describe('helpers', () => {
  it('finds the project root and project paths as the command line does', () => {
    expect(projectRoot(checkout)).toBe(repo)
    expect(projectPath(repo, checkout)).toBe('examples/shop/checkout.road')
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'rb-root-'))
    try {
      expect(projectRoot(path.join(dir, 'a.road'))).toBe(dir)
    } finally {
      fs.rmSync(dir, { recursive: true })
    }
  })

  it('opens only the .road files inside the project', () => {
    const root = path.resolve('/p/shop')
    expect(projectFile(root, 'checkout.road')).toBe(path.join(root, 'checkout.road'))
    expect(projectFile(root, 'charts/a.road')).toBe(path.join(root, 'charts', 'a.road'))
    expect(projectFile(root, 'charts/../a.road')).toBe(path.join(root, 'a.road'))
    expect(projectFile(root, '../other/a.road')).toBeUndefined()
    expect(projectFile(root, 'charts/../../a.road')).toBeUndefined()
    expect(projectFile(root, '/etc/a.road')).toBeUndefined()
    expect(projectFile(root, 'C:/x/a.road')).toBeUndefined()
    expect(projectFile(root, '..\\a.road')).toBeUndefined()
    expect(projectFile(root, 'notes.txt')).toBeUndefined()
  })

  it('declares roadbook.path restricted in untrusted workspaces', () => {
    const pkg = JSON.parse(fs.readFileSync(path.join(here, '../package.json'), 'utf8'))
    expect(pkg.capabilities.untrustedWorkspaces).toMatchObject({ supported: 'limited', restrictedConfigurations: ['roadbook.path'] })
    expect(pkg.contributes.configuration.properties['roadbook.path'].scope).toBe('machine-overridable')
  })

  it('reads the diagnostics of roadbook check --json', () => {
    const json = JSON.stringify({
      diagnostics: [
        { code: 'RB0201', severity: 'error', message: 'unknown target state `X`', file: 'a/c.road', line: 3, col: 4, endLine: 3, endCol: 5, help: null },
        { code: 'RB0901', severity: 'warning', message: 'w', file: 'a/types.road', line: 1, col: 1, endLine: 1, endCol: 2, help: 'h' },
      ],
    })
    expect(parseCheck(json)).toEqual([
      { code: 'RB0201', severity: 'error', message: 'unknown target state `X`', file: 'a/c.road', line: 3, col: 4, endLine: 3, endCol: 5, help: null },
      { code: 'RB0901', severity: 'warning', message: 'w', file: 'a/types.road', line: 1, col: 1, endLine: 1, endCol: 2, help: 'h' },
    ])
    expect(parseCheck('not json')).toBeUndefined()
  })

  it('reads what the language server answers to roadbook/ir', async () => {
    const ir = { irVersion: '0.5', sources: [{ path: 'c.road', package: null }], machines: {} }
    const ok = fromServer({ uri: 'file:///p/c.road', version: 3, root: '/p', entry: 'c.road', ir: JSON.stringify(ir), diagnostics: [] })
    expect(ok).toEqual({ kind: 'ok', ir, irText: JSON.stringify(ir), root: '/p', entry: 'c.road' })
    const warning = { code: 'RB0903', severity: 'warning', message: 'w', file: 'c.road', line: 1, col: 1, endLine: 1, endCol: 2, help: null }
    const error = { ...warning, code: 'RB0201', severity: 'error' }
    expect(fromServer({ uri: 'untitled:U', version: 1, root: null, entry: 'U.road', ir: null, diagnostics: [warning, error] })).toEqual({
      kind: 'errors',
      diagnostics: [error],
      text: null,
      root: null,
      entry: 'U.road',
    })
    expect(fromServer(null).kind).toBe('fatal')
    expect(fromServer({ entry: 'c.road', diagnostics: [], ir: '{' }).kind).toBe('fatal')

    // The request: its parameters; a refusal is shown, a server that cannot answer is not.
    const asked: unknown[] = []
    const answering = { sendRequest: async (method: string, params: unknown) => (asked.push([method, params]), { entry: 'c.road', root: '/p', ir: JSON.stringify(ir), diagnostics: [] }) }
    expect((await compileWithServer(answering, 'file:///p/c.road'))?.kind).toBe('ok')
    expect(asked).toEqual([['roadbook/ir', { textDocument: { uri: 'file:///p/c.road' } }]])
    const refusing = { sendRequest: () => Promise.reject(Object.assign(new Error('a file of another package'), { code: -32803 })) }
    expect(await compileWithServer(refusing, 'file:///p/x.road')).toEqual({ kind: 'fatal', message: 'a file of another package' })
    const stopped = { sendRequest: () => Promise.reject(Object.assign(new Error('Client is not running'), { code: -32097 })) }
    expect(await compileWithServer(stopped, 'file:///p/c.road')).toBeUndefined()
  })

  it('runs operations one at a time, in order', async () => {
    const serial = new Serial()
    const log: string[] = []
    const op = (name: string, ms: number, fail = false) => () =>
      new Promise<string>((resolve, reject) => {
        log.push(`start ${name}`)
        setTimeout(() => {
          log.push(`end ${name}`)
          if (fail) reject(new Error(name))
          else resolve(name)
        }, ms)
      })
    const a = serial.run(op('start', 30))
    const b = serial.run(op('restart', 1, true))
    const c = serial.run(op('stop', 1))
    await expect(a).resolves.toBe('start')
    await expect(b).rejects.toThrow('restart')
    await expect(c).resolves.toBe('stop')
    expect(log).toEqual(['start start', 'end start', 'start restart', 'end restart', 'start stop', 'end stop'])
  })

  it('finds the span of a state or a transition', () => {
    const span = (line: number, file = 0) => ({ file, line, col: 3, endLine: line, endCol: 9 })
    const ir: Ir = {
      irVersion: '0.5',
      sources: [{ path: 'c.road', package: null }, { path: 'x.road', package: 'pkg' }],
      machines: {
        M: {
          span: span(1),
          root: {
            id: 'M',
            span: span(1),
            transitions: [],
            states: [{ id: 'A', span: span(5), states: [{ id: 'A.B', span: span(6, 1), states: [], transitions: [] }], transitions: [{ span: span(7) }] }],
          },
        },
      },
    }
    expect(spanOf(ir, 'M', { kind: 'state', id: 'A' })?.line).toBe(5)
    expect(spanOf(ir, 'M', { kind: 'state', id: 'M' })?.line).toBe(1)
    expect(spanOf(ir, 'M', { kind: 'transition', state: 'A', index: 0 })?.line).toBe(7)
    expect(spanOf(ir, 'M', { kind: 'transition', state: 'A', index: 1 })).toBeUndefined()
    expect(spanOf(ir, 'toString', { kind: 'state', id: 'A' })).toBeUndefined()
    expect(sourcePath(ir, span(5))).toBe('c.road')
    expect(sourcePath(ir, span(6, 1))).toBeUndefined()
  })

  it('looks the command line up on PATH, or takes a path', async () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'rb-path-'))
    try {
      const exe = path.join(dir, 'roadbook')
      fs.writeFileSync(exe, '#!/bin/sh\n', { mode: 0o755 })
      fs.writeFileSync(path.join(dir, 'plain'), '', { mode: 0o644 })
      expect(await resolveExecutable('roadbook', undefined, { PATH: `/nonexistent${path.delimiter}${dir}` })).toBe(exe)
      expect(await resolveExecutable('roadbook', undefined, { PATH: '/nonexistent' })).toBeUndefined()
      expect(await resolveExecutable('${workspaceFolder}/roadbook', dir, {})).toBe(exe)
      expect(await resolveExecutable('./roadbook', dir, {})).toBe(exe)
      expect(await resolveExecutable(path.join(dir, 'plain'), undefined, {})).toBeUndefined()
    } finally {
      fs.rmSync(dir, { recursive: true })
    }
  })
})

describe('without the command line', () => {
  it('says that the command cannot run', async () => {
    const r = await compileChart(path.join(os.tmpdir(), 'no-such-roadbook'), checkout)
    expect(r).toEqual({ kind: 'fatal', message: expect.stringContaining('cannot run') })
  })
})

const roadbook = builtRoadbook()

describe.skipIf(!roadbook)('with the command line', () => {
  it('compiles the chart on disk', async () => {
    const r = await compileChart(roadbook!, checkout)
    expect(r.kind).toBe('ok')
    if (r.kind !== 'ok') return
    expect(r.entry).toBe('examples/shop/checkout.road')
    expect(r.ir.sources[0]).toEqual({ path: 'examples/shop/checkout.road', package: null })
    expect(Object.keys(r.ir.machines)).toEqual(['Checkout'])
  })

  it('gives the errors of the saved file', async () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'rb-errors-'))
    try {
      fs.writeFileSync(path.join(dir, 'roadbook.toml'), '')
      fs.copyFileSync(path.join(repo, 'examples/shop/types.road'), path.join(dir, 'types.road'))
      fs.writeFileSync(path.join(dir, 'checkout.road'), fs.readFileSync(checkout, 'utf8').replace('-> Charged', '-> Chargd'))
      const r = await compileChart(roadbook!, path.join(dir, 'checkout.road'))
      expect(r.kind).toBe('errors')
      if (r.kind !== 'errors') return
      expect(r.diagnostics).toEqual([
        expect.objectContaining({ code: 'RB0201', severity: 'error', file: 'checkout.road', line: 14, help: 'did you mean `Charged`?' }),
      ])
    } finally {
      fs.rmSync(dir, { recursive: true })
    }
  })

})
