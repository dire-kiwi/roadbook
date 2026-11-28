/**
 * Saved snapshots: making one from a run's snapshot, and the checks `restore` makes before it
 * starts a run from one (execution.md, "Restoring a snapshot"), in the order the spec gives
 * them, each refusal with its reason.
 */
import { childrenOf, type Field, type Ir, isHistory, type Machine, ownEvent, raiseNames, type State } from "./ir.ts"
import { copyJson, type Json } from "./json.ts"
import { expandType, fieldsOf, isOptionalType, type TypeRef } from "./typeref.ts"
import type { Saved, Snapshot } from "./types.ts"

/**
 * The `snapshotVersion` this runtime saves. It restores this one, version 3, written before
 * child runs were saved, which has no `child`, version 2, written before keyed spawn, which has
 * no `spawned`, and version 1, written before history states, which has no `history` either.
 */
export const SNAPSHOT_VERSION = 4

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
 * The saved form of `snapshot`: the version (always 4) added, the due time of each timer and
 * delayed raise from `due` (given its timer id), `calls` only when the run has pending calls,
 * `scheduled` only when it has pending delayed raises, and `locals` and `historyLocals` only
 * when they are not `{}`. No child run is nested in it: see
 * `withChildren`.
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
  ...(Object.keys(snapshot.locals).length > 0 ? { locals: snapshot.locals } : {}),
  invokes: snapshot.invokes,
  ...(snapshot.calls.length > 0 ? { calls: snapshot.calls } : {}),
  spawned: snapshot.spawned,
  timers: snapshot.timers.map((t) => ({ ...t, due: due(t.id) })),
  ...(snapshot.scheduled.length > 0 ? { scheduled: snapshot.scheduled.map((s) => ({ ...s, due: due(s.id) })) } : {}),
  nextInvoke: snapshot.nextInvoke,
  nextTimer: snapshot.nextTimer,
  nextMessage: snapshot.nextMessage,
  history: snapshot.history,
  ...(Object.keys(snapshot.historyLocals).length > 0 ? { historyLocals: snapshot.historyLocals } : {}),
  result: snapshot.result
})

/**
 * `saved` (version 4) with the saved snapshot `child(id)` of each of its invokes and spawned
 * children that has one nested as its `child` (execution.md, "Saved child runs"): those whose
 * work is a child run that has claimed its invoke and is open.
 */
export const withChildren = (saved: Saved, child: (id: number) => Saved | undefined): Saved => {
  const nest = <E extends { readonly id: number }>(entry: E): E => {
    const c = child(entry.id)
    return c === undefined ? entry : { ...entry, child: c }
  }
  return saved.spawned === undefined
    ? { ...saved, invokes: saved.invokes.map(nest) }
    : { ...saved, invokes: saved.invokes.map(nest), spawned: saved.spawned.map(nest) }
}

/**
 * `saved` without the snapshots of child runs nested in it, as a `restored` record holds it;
 * `saved` itself when it has none (a version 1 to 3 snapshot is held as it was given).
 */
export const withoutChildren = (saved: Saved): Saved => {
  const has = (e: { readonly child?: Saved }) => e.child !== undefined
  if (!saved.invokes.some(has) && !(saved.spawned ?? []).some(has)) return saved
  const strip = <E extends { readonly child?: Saved }>({ child: _, ...rest }: E): Omit<E, "child"> => rest
  return saved.spawned === undefined
    ? { ...saved, invokes: saved.invokes.map(strip) }
    : { ...saved, invokes: saved.invokes.map(strip), spawned: saved.spawned.map(strip) }
}

/** Each snapshot nested in `saved` (not deeper), with its entry and the machine the entry starts. */
const nestedOf = (ir: Ir, m: Machine | undefined, saved: Saved) => {
  const found: Array<{ readonly what: string; readonly id: number; readonly machine: string | undefined; readonly child: Saved }> = []
  for (const inv of saved.invokes) {
    if (inv.child === undefined) continue
    // A machine invoke's `src` is the machine; a task's or a call's is not one.
    const machine = Object.hasOwn(ir.machines, inv.src) ? inv.src : undefined
    found.push({ what: `invoke ${inv.id}`, id: inv.id, machine, child: inv.child })
  }
  for (const ch of saved.spawned ?? []) {
    if (ch.child === undefined) continue
    const decl = m !== undefined && Object.hasOwn(m.children, ch.children) ? m.children[ch.children] : undefined
    // A spawned task is not a child run, so a snapshot nested in its entry starts no machine.
    const machine = decl?.kind === "machine" && Object.hasOwn(ir.machines, decl.src) ? decl.src : undefined
    found.push({ what: `spawned child ${ch.id}`, id: ch.id, machine, child: ch.child })
  }
  return found
}

