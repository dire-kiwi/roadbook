// Builds the browser viewer into the one script `roadbook viz` embeds:
//   crates/roadbook-cli/assets/roadbook-viewer.js
// The CLI includes that file at compile time, so `cargo build` needs no Node. Rebuild and
// commit it whenever packages/viewer changes: `pnpm --filter @roadbook/viewer build`.
// `node build.mjs --check` (and the tests) fail when the committed file is stale. The Rust
// test `viewer_asset_is_fresh` checks the header without Node: the sources' hash, the versions
// of elkjs and esbuild in pnpm-lock.yaml, and the hash of the script after the header.

import { createHash } from 'node:crypto'
import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import * as esbuild from 'esbuild'

const here = path.dirname(fileURLToPath(import.meta.url))
export const ASSET = path.resolve(here, '../../crates/roadbook-cli/assets/roadbook-viewer.js')

/** The files the bundle is built from, besides dependencies: every file under src, and this script. */
function sourceFiles() {
  const out = []
  const walk = (dir) => {
    for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
      const p = path.join(dir, e.name)
      if (e.isDirectory()) walk(p)
      else out.push(p)
    }
  }
  walk(path.join(here, 'src'))
  out.push(path.join(here, 'build.mjs'))
  return out.map((p) => path.relative(here, p).split(path.sep).join('/')).sort()
}

/**
 * sha256 over each source file's path and bytes, in path order: `path NUL bytes NUL`. The Rust
 * test `viewer_asset_is_fresh` computes the same, so `cargo test` catches a stale asset too.
 */
export function sourcesHash() {
  const h = createHash('sha256')
  for (const f of sourceFiles()) {
    h.update(f + '\0')
    h.update(fs.readFileSync(path.join(here, f)))
    h.update('\0')
  }
  return h.digest('hex')
}

/** The packages that go into the bundle or make it. */
export const BUNDLE_DEPS = ['elkjs', 'esbuild']

/**
 * `elkjs@0.12.2 esbuild@0.28.2`: the versions pnpm-lock.yaml gives this package's `BUNDLE_DEPS`.
 * Read line by line (the Rust test does the same): in `importers:`, the `packages/viewer:` block.
 */
export function depsText() {
  const lines = fs.readFileSync(path.join(here, '../../pnpm-lock.yaml'), 'utf8').split('\n')
  const start = lines.indexOf('  packages/viewer:')
  if (start < 0) throw new Error('pnpm-lock.yaml has no packages/viewer importer')
  let end = start + 1
  while (end < lines.length && lines[end].startsWith('   ')) end++
  const block = lines.slice(start + 1, end)
  return BUNDLE_DEPS.map((dep) => {
    const at = block.indexOf(`      ${dep}:`)
    const version = at < 0 ? undefined : block.slice(at + 1, at + 3).find((l) => l.startsWith('        version: '))
    if (!version) throw new Error(`pnpm-lock.yaml: no version of ${dep} for packages/viewer`)
    return `${dep}@${version.slice('        version: '.length).trim()}`
  }).join(' ')
}

/** The bundle's text: a header naming the sources hash, the dependencies and the script's hash, then the minified script. */
export async function bundle() {
  const result = await esbuild.build({
    entryPoints: [path.join(here, 'src/app/main.ts')],
    bundle: true,
    format: 'iife',
    platform: 'browser',
    target: 'es2020',
    minify: true,
    legalComments: 'inline',
    charset: 'utf8',
    write: false,
    logLevel: 'silent',
  })
  const js = result.outputFiles[0].text
  const body = createHash('sha256').update(js).digest('hex')
  return `/* @roadbook/viewer, built by packages/viewer/build.mjs. Do not edit. sources sha256:${sourcesHash()} deps ${depsText()} body sha256:${body} */\n${js}`
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const text = await bundle()
  if (process.argv.includes('--check')) {
    const current = fs.existsSync(ASSET) ? fs.readFileSync(ASSET, 'utf8') : ''
    if (current !== text) {
      console.error(`${path.relative(process.cwd(), ASSET)} is stale: run \`pnpm --filter @roadbook/viewer build\` and commit it`)
      process.exit(1)
    }
    console.error('viewer asset is up to date')
  } else {
    fs.mkdirSync(path.dirname(ASSET), { recursive: true })
    fs.writeFileSync(ASSET, text)
    console.error(`wrote ${path.relative(process.cwd(), ASSET)} (${(text.length / 1024).toFixed(0)} kB)`)
  }
}
