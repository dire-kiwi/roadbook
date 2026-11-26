// Builds the extension into dist/:
//   dist/extension.js       the extension host side (CommonJS, `vscode` external), with the
//                           language client bundled in, so the packaged extension needs no
//                           node_modules
//   dist/bridge.js          the preview's own script in the webview, next to the viewer
//   dist/roadbook-viewer.js the viewer bundle `roadbook viz` embeds, copied from
//                           crates/roadbook-cli/assets (built by packages/viewer/build.mjs)
// `--tests` also builds the integration suite that runs inside VS Code: dist/test/suite.js.

import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import * as esbuild from 'esbuild'

const here = path.dirname(fileURLToPath(import.meta.url))
const dist = path.join(here, 'dist')
export const VIEWER = path.resolve(here, '../../crates/roadbook-cli/assets/roadbook-viewer.js')

const node = { bundle: true, platform: 'node', format: 'cjs', target: 'node20', external: ['vscode'], logLevel: 'warning' }

export async function build({ tests = false } = {}) {
  fs.mkdirSync(dist, { recursive: true })
  await esbuild.build({ ...node, minify: true, entryPoints: [path.join(here, 'src/extension.ts')], outfile: path.join(dist, 'extension.js') })
  await esbuild.build({
    entryPoints: [path.join(here, 'src/webview/bridge.ts')],
    outfile: path.join(dist, 'bridge.js'),
    bundle: true,
    platform: 'browser',
    format: 'iife',
    target: 'es2020',
    logLevel: 'warning',
  })
  fs.copyFileSync(VIEWER, path.join(dist, 'roadbook-viewer.js'))
  if (tests) {
    await esbuild.build({ ...node, entryPoints: [path.join(here, 'test/integration/suite.ts')], outfile: path.join(dist, 'test/suite.js') })
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  await build({ tests: process.argv.includes('--tests') })
  console.error(`built ${path.relative(process.cwd(), dist) || '.'}`)
}
