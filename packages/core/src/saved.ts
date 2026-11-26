/**
 * Saved snapshots: making one from a run's snapshot, and the checks `restore` makes before it
 * starts a run from one (execution.md, "Restoring a snapshot"), in the order the spec gives
 * them, each refusal with its reason.
 */
import { type Field, type Ir, type Machine, parseTypeRef, refMachine, type State } from "./ir.ts"
import { copyJson, type Json } from "./json.ts"
import type { Saved, Snapshot } from "./types.ts"

/** The `snapshotVersion` this runtime saves and restores. */
export const SNAPSHOT_VERSION = 1

/** Why a saved snapshot was refused. */
export type RefusalReason = "invalid" | "machine" | "ir" | "migration" | "ended" | "ref"

export interface Refusal {
  readonly reason: RefusalReason
  readonly message: string
}

/**
 * The host's migration: the old saved snapshot to a new one, both plain JSON, or the reason it
 * failed. Must not throw; the binding layer catches what host code throws and returns `error`.
 */
export type Migrate = (saved: Json) => { readonly value: Json } | { readonly error: string }

export type Checked =
  | { readonly ok: true; readonly saved: Saved; readonly migrated: string | null }
  | ({ readonly ok: false } & Refusal)

/** The saved form of `snapshot`: the version added, and each timer's due time from `due`. */
export const toSaved = (snapshot: Snapshot, due: (timer: number) => number): Saved => ({
  snapshotVersion: SNAPSHOT_VERSION,
  machine: snapshot.machine,
  run: snapshot.run,
  ir: snapshot.ir,
  status: snapshot.status,
  active: snapshot.active,
  input: snapshot.input,
  vars: snapshot.vars,
  invokes: snapshot.invokes,
  timers: snapshot.timers.map((t) => ({ ...t, due: due(t.id) })),
  nextInvoke: snapshot.nextInvoke,
  nextTimer: snapshot.nextTimer,
  nextMessage: snapshot.nextMessage,
  result: snapshot.result
})

/**
 * Check `json` as a saved snapshot of machine `machine` of `ir`, as `restore` does: its shape
 * and version (`invalid`), the machine (`machine`), the hash (`ir`, or `migration` when
 * `migrate` fails; its result is checked again from the top, never migrated twice), the status
 * (`ended`), `ref` parameters (`ref`), and whether it fits the machine (`invalid`).
 */
export const checkSaved = (ir: Ir, machine: string, json: unknown, migrate?: Migrate): Checked => {
  const refuse = (reason: RefusalReason, message: string): Checked => ({ ok: false, reason, message })
  const shape = shapeIssue(json)
  if (shape !== undefined) return refuse("invalid", `not a saved snapshot: ${shape}`)
  const saved = json as Saved
  if (saved.machine !== machine || !Object.hasOwn(ir.machines, saved.machine)) {
    return refuse("machine", `the snapshot is of machine ${saved.machine}, not ${machine}`)
  }
  const m = ir.machines[machine]!
  if (saved.ir !== m.hash) {
    if (migrate === undefined) {
      return refuse("ir", `the snapshot was saved with ${saved.ir}, and ${machine} is now ${m.hash}; give a migration`)
    }
    const result = migrate(saved as unknown as Json)
    if ("error" in result) return refuse("migration", `the migration failed: ${result.error}`)
    // The result is checked from the top, and never migrated again.
    const again = checkSaved(ir, machine, result.value)
    if (!again.ok) {
      return again.reason === "ir"
        ? refuse("ir", `the migration gave a snapshot with ${(result.value as { ir: string }).ir}, not ${m.hash}`)
        : again
    }
    return { ok: true, saved: again.saved, migrated: saved.ir }
  }
  if (saved.status !== "running") return refuse("ended", `the run has already ended (${saved.status})`)
  const ref = m.params.find((p) => holdsRef(ir, p.type))
  if (ref !== undefined) {
    return refuse("ref", `${machine} holds a ref in parameter ${ref.name}, which cannot be restored yet`)
  }
  const fit = fitIssue(ir, m, saved)
  if (fit !== undefined) return refuse("invalid", `the snapshot does not fit ${machine}: ${fit}`)
  // A copy: the run does not share anything with what the caller holds.
  return { ok: true, saved: copyJson(saved), migrated: null }
}

// ---- the shape: snapshot.schema.json --------------------------------------------------------

/** Whether `u` matches `snapshot.schema.json` with `snapshotVersion` 1. Its values are not checked. */
export const isSaved = (u: unknown): u is Saved => shapeIssue(u) === undefined

