// The language client: runs `roadbook lsp` over stdio. Diagnostics, go to definition,
// references, rename, hover, formatting, symbols and code actions all come from the server.

import path from 'node:path'
import * as vscode from 'vscode'
import { LanguageClient, TransportKind } from 'vscode-languageclient/node'
import type { LanguageClientOptions, ServerOptions } from 'vscode-languageclient/node'
import { hasLsp, resolveExecutable } from './cli.ts'

export class Cli implements vscode.Disposable {
  private client: LanguageClient | undefined
  private starting: Promise<void> | undefined
  /** The setting each warning was shown for, so it is shown once per value. */
  private warned = new Set<string>()
  /** Whether the server is running, for the tests: `true`, or why it is not. */
  status: true | string = 'not started'

  constructor(private readonly output: vscode.LogOutputChannel) {}

  private setting(): string {
    return vscode.workspace.getConfiguration('roadbook').get<string>('path', 'roadbook')
  }

  private base(): string | undefined {
    return vscode.workspace.workspaceFolders?.[0]?.uri.fsPath
  }

  /** The command line, or undefined after saying (once per setting) that it cannot be found. */
  async executable(): Promise<string | undefined> {
    const setting = this.setting()
    const exe = await resolveExecutable(setting, this.base())
    if (!exe) this.warnOnce(`missing:${setting}`, `Roadbook: cannot find the command line \`${setting}\` (setting \`roadbook.path\`). Highlighting works without it; diagnostics, navigation, formatting and the preview need it.`, true)
    return exe
  }

  private warnOnce(key: string, message: string, error: boolean) {
    this.output.appendLine(message)
    if (this.warned.has(key)) return
    this.warned.add(key)
    const shown = error ? vscode.window.showErrorMessage(message, 'Open Settings') : vscode.window.showWarningMessage(message, 'Open Settings')
    void shown.then((choice) => {
      if (choice) void vscode.commands.executeCommand('workbench.action.openSettings', 'roadbook.path')
    })
  }

  /** Starts the language server, if the command line has one. */
  start(): Promise<void> {
    this.starting ??= this.doStart().finally(() => (this.starting = undefined))
    return this.starting
  }

  private async doStart() {
    const exe = await this.executable()
    if (!exe) {
      this.status = 'no command line'
      return
    }
    const cwd = this.base() ?? path.dirname(exe)
    if (!(await hasLsp(exe, cwd))) {
      this.status = `\`${exe}\` has no lsp command`
      this.warnOnce(`nolsp:${exe}`, `Roadbook: \`${exe}\` has no \`lsp\` command (it is older than the language server). Diagnostics, navigation and formatting are off; highlighting and the preview still work.`, false)
      return
    }
    const server: ServerOptions = { command: exe, args: ['lsp'], transport: TransportKind.stdio, options: { cwd } }
    const options: LanguageClientOptions = {
      documentSelector: [
        { scheme: 'file', language: 'roadbook' },
        { scheme: 'untitled', language: 'roadbook' },
      ],
      synchronize: { fileEvents: vscode.workspace.createFileSystemWatcher('**/{*.road,roadbook.toml}') },
      outputChannel: this.output,
    }
    this.client = new LanguageClient('roadbook', 'Roadbook Language Server', server, options)
    try {
      await this.client.start()
      this.status = true
      this.output.appendLine(`Started \`${exe} lsp\`.`)
    } catch (e) {
      this.status = `the server did not start: ${e instanceof Error ? e.message : String(e)}`
      this.output.appendLine(`Roadbook: ${this.status}`)
    }
  }

  async stop() {
    const c = this.client
    this.client = undefined
    this.status = 'stopped'
    if (c) await c.stop().catch(() => undefined)
  }

  async restart() {
    await this.stop()
    await this.start()
  }

  dispose() {
    void this.stop()
  }
}
