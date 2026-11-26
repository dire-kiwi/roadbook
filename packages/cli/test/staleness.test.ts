// The committed WebAssembly module must be what the compiler crates build now; the check
// needs no Rust, and fails when a compiler source changes.
import { spawnSync } from 'node:child_process'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterAll, expect, it } from 'vitest'

const here = path.dirname(new URL(import.meta.url).pathname)
const repo = path.resolve(here, '../../..')
const build = (await import('../build.mjs')) as { staleness(): string | undefined; CRATES: string[] }

it('the committed wasm/ is current (rebuild: pnpm --filter @roadbook/cli build)', () => {
  expect(build.staleness()).toBeUndefined()
})

const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'roadbook-cli-stale-'))
afterAll(() => fs.rmSync(tmp, { recursive: true, force: true }))

/** A copy of what the check reads, in a temporary directory, and its verdict there. */
function copy(name: string) {
  const dir = path.join(tmp, name)
  const files = ['Cargo.toml', 'Cargo.lock', 'packages/cli/build.mjs', 'packages/cli/wasm', ...build.CRATES.flatMap((c) => [`crates/${c}/Cargo.toml`, `crates/${c}/src`])]
  for (const f of files) fs.cpSync(path.join(repo, f), path.join(dir, f), { recursive: true })
  const check = () => spawnSync(process.execPath, [path.join(dir, 'packages/cli/build.mjs'), '--check'], { encoding: 'utf8' })
  return { dir, check }
}

it('the check passes on an unchanged copy, and fails when a compiler source changes', () => {
  const { dir, check } = copy('source')
  expect(check().status).toBe(0)
  fs.appendFileSync(path.join(dir, 'crates/roadbook-compiler/src/lib.rs'), '\n// changed\n')
  const out = check()
  expect(out.status).toBe(1)
  expect(out.stderr).toContain('packages/cli/wasm is stale (a compiler source changed since it was built)')
})

it('fails when a source file is added, a dependency changes in Cargo.lock, or the module is edited', () => {
  const added = copy('added')
  fs.writeFileSync(path.join(added.dir, 'crates/roadbook-syntax/src/new.rs'), '')
  expect(added.check().status).toBe(1)

  const lock = copy('lock')
  const lockFile = path.join(lock.dir, 'Cargo.lock')
  const text = fs.readFileSync(lockFile, 'utf8')
  fs.writeFileSync(lockFile, text.replace(/(name = "serde_json"\nversion = ")[^"]+/, '$10.0.0'))
  const out = lock.check()
  expect(out.status).toBe(1)
  expect(out.stderr).toContain('Cargo.lock no longer has serde_json')

  const edited = copy('edited')
  fs.appendFileSync(path.join(edited.dir, 'packages/cli/wasm/roadbook_wasm_bg.wasm'), 'x')
  expect(edited.check().status).toBe(1)
})
