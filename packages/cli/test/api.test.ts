// The programmatic API: on the repository's project, and on files held in memory.
import fs from 'node:fs'
import path from 'node:path'
import { describe, expect, it } from 'vitest'
import { RoadbookError, check, compile, format, gen, version } from '../src/index.js'

const repo = path.resolve(path.dirname(new URL(import.meta.url).pathname), '../../..')

const hello = 'machine Hello() -> string {\n  initial Done\n  final state Done { output "hi" }\n}\n'

describe('on disk', () => {
  it('compiles an entry that imports a [packages] package, finding roadbook.toml', () => {
    const out = compile('examples/orders/orders.road', { cwd: repo })
    expect(out.errors).toBe(0)
    expect(out.ir?.['sources']).toContainEqual({ path: 'inventory.road', package: 'inventory' })
    expect(JSON.parse(out.irJson ?? '')).toEqual(out.ir)
  })

  it('checks the examples', () => {
    const out = check(['examples'], { cwd: repo })
    expect(out.report).toEqual({ diagnostics: [], errors: 0, warnings: 0, fatal: null })
    expect(out.json).toBe(JSON.stringify(out.report, null, 2))
  })

  it('generates the committed module', () => {
    const out = gen(path.join(repo, 'examples/shop/checkout.road'))
    expect(out.code).toBe(fs.readFileSync(path.join(repo, 'examples/shop/checkout.road.ts'), 'utf8'))
  })

  it('reads a file from `files` in front of the disk', () => {
    const out = check(['examples/shop/checkout.road'], { cwd: repo, files: { 'examples/shop/types.road': 'type Order {}\n' } })
    expect(out.errors).toBeGreaterThan(0)
    expect(out.report.diagnostics[0]?.file).toBe('examples/shop/checkout.road')
  })

  it('throws a RoadbookError for a path that does not exist', () => {
    expect(() => check(['nope.road'], { cwd: repo })).toThrow(RoadbookError)
  })
})

describe('in memory', () => {
  const files = {
    'a.road': `import { Hello } from "./b.road"\n\nmachine A(h: ref Hello) {\n  initial X\n  final state X {}\n}\n`,
    'b.road': hello,
  }

  it('compiles, checks and generates with no file system', () => {
    const out = compile('a.road', { cwd: '/virtual', files, disk: false })
    expect(out.errors).toBe(0)
    expect(Object.keys(out.ir?.['machines'] as object)).toEqual(['A', 'Hello'])
    expect(check([], { cwd: '/virtual', files, disk: false }).errors).toBe(0)
    expect(gen('b.road', { cwd: '/virtual', files, disk: false }).code).toContain('export const Hello = Roadbook.make<HelloDef>')
  })

  it('reports diagnostics, rendered and as JSON', () => {
    const out = check(['x.road'], { cwd: '/virtual', files: { 'x.road': 'machine X() {\n  initial Nope\n}\n' }, disk: false })
    expect(out.errors).toBe(1)
    expect(out.rendered).toContain('--> x.road:1:9')
    expect(out.report.diagnostics[0]?.code).toMatch(/^RB/)
    const missing = compile('a.road', { cwd: '/virtual', files: { 'a.road': files['a.road'] }, disk: false })
    expect(missing.ir).toBeNull()
    expect(missing.report.diagnostics[0]?.code).toBe('RB0111')
    expect(() => compile('zzz.road', { cwd: '/virtual', files, disk: false })).toThrow(RoadbookError)
  })
})

it('formats', () => {
  expect(format('machine Hello() -> string {\ninitial Done\n  final state Done { output "hi" }\n}\n')).toEqual({ formatted: hello, errors: [] })
  const bad = format('machine M( {\n', 'm.road')
  expect(bad.formatted).toBeNull()
  expect(bad.errors[0]).toContain('--> m.road:1:12')
  expect(version()).toMatch(/^\d+\.\d+\.\d+/)
})
