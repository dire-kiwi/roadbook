/**
 * The Roadbook compiler for Node, as WebAssembly: what the `roadbook` command does, as
 * functions for build tools. Every function is synchronous.
 *
 * A function finds the project as the command line does: the nearest directory upward from
 * the (first) path given holding a `roadbook.toml`, whose `[packages]` and
 * `allow-unassigned` it follows, and it reads files from disk. `files` puts files in memory
 * in front of the disk (unsaved editor buffers, generated charts); with `disk: false` they
 * are the only files, the project root is `cwd` and no `roadbook.toml` is read.
 */

export interface Options {
  /** The directory relative paths are resolved against. Default: `process.cwd()`. */
  cwd?: string
  /** Files held in memory, by path (absolute, or relative to `cwd`): their text. */
  files?: Record<string, string>
  /** Whether files not in `files`, and `roadbook.toml`, are read from disk. Default: `true`. */
  disk?: boolean
}

/** One range in a file: 1-based lines and columns (Unicode scalar values), end exclusive. */
export interface Range {
  file: string
  line: number
  col: number
  endLine: number
  endCol: number
}

/** A diagnostic, as `roadbook check --json` prints it (docs/design/language.md, "Diagnostics as JSON"). */
export interface Diagnostic extends Range {
  code: string
  severity: 'error' | 'warning'
  message: string
  label: string | null
  labels: (Range & { message: string })[]
  help: string | null
  fixes: { message: string; edits: (Range & { text: string })[] }[]
}

/** A step of a check's path, as `roadbook paths --json` gives the steps of a path. */
export interface CheckStep {
  from: string
  index: number
  trigger: string
  guard: string | null
  to: string | null
}

/** A `check` line and its verdict, as `roadbook check --json` lists it in `checks`. */
export interface Check extends Range {
  machine: string
  check: string
  doc: string | null
  verdict: 'proved' | 'conditional' | 'failed' | 'unchecked'
  assumptions: string[]
  path: CheckStep[]
}

/** The object `roadbook check --json` prints. */
export interface Report {
  diagnostics: Diagnostic[]
  errors: number
  warnings: number
  fatal: string | null
  checks: Check[]
}

export interface Diagnostics {
  /** The diagnostics, parsed. */
  report: Report
  /** The same, as the text `roadbook check --json` prints. */
  json: string
  /** The diagnostics in the human-readable form the command line prints on standard error. */
  rendered: string
  errors: number
  warnings: number
}

export interface CompileResult extends Diagnostics {
  /** The IR (spec/ir.md), or `null` if there are errors. */
  ir: Record<string, unknown> | null
  /** The IR as JSON in the committed-file form (`roadbook export ir`), or `null`. */
  irJson: string | null
}

export interface GenResult extends Diagnostics {
  /** The text of the `<entry>.ts` module for `@dire-kiwi/roadbook-effect`, or `null`. */
  code: string | null
  /** Why no module was generated, when it is not the diagnostics' errors, or `null`. */
  error: string | null
}

export type FormatResult = { formatted: string; errors: [] } | { formatted: null; errors: string[] }

/**
 * A usage or I/O error that stopped the call before anything was compiled: a path that does
 * not exist, an entry that cannot be read, a bad `roadbook.toml`.
 */
export declare class RoadbookError extends Error {}

/**
 * Checks files, as `roadbook check` does: each `.road` file (a directory stands for every one
 * under it, none for the whole project) is compiled as an entry with everything it imports.
 */
export declare function check(paths?: string[], options?: Options & { warnInvokeTimeout?: boolean }): Diagnostics

/** Compiles an entry and everything it imports to the IR, as `roadbook export ir` does. */
export declare function compile(entry: string, options?: Options): CompileResult

/** The TypeScript module for an entry, as `roadbook gen` writes it next to the file. */
export declare function gen(entry: string, options?: Options): GenResult

/**
 * A source in the canonical format, as `roadbook fmt` writes it, or its syntax errors
 * (rendered, naming the file `name`).
 */
export declare function format(source: string, name?: string): FormatResult

/** The compiler's version. */
export declare function version(): string
