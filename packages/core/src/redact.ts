/**
 * Redaction (`spec/trace.md#redaction`): the values of trace records with every value of a
 * `sensitive` field, parameter or variable replaced by {@link REDACTED}. Only trace records are
 * redacted: snapshots, commands and what host code sees keep the real values. The Rust core
 * (`roadbook-core`'s `redact.rs`) does the same, so both runtimes write the same bytes.
 *
 * A value is redacted by its declared type, which the IR gives: a record's or error's sensitive
 * fields are replaced, and its other fields, list items and present optional values are
 * redacted by their own types, through aliases. A member a type does not declare is left as it
 * is. Values are never changed in place: what is redacted is a copy.
 */
import { type Field, type Ir, type Machine, parseTypeRef } from "./ir.ts"
import type { Json } from "./json.ts"
import type { CoreRecord, Input, InvokeEntry, Saved } from "./types.ts"

/** What a redacted value is written as. */
export const REDACTED = "<redacted>"

type Obj = { readonly [key: string]: Json }

const isObject = (v: Json): v is Obj => typeof v === "object" && v !== null && !Array.isArray(v)

interface Marks {
  /** Whether anything in the IR is marked: if not, nothing is ever redacted. */
  readonly any: boolean
  /** For each declared type, whether one of its values can hold a sensitive value. */
  readonly carries: ReadonlyMap<string, boolean>
}

const marksOf = new WeakMap<Ir, Marks>()

const own = <T>(map: Readonly<Record<string, T>>, key: string): T | undefined =>
  Object.hasOwn(map, key) ? map[key] : undefined

const marks = (ir: Ir): Marks => {
  const cached = marksOf.get(ir)
  if (cached !== undefined) return cached
  const marked = (fs: ReadonlyArray<Field>) => fs.some((f) => f.sensitive)
  const any = Object.values(ir.types).some((t) => (t.kind === "record" || t.kind === "error") && marked(t.fields)) ||
    Object.values(ir.functions).some((f) => marked(f.params)) ||
    Object.values(ir.actions).some((a) => marked(a.params)) ||
    Object.values(ir.tasks).some((t) => marked(t.params)) ||
    Object.values(ir.machines).some((m) =>
      marked(m.params) || m.vars.some((v) => v.sensitive) ||
      [...Object.values(m.events), ...Object.values(m.emits)].some((e) => marked(e.fields))
    )
  const carries = new Map<string, boolean>()
  if (any) {
    const named = (name: string, seen: Set<string>): boolean => {
      if (seen.has(name)) return false
      seen.add(name)
      const decl = own(ir.types, name)
      if (decl === undefined) return false
      if (decl.kind === "record" || decl.kind === "error") {
        return decl.fields.some((f) => f.sensitive || named(parseTypeRef(f.type).base, seen))
      }
      return decl.kind === "alias" && named(parseTypeRef(decl.type).base, seen)
    }
    for (const name of Object.keys(ir.types)) carries.set(name, named(name, new Set()))
  }
  const result = { any, carries }
  marksOf.set(ir, result)
  return result
}

/** Redacts values of one IR. */
class Redactor {
  readonly active: boolean
  private readonly ir: Ir
  private readonly carries: ReadonlyMap<string, boolean>

  constructor(ir: Ir) {
    const m = marks(ir)
    this.ir = ir
    this.active = m.any
    this.carries = m.carries
  }

  /** `v`, a value of type `type`, redacted. */
  value(type: string, v: Json): Json {
    const t = parseTypeRef(type)
    return this.typed(t.base, t.suffixes, t.suffixes.length, v)
  }

  /** `v` redacted as a value of `base` with the first `n` of `suffixes` applied. */
  private typed(base: string, suffixes: ReadonlyArray<"[]" | "?">, n: number, v: Json): Json {
    if (n > 0) {
      if (suffixes[n - 1] === "?") return v === null ? v : this.typed(base, suffixes, n - 1, v)
      return Array.isArray(v) ? v.map((item: Json) => this.typed(base, suffixes, n - 1, item)) : v
    }
    if (!this.carries.get(base)) return v
    const decl = own(this.ir.types, base)
    if (decl?.kind === "record" || decl?.kind === "error") return this.fields(decl.fields, v)
    if (decl?.kind === "alias") return this.value(decl.type, v)
    return v
  }

  /** `v`, an object with these fields (a record, an error or an event), redacted. */
  fields(fields: ReadonlyArray<Field>, v: Json): Json {
    return isObject(v) ? this.object(fields, v) : v
  }

  /** `obj`, keyed by these fields or parameters, redacted. */
  object(fields: ReadonlyArray<Field>, obj: Obj): Obj {
    // A spread copies every own member as an own member, `__proto__` too; field names are
    // lower-case identifiers, so the assignments below never reach the prototype.
    const out: Record<string, Json> = { ...obj }
    for (const f of fields) {
      if (!Object.hasOwn(obj, f.name)) continue
      out[f.name] = f.sensitive ? REDACTED : this.value(f.type, obj[f.name]!)
    }
    return out
  }

