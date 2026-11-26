// The Roadbook extension: the `roadbook` language (highlighting from the TextMate grammar),
// the language client for `roadbook lsp`, and the live preview.

import * as vscode from 'vscode'
import { Cli } from './client.ts'
import { PreviewManager } from './preview/preview.ts'

/** What `activate` returns: for the integration tests. */
export interface Api {
  cli: Cli
  previews: PreviewManager
}

export function activate(context: vscode.ExtensionContext): Api {
  const output = vscode.window.createOutputChannel('Roadbook', { log: true })
  const cli = new Cli(output)
  const previews = new PreviewManager(context.extensionUri, () => cli.executable(), () => cli.running(), (m) => output.appendLine(m))
  context.subscriptions.push(
    output,
    cli,
    previews,
    vscode.commands.registerCommand('roadbook.showPreviewToSide', (uri?: unknown) => {
      const target = uri instanceof vscode.Uri ? uri : vscode.window.activeTextEditor?.document.uri
      if (!target) return void vscode.window.showInformationMessage('Roadbook: open a .road file to preview it.')
      previews.showToSide(target)
    }),
    vscode.commands.registerCommand('roadbook.restartServer', () => cli.restart()),
    vscode.workspace.onDidChangeConfiguration((e) => {
      if (e.affectsConfiguration('roadbook.path')) void cli.restart()
    }),
    // A trusted workspace's own `roadbook.path` applies from now on (see package.json,
    // `untrustedWorkspaces`).
    vscode.workspace.onDidGrantWorkspaceTrust(() => void cli.restart()),
  )
  void cli.start()
  return { cli, previews }
}

export function deactivate() {}
