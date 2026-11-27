// The npm command line prints what the Rust one prints, byte for byte, and exits with the same
// codes: `check --json`, `check`, `export ir`, `fmt` and `gen` on every chart under examples/
// and spec/conformance/charts/, on the whole project, on projects that import packages
// (`[packages]`, `node_modules`, `exports`), and on the errors (missing files, bad
// `roadbook.toml`, unreadable imports, syntax errors, generator errors).
//
// The Rust binary is $ROADBOOK_BIN, or built with `cargo build -p roadbook-cli`; without
// cargo the comparison is skipped.
import { type SpawnSyncReturns, spawnSync } from 'node:child_process'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterAll, describe, expect, it } from 'vitest'

const here = path.dirname(new URL(import.meta.url).pathname)
const repo = path.resolve(here, '../../..')
const cli = path.resolve(here, '../bin/roadbook.js')

function rustBinary(): string | undefined {
  if (process.env.ROADBOOK_BIN) return process.env.ROADBOOK_BIN
  const out = spawnSync('cargo', ['build', '-q', '-p', 'roadbook-cli', '--message-format=json'], {
    cwd: repo,
    encoding: 'utf8',
    maxBuffer: 1 << 28,
    stdio: ['ignore', 'pipe', 'inherit'],
  })
  if (out.error) return undefined
  if (out.status !== 0) throw new Error('cargo build -p roadbook-cli failed')
  for (const line of out.stdout.split('\n')) {
    if (!line.startsWith('{')) continue
    const m = JSON.parse(line) as { reason: string; target?: { name: string }; executable?: string | null }
    if (m.reason === 'compiler-artifact' && m.target?.name === 'roadbook' && m.executable) return m.executable
  }
  throw new Error('cargo build -p roadbook-cli built no `roadbook` binary')
}

const rust = rustBinary()
if (!rust) console.warn('skipping the comparison with the Rust command line: cargo is not installed')

type Run = { status: number | null; stdout: string; stderr: string }
const text = (r: SpawnSyncReturns<Buffer>): Run => ({ status: r.status, stdout: r.stdout.toString('utf8'), stderr: r.stderr.toString('utf8') })
const runRust = (args: string[], cwd: string) => text(spawnSync(rust as string, args, { cwd }))
const runNode = (args: string[], cwd: string) => text(spawnSync(process.execPath, [cli, ...args], { cwd }))

/** Runs both in `cwd` and expects the same output and exit code; returns what they printed. */
function same(args: string[], cwd = repo): Run {
  const r = runRust(args, cwd)
  const n = runNode(args, cwd)
  expect(n, `roadbook ${args.join(' ')}`).toEqual(r)
  return r
}

/** Every file under `dir` (not following links), as path → text. */
function tree(dir: string): Map<string, string> {
  const out = new Map<string, string>()
  const walk = (d: string) => {
    for (const e of fs.readdirSync(d, { withFileTypes: true }).sort((a, b) => (a.name < b.name ? -1 : 1))) {
      const p = path.join(d, e.name)
      if (e.isDirectory()) walk(p)
      else if (e.isFile()) out.set(path.relative(dir, p), fs.readFileSync(p, 'utf8'))
    }
  }
  walk(dir)
  return out
}

/** Writes files (path → text or bytes) under `dir`. */
function write(dir: string, files: Record<string, string | Buffer>) {
  for (const [p, content] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(dir, p)), { recursive: true })
    fs.writeFileSync(path.join(dir, p), content)
  }
}

const charts = (() => {
  const out: string[] = []
  const walk = (d: string) => {
    for (const e of fs.readdirSync(path.join(repo, d), { withFileTypes: true })) {
      if (e.name === 'node_modules' || e.name === 'target') continue
      const p = `${d}/${e.name}`
      if (e.isDirectory()) walk(p)
      else if (e.name.endsWith('.road')) out.push(p)
    }
  }
  walk('examples')
  walk('spec/conformance/charts')
  return out.sort()
})()

