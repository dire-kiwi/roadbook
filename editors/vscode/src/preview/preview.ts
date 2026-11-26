// The live preview: a webview beside a `.road` editor showing the viewer for its chart, updated
// on save and, debounced, as the text changes.
//
// Each update compiles the chart (compile.ts): with the language server, which has the text of
// every open editor, or, when it is not running, with the command line on the saved file. When
// the IR differs from the one shown, the page is replaced with one holding the new IR, and the
// bridge script in it restores the machine and the selection the reader had. When the chart
// does not compile, the last diagram stays and the errors are shown over it. A change to any
// open chart updates every preview, since the chart shown may import it.

import path from 'node:path'
import * as vscode from 'vscode'
import { projectFile } from '../cli.ts'
import { compileChart, compileWithServer } from './compile.ts'
import type { CompileResult, IrServer } from './compile.ts'
import { makeNonce, previewHtml } from './html.ts'
import { sourcePath, spanOf } from './ir.ts'
import type { Ir, Target } from './ir.ts'
import type { FromWebview, Problems, Theme, ToWebview, ViewState } from './protocol.ts'

export const VIEW_TYPE = 'roadbook.preview'

/** Finds the command line, or explains why it cannot (and says so once). */
export type ExecutableProvider = () => Promise<string | undefined>

/** The running language server, if there is one. */
export type ServerProvider = () => IrServer | undefined

/** What the integration tests watch. */
export interface PreviewEvent {
  uri: string
  kind: 'rendered' | 'problems' | 'csp' | 'error'
  detail?: unknown
}

export function themeOf(kind: vscode.ColorThemeKind): Theme {
  return kind === vscode.ColorThemeKind.Light || kind === vscode.ColorThemeKind.HighContrastLight ? 'light' : 'dark'
}

const settings = () => vscode.workspace.getConfiguration('roadbook')

export class PreviewManager implements vscode.Disposable {
  private readonly previews = new Map<string, Preview>()
  private readonly disposables: vscode.Disposable[] = []
  readonly events = new vscode.EventEmitter<PreviewEvent>()

  constructor(
    private readonly extensionUri: vscode.Uri,
    private readonly executable: ExecutableProvider,
    private readonly server: ServerProvider,
    private readonly log: (message: string) => void,
  ) {
    this.disposables.push(
      this.events,
      vscode.workspace.onDidChangeTextDocument((e) => {
        if (e.contentChanges.length === 0 || e.document.languageId !== 'roadbook' || !settings().get<boolean>('preview.updateOnChange', true)) return
        const delay = settings().get<number>('preview.debounce', 300)
        for (const p of this.previews.values()) p.schedule(delay)
      }),
      vscode.workspace.onDidSaveTextDocument((d) => {
        if (d.languageId !== 'roadbook') return
        for (const p of this.previews.values()) p.schedule(0)
      }),
      vscode.window.onDidChangeActiveColorTheme((t) => {
        for (const p of this.previews.values()) p.setTheme(themeOf(t.kind))
      }),
      vscode.window.registerWebviewPanelSerializer(VIEW_TYPE, {
        deserializeWebviewPanel: async (panel, state: unknown) => {
          const uri = (state as { uri?: unknown } | undefined)?.uri
          if (typeof uri !== 'string') return panel.dispose()
          this.attach(panel, vscode.Uri.parse(uri))
        },
      }),
    )
  }

  /** Opens (or reveals) the preview of `uri` beside the editor. */
  showToSide(uri: vscode.Uri) {
    const existing = this.previews.get(uri.toString())
    if (existing) return existing.panel.reveal(undefined, true)
    const panel = vscode.window.createWebviewPanel(VIEW_TYPE, title(uri), { viewColumn: vscode.ViewColumn.Beside, preserveFocus: true }, {})
    this.attach(panel, uri)
  }

  /** The preview of a document, for the tests. */
  get(uri: vscode.Uri): Preview | undefined {
    return this.previews.get(uri.toString())
  }