const UPPER = /^[A-Z][A-Za-z0-9]*$/
const LOWER = /^[a-z][A-Za-z0-9]*$/
const NAME = /^[A-Za-z][A-Za-z0-9]*$/
const STATE_ID = /^[A-Z][A-Za-z0-9]*(\.[A-Z][A-Za-z0-9]*)*$/
/**
 * The snapshot schema's pattern for a run id: the id of a root run, then the invoke ids of child
 * runs. A root run named anything else would save a snapshot that `restore` refuses as `invalid`.
 */
export const RUN_ID_PATTERN = "^[A-Za-z0-9_-]+(\\.[1-9][0-9]*)*$"
const RUN_ID = new RegExp(RUN_ID_PATTERN)
/** Whether `id` is a run id (`RUN_ID_PATTERN`). */
export const isRunId = (id: unknown): boolean => typeof id === "string" && RUN_ID.test(id)
const HASH = /^sha256:[0-9a-f]{64}$/
const STATUSES = ["running", "done", "failed", "cancelled", "defect"]
const MEMBERS = [
  "snapshotVersion",
  "machine",
  "run",
  "ir",
  "status",
  "active",
  "input",
  "vars",
  "invokes",
  "timers",
  "nextInvoke",
  "nextTimer",
  "nextMessage",
  "result"
]

const isObject = (u: unknown): u is { readonly [key: string]: unknown } =>
  typeof u === "object" && u !== null && !Array.isArray(u)

const isNatural = (u: unknown): u is number => Number.isSafeInteger(u) && (u as number) >= 0
const isId = (u: unknown): u is number => Number.isSafeInteger(u) && (u as number) >= 1
const matches = (re: RegExp, u: unknown): boolean => typeof u === "string" && re.test(u)

/** Members exactly `names`: none missing, none other. */
const membersIssue = (value: { readonly [key: string]: unknown }, names: ReadonlyArray<string>, what: string) => {
  const missing = names.find((n) => !Object.hasOwn(value, n))
  if (missing !== undefined) return `${what} has no ${missing}`
  const extra = Object.keys(value).find((k) => !names.includes(k))
  if (extra !== undefined) return `${what} has a member ${extra}, which a saved snapshot does not have`
  return undefined
}

/** What makes `json` not match snapshot.schema.json, or `undefined`. */
const shapeIssue = (json: unknown): string | undefined => {
  if (!isObject(json)) return "not a JSON object"
  const members = membersIssue(json, MEMBERS, "it")
  if (members !== undefined) return members
  if (json["snapshotVersion"] !== SNAPSHOT_VERSION) {
    return `snapshotVersion is ${JSON.stringify(json["snapshotVersion"])}, and this runtime reads ${SNAPSHOT_VERSION}`
  }
  if (!matches(UPPER, json["machine"])) return "machine is not a machine name"
  if (!isRunId(json["run"])) return "run is not a run id"
  if (!matches(HASH, json["ir"])) return "ir is not a sha256 hash"
  if (!STATUSES.includes(json["status"] as string)) return "status is not a status"
  const active = json["active"]
  if (!Array.isArray(active) || !active.every((id) => matches(STATE_ID, id))) return "active is not a list of state ids"
  if (new Set(active).size !== active.length) return "active names a state twice"
  for (const name of ["input", "vars"]) {
    const args = json[name]
    if (!isObject(args) || !Object.keys(args).every((k) => LOWER.test(k))) return `${name} is not an object keyed by name`
  }
  const invokes = json["invokes"]
  if (!Array.isArray(invokes)) return "invokes is not a list"
  for (const inv of invokes) {
    if (!isObject(inv)) return "an invoke is not an object"
    const issue = membersIssue(inv, ["id", "state", "src"], "an invoke")
    if (issue !== undefined) return issue
    if (!isId(inv["id"]) || !matches(STATE_ID, inv["state"]) || !matches(NAME, inv["src"])) return "an invoke is malformed"
  }
  const timers = json["timers"]
  if (!Array.isArray(timers)) return "timers is not a list"
  for (const tm of timers) {
    if (!isObject(tm)) return "a timer is not an object"
    const issue = membersIssue(tm, ["id", "state", "index", "delay", "due"], "a timer")
    if (issue !== undefined) return issue
    if (
      !isId(tm["id"]) || !matches(STATE_ID, tm["state"]) || !isNatural(tm["index"]) || !isNatural(tm["delay"]) ||
      !isNatural(tm["due"])
    ) return "a timer is malformed"
  }
  for (const name of ["nextInvoke", "nextTimer", "nextMessage"]) {
    if (!isId(json[name])) return `${name} is not a positive integer`
  }
  const result = json["result"]
  if (result !== null) {
    if (!isObject(result) || Object.keys(result).length !== 1) return "result is not null or an object of one member"
    if (Object.hasOwn(result, "error")) {
      const error = result["error"]
      if (!isObject(error) || !matches(UPPER, error["_tag"])) return "result.error is not a tagged value"
    } else if (Object.hasOwn(result, "defect")) {
      if (!isObject(result["defect"]) || !Object.hasOwn(result["defect"], "kind")) return "result.defect is not a defect"
    } else if (!Object.hasOwn(result, "output")) return "result has neither output, error nor defect"
  }
  return undefined
}

