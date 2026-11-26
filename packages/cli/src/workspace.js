// The project a command works on: its root, and the `.road` files it is given. A port of
// crates/roadbook-cli/src/workspace.rs, which documents the rules; the tests run both command
// lines and compare what they print.

import fs from 'node:fs'
import path from 'node:path'
import { ioMessage, readText, slash } from './host.js'

/** Directories never searched for `.road` files. */
const SKIPPED = ['target', 'node_modules', '.git']

/** Rust's `Path::is_dir` and `is_file`: following links, false on any error. */
const stat = (/** @type {string} */ p) => {
  try {
    return fs.statSync(p)
  } catch {
    return undefined
  }
}
export const isDir = (/** @type {string} */ p) => stat(p)?.isDirectory() ?? false
export const isFile = (/** @type {string} */ p) => stat(p)?.isFile() ?? false

/** Byte order, as Rust sorts strings. @param {string} a @param {string} b */
export const byBytes = (a, b) => Buffer.compare(Buffer.from(a), Buffer.from(b))

export class Workspace {
  /**
   * @param {string} root
   * @param {{ exclude?: string[], genExclude?: string[], allowUnassigned?: string[], packages?: [string, string][] }} [config]
   */
  constructor(root, config = {}) {
    /** The project root, absolute. */
    this.root = root
    this.exclude = config.exclude ?? []
    this.genExclude = config.genExclude ?? []
    this.allowUnassigned = config.allowUnassigned ?? []
    /** @type {[string, string][]} */
    this.packages = config.packages ?? []
  }

  /**
   * The workspace containing `start` (a file or directory), or the current directory's: the
   * nearest directory upward holding a `roadbook.toml`, else the current directory.
   * Throws the message of a usage or I/O error.
   * @param {string} [start]
   * @param {string} [cwd]
   */
  static find(start, cwd = process.cwd()) {
    let from = cwd
    if (start !== undefined) {
      const abs = path.resolve(cwd, start)
      from = isDir(abs) ? abs : path.dirname(abs)
    }
    for (let d = from; ; d = path.dirname(d)) {
      const config = path.join(d, 'roadbook.toml')
      if (isFile(config)) {
        let text
        try {
          text = readText(config)
        } catch (e) {
          throw new Error(`cannot read \`${config}\`: ${ioMessage(e)}`)
        }
        const where = (/** @type {() => any} */ f) => {
          try {
            return f()
          } catch (e) {
            throw new Error(`\`${config}\`: ${/** @type {Error} */ (e).message}`)
          }
        }
        return new Workspace(d, {
          exclude: where(() => stringList(text, 'exclude')),
          genExclude: where(() => stringList(text, 'gen-exclude')),
          allowUnassigned: where(() => stringList(text, 'allow-unassigned')),
          packages: where(() => stringTable(text, 'packages')),
        })
      }
      if (path.dirname(d) === d) break
    }
    return new Workspace(cwd)
  }

  /**
   * What the WebAssembly module needs to know of the project: its root, packages, and the
   * project paths of the `.road` files `allow-unassigned` matches.
   * @param {Partial<import('./host.js').Config>} [extra]
   * @returns {import('./host.js').Config}
   */
  config(extra = {}) {
    /** @type {string[]} */
    const files = []
    if (this.allowUnassigned.length > 0) this.walk(this.root, [], '.road', files)
    return {
      root: slash(this.root),
      packages: this.packages,
      nodeModules: true,
      allowUnassigned: files.map((f) => this.projectPath(f)).filter((f) => this.allowUnassigned.some((g) => glob(g, f))),
      ...extra,
    }
  }

  /** The project path of a file: relative to the root, `/`-separated. @param {string} p */
  projectPath(p) {
    return slash(path.relative(this.root, path.resolve(p)))
  }

  /** The path of a project file on disk. @param {string} projectPath */
  diskPath(projectPath) {
    return path.join(this.root, projectPath)
  }

