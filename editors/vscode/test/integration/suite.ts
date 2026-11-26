// Runs inside VS Code (test/integration/run.mjs starts it). The workspace holds `broken.road`,
// a copy of the shop's checkout chart with an unknown target state, and its `types.road`.

import assert from 'node:assert/strict'
import fs from 'node:fs'
import path from 'node:path'
import * as vscode from 'vscode'
import type { Api } from '../../src/extension.ts'
import type { PreviewEvent } from '../../src/preview/preview.ts'
import type { Problems, ViewState } from '../../src/preview/protocol.ts'

const repo = process.env.ROADBOOK_TEST_REPO!
const lsp = process.env.ROADBOOK_TEST_LSP === '1'

function waitFor<T>(event: vscode.Event<T>, matches: (e: T) => boolean, what: string, ms = 60_000): Promise<T> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      sub.dispose()
      reject(new Error(`timed out after ${ms} ms waiting for ${what}`))
    }, ms)
    const sub = event((e) => {
      if (!matches(e)) return
      clearTimeout(timer)
      sub.dispose()
      resolve(e)
    })
  })
}

async function step(name: string, body: () => Promise<void>) {
  const t = Date.now()
  await body()
  console.log(`  ok  ${name} (${Date.now() - t} ms)`)
}

export async function run(): Promise<void> {
  const ext = vscode.extensions.getExtension<Api>('roadbook.roadbook-vscode')
  assert.ok(ext, 'the extension is loaded')
  const csp: PreviewEvent[] = []
  let api: Api
  const checkout = vscode.Uri.file(path.join(repo, 'examples/shop/checkout.road'))
  const workspace = vscode.workspace.workspaceFolders![0]!.uri
  const broken = vscode.Uri.joinPath(workspace, 'broken.road')
  const on = (uri: vscode.Uri, kind: PreviewEvent['kind']) => (e: PreviewEvent) => e.uri === uri.toString() && e.kind === kind

  console.log('Roadbook VS Code integration test')

  await step('a .road file has the roadbook language', async () => {
    const doc = await vscode.workspace.openTextDocument(checkout)
    assert.equal(doc.languageId, 'roadbook')
    await vscode.window.showTextDocument(doc)
    api = await ext.activate()
    api.previews.events.event((e) => {
      if (e.kind === 'csp') csp.push(e)
      if (e.kind === 'csp' || e.kind === 'error') console.log(`      preview ${e.kind}: ${JSON.stringify(e.detail)}`)
    })
  })

  await step('the preview draws the chart', async () => {
    const drawn = waitFor(api.previews.events.event, on(checkout, 'rendered'), 'the preview of checkout.road')
    await vscode.commands.executeCommand('roadbook.showPreviewToSide')
    await drawn
    assert.ok(api.previews.get(checkout), 'a preview of checkout.road is open')
  })

  await step('a state clicked in the preview reveals its line', async () => {
    const preview = api.previews.get(checkout)!
    const line = fs.readFileSync(checkout.fsPath, 'utf8').split('\n').findIndex((l) => l.includes('state Backoff {'))
    const editor = await preview.reveal('Checkout', { kind: 'state', id: 'Checkout.Backoff' })
    assert.ok(editor, 'an editor shows the state')
    assert.equal(editor.document.uri.toString(), checkout.toString())
    assert.equal(editor.selection.active.line, line)
    // A transition: `after retryDelay(attempts) -> Charging`, Backoff's only one.
    const t = await preview.reveal('Checkout', { kind: 'transition', state: 'Checkout.Backoff', index: 0 })
    assert.equal(t?.selection.active.line, line + 2)
  })

  const doc = await vscode.workspace.openTextDocument(broken)
  await vscode.window.showTextDocument(doc, vscode.ViewColumn.One)

  await step(lsp ? 'the language server reports the error' : 'diagnostics SKIPPED: `roadbook lsp` is not available', async () => {
    if (!lsp) return
    const has = () => vscode.languages.getDiagnostics(broken).some((d) => d.severity === vscode.DiagnosticSeverity.Error && /Chargd/.test(d.message))
    if (!has()) await waitFor(vscode.languages.onDidChangeDiagnostics, has, 'diagnostics for broken.road', 30_000)
    assert.ok(api.cli.status === true, `the server runs (${api.cli.status})`)
  })

  await step('the preview of a broken chart shows its errors', async () => {
    const problems = waitFor(api.previews.events.event, on(broken, 'problems'), 'the problems of broken.road')
    await vscode.commands.executeCommand('roadbook.showPreviewToSide')
    const p = (await problems).detail as Problems
    assert.equal(p.entry, 'broken.road')
    assert.equal(p.stale, false)
    assert.deepEqual(
      p.diagnostics.map((d) => [d.code, d.file, d.line]),
      [['RB0201', 'broken.road', 14]],
    )
  })

  await step(lsp ? 'the preview follows unsaved changes' : 'unsaved changes SKIPPED: `roadbook lsp` is not available', async () => {
    if (!lsp) return
    const drawn = waitFor(api.previews.events.event, on(broken, 'rendered'), 'the preview of the fixed, unsaved broken.road')
    // As if the reader had selected a state: the new page selects it again.
    const selection = { kind: 'state', id: 'Checkout.Backoff' } as const
    await api.previews.get(broken)!.receive({ type: 'view', view: { machine: 'Checkout', selection } })
    const at = doc.getText().indexOf('Chargd')
    const edit = new vscode.WorkspaceEdit()
    edit.replace(broken, new vscode.Range(doc.positionAt(at), doc.positionAt(at + 'Chargd'.length)), 'Charged')
    assert.ok(await vscode.workspace.applyEdit(edit))
    assert.ok(doc.isDirty)
    const view = ((await drawn).detail as { view: ViewState }).view
    assert.deepEqual(view, { machine: 'Checkout', selection }, 'the selection is restored')
    // Broken again: the errors are shown over the last diagram.
    const problems = waitFor(api.previews.events.event, on(broken, 'problems'), 'the problems of the unsaved broken.road')
    const again = new vscode.WorkspaceEdit()
    again.insert(broken, doc.positionAt(doc.getText().indexOf('initial Charging')), 'initial Nowhere\n  // ')
    assert.ok(await vscode.workspace.applyEdit(again))
    const p = (await problems).detail as Problems
    assert.equal(p.stale, true)
    assert.ok(p.diagnostics.length > 0)
    assert.ok(fs.readdirSync(workspace.fsPath).every((f) => !f.includes('.preview-')), 'no temporary file is made')
    await vscode.commands.executeCommand('workbench.action.files.revert')
  })

  await step(lsp ? 'the preview follows unsaved changes to a file the chart imports' : 'imported files SKIPPED: `roadbook lsp` is not available', async () => {
    if (!lsp) return
    // broken.road, fixed and saved, draws.
    const fixed = waitFor(api.previews.events.event, on(broken, 'rendered'), 'the preview of the fixed broken.road')
    const fix = new vscode.WorkspaceEdit()
    const at = doc.getText().indexOf('Chargd')
    fix.replace(broken, new vscode.Range(doc.positionAt(at), doc.positionAt(at + 'Chargd'.length)), 'Charged')
    assert.ok(await vscode.workspace.applyEdit(fix))
    await doc.save()
    await fixed
    // An unsaved edit to types.road, which broken.road imports, breaks it.
    const typesUri = vscode.Uri.joinPath(workspace, 'types.road')
    const types = await vscode.workspace.openTextDocument(typesUri)
    await vscode.window.showTextDocument(types, vscode.ViewColumn.One)
    const problems = waitFor(api.previews.events.event, on(broken, 'problems'), 'the problems of broken.road after types.road changed')
    const name = 'Receipt'
    const rename = new vscode.WorkspaceEdit()
    const where = types.getText().indexOf(`type ${name}`) + 'type '.length
    rename.replace(typesUri, new vscode.Range(types.positionAt(where), types.positionAt(where + name.length)), `${name}Renamed`)
    assert.ok(await vscode.workspace.applyEdit(rename))
    assert.ok(types.isDirty)
    const p = (await problems).detail as Problems
    assert.ok(p.diagnostics.some((d) => d.file === 'broken.road' && d.message.includes(name)), JSON.stringify(p.diagnostics))
    const back = waitFor(api.previews.events.event, on(broken, 'rendered'), 'the preview of broken.road once types.road is reverted')
    await vscode.commands.executeCommand('workbench.action.files.revert')
    await back
  })

  await step('the preview opens only charts of the project', async () => {
    const preview = api.previews.get(broken)!
    const shown = await preview.open('types.road', 1, 1, true)
    assert.equal(shown?.document.uri.toString(), vscode.Uri.joinPath(workspace, 'types.road').toString())
    const outside = path.relative(workspace.fsPath, path.join(repo, 'examples/shop/types.road')).split(path.sep).join('/')
    assert.ok(outside.startsWith('../'), outside)
    assert.equal(await preview.open(outside, 1, 1, true), undefined)
    assert.equal(await preview.open('/etc/passwd', 1, 1, true), undefined)
    assert.equal(await preview.open('../settings.json', 1, 1, true), undefined)
  })

  await step(lsp ? 'an untitled chart has diagnostics and a preview' : 'untitled charts SKIPPED: `roadbook lsp` is not available', async () => {
    if (!lsp) return
    const untitled = await vscode.workspace.openTextDocument({ language: 'roadbook', content: 'machine M() {\n  initial A\n  state A { always -> Dne }\n}\n' })
    await vscode.window.showTextDocument(untitled, vscode.ViewColumn.One)
    const has = () => vscode.languages.getDiagnostics(untitled.uri).some((d) => /Dne/.test(d.message))
    if (!has()) await waitFor(vscode.languages.onDidChangeDiagnostics, has, 'diagnostics for the untitled chart', 30_000)
    const drawn = waitFor(api.previews.events.event, on(untitled.uri, 'rendered'), 'the preview of the untitled chart')
    const fix = new vscode.WorkspaceEdit()
    const at = untitled.getText().indexOf('Dne')
    fix.replace(untitled.uri, new vscode.Range(untitled.positionAt(at), untitled.positionAt(at + 3)), 'B }\n  final state B {')
    assert.ok(await vscode.workspace.applyEdit(fix))
    api.previews.showToSide(untitled.uri)
    await drawn
  })

  await step(lsp ? 'restarts and stops asked for while the server starts are done in order' : 'restarts SKIPPED: `roadbook lsp` is not available', async () => {
    if (!lsp) return
    const first = api.cli.restart()
    const second = api.cli.restart()
    await Promise.all([first, second])
    assert.equal(api.cli.status, true)
    assert.ok(api.cli.running(), 'a server runs after two restarts')
    const starting = api.cli.restart()
    const stopping = api.cli.stop()
    await Promise.all([starting, stopping])
    assert.equal(api.cli.status, 'stopped')
    assert.equal(api.cli.running(), undefined, 'no server runs after a stop asked for during a start')
    await api.cli.start()
    assert.equal(api.cli.status, true)
  })

  assert.deepEqual(csp, [], 'the preview broke its Content-Security-Policy')
  console.log('  ok  no Content-Security-Policy violations')
}
