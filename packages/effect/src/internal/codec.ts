/**
 * Values at the API boundary. The core holds every value in its JSON form (execution.md's
 * "Values"); host code sees records as plain objects, errors as instances of their tagged error
 * classes, enums as string literals, `duration` as an Effect `Duration`, optional as `null`,
 * `int` as a `number`, a map as a plain object with a member per key, and a `ref` as a handle to
 * the run it refers to (its run id in the core). An instance of a generic record is a plain
 * object like any record. A pending call (`call c`) never leaves its run, so host code never
 * sees one; it keeps its JSON form, `{from, invoke}`, both ways. The conversion is driven by the
 * IR's type references.
 */
import {
  type Builtin,
  expandType,
  type Field,
  fieldsOf,
  type Ir,
  isRunId,
  type Json,
  type Tagged,
  type TypeRef
} from "@dire-kiwi/roadbook-core"
import { Duration, Predicate } from "effect"
import { portOf } from "./port.ts"

/** A tagged error class, as the generated module exports it. */
export type ErrorClass = new(fields: any) => unknown

/** A value that is not of its declared type. The driver makes it a `host` defect. */
export class CodecError extends Error {
  override readonly name = "CodecError"
}

const MAX_INT = Number.MAX_SAFE_INTEGER

/**
 * Add member `key` to `object` as an own data property: by assignment, a key `__proto__` (a
 * map's, say) would set the prototype instead.
 */
const defineMember = (object: object, key: string, value: unknown) => {
  Object.defineProperty(object, key, { value, enumerable: true, writable: true, configurable: true })
}

/** Whether `value` is a plain object: made by `{}` or `Object.create(null)`, not a class instance. */
const isPlainObject = (value: unknown): value is Record<string, unknown> => {
  if (typeof value !== "object" || value === null) return false
  const proto = Object.getPrototypeOf(value)
  return proto === Object.prototype || proto === null
}

/**
 * A copy of host value `value` that is JSON, for a `json` or `extern` value: `null`, a boolean,
 * a finite number, a string, an array or a plain object of JSON values. Anything else (`NaN`,
 * `Infinity`, `undefined`, a function, a `Date`, a cycle) throws a `CodecError`, which makes it
 * a host defect before it can reach the snapshot or the trace. `ancestors` are the arrays and
 * objects `value` is inside.
 */
const toJson = (value: unknown, what: string, ancestors: Array<object>): Json => {
  switch (typeof value) {
    case "string":
    case "boolean":
      return value
    case "number":
      if (!Number.isFinite(value)) throw new CodecError(`${what} is ${value}, which is not JSON`)
      return value === 0 ? 0 : value
    case "object": {
      if (value === null) return null
      if (ancestors.includes(value)) throw new CodecError(`${what} contains itself, so it is not JSON`)
      ancestors.push(value)
      let out: Json
      if (Array.isArray(value)) {
        const items: Array<Json> = []
        for (let i = 0; i < value.length; i++) items.push(toJson(value[i], `${what}[${i}]`, ancestors))
        out = items
      } else {
        const proto = Object.getPrototypeOf(value)
        if (proto !== Object.prototype && proto !== null) {
          throw new CodecError(`${what} is a ${proto?.constructor?.name ?? "class instance"}, not a plain object, so it is not JSON`)
        }
        const members: { [key: string]: Json } = {}
        for (const [key, member] of Object.entries(value)) {
          defineMember(members, key, toJson(member, `${what}.${key}`, ancestors))
        }
        out = members
      }
      ancestors.pop()
      return out
    }
    default:
      throw new CodecError(`${what} is ${typeof value === "undefined" ? "undefined" : `a ${typeof value}`}, which is not JSON`)
  }
}

/** The handles a run was given, by run id: what its `ref`s decode to. */
export type Refs = Map<string, object>

export class Codec {
  readonly ir: Ir
  private readonly errors: Readonly<Record<string, ErrorClass>>
  /**
   * The handles of one run. Encoding a handle adds it here; decoding a run id reads it from
   * here. Without a table, handles encode to their run ids and run ids cannot be decoded.
   */
  readonly refs: Refs | undefined