// ---- fitting the machine ------------------------------------------------------------------

/** What makes `saved` not fit machine `m`, or `undefined`. */
const fitIssue = (ir: Ir, m: Machine, saved: Saved): string | undefined => {
  const states = new Map<string, { readonly state: State; readonly parent: State | undefined; readonly order: number }>()
  const walk = (state: State, parent: State | undefined) => {
    states.set(state.id, { state, parent, order: states.size })
    for (const child of state.states) walk(child, state)
  }
  walk(m.root, undefined)

  // A running run has no result yet.
  if (saved.result !== null) return "result is not null, though the run is running"

  // The configuration.
  const active = new Set(saved.active)
  if (saved.active[0] !== m.root.id) return `active does not start with the root, ${m.root.id}`
  let last = -1
  for (const id of saved.active) {
    const entry = states.get(id)
    if (entry === undefined) return `active names ${id}, which is not a state of the machine`
    if (entry.order <= last) return "active is not in document order"
    last = entry.order
    if (entry.parent !== undefined && !active.has(entry.parent.id)) return `${id} is active but its parent is not`
    const { state } = entry
    const children = state.states.filter((c) => active.has(c.id)).length
    if (state.kind === "compound" && children !== 1) return `${id} is an active compound state with ${children} active children`
    if (state.kind === "parallel" && children !== state.states.length) {
      return `${id} is an active parallel state whose regions are not all active`
    }
  }

  // The work.
  const invokeStates = new Set<string>()
  const invokeIds = new Set<number>()
  for (const inv of saved.invokes) {
    const state = states.get(inv.state)?.state
    if (state === undefined || !active.has(inv.state)) return `invoke ${inv.id} is of ${inv.state}, which is not active`
    if (state.invoke === null || state.invoke.src !== inv.src) return `${inv.state} has no invoke of ${inv.src}`
    if (invokeStates.has(inv.state)) return `${inv.state} has two invokes`
    invokeStates.add(inv.state)
    if (invokeIds.has(inv.id)) return `invoke id ${inv.id} is repeated`
    invokeIds.add(inv.id)
    if (inv.id >= saved.nextInvoke) return `invoke id ${inv.id} is not below nextInvoke`
  }
  const timerKeys = new Set<string>()
  const timerIds = new Set<number>()
  for (const tm of saved.timers) {
    const state = states.get(tm.state)?.state
    if (state === undefined || !active.has(tm.state)) return `timer ${tm.id} is of ${tm.state}, which is not active`
    const transition = state.transitions[tm.index]
    const first = transition !== undefined && transition.trigger.kind === "after" &&
      state.transitions.findIndex((t) => t.trigger.kind === "after" && t.group === transition.group) === tm.index
    if (!first) return `transition ${tm.index} of ${tm.state} does not begin an after block`
    const key = `${tm.state} ${tm.index}`
    if (timerKeys.has(key)) return `${tm.state} has two timers for transition ${tm.index}`
    timerKeys.add(key)
    if (timerIds.has(tm.id)) return `timer id ${tm.id} is repeated`
    timerIds.add(tm.id)
    if (tm.id >= saved.nextTimer) return `timer id ${tm.id} is not below nextTimer`
  }

  // The values.
  const params = new Set(m.params.map((p) => p.name))
  const missing = m.params.find((p) => !Object.hasOwn(saved.input, p.name))
  if (missing !== undefined) return `input has no ${missing.name}`
  const extra = Object.keys(saved.input).find((k) => !params.has(k))
  if (extra !== undefined) return `input has ${extra}, which is not a parameter`
  for (const p of m.params) {
    const issue = valueIssue(ir, p.type, saved.input[p.name] ?? null, `input.${p.name}`)
    if (issue !== undefined) return issue
  }
  for (const name of Object.keys(saved.vars)) {
    const v = m.vars.find((x) => x.name === name)
    if (v === undefined) return `vars has ${name}, which is not a variable`
    const issue = valueIssue(ir, v.type, saved.vars[name] ?? null, `vars.${name}`)
    if (issue !== undefined) return issue
  }
  const optional = m.vars.find((v) => isOptionalType(ir, v.type) && !Object.hasOwn(saved.vars, v.name))
  if (optional !== undefined) return `vars has no ${optional.name}, which is optional and always has a value`
  return undefined
}