  /** `v` redacted by its tag as one of `m`'s `emits` if `emits`, else of its `events`. */
  private event(m: Machine, emits: boolean, v: Json): Json {
    if (!isObject(v) || typeof v._tag !== "string") return v
    const decl = own(emits ? m.emits : m.events, v._tag)
    return decl === undefined ? v : this.fields(decl.fields, v)
  }

  /** `v`, a tagged error, redacted by its tag. */
  private error(v: Json): Json {
    return isObject(v) && typeof v._tag === "string" ? this.typed(v._tag, [], 0, v) : v
  }

  private returns(src: string): string | null {
    const task = own(this.ir.tasks, src)
    if (task !== undefined) return task.returns
    return own(this.ir.machines, src)?.returns ?? null
  }

  /** The input `input` of a run of `m`, whose running invokes are `invokes`, redacted. */
  input<I extends Input>(m: Machine, invokes: ReadonlyArray<InvokeEntry>, input: I): I {
    return this.anyInput(m, invokes, input) as I
  }

  private anyInput(m: Machine, invokes: ReadonlyArray<InvokeEntry>, input: Input): Input {
    switch (input.kind) {
      case "event":
      case "message":
        return { ...input, value: this.event(m, false, input.value) as typeof input.value }
      case "emitted": {
        // The receiving run does not always know the sender's machine (a peer, or a child
        // whose invoke has ended), so the event is redacted as each machine that emits an
        // event of its name would redact it.
        let value: Json = input.value
        for (const other of Object.values(this.ir.machines)) value = this.event(other, true, value)
        return { ...input, value: value as typeof input.value }
      }
      case "invoke.done": {
        const inv = invokes.find((i) => i.id === input.invoke)
        let value = input.value
        if (inv !== undefined) {
          const t = this.returns(inv.src)
          if (t !== null) value = this.value(t, value)
        } else {
          // A stale outcome: the invoke has ended, and with it what it ran. Redacted as every
          // task and machine of the IR would redact its result.
          for (const task of Object.values(this.ir.tasks)) if (task.returns !== null) value = this.value(task.returns, value)
          for (const machine of Object.values(this.ir.machines)) {
            if (machine.returns !== null) value = this.value(machine.returns, value)
          }
        }
        return { ...input, value }
      }
      case "invoke.fail":
        return { ...input, error: this.error(input.error) as typeof input.error }
      default:
        return input
    }
  }

  /**
   * A record a run of `m` emits redacted, other than an `event`, `dropped` or `stale` record,
   * whose input {@link Redactor.input} redacts when it is taken, and `sent` and the core's
   * `undelivered`, which {@link Redactor.sent} redacts where the receiver is known.
   */
  record(m: Machine, record: CoreRecord): CoreRecord {
    switch (record.kind) {
      case "started":
        return { ...record, input: isObject(record.input) ? this.object(m.params, record.input) : record.input }
      case "restored":
        return { ...record, snapshot: this.saved(m, record.snapshot) }
      case "set": {
        const v = m.vars.find((x) => x.name === record.var)
        if (v === undefined) return record
        return { ...record, value: v.sensitive ? REDACTED : this.value(v.type, record.value) }
      }
      case "action": {
        const a = own(this.ir.actions, record.action)
        return a === undefined ? record : { ...record, args: this.object(a.params, record.args) }
      }
      case "invoke.started": {
        const params = own(this.ir.tasks, record.src)?.params ?? own(this.ir.machines, record.src)?.params
        return params === undefined ? record : { ...record, args: this.object(params, record.args) }
      }
      case "emitted":
        return { ...record, value: this.event(m, true, record.value) as typeof record.value }
      case "done":
        return m.returns === null ? record : { ...record, output: this.value(m.returns, record.output) }
      case "failed":
        return { ...record, error: this.error(record.error) as typeof record.error }
      default:
        return record
    }
  }

  /** `value`, an event sent to a run of machine `to`, redacted. */
  sent(to: string, value: Json): Json {
    const m = own(this.ir.machines, to)
    return m === undefined ? value : this.event(m, false, value)
  }

  /** The copy of a saved snapshot of a run of `m` that a `restored` record holds, redacted. */
  private saved(m: Machine, saved: Saved): Saved {
    const vars: Record<string, Json> = { ...saved.vars }
    for (const v of m.vars) {
      if (!Object.hasOwn(saved.vars, v.name)) continue
      vars[v.name] = v.sensitive ? REDACTED : this.value(v.type, saved.vars[v.name]!)
    }
    let result = saved.result
    if (result !== null && "output" in result && m.returns !== null) {
      result = { output: this.value(m.returns, result.output) }
    } else if (result !== null && "error" in result) {
      result = { error: this.error(result.error) as typeof result.error }
    }
    return { ...saved, input: this.object(m.params, saved.input), vars, result }
  }
}

const redactors = new WeakMap<Ir, Redactor>()

/** The redactor of `ir`, made once per IR document. */
export const redactor = (ir: Ir): Redactor => {
  let r = redactors.get(ir)
  if (r === undefined) {
    r = new Redactor(ir)
    redactors.set(ir, r)
  }
  return r
}