/**
 * Whether a hash in the tree of `saved`, a snapshot of `m`, is not the hash of its machine: its
 * own, or a nested snapshot's against the machine its entry starts. Gives what differs.
 */
const hashIssue = (ir: Ir, name: string, saved: Saved, what = "the snapshot"): string | undefined => {
  const m = ir.machines[name]!
  if (saved.ir !== m.hash) return `${what} was saved with ${saved.ir}, and ${name} is now ${m.hash}`
  for (const nested of nestedOf(ir, m, saved)) {
    if (nested.machine === undefined) continue
    const issue = hashIssue(ir, nested.machine, nested.child, `the child run of ${nested.what} of ${what}`)
    if (issue !== undefined) return issue
  }
  return undefined
}

/** A machine of the tree of `saved`, a snapshot of `m`, that has a `ref` parameter, and which. */
const refIssue = (ir: Ir, name: string, saved: Saved): string | undefined => {
  const m = ir.machines[name]!
  const ref = m.params.find((p) => holdsRef(ir, p.type))
  if (ref !== undefined) return `${name} holds a ref in parameter ${ref.name}, which cannot be restored yet`
  for (const nested of nestedOf(ir, m, saved)) {
    // The machine its entry starts; a child on another entry (a task's or a call's) is refused
    // as `invalid` after this.
    if (nested.machine === undefined) continue
    const issue = refIssue(ir, nested.machine, nested.child)
    if (issue !== undefined) return issue
  }
  return undefined
}

