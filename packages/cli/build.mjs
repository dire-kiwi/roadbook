// Builds the compiler as WebAssembly into wasm/, which is committed so that the package works
// without a Rust toolchain:
//
//   wasm/roadbook_wasm_bg.wasm   the module (crates/roadbook-wasm and the compiler crates)
//   wasm/roadbook_wasm.js, .d.ts  its glue, made by the `wasm-bindgen` command
//   wasm/SOURCES                  what it was built from (below)
//
// Rebuild and commit whenever a compiler crate changes: `pnpm --filter @dire-kiwi/roadbook-cli build`.
// It needs the `wasm32-unknown-unknown` target (`rustup target add wasm32-unknown-unknown`) and
// the `wasm-bindgen` command of the version crates/roadbook-wasm pins
// (`cargo install wasm-bindgen-cli --version <it>`).
//
// `node build.mjs --check` (and the tests, and scripts/check-all.sh) fail when wasm/ is stale.
// It needs no Rust: wasm/SOURCES holds the sha256 of the sources (every file under `src/` of
// the crates the module is built from, their `Cargo.toml`, the workspace's `Cargo.toml`, and
// this script) and the crates.io packages the build used, which must still be in Cargo.lock.

import { execFileSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const here = path.dirname(fileURLToPath(import.meta.url))
const repo = path.resolve(here, '../..')
export const OUT = path.join(here, 'wasm')
export const SOURCES = path.join(OUT, 'SOURCES')

/** The workspace crates the module is built from. */
export const CRATES = ['roadbook-wasm', 'roadbook-compiler', 'roadbook-syntax', 'roadbook-ir', 'roadbook-gen-ts']

/** The source files, relative to the repository root, sorted. */
function sourceFiles() {
  const out = ['Cargo.toml', 'packages/cli/build.mjs']
  const walk = (/** @type {string} */ dir) => {
    for (const e of fs.readdirSync(path.join(repo, dir), { withFileTypes: true })) {
      const p = `${dir}/${e.name}`
      if (e.isDirectory()) walk(p)
      else out.push(p)
    }
  }
  for (const c of CRATES) {
    out.push(`crates/${c}/Cargo.toml`)
    walk(`crates/${c}/src`)
  }
  return out.sort()
}

/** sha256 over each source file's path and bytes, in path order: `path NUL bytes NUL`. */
export function sourcesHash() {
  const h = createHash('sha256')
  for (const f of sourceFiles()) {
    h.update(f + '\0')
    h.update(fs.readFileSync(path.join(repo, f)))
    h.update('\0')
  }
  return h.digest('hex')
}

/** The `name version` of each `[[package]]` of Cargo.lock. */
function lockPackages() {
  const text = fs.readFileSync(path.join(repo, 'Cargo.lock'), 'utf8')
  const out = new Set()
  for (const m of text.matchAll(/\[\[package\]\]\nname = "([^"]+)"\nversion = "([^"]+)"/g)) out.add(`${m[1]} ${m[2]}`)
  return out
}

/** Why wasm/ is stale, or `undefined` if it is current. */
export function staleness() {
  if (!fs.existsSync(SOURCES)) return 'wasm/SOURCES is missing'
  const lines = fs.readFileSync(SOURCES, 'utf8').split('\n')
  const hash = lines.find((l) => l.startsWith('sources sha256:'))?.slice('sources sha256:'.length)
  if (hash !== sourcesHash()) return 'a compiler source changed since it was built'
  const locked = lockPackages()
  const missing = lines.filter((l) => l.startsWith('crate ')).map((l) => l.slice('crate '.length)).filter((p) => !locked.has(p))
  if (missing.length > 0) return `Cargo.lock no longer has ${missing.join(', ')}`
  for (const f of ['roadbook_wasm_bg.wasm', 'roadbook_wasm.js']) {
    const want = lines.find((l) => l.startsWith(`${f} sha256:`))?.slice(`${f} sha256:`.length)
    const file = path.join(OUT, f)
    if (!fs.existsSync(file) || createHash('sha256').update(fs.readFileSync(file)).digest('hex') !== want) {
      return `wasm/${f} is not the file that was built`
    }
  }
  return undefined
}

/** The crates.io packages (`name version`) the module is built from, by `cargo metadata`. */
/** @param {string} target @returns {string[]} */
function registryCrates(target) {
  /** @type {{ packages: { id: string, name: string, version: string, source: string | null }[], resolve: { nodes: { id: string, deps: { pkg: string, dep_kinds: { kind: string | null }[] }[] }[] } }} */
  const meta = JSON.parse(
    execFileSync('cargo', ['metadata', '--format-version', '1', '--filter-platform', target], { cwd: repo, encoding: 'utf8', maxBuffer: 1 << 28 }),
  )
  const byId = new Map(meta.packages.map((p) => [p.id, p]))
  const nodes = new Map(meta.resolve.nodes.map((n) => [n.id, n]))
  const root = meta.packages.find((p) => p.name === 'roadbook-wasm')
  const seen = new Set()
  const visit = (/** @type {string} */ id) => {
    if (seen.has(id)) return
    seen.add(id)
    for (const d of nodes.get(id)?.deps ?? []) {
      if (d.dep_kinds.some((k) => k.kind === null)) visit(d.pkg)
    }
  }
  if (!root) throw new Error('cargo metadata has no roadbook-wasm')
  visit(root.id)
  return [...seen]
    .flatMap((id) => byId.get(id) ?? [])
    .filter((p) => p.source?.startsWith('registry+'))
    .map((p) => `${p.name} ${p.version}`)
    .sort()
}

function build() {
  const target = 'wasm32-unknown-unknown'
  const targetDir = process.env.CARGO_TARGET_DIR ?? path.join(repo, 'target')
  // Small rather than fast: the profile is given here, so the workspace's release profile is
  // left alone.
  const profile = { 'opt-level': "'s'", lto: 'true', 'codegen-units': '1', panic: "'abort'" }
  const config = Object.entries(profile).flatMap(([k, v]) => ['--config', `profile.release.${k}=${v}`])
  execFileSync('cargo', ['build', '-p', 'roadbook-wasm', '--release', '--target', target, ...config], { cwd: repo, stdio: 'inherit' })
  const cargoToml = fs.readFileSync(path.join(repo, 'crates/roadbook-wasm/Cargo.toml'), 'utf8')
  const pinned = /wasm-bindgen = "=([^"]+)"/.exec(cargoToml)?.[1]
  const cli = execFileSync('wasm-bindgen', ['--version'], { encoding: 'utf8' }).trim()
  if (cli !== `wasm-bindgen ${pinned}`) {
    throw new Error(`crates/roadbook-wasm pins wasm-bindgen ${pinned}, but the command is ${cli}: cargo install wasm-bindgen-cli --version ${pinned}`)
  }
  fs.rmSync(OUT, { recursive: true, force: true })
  execFileSync(
    'wasm-bindgen',
    [
      '--target',
      'experimental-nodejs-module',
      '--remove-name-section',
      '--remove-producers-section',
      '--out-dir',
      OUT,
      path.join(targetDir, target, 'release/roadbook_wasm.wasm'),
    ],
    { stdio: 'inherit' },
  )
  // The package is ESM already; wasm-bindgen's package.json would only repeat it.
  fs.rmSync(path.join(OUT, 'package.json'), { force: true })
  const digest = (/** @type {string} */ f) => createHash('sha256').update(fs.readFileSync(path.join(OUT, f))).digest('hex')
  const lines = [
    '# What wasm/ was built from, by packages/cli/build.mjs. Do not edit.',
    `sources sha256:${sourcesHash()}`,
    `roadbook_wasm_bg.wasm sha256:${digest('roadbook_wasm_bg.wasm')}`,
    `roadbook_wasm.js sha256:${digest('roadbook_wasm.js')}`,
    ...registryCrates(target).map((c) => `crate ${c}`),
  ]
  fs.writeFileSync(SOURCES, lines.join('\n') + '\n')
  const size = fs.statSync(path.join(OUT, 'roadbook_wasm_bg.wasm')).size
  console.error(`wrote ${path.relative(process.cwd(), OUT)} (module ${(size / 1024).toFixed(0)} kB)`)
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  if (process.argv.includes('--check')) {
    const why = staleness()
    if (why) {
      console.error(`packages/cli/wasm is stale (${why}): run \`pnpm --filter @dire-kiwi/roadbook-cli build\` and commit it`)
      process.exit(1)
    }
    console.error('packages/cli/wasm is up to date')
  } else {
    build()
  }
}