  /**
   * The files to work on: each path given (a directory stands for every `.road` file under
   * it), or every `.road` file under the root if none is given. Sorted, without duplicates.
   * @param {string[]} paths
   */
  inputs(paths) {
    return this.files(paths, [], '.road')
  }

  /** `inputs`, also skipping `gen-exclude`. @param {string[]} paths */
  genInputs(paths) {
    return this.files(paths, this.genExclude, '.road')
  }

  /**
   * The generated modules (`*.road.ts`) under the directories `roadbook gen` would search for
   * `paths`; a file named in `paths` stands for none.
   * @param {string[]} paths
   */
  genOutputs(paths) {
    const dirs = paths.filter(isDir)
    if (paths.length > 0 && dirs.length === 0) return []
    return this.files(dirs, this.genExclude, '.road.ts')
  }

  /**
   * @param {string[]} paths
   * @param {string[]} extra
   * @param {string} suffix
   * @returns {string[]}
   */
  files(paths, extra, suffix) {
    /** @type {string[]} */
    const files = []
    if (paths.length === 0) this.walk(this.root, extra, suffix, files)
    const named = []
    for (const p of paths) {
      if (isDir(p)) this.walk(p, extra, suffix, files)
      else if (isFile(p)) named.push(p)
      else throw new Error(`\`${p}\` does not exist`)
    }
    const out = files
      .map((f) => this.projectPath(f))
      .filter((f) => !this.excluded(f, extra))
      .concat(named.map((f) => this.projectPath(f)))
    return [...new Set(out)].sort(byBytes)
  }

  /**
   * Whether a project path, or a directory containing it, matches an `exclude` glob or one of
   * `extra`.
   * @param {string} projectPath
   * @param {string[]} extra
   */
  excluded(projectPath, extra) {
    let prefix = ''
    return projectPath.split('/').some((seg) => {
      if (prefix !== '') prefix += '/'
      prefix += seg
      return [...this.exclude, ...extra].some((g) => glob(g.replace(/\/+$/, ''), prefix))
    })
  }

  /**
   * Every file named `*<suffix>` under `dir`, sorted, skipping `SKIPPED` and excluded
   * directories and not following links to directories.
   * @param {string} dir
   * @param {string[]} extra
   * @param {string} suffix
   * @param {string[]} out
   */
  walk(dir, extra, suffix, out) {
    let entries
    try {
      entries = fs.readdirSync(dir, { withFileTypes: true })
    } catch {
      return
    }
    const sorted = entries.map((e) => ({ name: e.name, link: e.isSymbolicLink() })).sort((a, b) => byBytes(a.name, b.name))
    for (const { name, link } of sorted) {
      const p = path.join(dir, name)
      if (isDir(p)) {
        if (!link && !SKIPPED.includes(name) && !this.excluded(this.projectPath(p), extra)) this.walk(p, extra, suffix, out)
      } else if (name.length > suffix.length && name.endsWith(suffix)) {
        out.push(p)
      }
    }
  }
}

/**
 * The lines of a TOML text, as Rust's `str::lines` gives them, without comments, trimmed.
 * @param {string} toml
 */
function tomlLines(toml) {
  const lines = toml.split('\n')
  if (lines.at(-1) === '') lines.pop()
  return lines.map((l) => withoutComment(l.endsWith('\r') ? l.slice(0, -1) : l).trim())
}

/**
 * The list of strings top-level key `key` holds: `exclude = ["a", "b/**"]`, on one line or
 * several; empty if there is no such key.
 * @param {string} toml
 * @param {string} key
 * @returns {string[]}
 */