/**
 * Check `json` as a saved snapshot of machine `machine` of `ir`, as `restore` does, for the
 * whole tree (the snapshot and every one nested in it), each reason for the whole tree before
 * the next: its shape and version (`invalid`), the machine (`machine`), the hashes (`ir`, or
 * `migration` when `migrate` fails; called once for the whole tree, and its result is checked
 * again from the top, never migrated twice), the status (`ended`), `ref` parameters (`ref`),
 * pending calls (`calls`), and whether it fits the machine (`invalid`).
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
  const stale = hashIssue(ir, machine, saved)
  if (stale !== undefined) {
    if (migrate === undefined) return refuse("ir", `${stale}; give a migration`)
    const result = migrate(saved as unknown as Json)
    if ("error" in result) return refuse("migration", `the migration failed: ${result.error}`)
    // The result is checked from the top, and never migrated again.
    const again = checkSaved(ir, machine, result.value)
    if (!again.ok) {
      return again.reason === "ir"
        ? refuse("ir", `the migration gave a snapshot in which ${hashIssue(ir, machine, result.value as unknown as Saved)}`)
        : again
    }
    return { ok: true, saved: again.saved, migrated: saved.ir }
  }
  if (saved.status !== "running") return refuse("ended", `the run has already ended (${saved.status})`)
  const ref = refIssue(ir, machine, saved)
  if (ref !== undefined) return refuse("ref", ref)
  if (saved.calls !== undefined && saved.calls.length > 0) {
    return refuse("calls", `the run has ${saved.calls.length} pending calls, whose callers are gone`)
  }
  const fit = treeFitIssue(ir, m, saved)
  if (fit !== undefined) return refuse("invalid", `the snapshot does not fit ${machine}: ${fit}`)
  // A copy: the run does not share anything with what the caller holds.
  return { ok: true, saved: copyJson(saved), migrated: null }
}

// ---- the shape: snapshot.schema.json --------------------------------------------------------

/**
 * Whether `u` matches `snapshot.schema.json`: version 4 with `history` and `spawned` and maybe
 * nested child snapshots (`child`, each of version 4), version 3 with `history` and `spawned`,
 * version 2 with `history` only, or version 1 with neither. Its values are not checked.
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
  if (version !== 3 && version !== 4 && Object.hasOwn(json, "spawned")) {
    return `it is version ${JSON.stringify(version)} and has spawned, which version 3 added`
  }
  const expected = version === 1 ? MEMBERS : version === 2 ? [...MEMBERS, "history"] : [...MEMBERS, "history", "spawned"]
  // `calls`, `scheduled`, `locals` and `historyLocals` are written only when not empty, in any version.
  const members = membersIssue(json, expected, "it", ["calls", "scheduled", "locals", "historyLocals"])
  if (members !== undefined) return members
  if (version !== 1 && version !== 2 && version !== 3 && version !== SNAPSHOT_VERSION) {
    return `snapshotVersion is ${JSON.stringify(version)}, and this runtime reads 1 to ${SNAPSHOT_VERSION}`
  }
  // A child run's saved snapshot, nested in its entry: version 4 only, and itself version 4.
  const childIssue = (entry: { readonly [key: string]: unknown }, what: string): string | undefined => {
    if (!Object.hasOwn(entry, "child")) return undefined
    if (version !== SNAPSHOT_VERSION) {
      return `it is version ${JSON.stringify(version)} and ${what} has a child, which version 4 added`
    }
    const issue = shapeIssue(entry["child"])
    if (issue !== undefined) return `the child of ${what}: ${issue}`
    const nested = (entry["child"] as { readonly [key: string]: unknown })["snapshotVersion"]
    return nested === SNAPSHOT_VERSION ? undefined : `the child of ${what} is version ${JSON.stringify(nested)}, not 4`
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
  // A state's locals: an object keyed by name.
  const localsIssue = (locals: unknown, what: string): string | undefined => {
    if (!isObject(locals)) return `${what} is not an object keyed by state id`
    for (const [id, own] of Object.entries(locals)) {
      if (!STATE_ID.test(id)) return `${what} has ${JSON.stringify(id)}, which is not a state id`
      if (!isObject(own) || !Object.keys(own).every((k) => LOWER.test(k))) return `${what}.${id} is not an object keyed by name`
    }
    return undefined
  }
  if (Object.hasOwn(json, "locals")) {
    const issue = localsIssue(json["locals"], "locals")
    if (issue !== undefined) return issue
  }
  if (Object.hasOwn(json, "historyLocals")) {
    const historyLocals = json["historyLocals"]
    if (!isObject(historyLocals)) return "historyLocals is not an object keyed by state id"
    for (const [id, kept] of Object.entries(historyLocals)) {
      if (!STATE_ID.test(id)) return `historyLocals has ${JSON.stringify(id)}, which is not a state id`
      if (isObject(kept) && Object.keys(kept).length === 0) return `historyLocals.${id} is empty`
      const issue = localsIssue(kept, `historyLocals.${id}`)
      if (issue !== undefined) return issue
    }
  }
  const invokes = json["invokes"]
  if (!Array.isArray(invokes)) return "invokes is not a list"
  for (const inv of invokes) {
    if (!isObject(inv)) return "an invoke is not an object"
    const issue = membersIssue(inv, ["id", "state", "src"], "an invoke", ["child", "as"])
    if (issue !== undefined) return issue
    if (!isId(inv["id"]) || !matches(STATE_ID, inv["state"]) || !matches(SRC, inv["src"])) return "an invoke is malformed"
    if (Object.hasOwn(inv, "as") && !matches(LOWER, inv["as"])) return "an invoke's as is malformed"
    const child = childIssue(inv, `invoke ${inv["id"]}`)
    if (child !== undefined) return child
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
    const issue = membersIssue(ch, ["id", "children", "key", "args"], "a spawned child", ["child"])
    if (issue !== undefined) return issue
    const key = ch["key"]
    if (
      !isId(ch["id"]) || !matches(LOWER, ch["children"]) || !(typeof key === "string" || Number.isInteger(key)) ||
      !isObject(ch["args"]) || !Object.keys(ch["args"]).every((k) => LOWER.test(k))
    ) return "a spawned child is malformed"
    const child = childIssue(ch, `spawned child ${ch["id"]}`)
    if (child !== undefined) return child
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
  if (Object.hasOwn(json, "scheduled")) {
    // Written only when not empty.
    const scheduled = json["scheduled"]
    if (!Array.isArray(scheduled) || scheduled.length === 0) return "scheduled is not a list of at least one delayed raise"
    for (const sc of scheduled) {
      if (!isObject(sc)) return "a delayed raise is not an object"
      const issue = membersIssue(sc, ["id", "name", "delay", "value", "due"], "a delayed raise")
      if (issue !== undefined) return issue
      const value = sc["value"]
      if (
        !isId(sc["id"]) || !(sc["name"] === null || matches(LOWER, sc["name"])) || !isNatural(sc["delay"]) ||
        !isNatural(sc["due"]) || !isObject(value) || !matches(UPPER, value["_tag"])
      ) return "a delayed raise is malformed"
    }
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

/**
 * What makes `saved` not fit machine `m`, or a snapshot nested in it not fit its entry and its
 * own machine, to any depth; or `undefined`.
 */