  constructor(ir: Ir, errors: Readonly<Record<string, ErrorClass>>, refs?: Refs) {
    this.ir = ir
    this.errors = errors
    this.refs = refs
  }

  /** The same codec with a table of handles of its own, for one run. */
  withRefs(refs: Refs): Codec {
    return new Codec(this.ir, this.errors, refs)
  }

  // ---- host -> core ----

  /** The JSON form of host value `value` of type `type`. Throws a `CodecError`. */
  encode(type: TypeRef | string, value: unknown, what: string): Json {
    const t = expandType(this.ir, type)
    switch (t.kind) {
      case "optional":
        return value === null || value === undefined ? null : this.encode(t.of, value, what)
      case "list":
        if (!Array.isArray(value)) throw new CodecError(`${what} is not a list`)
        return value.map((item, i) => this.encode(t.of, item, `${what}[${i}]`))
      case "map": {
        if (!isPlainObject(value)) throw new CodecError(`${what} is not a map: a plain object, a member per key`)
        const out: { [key: string]: Json } = {}
        for (const key of Object.keys(value)) {
          defineMember(out, key, this.encode(t.value, value[key], `${what}[${JSON.stringify(key)}]`))
        }
        return out
      }
      case "builtin":
        return encodeBuiltin(t.name, value, what)
      case "ref": {
        const port = portOf(value)
        if (port === undefined) throw new CodecError(`${what} is not a handle to a running ${t.machine}`)
        if (port.machine !== t.machine) {
          throw new CodecError(`${what} is a handle to a ${port.machine}, not a ${t.machine}`)
        }
        const hash = this.ir.machines[t.machine]?.hash
        if (hash !== undefined && port.hash !== hash) {
          throw new CodecError(`${what} is a handle to another version of ${t.machine} (${port.hash}, not ${hash})`)
        }
        this.refs?.set(port.id, value as object)
        return port.id
      }
      case "call": {
        const call = value as { readonly from?: unknown; readonly invoke?: unknown }
        if (!Predicate.isObject(value) || !isRunId(call.from) || !Number.isSafeInteger(call.invoke) || (call.invoke as number) < 1) {
          throw new CodecError(`${what} is not a pending call: {from: a run id, invoke: a positive int}`)
        }
        return { from: call.from as string, invoke: call.invoke as number }
      }
      case "event":
        throw new CodecError(`${what} has unknown type ${t.machine}.${t.event}`)
      case "named":
        break
    }
    const decl = Object.hasOwn(this.ir.types, t.name) ? this.ir.types[t.name] : undefined
    switch (decl?.kind) {
      case undefined:
      case "alias":
        throw new CodecError(`${what} has unknown type ${t.name}`)
      case "enum":
        if (typeof value !== "string" || !decl.variants.includes(value)) {
          throw new CodecError(`${what} is not a variant of ${t.name}`)
        }
        return value
      case "record":
        return this.encodeFields(fieldsOf(this.ir, t)!.fields, value, what)
      case "error": {
        if (!Predicate.isObject(value) || (value as { _tag?: unknown })._tag !== t.name) {
          throw new CodecError(`${what} is not a ${t.name}`)
        }
        return { _tag: t.name, ...this.encodeFields(decl.fields, value, what) }
      }
      case "extern":
        return toJson(value, what, [])
    }
  }

  private encodeFields(fields: ReadonlyArray<Field>, value: unknown, what: string): { [key: string]: Json } {
    if (!Predicate.isObject(value)) throw new CodecError(`${what} is not a record`)
    const record = value as Record<string, unknown>
    const out: { [key: string]: Json } = {}
    for (const field of fields) out[field.name] = this.encode(field.type, record[field.name], `${what}.${field.name}`)
    return out
  }

  /**
   * The JSON form of a tagged value (an error a task failed with, an event): `_tag` and the
   * fields its declaration lists. `fields` gives the declaration when it is not in `types` (a
   * machine's event); a tag that is declared nowhere keeps no fields.
   */
  encodeTagged(value: unknown, what: string, fields?: ReadonlyArray<Field>): Tagged {
    const tag = Predicate.isObject(value) ? (value as { _tag?: unknown })._tag : undefined
    if (typeof tag !== "string") throw new CodecError(`${what} is not a tagged value`)
    const decl = this.ir.types[tag]
    const declared = fields ?? (decl?.kind === "error" || decl?.kind === "record" ? decl.fields : [])
    return { ...this.encodeFields(declared, value, what), _tag: tag }
  }