export function stringList(toml, key) {
  const lines = tomlLines(toml)
  for (let i = 0; i < lines.length; i++) {
    const line = /** @type {string} */ (lines[i])
    if (line.startsWith('[')) break
    if (!line.startsWith(key)) continue
    const rest = line.slice(key.length).trimStart()
    if (!rest.startsWith('=')) continue
    let text = rest.slice(1).trim()
    while (!text.includes(']') && i + 1 < lines.length) text += lines[++i]
    if (!text.startsWith('[')) throw new Error(`\`${key}\` must be a list of strings, such as \`${key} = ["tests/**"]\``)
    const inner = /** @type {string} */ (text.slice(1).split(']')[0])
    /** @type {string[]} */
    const out = []
    for (const item of inner
      .split(',')
      .map((s) => s.trim())
      .filter((s) => s !== '')) {
      if (item.length >= 2 && item.startsWith('"') && item.endsWith('"')) out.push(item.slice(1, -1))
      else throw new Error(`\`${key}\`: \`${item}\` is not a string`)
    }
    return out
  }
  return []
}

/**
 * The entries `key = "value"` of the table `[name]`, in order; empty if there is no such
 * table. A key may be quoted: `"@shop/inventory" = "packages/inventory"`.
 * @param {string} toml
 * @param {string} name
 * @returns {[string, string][]}
 */
export function stringTable(toml, name) {
  const lines = tomlLines(toml)
  const start = lines.indexOf(`[${name}]`)
  if (start < 0) return []
  /** @param {string} s */
  const unquote = (s) => {
    s = s.trim()
    return s.length >= 2 && s.startsWith('"') && s.endsWith('"') ? s.slice(1, -1) : undefined
  }
  /** @type {[string, string][]} */
  const out = []
  for (const line of lines.slice(start + 1)) {
    if (line.startsWith('[')) break
    if (line === '') continue
    const bad = () => new Error(`\`[${name}]\`: \`${line}\` is not \`name = "directory"\``)
    let key
    let value
    if (line.startsWith('"')) {
      const close = line.indexOf('"', 1)
      if (close < 0) throw bad()
      key = line.slice(1, close)
      const rest = line.slice(close + 1).trimStart()
      if (!rest.startsWith('=')) throw bad()
      value = rest.slice(1)
    } else {
      const eq = line.indexOf('=')
      if (eq < 0) throw bad()
      key = line.slice(0, eq).trim()
      value = line.slice(eq + 1)
    }
    if (key === '') throw bad()
    const v = unquote(value)
    if (v === undefined) throw bad()
    out.push([key, v])
  }
  return out
}

/** A TOML line without its `#` comment, if it has one outside a string. @param {string} line */
function withoutComment(line) {
  let inString = false
  for (let i = 0; i < line.length; i++) {
    const c = line[i]
    if (c === '"') inString = !inString
    else if (c === '#' && !inString) return line.slice(0, i)
  }
  return line
}

/**
 * Whether `path` matches the whole of `pattern`: `*` within a segment, `**` across segments,
 * `?` one character other than `/`. On bytes, as the Rust command line matches.
 * @param {string} pattern
 * @param {string} path
 */
export function glob(pattern, path) {
  const SLASH = 0x2f
  const STAR = 0x2a
  const QUESTION = 0x3f
  /** @param {Uint8Array} p @param {Uint8Array} s @returns {boolean} */
  const go = (p, s) => {
    if (p.length === 0) return s.length === 0
    if (p[0] === STAR && p[1] === STAR) {
      if (p.length === 2) return true
      const rest = p[2] === SLASH ? p.subarray(3) : p.subarray(2)
      for (let i = 0; i <= s.length; i++) {
        if ((i === 0 || s[i - 1] === SLASH) && go(rest, s.subarray(i))) return true
      }
      return go(rest, s)
    }
    if (p[0] === STAR) {
      for (let i = 0; i <= s.length; i++) {
        if (i > 0 && s[i - 1] === SLASH) break
        if (go(p.subarray(1), s.subarray(i))) return true
      }
      return false
    }
    if (p[0] === QUESTION) return s.length > 0 && s[0] !== SLASH && go(p.subarray(1), s.subarray(1))
    return s.length > 0 && s[0] === p[0] && go(p.subarray(1), s.subarray(1))
  }
  return go(Buffer.from(pattern), Buffer.from(path))
}