const treeFitIssue = (ir: Ir, m: Machine, saved: Saved): string | undefined => {
  const own = fitIssue(ir, m, saved)
  if (own !== undefined) return own
  for (const nested of nestedOf(ir, m, saved)) {
    const issue = nestedIssue(ir, m, saved, nested)
    if (issue !== undefined) return `the child run of ${nested.what}: ${issue}`
  }
  return undefined
}

/** What makes snapshot `nested.child`, nested in `saved` (which fits `m`), not fit, or `undefined`. */
const nestedIssue = (
  ir: Ir,
  m: Machine,
  saved: Saved,
  nested: ReturnType<typeof nestedOf>[number]
): string | undefined => {
  const { child } = nested
  if (nested.what.startsWith("invoke")) {
    // `saved` fits `m`, so the entry's state has an invoke of its `src`.
    const inv = saved.invokes.find((i) => i.id === nested.id)!
    const invokes = findState(m.root, inv.state)?.invokes ?? []
    const kind = (inv.as === undefined ? invokes[0] : invokes.find((i) => i.as === inv.as))?.kind
    if (kind !== "machine") return `it is nested in the invoke of a ${kind ?? "task"}, which is not a child run`
  }
  if (nested.machine === undefined) return `its entry starts no machine of the IR`
  if (child.machine !== nested.machine) return `it is of machine ${child.machine}, not ${nested.machine}`
  const run = `${saved.run}.${nested.id}`
  if (child.run !== run) return `its run is ${child.run}, not ${run}`
  if (child.status !== "running") return `it has ended (${child.status}), and only a running child run is saved`
  if (child.calls !== undefined) return "it has calls, which a child run never takes"
  return treeFitIssue(ir, ir.machines[nested.machine]!, child)
}