  private attach(panel: vscode.WebviewPanel, uri: vscode.Uri) {
    const dist = vscode.Uri.joinPath(this.extensionUri, 'dist')
    panel.webview.options = { enableScripts: true, localResourceRoots: [dist] }
    const preview = new Preview(panel, uri, dist, this.executable, this.server, this.log, (e) => this.events.fire(e))
    this.previews.set(uri.toString(), preview)
    panel.onDidDispose(() => {
      this.previews.delete(uri.toString())
      preview.dispose()
    })
    preview.schedule(0)
  }

  dispose() {
    for (const p of this.previews.values()) p.panel.dispose()
    for (const d of this.disposables) d.dispose()
  }
}

function title(uri: vscode.Uri): string {
  return `Preview ${path.basename(uri.fsPath)}`
}

export class Preview {
  /** The IR shown, as compiled: for revealing what is clicked, and to tell whether it changed. */
  private shown: { ir: Ir; irText: string } | null = null
  /** The chart's project root (null for a document that is not a file) and project path, once compiled. */
  private root: string | null = null
  private entry: string | undefined
  private problems: Problems | null = null
  private view: ViewState | null = null
  private theme: Theme = themeOf(vscode.window.activeColorTheme.kind)
  private timer: ReturnType<typeof setTimeout> | undefined
  private running = false
  private again = false
  private disposed = false
  private readonly disposables: vscode.Disposable[] = []

  constructor(
    readonly panel: vscode.WebviewPanel,
    readonly uri: vscode.Uri,
    private readonly dist: vscode.Uri,
    private readonly executable: ExecutableProvider,
    private readonly server: ServerProvider,
    private readonly log: (message: string) => void,
    private readonly fire: (e: PreviewEvent) => void,
  ) {
    this.disposables.push(panel.webview.onDidReceiveMessage((m: unknown) => this.receive(m)))
  }

  /** Updates after `delayMs`, unless another change comes first. */
  schedule(delayMs: number) {
    clearTimeout(this.timer)
    this.timer = setTimeout(() => void this.update(), Math.max(0, delayMs))
  }

  /** Compiles the chart and shows the result. One run at a time; a change during a run runs it again after. */
  async update(): Promise<void> {
    if (this.disposed) return
    if (this.running) {
      this.again = true
      return
    }
    this.running = true
    try {
      do {
        this.again = false
        await this.updateOnce()
      } while (this.again && !this.disposed)
    } finally {
      this.running = false
    }
  }

  private async compile(): Promise<CompileResult> {
    const server = this.server()
    const compiled = server && (await compileWithServer(server, this.uri.toString()))
    if (compiled) return compiled
    // No language server: the command line, which reads the saved file.
    if (this.uri.scheme !== 'file') return { kind: 'fatal', message: 'The preview of a chart that is not saved to a file needs the language server (`roadbook lsp`).' }
    const exe = await this.executable()
    if (!exe) return { kind: 'fatal', message: 'Cannot find the `roadbook` command line. Install it, or set `roadbook.path` to where it is.' }
    return compileChart(exe, this.uri.fsPath)
  }

  private async updateOnce() {
    const result = await this.compile()
    if (this.disposed) return
    if (result.kind === 'fatal') {
      return this.showProblems({ entry: path.basename(this.uri.fsPath), diagnostics: [], text: result.message, stale: this.shown !== null })
    }
    this.root = result.root
    this.entry = result.entry
    if (result.kind === 'errors') {
      return this.showProblems({ entry: result.entry, diagnostics: result.diagnostics, text: result.text, stale: this.shown !== null })
    }
    const changed = this.shown?.irText !== result.irText
    this.shown = { ir: result.ir, irText: result.irText }
    this.problems = null
    if (changed) this.render()
    else {
      this.post({ type: 'problems', problems: null })
      this.fire({ uri: this.uri.toString(), kind: 'rendered', detail: { unchanged: true } })
    }
  }

  private showProblems(p: Problems) {
    this.problems = p
    // With nothing drawn yet, the page is the problems alone.
    if (!this.shown && !this.panel.webview.html) this.render()
    else this.post({ type: 'problems', problems: p })
    this.fire({ uri: this.uri.toString(), kind: 'problems', detail: p })
  }

