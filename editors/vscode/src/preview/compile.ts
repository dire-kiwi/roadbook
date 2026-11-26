// Compiling a chart for the preview: `roadbook export ir <file>` for the IR, and on errors
// `roadbook check --json <file>` for the diagnostics. No `vscode` import.
//
// The command line reads charts from disk only. Unsaved text is written to a hidden file next
// to the chart (`.checkout.road.preview-<pid>-<n>`), so its relative imports resolve as they
// would from the chart, and the file is removed as soon as the commands have run. Its name does
// not end in `.road`, so `roadbook check`, `roadbook gen` and file watchers for `*.road` do not
// pick it up. Everything the commands say about it is said about the chart.

import fs from 'node:fs'
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
  /** `irText`: the IR as JSON, sources renamed back to the chart. */
  | { kind: 'ok'; ir: Ir; irText: string; root: string; entry: string }
  /** The chart does not compile. `text`: what the command printed, when it gave no diagnostics. */
  | { kind: 'errors'; diagnostics: Diagnostic[]; text: string | null; root: string; entry: string }
  /** The command could not run, or failed for another reason than the chart (exit code 2). */
  | { kind: 'fatal'; message: string }

let counter = 0

/** The hidden file unsaved text of `file` is written to. */
export function tempFileFor(file: string, pid = process.pid, n = ++counter): string {
  return path.join(path.dirname(file), `.${path.basename(file)}.preview-${pid}-${n}`)
}

/**
 * Compiles `file` with the command line `exe`. `text`: the editor's unsaved text, if it differs
 * from the file on disk.
 */
export async function compileChart(exe: string, file: string, text?: string, timeoutMs = 30_000): Promise<CompileResult> {
  file = path.resolve(file)
  const dir = path.dirname(file)
  const root = projectRoot(file)
  const entry = projectPath(root, file)
  let input = file
  if (text !== undefined) {
    input = tempFileFor(file)
    try {
      await fs.promises.writeFile(input, text, { flag: 'wx' })
    } catch (e) {
      return { kind: 'fatal', message: `cannot write the unsaved chart to \`${input}\`: ${e instanceof Error ? e.message : String(e)}` }
    }
  }
  const from = projectPath(root, input)
  try {
    const exported = await run(exe, ['export', 'ir', input], { cwd: dir, timeoutMs })
    if (exported.code === null) return { kind: 'fatal', message: exported.error ?? `cannot run \`${exe}\`` }
    if (exported.code === 0) {
      try {
        const ir = renameSource(JSON.parse(exported.stdout) as Ir, from, entry)
        return { kind: 'ok', ir, irText: JSON.stringify(ir), root, entry }
      } catch (e) {
        return { kind: 'fatal', message: `\`roadbook export ir\` printed something that is not IR: ${e instanceof Error ? e.message : String(e)}` }
      }
    }
    const stderr = renameInText(exported.stderr, from, entry).trim()
    if (exported.code !== 1) return { kind: 'fatal', message: stderr || `\`roadbook export ir\` exited with ${exported.code}` }
    const checked = await run(exe, ['check', '--json', input], { cwd: dir, timeoutMs })
    const diagnostics = parseCheck(checked.stdout, from, entry)
    const errors = diagnostics?.filter((d) => d.severity === 'error') ?? []
    return { kind: 'errors', diagnostics: errors, text: errors.length ? null : stderr, root, entry }
  } finally {
    if (input !== file) await fs.promises.rm(input, { force: true })
  }
}

/** The IR with the source named `from` (the temporary file) named `to` (the chart). */
export function renameSource(ir: Ir, from: string, to: string): Ir {
  if (from === to || !Array.isArray(ir.sources)) return ir
  for (const s of ir.sources) if (s.package === null && s.path === from) s.path = to
  return ir
}

/** Text the command printed, with the temporary file's project path replaced by the chart's. */
export function renameInText(text: string, from: string, to: string): string {
  return from === to ? text : text.split(from).join(to)
}

/** The diagnostics of `roadbook check --json` output, renamed as above; undefined if it is not that. */
export function parseCheck(stdout: string, from: string, to: string): Diagnostic[] | undefined {
  let out: { diagnostics?: unknown }
  try {
    out = JSON.parse(stdout) as { diagnostics?: unknown }
  } catch {
    return undefined
  }
  if (!Array.isArray(out.diagnostics)) return undefined
  return (out.diagnostics as Diagnostic[]).map((d) => ({
    code: String(d.code),
    severity: d.severity === 'warning' ? 'warning' : 'error',
    message: renameInText(String(d.message), from, to),
    file: d.file === from ? to : String(d.file),
    line: Number(d.line),
    col: Number(d.col),
    endLine: Number(d.endLine),
    endCol: Number(d.endCol),
    help: typeof d.help === 'string' ? renameInText(d.help, from, to) : null,
  }))
}