const findState = (state: State, id: string): State | undefined => {
  if (state.id === id) return state
  for (const child of state.states) {
    const found = findState(child, id)
    if (found !== undefined) return found
  }
  return undefined
}

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
  const invokesSeen = new Set<string>()
  const invokeIds = new Set<number>()
  for (const inv of saved.invokes) {
    const state = states.get(inv.state)?.state
    if (state === undefined || !active.has(inv.state)) return `invoke ${inv.id} is of ${inv.state}, which is not active`
    const several = state.invokes.length > 1
    if (inv.as !== undefined && !several) return `invoke ${inv.id} has as ${inv.as}, but ${inv.state} has at most one invoke`
    if (inv.as === undefined && several) return `invoke ${inv.id} has no as, but ${inv.state} has several invokes`
    const invoke = inv.as === undefined ? state.invokes[0] : state.invokes.find((i) => i.as === inv.as)
    const src = invoke?.kind === "call" && invoke.to !== null ? `${invoke.to.name}.${invoke.src}` : invoke?.src
    if (src !== inv.src) return `${inv.state} has no invoke of ${inv.src}${inv.as === undefined ? "" : ` as ${inv.as}`}`
    const seen = `${inv.state} ${inv.as ?? ""}`
    if (invokesSeen.has(seen)) return `${inv.state} runs one invoke twice`
    invokesSeen.add(seen)
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
    const child = decl.kind === "task" ? ir.tasks[decl.src] : ir.machines[decl.src]
    if (child === undefined) return `the ${decl.kind} of ${ch.children} is not in the IR`
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
  // Delayed raises: their ids are timer ids.
  const names = raiseNames(m)
  const taken = new Set<string>()
  for (const sc of saved.scheduled ?? []) {
    const decl = ownEvent(m, sc.value._tag)
    const issue = decl === undefined
      ? `the value of delayed raise ${sc.id} is not one of ${saved.machine}'s events`
      : taggedIssue(ir, sc.value._tag, decl.fields, sc.value, `the value of delayed raise ${sc.id}`)
    if (issue !== undefined) return issue
    if (sc.name !== null) {
      if (!names.includes(sc.name)) return `delayed raise ${sc.id} is named ${sc.name}, which no raise of ${saved.machine} uses`
      if (taken.has(sc.name)) return `two delayed raises are named ${sc.name}`
      taken.add(sc.name)
    }
    if (timerIds.has(sc.id)) return `timer id ${sc.id} is repeated`
    timerIds.add(sc.id)
    if (sc.id >= saved.nextTimer) return `timer id ${sc.id} is not below nextTimer`
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

  // The active states' locals: a member for each that declares any.
  const locals = saved.locals ?? {}
  for (const id of saved.active) {
    const state = states.get(id)!.state
    if (declaresLocals(state) && !Object.hasOwn(locals, id)) return `locals has no ${id}, which is active and has locals`
  }
  for (const id of Object.keys(locals)) {
    const state = states.get(id)?.state
    if (state === undefined || !active.has(id)) return `locals has ${id}, which is not an active state`
    if (!declaresLocals(state)) return `locals has ${id}, which has no parameters or variables`
    const issue = ownLocalsIssue(ir, state, locals[id]!, `locals.${id}`)
    if (issue !== undefined) return issue
  }
  // The locals the history states kept, of the states their records re-enter. A record that
  // re-enters such a state needs its member, or entering the history state would leave their
  // parameters unbound.
  const historyLocals = saved.historyLocals ?? {}
  for (const id of Object.keys(historyLocals)) {
    const entry = states.get(id)
    if (entry === undefined || !isHistory(entry.state) || saved.history?.[id] === undefined) {
      return `historyLocals has ${id}, which is not a history state with a record`
    }
  }
  for (const [id, record] of Object.entries(saved.history ?? {})) {
    const entry = states.get(id)!
    const parent = entry.parent!
    const reentered = new Set<string>()
    for (const r of record) {
      reentered.add(r)
      if (entry.state.kind === "deepHistory") {
        for (let p = states.get(r)?.parent; p !== undefined && p !== parent; p = states.get(p.id)?.parent) {
          reentered.add(p.id)
        }
      }
    }
    const expected = [...reentered].filter((s) => declaresLocals(states.get(s)!.state))
    const kept = historyLocals[id]
    if (kept === undefined) {
      if (expected.length > 0) return `historyLocals has no ${id}, whose record re-enters ${expected[0]}, which has locals`
      continue
    }
    const absent = expected.find((s) => !Object.hasOwn(kept, s))
    if (absent !== undefined) return `historyLocals.${id} has no ${absent}, which its record re-enters`
    for (const s of Object.keys(kept)) {
      if (!expected.includes(s)) return `historyLocals.${id} has ${s}, which its record does not re-enter with locals`
      const issue = ownLocalsIssue(ir, states.get(s)!.state, kept[s]!, `historyLocals.${id}.${s}`)
      if (issue !== undefined) return issue
    }
  }
  return undefined
}

/** Whether `state` declares a parameter or a variable. */
const declaresLocals = (state: State): boolean => (state.params ?? []).length > 0 || (state.vars ?? []).length > 0

/**
 * What makes `own`, the locals of `state` held at `what`, not fit it: a name that is not one of
 * its locals, a parameter or an optional variable missing, or a value not of its type.
 */
const ownLocalsIssue = (ir: Ir, state: State, own: { readonly [name: string]: Json }, what: string) => {
  const params = state.params ?? []
  const vars = state.vars ?? []
  for (const name of Object.keys(own)) {
    const local = params.find((p) => p.name === name) ?? vars.find((v) => v.name === name)
    if (local === undefined) return `${what} has ${name}, which is not a parameter or variable of ${state.id}`
  }
  const param = params.find((p) => !Object.hasOwn(own, p.name))
  if (param !== undefined) return `${what} has no ${param.name}, a parameter of ${state.id}`
  const optional = vars.find((v) => isOptionalType(ir, v.type) && !Object.hasOwn(own, v.name))
  if (optional !== undefined) return `${what} has no ${optional.name}, which is optional and always has a value`
  for (const local of [...params, ...vars]) {
    if (!Object.hasOwn(own, local.name)) continue
    const issue = valueIssue(ir, local.type, own[local.name] ?? null, `${what}.${local.name}`)
    if (issue !== undefined) return issue
  }
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
      // One the machine accepts or emits, or, for a lower-case name, one the task emits.
      const task = Object.hasOwn(ir.tasks, t.machine) ? ir.tasks[t.machine] : undefined
      const m = Object.hasOwn(ir.machines, t.machine) ? ir.machines[t.machine] : undefined
      const decl = task !== undefined
        ? Object.hasOwn(task.emits, t.event) ? task.emits[t.event] : undefined
        : m === undefined
        ? undefined
        : ownEvent(m, t.event) ?? m.emits[t.event]
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
