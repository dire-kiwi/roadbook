// Compiling a chart for the preview. No `vscode` import.
//
// The language server compiles it: the `roadbook/ir` request (crates/roadbook-lsp/README.md)
// compiles the chart as `roadbook export ir` would, under its own project path and with its
// project's options, but with the text of every open editor in place of the file on disk. So
// the preview follows unsaved edits to the chart and to the files it imports, and agrees with
// the command line. Only when the server is not running does the preview run the command line,
// on the saved file: `roadbook export ir <file>` for the IR, and on errors
// `roadbook check --json <file>` for the diagnostics.

import path from 'node:path'
import { projectPath, projectRoot, run } from '../cli.ts'
import type { Ir } from './ir.ts'

/** A diagnostic as `roadbook check --json` prints it (docs/design/language.md, "Diagnostics as JSON"). */
export interface Diagnostic {
  code: string
  severity: 'error' | 'warning'
  message: string
  file: string
  line: number
  col: number
  endLine: number
  endCol: number
  help: string | null
}

export type CompileResult =
  /**
   * `irText`: the IR as JSON. `root`: the project's root directory, null for a document that
   * is not a file. `entry`: the chart's project path, as the IR's `sources` name it.
   */
  | { kind: 'ok'; ir: Ir; irText: string; root: string | null; entry: string }
  /** The chart does not compile. `text`: what the command printed, when it gave no diagnostics. */
  | { kind: 'errors'; diagnostics: Diagnostic[]; text: string | null; root: string | null; entry: string }
  /** The chart could not be compiled for another reason than its errors. */
  | { kind: 'fatal'; message: string }

/** What the language server answers to `roadbook/ir`. */
export interface ServerIr {
  uri: string
  version: number | null
  root: string | null
  entry: string
  /** The IR as `roadbook export ir` prints it; null when the chart has errors. */
  ir: string | null
  diagnostics: unknown[]
}

/** The part of the language client the preview uses. */
export interface IrServer {
  sendRequest(method: string, params: unknown): Promise<unknown>
}

/** The JSON-RPC error code of a request the server could not do (`RequestFailed`). */
const REQUEST_FAILED = -32803

/**
 * Compiles the document `uri` with the language server. Undefined if the server cannot be
 * asked (it stopped, or is too old to know the request), so that the caller can fall back to
 * the command line.
 */
export async function compileWithServer(server: IrServer, uri: string): Promise<CompileResult | undefined> {
  let answer: unknown
  try {
    answer = await server.sendRequest('roadbook/ir', { textDocument: { uri } })
  } catch (e) {
    if ((e as { code?: unknown } | null)?.code === REQUEST_FAILED) return { kind: 'fatal', message: e instanceof Error ? e.message : String(e) }
    return undefined
  }
  return fromServer(answer)
}

/** The result of a `roadbook/ir` answer. */
export function fromServer(raw: unknown): CompileResult {
  const a = raw as Partial<ServerIr> | null
  if (!a || typeof a.entry !== 'string' || !Array.isArray(a.diagnostics)) {
    return { kind: 'fatal', message: 'the language server answered `roadbook/ir` with something else than a compiled chart' }
  }
  const root = typeof a.root === 'string' ? a.root : null
  const entry = a.entry
  if (typeof a.ir === 'string') {
    let ir: Ir
    try {
      ir = JSON.parse(a.ir) as Ir
    } catch (e) {
      return { kind: 'fatal', message: `the language server gave something that is not IR: ${e instanceof Error ? e.message : String(e)}` }
    }
    return { kind: 'ok', ir, irText: JSON.stringify(ir), root, entry }
  }
  const errors = parseDiagnostics(a.diagnostics).filter((d) => d.severity === 'error')
  return { kind: 'errors', diagnostics: errors, text: errors.length ? null : 'The chart does not compile.', root, entry }
}

/** Compiles the saved file `file` with the command line `exe`. */
export async function compileChart(exe: string, file: string, timeoutMs = 30_000): Promise<CompileResult> {
  file = path.resolve(file)
  const dir = path.dirname(file)
  const root = projectRoot(file)
  const entry = projectPath(root, file)
  const exported = await run(exe, ['export', 'ir', file], { cwd: dir, timeoutMs })
  if (exported.code === null) return { kind: 'fatal', message: exported.error ?? `cannot run \`${exe}\`` }
  if (exported.code === 0) {
    try {
      const ir = JSON.parse(exported.stdout) as Ir
      return { kind: 'ok', ir, irText: JSON.stringify(ir), root, entry }
    } catch (e) {
      return { kind: 'fatal', message: `\`roadbook export ir\` printed something that is not IR: ${e instanceof Error ? e.message : String(e)}` }
    }
  }
  const stderr = exported.stderr.trim()
  if (exported.code !== 1) return { kind: 'fatal', message: stderr || `\`roadbook export ir\` exited with ${exported.code}` }
  const checked = await run(exe, ['check', '--json', file], { cwd: dir, timeoutMs })
  const diagnostics = parseCheck(checked.stdout)
  const errors = diagnostics?.filter((d) => d.severity === 'error') ?? []
  return { kind: 'errors', diagnostics: errors, text: errors.length ? null : stderr, root, entry }
}

/** The diagnostics of `roadbook check --json` output; undefined if it is not that. */
export function parseCheck(stdout: string): Diagnostic[] | undefined {
  let out: { diagnostics?: unknown }
  try {
    out = JSON.parse(stdout) as { diagnostics?: unknown }
  } catch {
    return undefined
  }
  if (!Array.isArray(out.diagnostics)) return undefined
  return parseDiagnostics(out.diagnostics)
}

/** Diagnostics in the form of `roadbook check --json`, checked. */
export function parseDiagnostics(list: unknown[]): Diagnostic[] {
  return (list as Diagnostic[]).map((d) => ({
    code: String(d.code),
    severity: d.severity === 'warning' ? 'warning' : 'error',
    message: String(d.message),
    file: String(d.file),
    line: Number(d.line),
    col: Number(d.col),
    endLine: Number(d.endLine),
    endCol: Number(d.endCol),
    help: typeof d.help === 'string' ? d.help : null,
  }))
}
