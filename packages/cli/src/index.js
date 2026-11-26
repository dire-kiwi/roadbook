// The programmatic API of @dire-kiwi/roadbook-cli, for build tools: `check`, `compile`, `gen` and
// `format`. Types and documentation are in index.d.ts.

import path from 'node:path'
import * as wasm from './host.js'
import { nodeHost, slash } from './host.js'
import { Workspace, byBytes } from './workspace.js'

export class RoadbookError extends Error {
  /** @param {string} message */
  constructor(message) {
    super(message)
    this.name = 'RoadbookError'
  }
}

/**
 * The project, host, configuration and project paths for a call.
 * @param {string[]} paths
 * @param {import("./api.d.ts").Options} options
 */
function setup(paths, options) {
  const cwd = path.resolve(options.cwd ?? process.cwd())
  /** @type {Record<string, string>} */
  const files = {}
  for (const [p, text] of Object.entries(options.files ?? {})) files[path.resolve(cwd, p)] = text
  const abs = paths.map((p) => path.resolve(cwd, p))
  if (options.disk === false) {
    const ws = new Workspace(cwd)
    const config = ws.config({ files: rekey(files) })
    const inMemory = Object.keys(files).filter((f) => f.endsWith('.road'))
    const roots = (abs.length > 0 ? abs : inMemory).map((p) => {
      if (files[p] !== undefined) return ws.projectPath(p)
      const under = inMemory.filter((f) => f.startsWith(p + path.sep))
      if (under.length === 0) throw new RoadbookError(`\`${p}\` does not exist`)
      return under.map((f) => ws.projectPath(f))
    })
    return { host: null, config, roots: [...new Set(roots.flat())].sort(byBytes) }
  }
  try {
    const ws = Workspace.find(abs[0], cwd)
    const config = ws.config({ files: rekey(files) })
    const onDisk = abs.filter((p) => files[p] === undefined)
    const roots = abs.length > 0 && onDisk.length === 0 ? [] : ws.inputs(onDisk)
    for (const p of abs) if (files[p] !== undefined) roots.push(ws.projectPath(p))
    return { host: nodeHost, config, roots: [...new Set(roots)].sort(byBytes) }
  } catch (e) {
    if (e instanceof RoadbookError) throw e
    throw new RoadbookError(/** @type {Error} */ (e).message)
  }
}

/** @param {Record<string, string>} files */
const rekey = (files) => Object.fromEntries(Object.entries(files).map(([p, t]) => [slash(p), t]))

/**
 * @template {{ fatal?: string | undefined }} T
 * @param {T} result
 */
function ok(result) {
  if (result.fatal !== undefined) throw new RoadbookError(result.fatal)
  return result
}

/**
 * @param {import('./host.js').Diagnostics} d
 * @returns {import("./api.d.ts").Diagnostics}
 */
const diagnostics = (d) => ({
  report: JSON.parse(d.report),
  json: d.report,
  rendered: d.rendered,
  errors: d.errors,
  warnings: d.warnings,
})

/**
 * @param {string[]} [paths]
 * @param {import("./api.d.ts").Options & { warnInvokeTimeout?: boolean }} [options]
 * @returns {import("./api.d.ts").Diagnostics}
 */
export function check(paths = [], options = {}) {
  const { host, config, roots } = setup(paths, options)
  config.warnInvokeTimeout = options.warnInvokeTimeout ?? false
  return diagnostics(ok(wasm.check(host, config, roots)))
}

/**
 * @param {string} entry
 * @param {import("./api.d.ts").Options} [options]
 * @returns {import("./api.d.ts").CompileResult}
 */
export function compile(entry, options = {}) {
  const { host, config, roots } = setup([entry], options)
  const result = ok(wasm.compile(host, config, /** @type {string} */ (roots[0])))
  return { ...diagnostics(result), ir: result.ir === null ? null : JSON.parse(result.ir), irJson: result.ir }
}

/**
 * @param {string} entry
 * @param {import("./api.d.ts").Options} [options]
 * @returns {import("./api.d.ts").GenResult}
 */
export function gen(entry, options = {}) {
  const { host, config, roots } = setup([entry], options)
  const result = ok(wasm.gen(host, config, /** @type {string} */ (roots[0])))
  return { ...diagnostics(result), code: result.code, error: result.error }
}

/**
 * @param {string} source
 * @param {string} [name]
 * @returns {import("./api.d.ts").FormatResult}
 */
export function format(source, name = 'input.road') {
  const result = wasm.format(name, source)
  return result.errors ? { formatted: null, errors: result.errors } : { formatted: result.formatted, errors: [] }
}

/** The compiler's version. */
export const version = () => wasm.version()
