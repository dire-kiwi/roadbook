// `roadbook`, the command line, on the WebAssembly compiler: `check`, `fmt`, `gen` and
// `export ir`, printing what crates/roadbook-cli prints, byte for byte, with the same exit
// codes (0: no errors; 1: the input has errors, or is not formatted or generated; 2: bad
// usage or an I/O error). Each command follows its Rust module in crates/roadbook-cli/src/commands.

import fs from 'node:fs'
import * as wasm from './host.js'
import { ioMessage, nodeHost, readText } from './host.js'
import { Workspace, isFile } from './workspace.js'

const OK = 0
const ERRORS = 1
const USAGE = 2

/** @param {string} s */
const out = (s) => void process.stdout.write(s)
/** @param {string} s */
const err = (s) => void process.stderr.write(s)

/** An I/O or usage error. @param {string} message */
function fatal(message) {
  err(`error: ${message}\n`)
  return USAGE
}

/** An I/O or usage error, also printed as the JSON object if `json`. @param {string} message @param {boolean} json */
function fatalJson(message, json) {
  if (json) out(`${wasm.fatalReport(message)}\n`)
  return fatal(message)
}

/** "2 errors, 1 warning". @param {number} errors @param {number} warnings */
export function summary(errors, warnings) {
  const n = (/** @type {number} */ k, /** @type {string} */ what) => `${k} ${what}${k === 1 ? '' : 's'}`
  if (errors === 0) return n(warnings, 'warning')
  if (warnings === 0) return n(errors, 'error')
  return `${n(errors, 'error')}, ${n(warnings, 'warning')}`
}

/**
 * Prints the diagnostics of a compilation, then, if it evaluated checks, the line counting
 * their verdicts, with every check listed before it if `list`. Returns the exit code the
 * diagnostics call for.
 * @param {import('./host.js').Diagnostics} d
 * @param {boolean} json
 * @param {boolean} [list]
 */
function diagnostics(d, json, list = false) {
  if (json) out(`${d.report}\n`)
  else {
    if (d.errors + d.warnings > 0) err(`${d.rendered}\n${summary(d.errors, d.warnings)}\n`)
    if (d.checksSummary !== null) err(`${list ? d.checksList : ''}${d.checksSummary}\n`)
  }
  return d.errors > 0 ? ERRORS : OK
}

/** A usage error. */
class Usage extends Error {}

/**
 * The arguments of a command: flags (each with whether it takes a value) and positionals.
 * @param {string[]} args
 * @param {Record<string, { name: string, value: boolean }>} flags by spelling: `--json`, `-o`
 */
function parse(args, flags) {
  /** @type {Record<string, string[]>} */
  const values = {}
  /** @type {string[]} */
  const positional = []
  let rest = false
  for (let i = 0; i < args.length; i++) {
    const a = /** @type {string} */ (args[i])
    if (rest || a === '-' || !a.startsWith('-')) {
      positional.push(a)
      continue
    }
    if (a === '--') {
      rest = true
      continue
    }
    const eq = a.startsWith('--') ? a.indexOf('=') : -1
    let spelling = eq < 0 ? a : a.slice(0, eq)
    let inline = eq < 0 ? undefined : a.slice(eq + 1)
    if (!a.startsWith('--') && a.length > 2) {
      spelling = a.slice(0, 2)
      inline = a.slice(2)
    }
    const flag = flags[spelling]
    if (!flag) throw new Usage(`unexpected argument '${a}' found`)
    if (flag.value) {
      const v = inline ?? args[++i]
      if (v === undefined) throw new Usage(`a value is required for '${spelling}' but none was supplied`)
      ;(values[flag.name] ??= []).push(v)
    } else {
      if (inline !== undefined) throw new Usage(`unexpected value '${inline}' for '${spelling}' found`)
      ;(values[flag.name] ??= []).push('')
    }
  }
  return { values, positional }
}

