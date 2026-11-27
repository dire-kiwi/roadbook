/**
 * Saved snapshots: making one from a run's snapshot, and the checks `restore` makes before it
 * starts a run from one (execution.md, "Restoring a snapshot"), in the order the spec gives
 * them, each refusal with its reason.
 */
import { childrenOf, type Field, type Ir, isHistory, type Machine, type State } from "./ir.ts"
import { copyJson, type Json } from "./json.ts"
import { expandType, fieldsOf, isOptionalType, type TypeRef } from "./typeref.ts"
import type { Saved, Snapshot } from "./types.ts"

/**
 * The `snapshotVersion` this runtime saves. It restores this one, version 2, written before
 * keyed spawn, which has no `spawned`, and version 1, written before history states, which has
 * no `history` either.
 */
export const SNAPSHOT_VERSION = 3

/** Why a saved snapshot was refused. */
export type RefusalReason = "invalid" | "machine" | "ir" | "migration" | "ended" | "ref" | "calls"

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

/**
 * The saved form of `snapshot`: the version (always 3) added, each timer's due time from `due`,
 * and `calls` only when the run has pending calls.
 */
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
  ...(snapshot.calls.length > 0 ? { calls: snapshot.calls } : {}),
  spawned: snapshot.spawned,
  timers: snapshot.timers.map((t) => ({ ...t, due: due(t.id) })),
  nextInvoke: snapshot.nextInvoke,
  nextTimer: snapshot.nextTimer,
  nextMessage: snapshot.nextMessage,
  history: snapshot.history,
  result: snapshot.result
})

/**
 * Check `json` as a saved snapshot of machine `machine` of `ir`, as `restore` does: its shape
 * and version (`invalid`), the machine (`machine`), the hash (`ir`, or `migration` when
 * `migrate` fails; its result is checked again from the top, never migrated twice), the status
 * (`ended`), `ref` parameters (`ref`), pending calls (`calls`), and whether it fits the machine
 * (`invalid`).
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
  if (saved.calls !== undefined && saved.calls.length > 0) {
    return refuse("calls", `the run has ${saved.calls.length} pending calls, whose callers are gone`)
  }
  const fit = fitIssue(ir, m, saved)
  if (fit !== undefined) return refuse("invalid", `the snapshot does not fit ${machine}: ${fit}`)
  // A copy: the run does not share anything with what the caller holds.
  return { ok: true, saved: copyJson(saved), migrated: null }
}

// ---- the shape: snapshot.schema.json --------------------------------------------------------

/**
 * Whether `u` matches `snapshot.schema.json`: version 3 with `history` and `spawned`, version 2
 * with `history` only, or version 1 with neither. Its values are not checked.
 */
export const isSaved = (u: unknown): u is Saved => shapeIssue(u) === undefined

const UPPER = /^[A-Z][A-Za-z0-9]*$/
const LOWER = /^[a-z][A-Za-z0-9]*$/
/** An invoke's `src`: a task or machine, or for a call the address and the call (`inventory.reserve`). */
const SRC = /^([A-Za-z][A-Za-z0-9]*|[a-z][A-Za-z0-9]*\.[a-z][A-Za-z0-9]*)$/
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

/** Members exactly `names`, and any of `optional`: none missing, none other. */
const membersIssue = (
  value: { readonly [key: string]: unknown },
  names: ReadonlyArray<string>,
  what: string,
  optional: ReadonlyArray<string> = []
) => {
  const missing = names.find((n) => !Object.hasOwn(value, n))
  if (missing !== undefined) return `${what} has no ${missing}`
  const extra = Object.keys(value).find((k) => !names.includes(k) && !optional.includes(k))
  if (extra !== undefined) return `${what} has a member ${extra}, which a saved snapshot does not have`
  return undefined
}

