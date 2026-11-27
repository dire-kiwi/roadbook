/**
 * Redaction (`spec/trace.md#redaction`): the values of trace records with every value of a
 * `sensitive` field, parameter or variable replaced by {@link REDACTED}. Only trace records are
 * redacted: snapshots, commands and what host code sees keep the real values. The Rust core
 * (`roadbook-core`'s `redact.rs`) does the same, so both runtimes write the same bytes.
 *
 * A value is redacted by its declared type, which the IR gives: a record's or error's sensitive
 * fields are replaced, and its other fields, list items, map values and present optional values
 * are redacted by their own types, through aliases, with the arguments of an instance of a
 * generic type substituted. A member a type does not declare is left as it is. Values are never
 * changed in place: what is redacted is a copy.
 */
import type { CallDecl, Field, Ir, Machine, State } from "./ir.ts"
import { expandType, fieldsOf, parseTypeRef, type TypeRef } from "./typeref.ts"
import type { Json } from "./json.ts"
import type { CoreRecord, Input, InvokeEntry, Saved, SpawnedEntry } from "./types.ts"

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
      [...Object.values(m.events), ...Object.values(m.emits)].some((e) => marked(e.fields)) ||
      Object.values(m.calls).some((c) => marked(c.params))
    )
  const carries = new Map<string, boolean>()
  if (any) {
    // Whether a value of declared type `name` can hold a sensitive value whatever its type
    // arguments: a type parameter (not a key of `types`) carries nothing of its own.
    const named = (name: string, seen: Set<string>): boolean => {
      if (seen.has(name)) return false
      seen.add(name)
      const decl = own(ir.types, name)
      if (decl === undefined) return false
      if (decl.kind === "record" || decl.kind === "error") {
        return decl.fields.some((f) => f.sensitive || ref(parseTypeRef(f.type), seen))
      }
      return decl.kind === "alias" && ref(parseTypeRef(decl.type), seen)
    }
    const ref = (t: TypeRef, seen: Set<string>): boolean => {
      switch (t.kind) {
        case "named":
          return named(t.name, seen) || t.args.some((a) => ref(a, seen))
        case "list":
        case "optional":
          return ref(t.of, seen)
        case "map":
          return ref(t.value, seen)
        default:
          return false
      }
    }
    for (const name of Object.keys(ir.types)) carries.set(name, named(name, new Set()))
  }
  const result = { any, carries }
  marksOf.set(ir, result)
  return result
}

/** Redacts values of one IR. Mirrors the Rust core's `redact.rs` rule for rule. */
class Redactor {
  readonly active: boolean
  private readonly ir: Ir
  private readonly carryNamed: ReadonlyMap<string, boolean>

  constructor(ir: Ir) {
    const m = marks(ir)
    this.ir = ir
    this.active = m.any
    this.carryNamed = m.carries
  }

  /**
   * Whether a value of type `t` can hold a sensitive value. An instance of a generic type can if
   * its type can whatever the arguments, or if an argument can: the compiler makes every type
   * parameter used, so each argument's values are in the instance's.
   */
  private carries(t: TypeRef | string): boolean {
    const type = typeof t === "string" ? parseTypeRef(t) : t
    switch (type.kind) {
      case "named":
        return this.carryNamed.get(type.name) === true || type.args.some((a) => this.carries(a))
      case "list":
      case "optional":
        return this.carries(type.of)
      case "map":
        return this.carries(type.value)
      default:
        return false
    }
  }

  /**
   * `v`, a value of type `type`, redacted. A value of a type that can hold a sensitive value
   * is redacted whole when it does not have the type's shape (a list that is not an array, a
   * record or map that is not an object), so a value host code got wrong shows nothing.
   */
  value(type: TypeRef | string, v: Json): Json {
    const t = typeof type === "string" ? parseTypeRef(type) : type
    if (!this.carries(t)) return v
    switch (t.kind) {
      case "optional":
        return v === null ? v : this.value(t.of, v)
      case "list":
        return Array.isArray(v) ? v.map((item: Json) => this.value(t.of, item)) : REDACTED
      case "map": {
        if (!isObject(v)) return REDACTED
        const out: Record<string, Json> = {}
        for (const key of Object.keys(v)) {
          // defineProperty, so that a key `__proto__` stays a key.
          Object.defineProperty(out, key, {
            value: this.value(t.value, v[key]!),
            enumerable: true,
            writable: true,
            configurable: true
          })
        }
        return out
      }
      case "named": {
        const record = fieldsOf(this.ir, t)
        if (record !== undefined) return this.fields(record.fields, v)
        const expanded = expandType(this.ir, t)
        return expanded === t ? v : this.value(expanded, v)
      }
      default:
        return v
    }
  }

  /** `v`, a record, error or event with these fields that can hold a sensitive value. */
  private fields(fields: ReadonlyArray<Field>, v: Json): Json {
    return isObject(v) ? this.object(fields, v) : REDACTED
  }

