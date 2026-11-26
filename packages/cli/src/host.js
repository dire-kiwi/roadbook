// The host side of the WebAssembly compiler: Node's file system, and the module's functions
// with their JSON parsed.

import fs from 'node:fs'
import path from 'node:path'
import * as wasm from '../wasm/roadbook_wasm.js'

/**
 * The text of an I/O error as the Rust command line prints it (`std::io::Error`), so that
 * both print the same: `No such file or directory (os error 2)`.
 * @param {unknown} e
 * @returns {string}
 */
export function ioMessage(e) {
  const err = /** @type {NodeJS.ErrnoException} */ (e)
  if (err instanceof InvalidUtf8) return err.message
  const errno = typeof err?.errno === 'number' ? Math.abs(err.errno) : undefined
  const text = err?.code ? OS_ERRORS[err.code] : undefined
  if (text && errno !== undefined && process.platform !== 'win32') return `${text} (os error ${errno})`
  return String(err?.message ?? err)
}

/** strerror(3) for the errors reading a project can meet. */
/** @type {Record<string, string>} */
const OS_ERRORS = {
  ENOENT: 'No such file or directory',
  EACCES: 'Permission denied',
  EPERM: 'Operation not permitted',
  EISDIR: 'Is a directory',
  ENOTDIR: 'Not a directory',
  ELOOP: 'Too many levels of symbolic links',
  ENAMETOOLONG: 'File name too long',
  EMFILE: 'Too many open files',
  EIO: 'Input/output error',
}

class InvalidUtf8 extends Error {
  constructor() {
    super('stream did not contain valid UTF-8')
  }
}

const utf8 = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true })

/**
 * A file's text, as Rust's `read_to_string` reads it: invalid UTF-8 is an error, and a
 * byte order mark is kept.
 * @param {string} file
 * @returns {string}
 */
export function readText(file) {
  const bytes = fs.readFileSync(file)
  try {
    return utf8.decode(bytes)
  } catch {
    throw new InvalidUtf8()
  }
}

/** A path as the module takes it: absolute, with `/` separators. @param {string} p */
export const slash = (p) => (path.sep === '/' ? p : p.split(path.sep).join('/'))

/**
 * A file system the module reads through (see crates/roadbook-wasm): paths are absolute and
 * `/`-separated; `readFile` throws the reason, as a string, when it cannot read a file.
 * @typedef {object} Host
 * @property {(path: string) => string} readFile
 * @property {(path: string) => boolean} isDir
 * @property {(path: string) => string | undefined} realpath
 */

/**
 * The host object the module reads files through: Node's file system.
 * @type {Host}
 */
export const nodeHost = {
  readFile(p) {
    try {
      return readText(p)
    } catch (e) {
      throw ioMessage(e)
    }
  },
  isDir(p) {
    try {
      return fs.statSync(p).isDirectory()
    } catch {
      return false
    }
  },
  realpath(p) {
    try {
      return slash(fs.realpathSync(p))
    } catch {
      return undefined
    }
  },
}

/**
 * @typedef {object} Config What the module needs to know of a project.
 * @property {string} root
 * @property {[string, string][]} [packages]
 * @property {boolean} [nodeModules]
 * @property {string[]} [allowUnassigned]
 * @property {boolean} [warnInvokeTimeout]
 * @property {Record<string, string>} [files]
 */

/**
 * @typedef {object} Diagnostics
 * @property {string} report The `roadbook check --json` text.
 * @property {string} rendered The human-readable diagnostics.
 * @property {number} errors
 * @property {number} warnings
 */

/**
 * What a function returns: `fatal` (and nothing else) if the entry could not be read.
 * @template T
 * @typedef {Diagnostics & T & { fatal?: string | undefined }} Result
 */

/**
 * @param {Host | null} host
 * @param {Config} config
 * @param {string[]} roots
 * @returns {Result<{}>}
 */
export const check = (host, config, roots) => JSON.parse(wasm.check(host, JSON.stringify(config), JSON.stringify(roots)))

/**
 * @param {Host | null} host
 * @param {Config} config
 * @param {string} entry
 * @returns {Result<{ ir: string | null }>}
 */
export const compile = (host, config, entry) => JSON.parse(wasm.compile(host, JSON.stringify(config), entry))

/**
 * @param {Host | null} host
 * @param {Config} config
 * @param {string} entry
 * @returns {Result<{ code: string | null, error: string | null }>}
 */
export const gen = (host, config, entry) => JSON.parse(wasm.gen(host, JSON.stringify(config), entry))

/**
 * @param {string} name
 * @param {string} source
 * @returns {{ formatted: string, errors?: undefined } | { formatted?: undefined, errors: string[] }}
 */
export const format = (name, source) => JSON.parse(wasm.format(name, source))

export const fatalReport = wasm.fatalReport
export const version = wasm.version
