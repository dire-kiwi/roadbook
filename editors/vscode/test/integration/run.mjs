// Runs test/integration/suite.ts in a real VS Code with the extension loaded from source.
//
// It skips, saying why, unless it can run:
// - It needs the command line built: ROADBOOK_BIN, or debug/roadbook under CARGO_TARGET_DIR
//   or the repository's target/.
// - It needs VS Code. With ROADBOOK_VSCODE_TEST=1 it downloads it (the latest stable, or
//   ROADBOOK_VSCODE_VERSION) into ROADBOOK_VSCODE_CACHE (default editors/vscode/.vscode-test);
//   otherwise it uses a copy already there. ROADBOOK_VSCODE_TEST=0 always skips.
// - On Linux it runs VS Code with Chromium's headless Ozone platform: no window, no display
//   needed. ROADBOOK_VSCODE_HEADLESS=0 uses the display instead (DISPLAY or WAYLAND_DISPLAY),
//   or runs itself under xvfb-run if there is none.
// The diagnostics step needs `roadbook lsp`; without it, that step skips and says so.

import { spawnSync } from 'node:child_process'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { downloadAndUnzipVSCode, runTests } from '@vscode/test-electron'
import { build } from '../../build.mjs'

const here = path.dirname(fileURLToPath(import.meta.url))
const extensionPath = path.resolve(here, '../..')
const repo = path.resolve(extensionPath, '../..')
const env = process.env

function skip(why) {
  console.log(`VS Code integration test skipped: ${why}`)
  process.exit(0)
}

if (env.ROADBOOK_VSCODE_TEST === '0') skip('ROADBOOK_VSCODE_TEST=0')

const roadbook = [env.ROADBOOK_BIN, env.CARGO_TARGET_DIR && path.join(env.CARGO_TARGET_DIR, 'debug/roadbook'), path.join(repo, 'target/debug/roadbook')].find(
  (c) => c && fs.existsSync(c),
)
if (!roadbook) skip('no roadbook command line: build it (cargo build -p roadbook-cli) or set ROADBOOK_BIN')

const headless = process.platform === 'linux' && env.ROADBOOK_VSCODE_HEADLESS !== '0'
if (process.platform === 'linux' && !headless && !env.DISPLAY && !env.WAYLAND_DISPLAY) {
  const xvfb = spawnSync('sh', ['-c', 'command -v xvfb-run'], { encoding: 'utf8' }).stdout.trim()
  if (!xvfb || env.ROADBOOK_UNDER_XVFB) skip('ROADBOOK_VSCODE_HEADLESS=0, and there is no display (DISPLAY, WAYLAND_DISPLAY) and no xvfb-run')
  const r = spawnSync(xvfb, ['-a', process.execPath, fileURLToPath(import.meta.url)], { stdio: 'inherit', env: { ...env, ROADBOOK_UNDER_XVFB: '1' } })
  process.exit(r.status ?? 1)
}

const cachePath = path.resolve(env.ROADBOOK_VSCODE_CACHE ?? path.join(extensionPath, '.vscode-test'))
let version = env.ROADBOOK_VSCODE_VERSION ?? 'stable'
if (env.ROADBOOK_VSCODE_TEST !== '1') {
  const cached = fs.existsSync(cachePath) ? fs.readdirSync(cachePath).filter((d) => /^vscode-[a-z0-9-]+-[0-9.]+$/.test(d)) : []
  if (!cached.length) skip(`no VS Code in ${cachePath}; ROADBOOK_VSCODE_TEST=1 downloads one there (about 150 MB)`)
  version = cached.sort().at(-1).replace(/^.*-/, '')
}

const lsp = spawnSync(roadbook, ['lsp', '--help'], { encoding: 'utf8' }).status === 0

// A workspace with a broken copy of the shop's checkout chart, and the setting for the command line.
const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'rb-vscode-'))
const workspace = path.join(tmp, 'workspace')
fs.mkdirSync(path.join(workspace, '.vscode'), { recursive: true })
fs.writeFileSync(path.join(workspace, 'roadbook.toml'), '')
fs.copyFileSync(path.join(repo, 'examples/shop/types.road'), path.join(workspace, 'types.road'))
const checkout = fs.readFileSync(path.join(repo, 'examples/shop/checkout.road'), 'utf8')
if (!checkout.includes('-> Charged')) throw new Error('examples/shop/checkout.road changed: update test/integration/run.mjs')
fs.writeFileSync(path.join(workspace, 'broken.road'), checkout.replace('-> Charged', '-> Chargd'))
fs.writeFileSync(path.join(workspace, '.vscode/settings.json'), JSON.stringify({ 'roadbook.path': path.resolve(roadbook), 'roadbook.preview.debounce': 100 }, null, 2))

let code = 1
try {
  await build({ tests: true })
  const vscodeExecutablePath = await downloadAndUnzipVSCode({ cachePath, version })
  code = await runTests({
    vscodeExecutablePath,
    extensionDevelopmentPath: extensionPath,
    extensionTestsPath: path.join(extensionPath, 'dist/test/suite.js'),
    extensionTestsEnv: { ROADBOOK_TEST_REPO: repo, ROADBOOK_TEST_LSP: lsp ? '1' : '0' },
    launchArgs: [
      workspace,
      '--disable-extensions',
      '--disable-workspace-trust',
      '--skip-welcome',
      '--skip-release-notes',
      '--disable-gpu',
      `--user-data-dir=${path.join(tmp, 'user')}`,
      `--extensions-dir=${path.join(tmp, 'extensions')}`,
      ...(headless ? ['--ozone-platform=headless'] : []),
    ],
  })
} catch (e) {
  console.error('VS Code integration test failed:', e)
} finally {
  fs.rmSync(tmp, { recursive: true, force: true })
}
process.exit(code)