  /**
   * `obj`, keyed by these fields or parameters, redacted: a sensitive one's member whole, any
   * other declared one's by its type, and a member none declares (other than `_tag`) whole.
   */
  object(fields: ReadonlyArray<Field>, obj: Obj): Obj {
    const out: Record<string, Json> = {}
    for (const key of Object.keys(obj)) {
      const f = fields.find((x) => x.name === key)
      const member = obj[key]!
      const value = f === undefined ? (key === "_tag" ? member : REDACTED) : f.sensitive ? REDACTED : this.value(f.type, member)
      // defineProperty, so that a member named `__proto__` stays a member.
      Object.defineProperty(out, key, { value, enumerable: true, writable: true, configurable: true })
    }
    return out
  }

  /**
   * `v`, a tagged value (an event or an error) that `decls` may declare, redacted by each
   * declaration that can hold a sensitive value. One that none declares has every member but
   * `_tag` redacted, and one that is not a tagged object is redacted whole.
   */
  private tagged(decls: ReadonlyArray<ReadonlyArray<Field>>, v: Json): Json {
    if (!isObject(v) || typeof v._tag !== "string") return REDACTED
    if (decls.length === 0) return this.object([], v)
    let out: Obj = v
    for (const fields of decls) {
      if (fields.some((f) => f.sensitive || this.carries(f.type))) out = this.object(fields, out)
    }
    return out
  }

  private tagOf(v: Json): string {
    return isObject(v) && typeof v._tag === "string" ? v._tag : ""
  }

  /** `v` redacted by its tag as one of `m`'s `emits` if `emits`, else of its `events`. */
  private event(m: Machine, emits: boolean, v: Json): Json {
    const decl = own(emits ? m.emits : m.events, this.tagOf(v))
    return this.tagged(decl === undefined ? [] : [decl.fields], v)
  }

  /**
   * `v`, an emitted event, redacted as each machine of the IR that emits an event of its tag
   * with exactly its members would: the receiving run does not always know which machine sent
   * it (a peer, or a child whose invoke has ended), and the sender's own declaration is always
   * one of those, since a core builds the event with every field it declares.
   */
  private emitted(v: Json): Json {
    const tag = this.tagOf(v)
    const members = isObject(v) ? Object.keys(v).filter((k) => k !== "_tag") : []
    const fits = (fields: ReadonlyArray<Field>) =>
      fields.length === members.length && fields.every((f) => members.includes(f.name))
    const decls: Array<ReadonlyArray<Field>> = []
    for (const m of Object.values(this.ir.machines)) {
      const decl = own(m.emits, tag)
      if (decl !== undefined && fits(decl.fields)) decls.push(decl.fields)
    }
    return this.tagged(decls, v)
  }

  /** `v`, a tagged error, redacted by its tag. */
  private error(v: Json): Json {
    const decl = own(this.ir.types, this.tagOf(v))
    return this.tagged(decl?.kind === "error" || decl?.kind === "record" ? [decl.fields] : [], v)
  }

  /** The call that the invoke of state `state` of `m` makes, if it is a call. */
  private callOf(m: Machine, state: string): CallDecl | undefined {
    const find = (s: State): State | undefined => s.id === state ? s : s.states.map(find).find((x) => x !== undefined)
    const inv = find(m.root)?.invoke
    if (inv === null || inv === undefined || inv.to === null) return undefined
    const callee = own(this.ir.machines, inv.to.machine)
    return callee === undefined ? undefined : own(callee.calls, inv.src)
  }

  /** What the invoke of state `state` of `m`, of `src`, returns: a call's result type, or the task's or machine's. */
  private invokedReturns(m: Machine, state: string, src: string): string | null {
    if (src.includes(".")) return this.callOf(m, state)?.returns ?? null
    return this.returns(src)
  }

  private returns(src: string): string | null {
    const task = own(this.ir.tasks, src)
    if (task !== undefined) return task.returns
    return own(this.ir.machines, src)?.returns ?? null
  }

  /** `output`, a result of something that returns `type` (`null`: nothing), redacted. */
  private result(type: string | null, output: Json): Json {
    if (type !== null) return this.value(type, output)
    return output === null ? output : REDACTED
  }

  /**
   * The input `input` of a run of `m`, whose running invokes are `invokes` and running spawned
   * children `spawned`, redacted.
   */
  input<I extends Input>(
    m: Machine,
    invokes: ReadonlyArray<InvokeEntry>,
    input: I,
    spawned: ReadonlyArray<SpawnedEntry> = []
  ): I {
    return this.active ? this.anyInput(m, invokes, spawned, input) as I : input
  }

  /** The machine the children of collection `children` of `m` run. */
  private collection(m: Machine, children: string): Machine | undefined {
    const decl = own(m.children, children)
    return decl === undefined ? undefined : own(this.ir.machines, decl.machine)
  }