const HELP = `Statecharts as code: check, format, generate and export \`.road\` files

Usage: roadbook <COMMAND>

Commands:
  check       Compile and report diagnostics
  fmt         Format \`.road\` files in place, canonically
  gen         Write the TypeScript module for each \`.road\` file next to it: \`<file>.road.ts\`
  export ir   Write a chart's IR as JSON, in the committed-file form

  check [--json] [--checks] [--warn invoke-timeout] [PATHS]...
  fmt [--check] [PATHS]...
  gen [--check] [PATHS]...
  export ir <FILE> [-o <FILE>]

This is the npm distribution of the compiler (WebAssembly). \`viz\`, \`paths\`, \`lsp\` and
\`export mermaid\` are in the native command line: cargo install roadbook-cli.

Options:
  -h, --help     Print help
  -V, --version  Print version
`

/**
 * Runs the command line and returns the exit code.
 * @param {string[]} argv the arguments after the program's name
 * @returns {number}
 */
export function main(argv) {
  const [command, ...args] = argv
  try {
    switch (command) {
      case undefined:
        err(HELP)
        return USAGE
      case '-h':
      case '--help':
      case 'help':
        out(HELP)
        return OK
      case '-V':
      case '--version':
        out(`roadbook ${wasm.version()}\n`)
        return OK
      case 'check':
        return check(args)
      case 'fmt':
        return fmtCommand(args)
      case 'gen':
        return gen(args)
      case 'export':
        if (args[0] === 'ir') return exportIr(args.slice(1))
        if (args[0] === 'mermaid') throw new Usage('`roadbook export mermaid` is only in the native command line (cargo install roadbook-cli)')
        throw new Usage(`unrecognized subcommand '${args[0] ?? ''}': the npm command line exports \`ir\``)
      case 'viz':
      case 'paths':
      case 'lsp':
        throw new Usage(`\`roadbook ${command}\` is only in the native command line (cargo install roadbook-cli)`)
      default:
        throw new Usage(`unrecognized subcommand '${command}'`)
    }
  } catch (e) {
    if (e instanceof Usage) {
      err(`error: ${e.message}\n\nFor more information, try 'roadbook --help'.\n`)
      return USAGE
    }
    throw e
  }
}

const HELP_FLAG = { '-h': { name: 'help', value: false }, '--help': { name: 'help', value: false } }

/** `roadbook check [--json] [--checks] [--warn invoke-timeout] [paths…]` @param {string[]} args */
function check(args) {
  const { values, positional: paths } = parse(args, {
    ...HELP_FLAG,
    '--json': { name: 'json', value: false },
    '--checks': { name: 'checks', value: false },
    '--warn': { name: 'warn', value: true },
  })
  if (values.help) return help()
  for (const w of values.warn ?? []) {
    if (w !== 'invoke-timeout') throw new Usage(`invalid value '${w}' for '--warn <WARNING>' [possible values: invoke-timeout]`)
  }
  const json = values.json !== undefined
  let ws
  let files
  try {
    ws = Workspace.find(paths[0])
    files = ws.inputs(paths)
  } catch (e) {
    return fatalJson(/** @type {Error} */ (e).message, json)
  }
  const config = ws.config({ warnInvokeTimeout: values.warn !== undefined })
  const result = wasm.check(nodeHost, config, files)
  if (result.fatal !== undefined) return fatalJson(result.fatal, json)
  return diagnostics(result, json, values.checks !== undefined)
}

/** `roadbook fmt [--check] [paths…]` @param {string[]} args */
function fmtCommand(args) {
  const { values, positional: paths } = parse(args, { ...HELP_FLAG, '--check': { name: 'check', value: false } })
  if (values.help) return help()
  const checkOnly = values.check !== undefined
  let ws
  let files
  try {
    ws = Workspace.find(paths[0])
    files = ws.inputs(paths)
  } catch (e) {
    return fatal(/** @type {Error} */ (e).message)
  }
  let exit = OK
  for (const file of files) {
    const disk = ws.diskPath(file)
    let src
    try {
      src = readText(disk)
    } catch (e) {
      return fatal(`cannot read \`${file}\`: ${ioMessage(e)}`)
    }
    const result = wasm.format(file, src)
    if (result.errors) {
      for (const d of result.errors) err(`${d}\n`)
      err(`\`${file}\` has syntax errors and was not formatted\n`)
      exit = ERRORS
    } else if (result.formatted !== src) {
      if (checkOnly) {
        out(`${file}\n`)
        exit = ERRORS
      } else {
        try {
          fs.writeFileSync(disk, result.formatted)
        } catch (e) {
          return fatal(`cannot write \`${file}\`: ${ioMessage(e)}`)
        }
      }
    }
  }
  return exit
}

