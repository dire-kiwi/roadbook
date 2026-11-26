// Compiling a chart for the preview: the helpers, and the real command line when one is built
// (ROADBOOK_BIN, or target/debug/roadbook under CARGO_TARGET_DIR or the repository).

import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'
import { projectPath, projectRoot, resolveExecutable } from '../src/cli.ts'
import { compileChart, parseCheck, renameInText, renameSource, tempFileFor } from '../src/preview/compile.ts'
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

  it('names the temporary file next to the chart, hidden and not *.road', () => {
    const t = tempFileFor('/p/shop/checkout.road', 42, 7)
    expect(t).toBe(path.join('/p/shop', '.checkout.road.preview-42-7'))
    expect(t.endsWith('.road')).toBe(false)
  })

  it('renames the temporary file back to the chart in the IR, the diagnostics and text', () => {
    const ir = { irVersion: '0.2', sources: [{ path: 'a/.c.road.preview-1-1', package: null }, { path: 'a/.c.road.preview-1-1', package: 'pkg' }], machines: {} }
    expect(renameSource(ir, 'a/.c.road.preview-1-1', 'a/c.road').sources).toEqual([
      { path: 'a/c.road', package: null },
      { path: 'a/.c.road.preview-1-1', package: 'pkg' },
    ])
    expect(renameInText('--> a/.c.road.preview-1-1:3:4', 'a/.c.road.preview-1-1', 'a/c.road')).toBe('--> a/c.road:3:4')
    const json = JSON.stringify({
      diagnostics: [
        { code: 'RB0201', severity: 'error', message: 'unknown target state `X`', file: 'a/.c.road.preview-1-1', line: 3, col: 4, endLine: 3, endCol: 5, help: null },
        { code: 'RB0901', severity: 'warning', message: 'w', file: 'a/types.road', line: 1, col: 1, endLine: 1, endCol: 2, help: 'h' },
      ],
    })
    expect(parseCheck(json, 'a/.c.road.preview-1-1', 'a/c.road')).toEqual([
      { code: 'RB0201', severity: 'error', message: 'unknown target state `X`', file: 'a/c.road', line: 3, col: 4, endLine: 3, endCol: 5, help: null },
      { code: 'RB0901', severity: 'warning', message: 'w', file: 'a/types.road', line: 1, col: 1, endLine: 1, endCol: 2, help: 'h' },
    ])
    expect(parseCheck('not json', 'x', 'y')).toBeUndefined()
  })

  it('finds the span of a state or a transition', () => {
    const span = (line: number, file = 0) => ({ file, line, col: 3, endLine: line, endCol: 9 })
    const ir: Ir = {
      irVersion: '0.2',
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

  it('compiles unsaved text, with its imports, and leaves no file behind', async () => {
    const before = fs.readdirSync(path.dirname(checkout))
    const text = fs.readFileSync(checkout, 'utf8').replace('state Backoff {', 'state Waiting {').replaceAll('-> Backoff', '-> Waiting')
    const r = await compileChart(roadbook!, checkout, text)
    expect(r.kind).toBe('ok')
    if (r.kind !== 'ok') return
    expect(r.ir.sources.map((s) => s.path)).toEqual(['examples/shop/checkout.road', 'examples/shop/types.road'])
    expect(r.irText).toContain('"Checkout.Waiting"')
    expect(fs.readdirSync(path.dirname(checkout))).toEqual(before)
  })

  it('gives the errors of unsaved text, named after the chart', async () => {
    const text = fs.readFileSync(checkout, 'utf8').replace('-> Charged', '-> Chargd')
    const r = await compileChart(roadbook!, checkout, text)
    expect(r.kind).toBe('errors')
    if (r.kind !== 'errors') return
    expect(r.diagnostics).toEqual([
      expect.objectContaining({ code: 'RB0201', severity: 'error', file: 'examples/shop/checkout.road', line: 14, help: 'did you mean `Charged`?' }),
    ])
    expect(r.diagnostics[0]!.message).not.toContain('.preview-')
  })

})