/** What makes `json` not match snapshot.schema.json, or `undefined`. */
const shapeIssue = (json: unknown): string | undefined => {
  if (!isObject(json)) return "not a JSON object"
  const version = json["snapshotVersion"]
  if (version === 1 && Object.hasOwn(json, "history")) return "it is version 1 and has history, which version 2 added"
  if (version !== SNAPSHOT_VERSION && Object.hasOwn(json, "spawned")) {
    return `it is version ${JSON.stringify(version)} and has spawned, which version 3 added`
  }
  const expected = version === 1 ? MEMBERS : version === 2 ? [...MEMBERS, "history"] : [...MEMBERS, "history", "spawned"]
  // `calls` is written only when the run has pending calls, in any version.
  const members = membersIssue(json, expected, "it", ["calls"])
  if (members !== undefined) return members
  if (version !== 1 && version !== 2 && version !== SNAPSHOT_VERSION) {
    return `snapshotVersion is ${JSON.stringify(version)}, and this runtime reads 1, 2 and ${SNAPSHOT_VERSION}`
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
    if (!isId(inv["id"]) || !matches(STATE_ID, inv["state"]) || !matches(SRC, inv["src"])) return "an invoke is malformed"
  }
  if (Object.hasOwn(json, "calls")) {
    // Written only when not empty.
    const calls = json["calls"]
    if (!Array.isArray(calls) || calls.length === 0) return "calls is not a list of at least one call"
    for (const call of calls) {
      if (!isObject(call)) return "a call is not an object"
      const issue = membersIssue(call, ["from", "invoke", "call"], "a call")
      if (issue !== undefined) return issue
      if (!isRunId(call["from"]) || !isId(call["invoke"]) || !matches(LOWER, call["call"])) return "a call is malformed"
    }
  }
  const spawned = json["spawned"] ?? []
  if (!Array.isArray(spawned)) return "spawned is not a list"
  for (const ch of spawned) {
    if (!isObject(ch)) return "a spawned child is not an object"
    const issue = membersIssue(ch, ["id", "children", "key", "args"], "a spawned child")
    if (issue !== undefined) return issue
    const key = ch["key"]
    if (
      !isId(ch["id"]) || !matches(LOWER, ch["children"]) || !(typeof key === "string" || Number.isInteger(key)) ||
      !isObject(ch["args"]) || !Object.keys(ch["args"]).every((k) => LOWER.test(k))
    ) return "a spawned child is malformed"
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
  if (version !== 1) {
    const history = json["history"]
    if (!isObject(history)) return "history is not an object"
    for (const [id, record] of Object.entries(history)) {
      if (!STATE_ID.test(id)) return `history has ${JSON.stringify(id)}, which is not a state id`
      if (!Array.isArray(record) || record.length === 0 || !record.every((s) => matches(STATE_ID, s))) {
        return `history.${id} is not a non-empty list of state ids`
      }
      if (new Set(record).size !== record.length) return `history.${id} names a state twice`
    }
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
    if (isHistory(state)) return `active names ${id}, which is a history state`
    const children = state.states.filter((c) => active.has(c.id)).length
    if (state.kind === "compound" && children !== 1) return `${id} is an active compound state with ${children} active children`
    if (state.kind === "parallel" && children !== childrenOf(state).length) {
      return `${id} is an active parallel state whose regions are not all active`
    }
  }

  // What the history states recorded.
  for (const [id, record] of Object.entries(saved.history ?? {})) {
    const entry = states.get(id)
    if (entry === undefined || !isHistory(entry.state)) return `history has ${id}, which is not a history state`
    const issue = recordIssue(entry.state, entry.parent!, record, (s) => states.get(s))
    if (issue !== undefined) return `history.${id} is not what ${id} could have recorded: ${issue}`
  }

  // The work.
  const invokeStates = new Set<string>()
  const invokeIds = new Set<number>()
  for (const inv of saved.invokes) {
    const state = states.get(inv.state)?.state
    if (state === undefined || !active.has(inv.state)) return `invoke ${inv.id} is of ${inv.state}, which is not active`
    const src = state.invoke?.kind === "call" && state.invoke.to !== null
      ? `${state.invoke.to.name}.${state.invoke.src}`
      : state.invoke?.src
    if (src !== inv.src) return `${inv.state} has no invoke of ${inv.src}`
    if (invokeStates.has(inv.state)) return `${inv.state} has two invokes`
    invokeStates.add(inv.state)
    if (invokeIds.has(inv.id)) return `invoke id ${inv.id} is repeated`
    invokeIds.add(inv.id)
    if (inv.id >= saved.nextInvoke) return `invoke id ${inv.id} is not below nextInvoke`
  }
  const keys = new Set<string>()
  for (const ch of saved.spawned ?? []) {
    const decl = Object.hasOwn(m.children, ch.children) ? m.children[ch.children] : undefined
    if (decl === undefined) return `spawned child ${ch.id} is of ${ch.children}, which is not a collection of the machine`
    const keyIssue = valueIssue(ir, decl.key, ch.key, `the key of spawned child ${ch.id}`)
    if (keyIssue !== undefined) return keyIssue
    const child = ir.machines[decl.machine]
    if (child === undefined) return `the machine of ${ch.children} is not in the IR`
    const names = new Set(child.params.map((p) => p.name))
    const absent = child.params.find((p) => !Object.hasOwn(ch.args, p.name))
    if (absent !== undefined) return `spawned child ${ch.id} has no argument ${absent.name}`
    const other = Object.keys(ch.args).find((k) => !names.has(k))
    if (other !== undefined) return `spawned child ${ch.id} has an argument ${other}, which is not a parameter`
    for (const p of child.params) {
      const issue = valueIssue(ir, p.type, ch.args[p.name] ?? null, `the argument ${p.name} of spawned child ${ch.id}`)
      if (issue !== undefined) return issue
    }
    const key = `${ch.children} ${JSON.stringify(ch.key)}`
    if (keys.has(key)) return `two spawned children of ${ch.children} have the key ${JSON.stringify(ch.key)}`
    keys.add(key)
    if (invokeIds.has(ch.id)) return `invoke id ${ch.id} is repeated`
    invokeIds.add(ch.id)
    if (ch.id >= saved.nextInvoke) return `spawned child id ${ch.id} is not below nextInvoke`
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

/**
 * What makes `record` not a record history state `h` with parent `parent` could have made, or
 * `undefined`. A shallow one records one child of a compound parent or every child of a
 * parallel one; a deep one the leaves of a configuration below its parent. In document order
 * either way.
 */
const recordIssue = (
  h: State,
  parent: State,
  record: ReadonlyArray<string>,
  lookup: (id: string) => { readonly state: State; readonly parent: State | undefined; readonly order: number } | undefined
): string | undefined => {
  const children = childrenOf(parent)
  if (h.kind === "history") {
    if (parent.kind === "compound") {
      return record.length === 1 && children.some((c) => c.id === record[0]) ? undefined : `not one child of ${parent.id}`
    }
    const every = record.length === children.length && children.every((c, i) => c.id === record[i])
    return every ? undefined : `not every child of ${parent.id}, in order`
  }
  // The leaves and their ancestors up to the parent must be a configuration below it.
  const below = new Set<string>([parent.id])
  let last = -1
  for (const id of record) {
    const entry = lookup(id)
    if (entry === undefined) return `${id} is not a state of the machine`
    if (entry.state.kind !== "atomic" && entry.state.kind !== "final") return `${id} is not an atomic or final state`
    if (entry.order <= last) return "it is not in document order"
    last = entry.order
    let s: { readonly state: State; readonly parent: State | undefined } | undefined = entry
    while (s !== undefined && s.state !== parent) {
      below.add(s.state.id)
      s = s.parent === undefined ? undefined : lookup(s.parent.id)
    }
    if (s === undefined) return `${id} is not below ${parent.id}`
  }
  for (const id of below) {
    const state = lookup(id)!.state
    const count = childrenOf(state).filter((c) => below.has(c.id)).length
    if (state.kind === "compound" && count !== 1) return `${id} would have ${count} active children`
    if (state.kind === "parallel" && count !== childrenOf(state).length) {
      return `${id} would not have all its regions active`
    }
  }
  return undefined
}

/** Whether a parameter type holds a `ref`: a `ref`, or an optional, list or map of them, through aliases. */
const holdsRef = (ir: Ir, type: TypeRef | string): boolean => {
  const t = expandType(ir, type)
  switch (t.kind) {
    case "ref":
      return true
    case "optional":
    case "list":
      return holdsRef(ir, t.of)
    case "map":
      return holdsRef(ir, t.value)
    default:
      return false
  }
}

// ---- values ---------------------------------------------------------------------------------

/**
 * What makes JSON value `value` not the JSON form of type `type` (execution.md, "Values"), or
 * `undefined`. A record, error or event has exactly its fields (an optional one as `null`); a
 * map is an object, each member a value of its value type.
 */
export const valueIssue = (ir: Ir, type: TypeRef | string, value: Json, what: string): string | undefined => {
  const t = expandType(ir, type)
  const fail = (expected: string) => `${what} is not ${expected}`
  switch (t.kind) {
    case "optional":
      return value === null ? undefined : valueIssue(ir, t.of, value, what)
    case "list": {
      if (!Array.isArray(value)) return fail("a list")
      for (let i = 0; i < value.length; i++) {
        const issue = valueIssue(ir, t.of, value[i] ?? null, `${what}[${i}]`)
        if (issue !== undefined) return issue
      }
      return undefined
    }
    case "map": {
      if (!isObject(value)) return fail("a map")
      for (const key of Object.keys(value)) {
        const issue = valueIssue(ir, t.value, value[key] as Json, `${what}[${JSON.stringify(key)}]`)
        if (issue !== undefined) return issue
      }
      return undefined
    }
    case "builtin":
      switch (t.name) {
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
    case "ref":
      return typeof value === "string" ? undefined : fail("a run id")
    case "call": {
      // A pending-call value: the caller's run id and the id of its invoke.
      if (!isObject(value)) return fail("a pending call")
      const issue = membersIssue(value, ["from", "invoke"], what)
      if (issue !== undefined) return issue
      return isRunId(value["from"]) && isId(value["invoke"]) ? undefined : fail("a pending call")
    }
    case "event": {
      // One the machine accepts or emits.
      const m = Object.hasOwn(ir.machines, t.machine) ? ir.machines[t.machine] : undefined
      const decl = m === undefined ? undefined : Object.hasOwn(m.events, t.event) ? m.events[t.event] : m.emits[t.event]
      if (decl === undefined) return `${what} has unknown type ${t.machine}.${t.event}`
      return taggedIssue(ir, t.event, decl.fields, value, what)
    }
    case "named": {
      const decl = Object.hasOwn(ir.types, t.name) ? ir.types[t.name] : undefined
      if (decl === undefined) return `${what} has unknown type ${t.name}`
      switch (decl.kind) {
        case "enum":
          return typeof value === "string" && decl.variants.includes(value) ? undefined : fail(`a variant of ${t.name}`)
        case "extern":
          return undefined
        case "record":
        case "error": {
          const record = fieldsOf(ir, t)!
          return record.error
            ? taggedIssue(ir, t.name, record.fields, value, what)
            : fieldsIssue(ir, record.fields, value, what, [])
        }
        case "alias":
          return `${what} has a type whose aliases nest too deep`
      }
    }
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
