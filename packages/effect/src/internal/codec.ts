/**
 * Values at the API boundary. The core holds every value in its JSON form (execution.md's
 * "Values"); host code sees records as plain objects, errors as instances of their tagged error
 * classes, enums as string literals, `duration` as an Effect `Duration`, optional as `null` and
 * `int` as a `number`, and a `ref` as a handle to the run it refers to (its run id in the core).
 * A pending call (`call c`) never leaves its run, so host code never sees one; it keeps its JSON
 * form, `{from, invoke}`, both ways.
 * The conversion is driven by the IR's type references.
 */
import { callName, type Field, type Ir, isRunId, type Json, parseTypeRef, refMachine, type Tagged } from "@dire-kiwi/roadbook-core"
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
          // defineProperty, so that a member named `__proto__` stays a member.
          Object.defineProperty(members, key, {
            value: toJson(member, `${what}.${key}`, ancestors),
            enumerable: true,
            writable: true,
            configurable: true
          })
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
  encode(type: string, value: unknown, what: string): Json {
    const { base, suffixes } = parseTypeRef(type)
    return this.encodeRef(base, suffixes, suffixes.length, value, what)
  }

  private encodeRef(
    base: string,
    suffixes: ReadonlyArray<"[]" | "?">,
    n: number,
    value: unknown,
    what: string
  ): Json {
    if (n > 0) {
      if (suffixes[n - 1] === "?") {
        return value === null || value === undefined ? null : this.encodeRef(base, suffixes, n - 1, value, what)
      }
      if (!Array.isArray(value)) throw new CodecError(`${what} is not a list`)
      return value.map((item, i) => this.encodeRef(base, suffixes, n - 1, item, `${what}[${i}]`))
    }
    switch (base) {
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
    const machine = refMachine(base)
    if (machine !== undefined) {
      const port = portOf(value)
      if (port === undefined) throw new CodecError(`${what} is not a handle to a running ${machine}`)
      if (port.machine !== machine) throw new CodecError(`${what} is a handle to a ${port.machine}, not a ${machine}`)
      const hash = this.ir.machines[machine]?.hash
      if (hash !== undefined && port.hash !== hash) {
        throw new CodecError(`${what} is a handle to another version of ${machine} (${port.hash}, not ${hash})`)
      }
      this.refs?.set(port.id, value as object)
      return port.id
    }
    if (callName(base) !== undefined) {
      const call = value as { readonly from?: unknown; readonly invoke?: unknown }
      if (!Predicate.isObject(value) || !isRunId(call.from) || !Number.isSafeInteger(call.invoke) || (call.invoke as number) < 1) {
        throw new CodecError(`${what} is not a pending call: {from: a run id, invoke: a positive int}`)
      }
      return { from: call.from as string, invoke: call.invoke as number }
    }
    const decl = Object.hasOwn(this.ir.types, base) ? this.ir.types[base] : undefined
    if (decl === undefined) throw new CodecError(`${what} has unknown type ${base}`)
    switch (decl.kind) {
      case "alias":
        return this.encode(decl.type, value, what)
      case "enum":
        if (typeof value !== "string" || !decl.variants.includes(value)) {
          throw new CodecError(`${what} is not a variant of ${base}`)
        }
        return value
      case "record":
        return this.encodeFields(decl.fields, value, what)
      case "error": {
        if (!Predicate.isObject(value) || (value as { _tag?: unknown })._tag !== base) {
          throw new CodecError(`${what} is not a ${base}`)
        }
        return { _tag: base, ...this.encodeFields(decl.fields, value, what) }
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
  decode(type: string, value: Json): unknown {
    const { base, suffixes } = parseTypeRef(type)
    return this.decodeRef(base, suffixes, suffixes.length, value)
  }

  private decodeRef(base: string, suffixes: ReadonlyArray<"[]" | "?">, n: number, value: Json): unknown {
    if (n > 0) {
      if (suffixes[n - 1] === "?") return value === null ? null : this.decodeRef(base, suffixes, n - 1, value)
      return (value as ReadonlyArray<Json>).map((item) => this.decodeRef(base, suffixes, n - 1, item))
    }
    switch (base) {
      case "duration":
        return Duration.millis(value as number)
      case "string":
      case "int":
      case "float":
      case "bool":
      case "json":
        return value
    }
    // A pending call keeps its JSON form.
    if (callName(base) !== undefined) return value
    if (refMachine(base) !== undefined) {
      const handle = this.refs?.get(value as string)
      if (handle === undefined) throw new CodecError(`no handle to the run ${JSON.stringify(value)}`)
      return handle
    }
    const decl = Object.hasOwn(this.ir.types, base) ? this.ir.types[base] : undefined
    switch (decl?.kind) {
      case "alias":
        return this.decode(decl.type, value)
      case "record":
        return this.decodeFields(decl.fields, value)
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