  /** The machine's arguments, keyed by parameter name. */
  encodeArgs(params: ReadonlyArray<Field>, args: unknown, what: string): { [param: string]: Json } {
    return this.encodeFields(params, args ?? {}, what)
  }

  // ---- core -> host ----

  /** The host value of JSON value `value` of type `type`. */
  decode(type: TypeRef | string, value: Json): unknown {
    const t = expandType(this.ir, type)
    switch (t.kind) {
      case "optional":
        return value === null ? null : this.decode(t.of, value)
      case "list":
        return (value as ReadonlyArray<Json>).map((item) => this.decode(t.of, item))
      case "map": {
        const map = value as { readonly [key: string]: Json }
        const out: Record<string, unknown> = {}
        for (const key of Object.keys(map)) defineMember(out, key, this.decode(t.value, map[key]!))
        return out
      }
      case "builtin":
        return t.name === "duration" ? Duration.millis(value as number) : value
      case "ref": {
        const handle = this.refs?.get(value as string)
        if (handle === undefined) throw new CodecError(`no handle to the run ${JSON.stringify(value)}`)
        return handle
      }
      // A pending call keeps its JSON form.
      case "call":
      case "event":
        return value
      case "named":
        break
    }
    const decl = Object.hasOwn(this.ir.types, t.name) ? this.ir.types[t.name] : undefined
    switch (decl?.kind) {
      case "record":
        return this.decodeFields(fieldsOf(this.ir, t)!.fields, value)
      case "error":
        return this.decodeError(value as Tagged)
      default:
        return value
    }
  }

  private decodeFields(fields: ReadonlyArray<Field>, value: Json): Record<string, unknown> {
    const record = value as { readonly [key: string]: Json }
    const out: Record<string, unknown> = {}
    for (const field of fields) out[field.name] = this.decode(field.type, record[field.name] ?? null)
    return out
  }

  /** An error value as an instance of its class, or a plain tagged object if there is none. */
  decodeError(value: Tagged): unknown {
    const decl = this.ir.types[value._tag]
    const fields = decl?.kind === "error" ? this.decodeFields(decl.fields, value) : {}
    const ErrorClass = Object.hasOwn(this.errors, value._tag) ? this.errors[value._tag] : undefined
    return ErrorClass === undefined ? { _tag: value._tag, ...fields } : new ErrorClass(fields)
  }

  /** Arguments keyed by parameter name, as host values. */
  decodeArgs(params: ReadonlyArray<Field>, args: { readonly [param: string]: Json }): Record<string, unknown> {
    return this.decodeFields(params, args)
  }
}

/** The JSON form of host value `value` of builtin type `name`. Throws a `CodecError`. */
const encodeBuiltin = (name: Builtin, value: unknown, what: string): Json => {
  switch (name) {
    case "string":
      if (typeof value !== "string") throw new CodecError(`${what} is not a string`)
      return value
    case "int":
      if (typeof value !== "number" || !Number.isInteger(value)) throw new CodecError(`${what} is not an int`)
      if (Math.abs(value) > MAX_INT) throw new CodecError(`${what} is outside the int range ±(2^53 - 1)`)
      return value === 0 ? 0 : value
    case "float":
      if (typeof value !== "number" || !Number.isFinite(value)) throw new CodecError(`${what} is not a finite float`)
      return value
    case "bool":
      if (typeof value !== "boolean") throw new CodecError(`${what} is not a bool`)
      return value
    case "duration": {
      if (!Duration.isDuration(value)) throw new CodecError(`${what} is not a Duration`)
      if (!Duration.isFinite(value)) throw new CodecError(`${what} is not a finite duration`)
      const millis = Math.trunc(Duration.toMillis(value))
      if (millis < 0) throw new CodecError(`${what} is a negative duration`)
      if (millis > MAX_INT) throw new CodecError(`${what} is longer than 2^53 - 1 milliseconds`)
      return millis === 0 ? 0 : millis
    }
    case "json":
      return toJson(value, what, [])
  }
}