/** `roadbook gen [--check] [paths…]` @param {string[]} args */
function gen(args) {
  const { values, positional: paths } = parse(args, { ...HELP_FLAG, '--check': { name: 'check', value: false } })
  if (values.help) return help()
  const checkOnly = values.check !== undefined
  let ws
  let files
  let orphans
  try {
    ws = Workspace.find(paths[0])
    files = ws.genInputs(paths)
    orphans = findOrphans(ws, paths)
  } catch (e) {
    return fatal(/** @type {Error} */ (e).message)
  }
  let exit = OK
  for (const [module, chart] of orphans) {
    if (checkOnly) {
      out(`${module}\n`)
      exit = ERRORS
    }
    err(`${checkOnly ? 'error' : 'warning'}: \`${module}\` was generated from \`${chart}\`, which no longer exists; delete it\n`)
  }
  const config = ws.config()
  for (const file of files) {
    const result = wasm.gen(nodeHost, config, file)
    if (result.fatal !== undefined) return fatal(result.fatal)
    if (result.errors > 0) {
      diagnostics(result, false)
      err(`\`${file}\` has errors; nothing was generated for it\n`)
      exit = ERRORS
      continue
    }
    if (result.code === null) {
      err(`error: ${result.error}\n`)
      exit = ERRORS
      continue
    }
    const target = `${file}.ts`
    const disk = ws.diskPath(target)
    let old
    try {
      old = readText(disk)
    } catch {
      old = undefined
    }
    if (old === result.code) continue
    if (checkOnly) {
      out(`${target}\n`)
      exit = ERRORS
    } else {
      try {
        fs.writeFileSync(disk, result.code)
      } catch (e) {
        return fatal(`cannot write \`${target}\`: ${ioMessage(e)}`)
      }
    }
  }
  return exit
}

/**
 * The modules `roadbook gen` wrote whose chart is gone, with the chart's path.
 * @param {Workspace} ws
 * @param {string[]} paths
 * @returns {[string, string][]}
 */
function findOrphans(ws, paths) {
  /** @type {[string, string][]} */
  const found = []
  for (const module of ws.genOutputs(paths)) {
    let chart = module
    while (chart.endsWith('.ts')) chart = chart.slice(0, -'.ts'.length)
    if (fs.existsSync(ws.diskPath(chart))) continue
    let text = ''
    try {
      text = readText(ws.diskPath(module))
    } catch {
      continue
    }
    if (text.startsWith('// Generated by roadbook gen from ')) found.push([module, chart])
  }
  return found
}

/** `roadbook export ir <file> [-o out]` @param {string[]} args */
function exportIr(args) {
  const { values, positional } = parse(args, {
    ...HELP_FLAG,
    '-o': { name: 'output', value: true },
    '--output': { name: 'output', value: true },
  })
  if (values.help) return help()
  const [file, ...extra] = positional
  if (file === undefined) throw new Usage('the following required arguments were not provided: <FILE>')
  if (extra.length > 0) throw new Usage(`unexpected argument '${extra[0]}' found`)
  if ((values.output?.length ?? 0) > 1) throw new Usage("the argument '--output <FILE>' cannot be used multiple times")
  const output = values.output?.[0]
  if (!isFile(file)) return fatal(`\`${file}\` is not a file`)
  let ws
  try {
    ws = Workspace.find(file)
  } catch (e) {
    return fatal(/** @type {Error} */ (e).message)
  }
  const result = wasm.compile(nodeHost, ws.config(), ws.projectPath(file))
  if (result.fatal !== undefined) return fatal(result.fatal)
  const exit = diagnostics(result, false)
  if (result.ir === null) return exit
  if (output === undefined) {
    out(result.ir)
  } else {
    try {
      fs.writeFileSync(output, result.ir)
    } catch (e) {
      return fatal(`cannot write \`${output}\`: ${ioMessage(e)}`)
    }
  }
  return exit
}

function help() {
  out(HELP)
  return OK
}