/** Whether a type reference is optional once aliases are expanded. */
const isOptionalType = (ir: Ir, type: string): boolean => {
  const { base, suffixes } = parseTypeRef(type)
  if (suffixes.length > 0) return suffixes[suffixes.length - 1] === "?"
  const decl = Object.hasOwn(ir.types, base) ? ir.types[base] : undefined
  return decl?.kind === "alias" ? isOptionalType(ir, decl.type) : false
}

/** Whether a parameter type is a `ref`, an optional one or a list of them, through aliases. */
const holdsRef = (ir: Ir, type: string): boolean => {
  const { base } = parseTypeRef(type)
  if (refMachine(base) !== undefined) return true
  const decl = Object.hasOwn(ir.types, base) ? ir.types[base] : undefined
  return decl?.kind === "alias" ? holdsRef(ir, decl.type) : false
}

// ---- values ---------------------------------------------------------------------------------

/**
 * What makes JSON value `value` not the JSON form of type `type` (execution.md, "Values"), or
 * `undefined`. A record, error or event has exactly its fields (an optional one as `null`).
 */
export const valueIssue = (ir: Ir, type: string, value: Json, what: string): string | undefined => {
  const { base, suffixes } = parseTypeRef(type)
  return refIssue(ir, base, suffixes, suffixes.length, value, what)
}

const refIssue = (
  ir: Ir,
  base: string,
  suffixes: ReadonlyArray<"[]" | "?">,
  n: number,
  value: Json,
  what: string
): string | undefined => {
  if (n > 0) {
    if (suffixes[n - 1] === "?") return value === null ? undefined : refIssue(ir, base, suffixes, n - 1, value, what)
    if (!Array.isArray(value)) return `${what} is not a list`
    for (let i = 0; i < value.length; i++) {
      const issue = refIssue(ir, base, suffixes, n - 1, value[i] ?? null, `${what}[${i}]`)
      if (issue !== undefined) return issue
    }
    return undefined
  }
  const fail = (expected: string) => `${what} is not ${expected}`
  switch (base) {
    case "string":
      return typeof value === "string" ? undefined : fail("a string")
    case "int":
      return Number.isSafeInteger(value) ? undefined : fail("an int within ±(2^53 - 1)")
    case "float":
      return typeof value === "number" && Number.isFinite(value) ? undefined : fail("a float")
    case "bool":
      return typeof value === "boolean" ? undefined : fail("a bool")
    case "duration":
      return isNatural(value) ? undefined : fail("a duration in whole milliseconds")
    case "json":
      return undefined
  }
  if (refMachine(base) !== undefined) return typeof value === "string" ? undefined : fail("a run id")
  const dot = base.indexOf(".")
  if (dot !== -1) {
    // An event type, `Machine.Name`: one the machine accepts or emits.
    const m = ir.machines[base.slice(0, dot)]
    const name = base.slice(dot + 1)
    const decl = m === undefined ? undefined : Object.hasOwn(m.events, name) ? m.events[name] : m.emits[name]
    if (decl === undefined) return `${what} has unknown type ${base}`
    return taggedIssue(ir, name, decl.fields, value, what)
  }
  const decl = Object.hasOwn(ir.types, base) ? ir.types[base] : undefined
  if (decl === undefined) return `${what} has unknown type ${base}`
  switch (decl.kind) {
    case "alias":
      return valueIssue(ir, decl.type, value, what)
    case "enum":
      return typeof value === "string" && decl.variants.includes(value) ? undefined : fail(`a variant of ${base}`)
    case "record":
      return fieldsIssue(ir, decl.fields, value, what, [])
    case "error":
      return taggedIssue(ir, base, decl.fields, value, what)
    case "extern":
      return undefined
  }
}

const taggedIssue = (ir: Ir, tag: string, fields: ReadonlyArray<Field>, value: Json, what: string) => {
  if (!isObject(value) || value["_tag"] !== tag) return `${what} is not a ${tag}`
  return fieldsIssue(ir, fields, value, what, ["_tag"])
}

const fieldsIssue = (
  ir: Ir,
  fields: ReadonlyArray<Field>,
  value: Json,
  what: string,
  allowed: ReadonlyArray<string>
): string | undefined => {
  if (!isObject(value)) return `${what} is not a record`
  for (const field of fields) {
    if (!Object.hasOwn(value, field.name)) return `${what} has no field ${field.name}`
    const issue = valueIssue(ir, field.type, value[field.name] as Json, `${what}.${field.name}`)
    if (issue !== undefined) return issue
  }
  const extra = Object.keys(value).find((k) => !allowed.includes(k) && !fields.some((f) => f.name === k))
  return extra === undefined ? undefined : `${what} has ${extra}, which is not a field`
}