  private render() {
    const w = this.panel.webview
    w.html = previewHtml({
      cspSource: w.cspSource,
      nonce: makeNonce(),
      viewerUri: w.asWebviewUri(vscode.Uri.joinPath(this.dist, 'roadbook-viewer.js')).toString(),
      bridgeUri: w.asWebviewUri(vscode.Uri.joinPath(this.dist, 'bridge.js')).toString(),
      title: title(this.uri),
      entry: this.entry ?? '',
      irText: this.shown?.irText ?? null,
      config: { theme: this.theme, view: this.view, uri: this.uri.toString() },
    })
  }

  setTheme(theme: Theme) {
    this.theme = theme
    this.post({ type: 'theme', theme })
  }

  private post(m: ToWebview) {
    void this.panel.webview.postMessage(m)
  }

  /** Handles a message from the page. Public for the tests, which cannot click in a webview. */
  async receive(raw: unknown) {
    const m = raw as FromWebview
    switch (m?.type) {
      case 'ready':
        // Whatever was said before the page was listening.
        this.post({ type: 'theme', theme: this.theme })
        this.post({ type: 'problems', problems: this.problems })
        if (!this.shown && this.problems) this.fire({ uri: this.uri.toString(), kind: 'problems', detail: this.problems })
        return
      case 'rendered':
        if (m.view && typeof m.view === 'object') this.view = m.view
        this.fire({ uri: this.uri.toString(), kind: 'rendered', detail: { view: m.view } })
        return
      case 'view':
        if (m.view && typeof m.view === 'object') this.view = m.view
        return
      case 'csp':
        this.log(`the preview broke its Content-Security-Policy: ${String(m.directive)} blocked ${String(m.blocked)}`)
        this.fire({ uri: this.uri.toString(), kind: 'csp', detail: { directive: m.directive, blocked: m.blocked } })
        return
      case 'error':
        this.log(`preview: ${String(m.message)}`)
        this.fire({ uri: this.uri.toString(), kind: 'error', detail: String(m.message) })
        return
      case 'reveal':
        if (typeof m.machine === 'string' && isTarget(m.target) && settings().get<boolean>('preview.revealSource', true)) await this.reveal(m.machine, m.target)
        return
      case 'open':
        if (typeof m.file === 'string' && Number.isInteger(m.line) && Number.isInteger(m.col)) await this.open(m.file, m.line, m.col, false)
        return
    }
  }

  /** Reveals the source of a state or transition of the IR shown. */
  async reveal(machine: string, target: Target): Promise<vscode.TextEditor | undefined> {
    const shown = this.shown
    if (!shown) return undefined
    const span = spanOf(shown.ir, machine, target)
    if (!span) return undefined
    const file = sourcePath(shown.ir, span)
    if (file === undefined) return undefined
    return this.open(file, span.line, span.col, true)
  }

  /**
   * Shows a file of the chart's project at a 1-based line and column, beside the preview: the
   * chart itself, or a `.road` file inside the project's root. Anything else a page asks for
   * (`../` out of the project, an absolute path) is not opened.
   */
  async open(file: string, line: number, col: number, preserveFocus: boolean): Promise<vscode.TextEditor | undefined> {
    const inProject = this.root === null ? undefined : projectFile(this.root, file)
    const uri = file === this.entry ? this.uri : inProject ? vscode.Uri.file(inProject) : undefined
    if (!uri) {
      this.log(`preview: not opening \`${file}\`, which is not a chart of the project`)
      return undefined
    }
    const pos = new vscode.Position(Math.max(0, line - 1), Math.max(0, col - 1))
    const visible = vscode.window.visibleTextEditors.find((e) => e.document.uri.toString() === uri.toString())
    const editor = await vscode.window.showTextDocument(uri, {
      viewColumn: visible?.viewColumn ?? vscode.ViewColumn.One,
      preserveFocus,
      selection: new vscode.Range(pos, pos),
    })
    editor.revealRange(new vscode.Range(pos, pos), vscode.TextEditorRevealType.InCenterIfOutsideViewport)
    return editor
  }

  dispose() {
    this.disposed = true
    clearTimeout(this.timer)
    for (const d of this.disposables) d.dispose()
  }
}

function isTarget(t: unknown): t is Target {
  const x = t as Target | undefined
  if (x?.kind === 'state') return typeof x.id === 'string'
  if (x?.kind === 'transition') return typeof x.state === 'string' && Number.isInteger(x.index)
  return false
}