  private anyInput(
    m: Machine,
    invokes: ReadonlyArray<InvokeEntry>,
    spawned: ReadonlyArray<SpawnedEntry>,
    input: Input
  ): Input {
    switch (input.kind) {
      case "event":
      case "message":
        return { ...input, value: this.event(m, false, input.value) as typeof input.value }
      case "emitted":
        // The receiving run does not always know the sender's machine (a peer, or a child
        // whose invoke has ended), so the event is redacted as each machine that emits an
        // event of its name would redact it.
        return { ...input, value: this.emitted(input.value) as typeof input.value }
      case "call":
        // A call's arguments, by the parameters of the machine's call.
        return { ...input, args: this.object(own(m.calls, input.call)?.params ?? [], input.args) }
      case "invoke.done": {
        const inv = invokes.find((i) => i.id === input.invoke)
        if (inv !== undefined) return { ...input, value: this.result(this.invokedReturns(m, inv.state, inv.src), input.value) }
        // A stale outcome: the invoke has ended, and with it what it ran, so the type of the
        // value is not known. Redacted whole: the run ignores it.
        return { ...input, value: REDACTED }
      }
      case "invoke.fail":
      case "spawn.fail":
        return { ...input, error: this.error(input.error) as typeof input.error }
      case "spawn.done": {
        const ch = spawned.find((c) => c.id === input.spawn)
        const child = ch === undefined ? undefined : this.collection(m, ch.children)
        if (child !== undefined) return { ...input, value: this.result(child.returns, input.value) }
        // A stale outcome: the child is no longer in the snapshot, so its type is not known.
        return { ...input, value: REDACTED }
      }
      default:
        return input
    }
  }

  /** `value`, the value of variable `name` of `m`: whole if it is sensitive, or if `m` has none. */
  private variable(m: Machine, name: string, value: Json): Json {
    const v = m.vars.find((x) => x.name === name)
    return v === undefined || v.sensitive ? REDACTED : this.value(v.type, value)
  }

  /**
   * A record a run of `m` emits redacted, other than an `event`, `dropped` or `stale` record,
   * whose input {@link Redactor.input} redacts when it is taken, and `sent` and the core's
   * `undelivered`, which {@link Redactor.sent} redacts where the receiver is known.
   */
  record(m: Machine, record: CoreRecord): CoreRecord {
    if (!this.active) return record
    switch (record.kind) {
      case "started":
        return { ...record, input: isObject(record.input) ? this.object(m.params, record.input) : REDACTED }
      case "restored":
        return { ...record, snapshot: this.saved(m, record.snapshot) }
      case "set":
        return { ...record, value: this.variable(m, record.var, record.value) }
      case "action":
        return { ...record, args: this.object(own(this.ir.actions, record.action)?.params ?? [], record.args) }
      case "invoke.started": {
        const task = own(this.ir.tasks, record.src)
        // A call's arguments go to the parameters of the callee's call.
        const params = task !== undefined
          ? task.params
          : record.src.includes(".")
          ? this.callOf(m, record.state)?.params ?? []
          : own(this.ir.machines, record.src)?.params ?? []
        return { ...record, args: this.object(params, record.args) }
      }
      case "spawn.started":
        return { ...record, args: this.object(own(this.ir.machines, record.src)?.params ?? [], record.args) }
      case "emitted":
        return { ...record, value: this.event(m, true, record.value) as typeof record.value }
      case "done":
        return { ...record, output: this.result(m.returns, record.output) }
      case "failed":
        return { ...record, error: this.error(record.error) as typeof record.error }
      case "replied":
        // The answer to one of `m`'s calls: by the call's result type, or as an error.
        return {
          ...record,
          value: record.outcome === "done"
            ? this.result(own(m.calls, record.call)?.returns ?? null, record.value)
            : this.error(record.value)
        }
      default:
        return record
    }
  }

  /** `value`, an event sent to a run of machine `to`, redacted. */
  sent(to: string, value: Json): Json {
    if (!this.active) return value
    const m = own(this.ir.machines, to)
    return m === undefined ? this.tagged([], value) : this.event(m, false, value)
  }

  /** The copy of a saved snapshot of a run of `m` that a `restored` record holds, redacted. */
  private saved(m: Machine, saved: Saved): Saved {
    const vars: Record<string, Json> = {}
    for (const name of Object.keys(saved.vars)) {
      Object.defineProperty(vars, name, {
        value: this.variable(m, name, saved.vars[name]!),
        enumerable: true,
        writable: true,
        configurable: true
      })
    }
    let result = saved.result
    if (result !== null && "output" in result) result = { output: this.result(m.returns, result.output) }
    else if (result !== null && "error" in result) result = { error: this.error(result.error) as typeof result.error }
    const redacted = { ...saved, input: this.object(m.params, saved.input), vars, result }
    // A version 1 or 2 snapshot has no `spawned`, and the record holds it as it was given.
    if (saved.spawned === undefined) return redacted
    const spawned = saved.spawned.map((ch) => ({
      ...ch,
      args: this.object(this.collection(m, ch.children)?.params ?? [], ch.args)
    }))
    return { ...redacted, spawned }
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
