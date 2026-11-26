// Finding and running the `roadbook` command line. No `vscode` import, so the tests run it in
// plain Node.

import { execFile } from 'node:child_process'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'

/**
 * The executable the `roadbook.path` setting names, or undefined if there is none. A bare name
 * is looked up on `PATH` (with `PATHEXT` on Windows); a path is taken relative to `base` (the
 * first workspace folder). `${workspaceFolder}` and a leading `~` are expanded.
 */
export async function resolveExecutable(setting: string, base: string | undefined, env: NodeJS.ProcessEnv = process.env): Promise<string | undefined> {
  let p = setting.trim() || 'roadbook'
  if (base) p = p.replaceAll('${workspaceFolder}', base)
  if (p === '~' || p.startsWith('~/')) p = path.join(os.homedir(), p.slice(1))
  const exts = process.platform === 'win32' ? ['', ...(env.PATHEXT ?? '.EXE;.CMD;.BAT').split(';')] : ['']
  const candidates: string[] = []
  if (p.includes('/') || p.includes('\\')) {
    const abs = path.isAbsolute(p) ? p : path.resolve(base ?? process.cwd(), p)
    candidates.push(...exts.map((e) => abs + e))
  } else {
    for (const dir of (env.PATH ?? '').split(path.delimiter).filter(Boolean)) candidates.push(...exts.map((e) => path.join(dir, p + e)))
  }
  for (const c of candidates) if (await isExecutable(c)) return c
  return undefined
}

async function isExecutable(file: string): Promise<boolean> {
  try {
    const st = await fs.promises.stat(file)
    if (!st.isFile()) return false
    await fs.promises.access(file, fs.constants.X_OK)
    return true
  } catch {
    return false
  }
}

export interface RunResult {
  /** The exit code; null if the process could not start, timed out or was killed. */
  code: number | null
  stdout: string
  stderr: string
  /** Why the process did not run to its end, if it did not. */
  error?: string
}

/** Runs `exe args…` in `cwd`, collecting its output. Never rejects. */
export function run(exe: string, args: string[], opts: { cwd: string; timeoutMs?: number }): Promise<RunResult> {
  return new Promise((resolve) => {
    execFile(exe, args, { cwd: opts.cwd, timeout: opts.timeoutMs ?? 30_000, maxBuffer: 64 * 1024 * 1024, windowsHide: true }, (err, stdout, stderr) => {
      if (!err) return resolve({ code: 0, stdout, stderr })
      const e = err as NodeJS.ErrnoException & { code?: number | string; killed?: boolean }
      if (typeof e.code === 'number') return resolve({ code: e.code, stdout, stderr })
      const why = e.killed ? `timed out after ${opts.timeoutMs ?? 30_000} ms` : e.code === 'ENOENT' ? 'not found' : err.message
      resolve({ code: null, stdout, stderr, error: `cannot run \`${exe}\`: ${why}` })
    })
  })
}

/** Whether this roadbook has the `lsp` command (it came with milestone M8). */
export async function hasLsp(exe: string, cwd: string): Promise<boolean> {
  const r = await run(exe, ['lsp', '--help'], { cwd, timeoutMs: 10_000 })
  return r.code === 0
}

/**
 * The project root of a file, as the command line finds it: the nearest directory above it
 * holding a `roadbook.toml`, else the file's own directory (the command line takes the
 * current directory, and the extension runs it in the file's directory).
 */
export function projectRoot(file: string): string {
  const start = path.dirname(path.resolve(file))
  for (let dir = start; ; ) {
    if (fs.existsSync(path.join(dir, 'roadbook.toml'))) return dir
    const up = path.dirname(dir)
    if (up === dir) return start
    dir = up
  }
}

/** A file's project path: relative to the root, `/`-separated, as diagnostics and the IR name it. */
export function projectPath(root: string, file: string): string {
  return path.relative(root, path.resolve(file)).split(path.sep).join('/')
}
