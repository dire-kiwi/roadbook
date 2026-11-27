// Values in their JSON form (spec/execution.md, "Values"), for the forms of simulate mode: a
// sample value of a type, to prefill a form, and a check that a value the reader typed is of
// the type, so the core is only ever given well-formed inputs. Pure and DOM-free.

import { expandType, fieldsOf, typeRefText } from '@dire-kiwi/roadbook-core'
import type { Json, TypeRef } from '@dire-kiwi/roadbook-core'
import type { Field, Ir } from './ir.ts'
import { own } from './ir.ts'

/** The largest integer the TypeScript runtime holds exactly, and so the `int` range here. */
const INT_MAX = Number.MAX_SAFE_INTEGER

/** The fields of an event type `Machine.Name`, or undefined. */
function eventFields(ir: Ir, machine: string, event: string): Field[] | undefined {
  const m = own(ir.machines, machine)
  return (m && (own(m.events, event) ?? own(m.emits, event)))?.fields
}

/**
 * A value of `type` to start a form with: zeros, empty strings, lists and maps, `null` for
 * optionals.
 */
export function sampleValue(ir: Ir, type: TypeRef | string, depth = 0): Json {
  const t = expandType(ir, type)
  switch (t.kind) {
    case 'optional':
      return null
    case 'list':
      return []
    case 'map':
      return {}
    case 'builtin':
      return t.name === 'string' ? '' : t.name === 'bool' ? false : t.name === 'json' ? null : 0
    case 'ref':
      return 'r2'
    case 'call':
      // A pending call is only ever made by the core: no input or result has the type.
      return null
  }
  if (depth > 8) return null // a recursive type: leave the rest to the reader
  if (t.kind === 'event') {
    const ev = eventFields(ir, t.machine, t.event)
    return ev ? sampleTagged(ir, t.event, ev, depth + 1) : null
  }
  const decl = own(ir.types, t.name)
  if (!decl) return null
  switch (decl.kind) {
    case 'record':
      return sampleFields(ir, fieldsOf(ir, t)!.fields, depth + 1)
    case 'error':
      return sampleTagged(ir, t.name, decl.fields, depth + 1)
    case 'enum':
      return decl.variants[0] ?? ''
    case 'alias':
    case 'extern':
      return null
  }
}

/** An object with a sample value for each field (or parameter), by name. */
export function sampleFields(ir: Ir, fields: readonly Field[], depth = 0): { [name: string]: Json } {
  const out: { [name: string]: Json } = {}
  for (const f of fields) out[f.name] = sampleValue(ir, f.type, depth)
  return out
}

/** A tagged record (an event or an error value) with sample fields. */
export function sampleTagged(ir: Ir, tag: string, fields: readonly Field[], depth = 0): { [name: string]: Json } {
  return { _tag: tag, ...sampleFields(ir, fields, depth) }
}

const describe = (v: unknown): string => (v === null ? 'null' : Array.isArray(v) ? 'a list' : typeof v === 'object' ? 'an object' : JSON.stringify(v))

/**
 * What is wrong with `value` as a value of `type`, one message per problem, each starting with
 * the path to it (`order.items[0].qty`, `scores["ann"]`). Empty when it is well formed.
 */
export function checkValue(ir: Ir, type: TypeRef | string, value: unknown, path = 'value'): string[] {
  const t = expandType(ir, type)
  const bad = (what: string) => [`${path}: expected ${what}, got ${describe(value)}`]
  switch (t.kind) {
    case 'optional':
      return value === null ? [] : checkValue(ir, t.of, value, path)
    case 'list':
      if (!Array.isArray(value)) return [`${path}: expected a list, got ${describe(value)}`]
      return value.flatMap((v, i) => checkValue(ir, t.of, v, `${path}[${i}]`))
    case 'map':
      if (!isObject(value)) return bad(`a map: an object with a member per key (${typeRefText(t)})`)
      return Object.keys(value).flatMap((k) => checkValue(ir, t.value, value[k], `${path}[${JSON.stringify(k)}]`))
    case 'builtin':
      switch (t.name) {
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
    case 'ref':
      return typeof value === 'string' ? [] : bad(`the id of a run of ${t.machine}, a string such as "r2"`)
    case 'call':
      return []
    case 'event': {
      const ev = eventFields(ir, t.machine, t.event)
      return ev ? checkTagged(ir, t.event, ev, value, path) : []
    }
  }
  const decl = own(ir.types, t.name)
  if (!decl) return []
  switch (decl.kind) {
    case 'record':
      return checkFields(ir, fieldsOf(ir, t)!.fields, value, path)
    case 'error':
      return checkTagged(ir, t.name, decl.fields, value, path)
    case 'enum':
      return typeof value === 'string' && decl.variants.includes(value) ? [] : bad(`one of ${decl.variants.map((v) => JSON.stringify(v)).join(', ')}`)
    case 'alias':
    case 'extern':
      return []
  }
}

const isObject = (v: unknown): v is Record<string, unknown> => typeof v === 'object' && v !== null && !Array.isArray(v)

/** Checks an object with exactly these fields (or parameters), by name. */
export function checkFields(ir: Ir, fields: readonly Field[], value: unknown, path = 'value', extra: string[] = []): string[] {
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
export function checkTagged(ir: Ir, tag: string, fields: readonly Field[], value: unknown, path = 'value'): string[] {
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
