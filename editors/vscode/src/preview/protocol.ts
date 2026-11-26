// What the preview's webview (src/webview/bridge.ts) and the extension say to each other.

import type { Diagnostic } from './compile.ts'
import type { Target } from './ir.ts'

export type Theme = 'light' | 'dark'

/** What the reader was looking at: the machine open and what is selected in it. */
export interface ViewState {
  machine: string | null
  selection: Target | null
}

/** The bridge's configuration, in the page as `<script type="application/json" id="rbx-config">`. */
export interface BridgeConfig {
  theme: Theme
  /** The view to restore once the viewer has drawn, from before the page was replaced. */
  view: ViewState | null
  /** The document's URI, kept in the webview's state so VS Code can restore the panel. */
  uri: string
}

/** Why the preview shows no current diagram: compile errors, or a command that could not run. */
export interface Problems {
  /** The chart's project path. */
  entry: string
  diagnostics: Diagnostic[]
  /** Text to show when there are no diagnostics: the command's output or a fatal error. */
  text: string | null
  /** Whether an older diagram is still shown below. */
  stale: boolean
}

export type ToWebview = { type: 'theme'; theme: Theme } | { type: 'problems'; problems: Problems | null }

export type FromWebview =
  | { type: 'ready' }
  /** The viewer has drawn a diagram, and the view has been restored. */
  | { type: 'rendered'; view: ViewState }
  | { type: 'view'; view: ViewState }
  /** A state or transition was clicked in the diagram. */
  | { type: 'reveal'; machine: string; target: Target }
  /** A diagnostic was clicked in the problems list. */
  | { type: 'open'; file: string; line: number; col: number }
  /** The page broke its Content-Security-Policy (reported so the tests can see it). */
  | { type: 'csp'; directive: string; blocked: string }
  /** A script error in the page, or a diagram that never appeared. */
  | { type: 'error'; message: string }
