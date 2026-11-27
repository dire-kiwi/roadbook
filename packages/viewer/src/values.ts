// Values in their JSON form (spec/execution.md, "Values"), for the forms of simulate mode: a
// sample value of a type, to prefill a form, and a check that a value the reader typed is of
// the type, so the core is only ever given well-formed inputs. Pure and DOM-free.

import { parseTypeRef, refMachine } from '@dire-kiwi/roadbook-core'
import type { Json } from '@dire-kiwi/roadbook-core'
import type { Field, Ir } from './ir.ts'
import { own } from './ir.ts'

/** The largest integer the TypeScript runtime holds exactly, and so the `int` range here. */
const INT_MAX = Number.MAX_SAFE_INTEGER

/** The fields of an event type `Machine.Name`, or undefined. */
function eventFields(ir: Ir, base: string): Field[] | undefined {
  const dot = base.indexOf('.')
  if (dot < 0) return undefined
  const m = own(ir.machines, base.slice(0, dot))
  const name = base.slice(dot + 1)
  return (m && (own(m.events, name) ?? own(m.emits, name)))?.fields
}

/** A value of `type` to start a form with: zeros, empty strings and lists, `null` for optionals. */
export function sampleValue(ir: Ir, type: string, depth = 0): Json {
  const t = parseTypeRef(type)
  if (t.suffixes.length) return t.suffixes[t.suffixes.length - 1] === '?' ? null : []
  return sampleBase(ir, t.base, depth)
}

function sampleBase(ir: Ir, base: string, depth: number): Json {
  switch (base) {
    case 'string':
      return ''
    case 'int':
    case 'float':
    case 'duration':
      return 0
    case 'bool':
      return false
    case 'json':
      return null
  }
  if (refMachine(base)) return 'r2'
  if (depth > 8) return null // a recursive type: leave the rest to the reader
  const ev = eventFields(ir, base)
  if (ev) return sampleTagged(ir, base.slice(base.indexOf('.') + 1), ev, depth + 1)
  const decl = own(ir.types, base)
  if (!decl) return null
  switch (decl.kind) {
    case 'record':
      return sampleFields(ir, decl.fields, depth + 1)
    case 'error':
      return sampleTagged(ir, base, decl.fields, depth + 1)
    case 'alias':
      return sampleValue(ir, decl.type, depth + 1)
    case 'enum':
      return decl.variants[0] ?? ''
    case 'extern':
      return null
  }
}

/** An object with a sample value for each field (or parameter), by name. */
export function sampleFields(ir: Ir, fields: Field[], depth = 0): { [name: string]: Json } {
  const out: { [name: string]: Json } = {}
  for (const f of fields) out[f.name] = sampleValue(ir, f.type, depth)
  return out
}

/** A tagged record (an event or an error value) with sample fields. */
export function sampleTagged(ir: Ir, tag: string, fields: Field[], depth = 0): { [name: string]: Json } {
  return { _tag: tag, ...sampleFields(ir, fields, depth) }
}

const describe = (v: unknown): string => (v === null ? 'null' : Array.isArray(v) ? 'a list' : typeof v === 'object' ? 'an object' : JSON.stringify(v))

/**
 * What is wrong with `value` as a value of `type`, one message per problem, each starting with
 * the path to it (`order.items[0].qty`). Empty when it is well formed.
 */
export function checkValue(ir: Ir, type: string, value: unknown, path = 'value'): string[] {
  const t = parseTypeRef(type)
  return checkSuffixed(ir, t.base, t.suffixes, value, path)
}

function checkSuffixed(ir: Ir, base: string, suffixes: ReadonlyArray<'[]' | '?'>, value: unknown, path: string): string[] {
  if (!suffixes.length) return checkBase(ir, base, value, path)
  const outer = suffixes[suffixes.length - 1]
  const inner = suffixes.slice(0, -1)
  if (outer === '?') return value === null ? [] : checkSuffixed(ir, base, inner, value, path)
  if (!Array.isArray(value)) return [`${path}: expected a list, got ${describe(value)}`]
  return value.flatMap((v, i) => checkSuffixed(ir, base, inner, v, `${path}[${i}]`))
}

function checkBase(ir: Ir, base: string, value: unknown, path: string): string[] {
  const bad = (what: string) => [`${path}: expected ${what}, got ${describe(value)}`]
  switch (base) {
    case 'string':
      return typeof value === 'string' ? [] : bad('a string')
    case 'int':
      return Number.isInteger(value) && Math.abs(value as number) <= INT_MAX ? [] : bad(`an integer within ±${INT_MAX}`)
    case 'float':
      return typeof value === 'number' && Number.isFinite(value) ? [] : bad('a number')
    case 'duration':
      return Number.isInteger(value) && (value as number) >= 0 ? [] : bad('a duration: a whole number of milliseconds, 0 or more')
    case 'bool':
      return typeof value === 'boolean' ? [] : bad('true or false')
    case 'json':
      return []
  }
  if (refMachine(base)) return typeof value === 'string' ? [] : bad(`the id of a run of ${refMachine(base)}, a string such as "r2"`)
  const ev = eventFields(ir, base)
  if (ev) return checkTagged(ir, base.slice(base.indexOf('.') + 1), ev, value, path)
  const decl = own(ir.types, base)
  if (!decl) return []
  switch (decl.kind) {
    case 'record':
      return checkFields(ir, decl.fields, value, path)
    case 'error':
      return checkTagged(ir, base, decl.fields, value, path)
    case 'alias':
      return checkValue(ir, decl.type, value, path)
    case 'enum':
      return typeof value === 'string' && decl.variants.includes(value) ? [] : bad(`one of ${decl.variants.map((v) => JSON.stringify(v)).join(', ')}`)
    case 'extern':
      return []
  }
}

const isObject = (v: unknown): v is Record<string, unknown> => typeof v === 'object' && v !== null && !Array.isArray(v)

/** Checks an object with exactly these fields (or parameters), by name. */
export function checkFields(ir: Ir, fields: Field[], value: unknown, path = 'value', extra: string[] = []): string[] {
  if (!isObject(value)) return [`${path}: expected an object, got ${describe(value)}`]
  const out: string[] = []
  const join = (k: string) => (path ? `${path}.${k}` : k)
  for (const f of fields) {
    if (!Object.prototype.hasOwnProperty.call(value, f.name)) out.push(`${join(f.name)}: missing (${f.type})`)
    else out.push(...checkValue(ir, f.type, value[f.name], join(f.name)))
  }
  const known = new Set([...fields.map((f) => f.name), ...extra])
  for (const k of Object.keys(value)) if (!known.has(k)) out.push(`${join(k)}: not a field`)
  return out
}

/** Checks a tagged record: `_tag` is `tag`, and the fields. */
export function checkTagged(ir: Ir, tag: string, fields: Field[], value: unknown, path = 'value'): string[] {
  if (!isObject(value)) return [`${path}: expected an object, got ${describe(value)}`]
  const out = value._tag === tag ? [] : [`${path}._tag: expected ${JSON.stringify(tag)}`]
  return [...out, ...checkFields(ir, fields, value, path, ['_tag'])]
}

/** Parses JSON text; the problem in words if it is not JSON. */
export function parseJson(text: string): { value: Json } | { error: string } {
  try {
    return { value: JSON.parse(text) as Json }
  } catch (e) {
    return { error: `not JSON: ${e instanceof Error ? e.message : String(e)}` }
  }
}