const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'roadbook-cli-parity-'))
afterAll(() => fs.rmSync(tmp, { recursive: true, force: true }))

describe.skipIf(!rust)('the npm command line prints what the Rust one does', () => {
  it('finds the charts', () => {
    expect(charts.length).toBeGreaterThan(30)
  })

  it.each(charts)('%s: check --json, check, export ir, fmt --check, gen --check', (chart) => {
    for (const args of [['check', '--json'], ['check'], ['export', 'ir'], ['fmt', '--check'], ['gen', '--check']]) {
      same([...args, chart])
    }
  })

  it('the whole project: check --json, check, gen --check, fmt --check', () => {
    const json = same(['check', '--json'])
    expect(json.status).toBe(0)
    expect(JSON.parse(json.stdout).fatal).toBeNull()
    same(['check'])
    same(['check', '--json', '--warn', 'invoke-timeout', 'examples'])
    expect(same(['gen', '--check']).status).toBe(0)
    expect(same(['fmt', '--check', 'examples', 'spec/conformance/charts']).status).toBe(0)
    same(['check', '--json', 'examples/shop', 'examples/orders/orders.road'])
  })

  it('checks: the summary line, --checks and --json, for each verdict', () => {
    const dir = path.join(tmp, 'checks')
    const pay = [
      'fn ok() -> bool',
      '',
      'machine Pay() {',
      '  event Go',
      '  event Back',
      '',
      '  initial Idle',
      '',
      '  state Idle    { on Go if ok() -> Paying }',
      '  state Paying {',
      '    on Back -> Idle',
      '    always -> Charged',
      '  }',
      '  state Charged { on Go -> Done }',
      '  final state Done {}',
      '',
      '  check reachable Idle',
      '  /// Charging needs the payment approved.',
      '  check reachable Charged',
      '  check never Charged',
      '  check Charged only after Paying',
      '  check can finish',
      '}',
      '',
    ].join('\n')
    write(dir, { 'roadbook.toml': '', 'pay.road': pay, 'passing.road': pay.replace('machine Pay', 'machine Passing').replace('  check never Charged\n', '') })
    for (const args of [['check'], ['check', '--checks'], ['check', '--json'], ['check', '--json', '--checks'], ['check', '--checks', 'passing.road']]) {
      same(args, dir)
    }
    const listed = same(['check', '--checks'], dir)
    expect(listed.status).toBe(1)
    expect(listed.stderr).toContain('checks: 4 proved, 4 if guards allow, 1 failed\n')
    expect(same(['check', 'passing.road'], dir).status).toBe(0)
    // The repository's own checks.
    same(['check', '--checks'])
    same(['check', '--json', 'crates/roadbook-compiler/tests/pass'])
  })

  it('gen and fmt write the same files', () => {
    const copies = ['rust', 'node'].map((name) => {
      const dir = path.join(tmp, 'write', name)
      fs.mkdirSync(dir, { recursive: true })
      fs.copyFileSync(path.join(repo, 'roadbook.toml'), path.join(dir, 'roadbook.toml'))
      for (const d of ['examples', 'spec/conformance/charts']) {
        fs.cpSync(path.join(repo, d), path.join(dir, d), {
          recursive: true,
          filter: (src) => !['node_modules', 'target'].includes(path.basename(src)),
        })
      }
      for (const f of tree(dir).keys()) {
        const p = path.join(dir, f)
        if (f.endsWith('.road.ts')) fs.rmSync(p)
        // Unformat every chart: twice the indentation.
        else if (f.endsWith('.road')) fs.writeFileSync(p, fs.readFileSync(p, 'utf8').replace(/^( +)/gm, '$1$1'))
      }
      write(dir, {
        'examples/gone.road.ts': '// Generated by roadbook gen from examples/gone.road\n',
        'examples/broken.road': 'machine Broken( {\n',
      })
      return dir
    })
    const [r, n] = copies as [string, string]
    const both = (args: string[]) => {
      const a = runRust(args, r)
      expect(runNode(args, n), `roadbook ${args.join(' ')}`).toEqual(a)
      expect(tree(n)).toEqual(tree(r))
      return a
    }
    expect(both(['fmt', '--check']).status).toBe(1)
    expect(both(['fmt']).status).toBe(1) // broken.road
    fs.rmSync(path.join(r, 'examples/broken.road'))
    fs.rmSync(path.join(n, 'examples/broken.road'))
    expect(both(['fmt']).status).toBe(0)
    expect(both(['gen', '--check']).status).toBe(1)
    expect(both(['gen']).status).toBe(0)
    expect(both(['gen', ...charts.filter((c) => c.startsWith('spec/'))]).status).toBe(0)
    expect(both(['gen', '--check', 'examples']).status).toBe(1) // the orphan
    // The modules are the committed ones.
    for (const c of charts.filter((c) => c.startsWith('examples/'))) {
      expect(fs.readFileSync(path.join(r, `${c}.ts`), 'utf8')).toBe(fs.readFileSync(path.join(repo, `${c}.ts`), 'utf8'))
    }
  })

  /** The projects of crates/roadbook-cli's test `gen_imports_other_packages_modules_where_they_are`. */
  function packages(dir: string) {
    const peer = (name: string, other: string) =>
      `import { ${other} } from "${other.toLowerCase()}/${other.toLowerCase()}.road"\n\nmachine ${name}(peer: ref ${other}) {\n  event Poke\n  emits Poked\n\n  initial X\n  state X {\n    on Poke -> X  emit Poked\n    on peer.Poked -> X\n  }\n}\n`
    const leaf = (name: string) => `machine ${name}() {\n  initial X\n  final state X {}\n}\n`
    const project = path.join(dir, 'project')
    write(project, {
      'app/order.road': `import { Inv } from "@shop/inv/inv.road"\nimport { Item } from "@shop/inv/types.road"\nimport { Stock } from "stock/stock.road"\nimport { Far } from "far/far.road"\nimport { A } from "a/a.road"\n\nmachine Order(item: Item, i: ref Inv, s: ref Stock, f: ref Far, a: ref A) {\n  initial Done\n  final state Done {}\n}\n`,
      'a/a.road': peer('A', 'B'),
      'b/b.road': peer('B', 'A'),
      'pkgs/inv/charts/inv.road': `import { Item } from "./types.road"\n\n${leaf('Inv')}`,
      'pkgs/inv/charts/types.road': 'type Item { sku: string }\n',
      'pkgs/inv/package.json': '{ "name": "@shop/inv", "type": "module", "exports": { ".": "./index.js", "./*.road": "./charts/*.road", "./*.road.js": "./charts/*.road.js" } }',
      'libs/stock/src/stock.road': leaf('Stock'),
      'libs/stock/package.json': '{ "name": "stock", "type": "module", "exports": { "./*.road": "./src/*.road" } }',
      'package.json': '{ "type": "module" }',
      'roadbook.toml': `[packages]\nstock = "libs/stock"\nfar = "../outside/far"\na = "a"\nb = "b"\n`,
      // A package in node_modules that exports its charts but not their modules.
      'node_modules/hidden/package.json': '{ "name": "hidden", "exports": { "./*.road": "./charts/*.road" } }',
      'node_modules/hidden/charts/h.road': leaf('H'),
      'uses-hidden/u.road': `import { H } from "hidden/h.road"\n\nmachine U(h: ref H) {\n  initial X\n  final state X {}\n}\n`,
      // Not exported at all.
      'node_modules/closed/package.json': '{ "name": "closed", "exports": { "./a.js": "./a.js" } }',
      'node_modules/closed/c.road': leaf('C'),
      'uses-closed/u.road': `import { C } from "closed/c.road"\n\nmachine V(c: ref C) {\n  initial X\n  final state X {}\n}\n`,
    })
    fs.mkdirSync(path.join(project, 'node_modules/@shop'))
    fs.symlinkSync('../../pkgs/inv', path.join(project, 'node_modules/@shop/inv'))
    write(path.join(dir, 'outside'), { 'roadbook.toml': '', 'package.json': '{ "type": "module" }', 'far/far.road': leaf('Far') })
    return project
  }

  it.skipIf(process.platform === 'win32')('packages: [packages], node_modules, exports, a directory outside the project', () => {
    const read = packages(path.join(tmp, 'packages'))
    same(['check', '--json'], read)
    same(['check'], read)
    same(['export', 'ir', 'app/order.road'], read)
    same(['export', 'ir', 'uses-closed/u.road'], read)
    same(['gen', '--check'], read)
    same(['gen', '--check', 'uses-hidden/u.road'], read)
    const [r, n] = ['rust', 'node'].map((name) => packages(path.join(tmp, `packages-${name}`))) as [string, string]
    const a = runRust(['gen'], r)
    expect(a.status).toBe(1) // uses-hidden, uses-closed
    expect(runNode(['gen'], n)).toEqual(a)
    expect(tree(n)).toEqual(tree(r))
    expect(tree(n).has('app/order.road.ts')).toBe(true)
  })

  it('errors: missing paths and imports, bad roadbook.toml, unreadable files, generator errors', () => {
    const dir = path.join(tmp, 'errors')
    write(dir, {
      'roadbook.toml': 'exclude = ["skipped"]\n',
      'skipped/s.road': 'machine S( {\n',
      'missing.road': 'import { X } from "./nowhere.road"\n\nmachine M() {\n  initial A\n  final state A {}\n}\n',
      'utf8.road': Buffer.from([0x6d, 0xff, 0x0a]),
      'bom.road': '﻿machine B() {\n  initial A\n  final state A {}\n}\n',
      'w.road': '// An error whose class would be `never`.\nerror Weird {\n  why: string,\n  name: bool,\n}\n',
      'unformatted.road': 'machine U() {\ninitial A\n  final state A {}\n}\n',
      'sub/ok.road': 'machine Ok() {\n  initial A\n  final state A {}\n}\n',
      'bad/roadbook.toml': 'exclude = "a"\n',
      'bad/a.road': 'machine A() {\n  initial A\n  final state A {}\n}\n',
      'badtable/roadbook.toml': '[packages]\ninventory = examples\n',
    })
    fs.symlinkSync('sub', path.join(dir, 'linked'))
    for (const args of [
      ['check', '--json'],
      ['check'],
      ['check', '--json', 'nope.road'],
      ['check', 'skipped/s.road'],
      ['check', '--json', 'missing.road'],
      ['check', '--json', 'utf8.road'],
      ['check', '--json', 'bom.road'],
      ['check', '--json', 'linked'],
      ['export', 'ir', 'missing.road'],
      ['export', 'ir', 'nope.road'],
      ['export', 'ir', 'sub'],
      ['export', 'ir', 'sub/ok.road'],
      ['fmt', '--check'],
      ['fmt', '--check', 'utf8.road'],
      ['fmt', '--check', 'nope'],
      ['gen', '--check'],
      ['gen', '--check', 'w.road'],
      ['gen', '--check', 'missing.road'],
      ['check', '--json', 'bad/a.road'],
      ['fmt', '--check', 'bad'],
      ['check', '--json', 'badtable'],
    ]) {
      same(args, dir)
    }
    // Usage errors: clap's messages are its own, the exit code is the same.
    for (const args of [['check', '--warn', 'bogus'], ['export', 'ir'], ['fmt', '--nope']]) {
      expect(runNode(args, dir).status).toBe(runRust(args, dir).status)
    }
  })
})
