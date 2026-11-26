/**
 * The Roadbook IR, version 0.2, as `spec/ir.md` describes it. The types mirror the JSON;
 * {@link loadIr} checks the version before anything else.
 */
import type { Json } from "./json.ts"

/** The IR version this package reads. While the major version is 0 only this exact one. */
export const IR_VERSION = "0.2"

export interface Span {
  readonly file: number
  readonly line: number
  readonly col: number
  readonly endLine: number
  readonly endCol: number
}

/** A field of a record, error or event, or a parameter. */
export interface Field {
  readonly name: string
  /** A type reference in canonical source form: `LineItem[]`, `string?`, `ref Inventory`. */
  readonly type: string
  readonly doc: string
}

export type TypeDecl =
  | { readonly kind: "record"; readonly fields: ReadonlyArray<Field>; readonly doc: string; readonly span: Span }
  | { readonly kind: "alias"; readonly type: string; readonly doc: string; readonly span: Span }
  | { readonly kind: "enum"; readonly variants: ReadonlyArray<string>; readonly doc: string; readonly span: Span }
  | { readonly kind: "error"; readonly fields: ReadonlyArray<Field>; readonly doc: string; readonly span: Span }
  | { readonly kind: "extern"; readonly doc: string; readonly span: Span }

export interface FunctionDecl {
  readonly params: ReadonlyArray<Field>
  readonly returns: string
  readonly doc: string
  readonly span: Span
}

export interface ActionDecl {
  readonly params: ReadonlyArray<Field>
  readonly doc: string
  readonly span: Span
}

export interface TaskDecl {
  readonly params: ReadonlyArray<Field>
  readonly returns: string | null
  readonly errors: ReadonlyArray<string>
  readonly doc: string
  readonly span: Span
}

export interface Variable {
  readonly name: string
  readonly type: string
  readonly init: Expr | null
  readonly doc: string
  readonly span: Span
}

export interface EventDecl {
  readonly fields: ReadonlyArray<Field>
  readonly doc: string
  readonly span: Span
}

export interface Machine {
  readonly doc: string
  readonly span: Span
  readonly params: ReadonlyArray<Field>
  readonly returns: string | null
  readonly errors: ReadonlyArray<string>
  readonly vars: ReadonlyArray<Variable>
  readonly events: Readonly<Record<string, EventDecl>>
  readonly emits: Readonly<Record<string, EventDecl>>
  readonly root: State
  readonly hash: string
}

export type StateKind = "atomic" | "compound" | "parallel" | "final"

export interface State {
  readonly id: string
  readonly name: string
  readonly kind: StateKind
  readonly doc: string
  readonly span: Span
  readonly initial: string | null
  readonly states: ReadonlyArray<State>
  readonly entry: ReadonlyArray<Effect>
  readonly exit: ReadonlyArray<Effect>
  readonly invoke: Invoke | null
  readonly transitions: ReadonlyArray<Transition>
  readonly output: Expr | null
  readonly fail: Expr | null
}

export interface Invoke {
  readonly kind: "task" | "machine"
  readonly src: string
  readonly as: string | null
  readonly args: ReadonlyArray<Expr>
  readonly span: Span
}

/** Another running machine: an invoked child by its `as` name, or a `ref` parameter or variable. */
export interface Address {
  readonly kind: "child" | "ref"
  readonly name: string
  readonly machine: string
  readonly state: string | null
}

export type Trigger =
  | { readonly kind: "event"; readonly event: string }
  | { readonly kind: "emitted"; readonly from: Address; readonly event: string }
  | { readonly kind: "any" }
  | { readonly kind: "after"; readonly delay: Expr }
  | { readonly kind: "always" }
  | { readonly kind: "done" }
  | { readonly kind: "invoke.done" }
  | { readonly kind: "invoke.fail"; readonly error: string }

export interface Transition {
  readonly trigger: Trigger
  readonly group: number
  readonly guard: Expr | null
  readonly target: string | null
  readonly effects: ReadonlyArray<Effect>
  readonly span: Span
}

export type Effect =
  | { readonly kind: "set"; readonly var: string; readonly value: Expr }
  | { readonly kind: "do"; readonly action: string; readonly args: ReadonlyArray<Expr> }
  | { readonly kind: "send"; readonly to: Address; readonly event: string; readonly value: Expr }
  | { readonly kind: "emit"; readonly event: string; readonly value: Expr }

interface ExprBase {
  readonly type: string
  readonly text: string
}

export type Expr =
  | (ExprBase & { readonly kind: "lit"; readonly value: Json })
  | (ExprBase & { readonly kind: "ref"; readonly path: ReadonlyArray<string> })
  | (ExprBase & { readonly kind: "field"; readonly of: Expr; readonly name: string })
  | (ExprBase & { readonly kind: "call"; readonly fn: string; readonly args: ReadonlyArray<Expr> })
  | (ExprBase & { readonly kind: "op"; readonly op: Op; readonly args: ReadonlyArray<Expr> })
  | (ExprBase & {
    readonly kind: "record"
    readonly fields: ReadonlyArray<{ readonly name: string; readonly value: Expr }>
  })
  | (ExprBase & { readonly kind: "enum"; readonly variant: string })

export type Op = "==" | "!=" | "<" | "<=" | ">" | ">=" | "+" | "-" | "*" | "and" | "or" | "not" | "neg"

export interface Ir {
  readonly irVersion: string
  readonly sources: ReadonlyArray<{ readonly path: string; readonly package: string | null }>
  readonly types: Readonly<Record<string, TypeDecl>>
  readonly functions: Readonly<Record<string, FunctionDecl>>
  readonly actions: Readonly<Record<string, ActionDecl>>
  readonly tasks: Readonly<Record<string, TaskDecl>>
  readonly machines: Readonly<Record<string, Machine>>
}

/** Thrown by {@link loadIr} for a document this package cannot run. */
export class IrError extends Error {
  override readonly name = "IrError"
}

/**
 * Check that `json` is an IR document of the version this package reads and return it typed.
 * The version is checked before anything else, as `spec/ir.md` requires. The shape is trusted
 * otherwise: the IR is produced by the compiler and checked against its schema there.
 */
export const loadIr = (json: unknown): Ir => {
  if (typeof json !== "object" || json === null || Array.isArray(json)) {
    throw new IrError("not a Roadbook IR document: expected a JSON object")
  }
  const version = (json as { irVersion?: unknown }).irVersion
  if (version !== IR_VERSION) {
    throw new IrError(
      `unsupported IR version ${JSON.stringify(version ?? null)}: this runtime reads IR version ${IR_VERSION}`
    )
  }
  return json as Ir
}

/**
 * The machine `name` of `ir`, checked to be one this version of the runtime can run. Parallel
 * states (milestone 6) and messaging between machines (milestone 5: `emits`, `ref`,
 * `invoke … as`, `on name.Event`, `send`, `emit`) are refused here, before any run starts.
 */
export const getMachine = (ir: Ir, name: string): Machine => {
  const machine = Object.hasOwn(ir.machines, name) ? ir.machines[name] : undefined
  if (machine === undefined) {
    throw new IrError(`the IR has no machine ${JSON.stringify(name)}`)
  }
  const parallel = findParallel(machine.root)
  if (parallel !== undefined) {
    throw new IrError(
      `machine ${name}: state ${parallel} is a parallel state, which this runtime does not run yet (milestone 6)`
    )
  }
  const messaging = findMessaging(machine)
  if (messaging !== undefined) {
    throw new IrError(
      `machine ${name} uses ${messaging}: messaging between machines is not supported by this runtime yet (milestone 5)`
    )
  }
  return machine
}

/** The first messaging construct in a machine, described, or `undefined` if it uses none. */
const findMessaging = (machine: Machine): string | undefined => {
  const emitted = Object.keys(machine.emits)[0]
  if (emitted !== undefined) return `emits ${emitted}`
  const isRef = (type: string) => type.startsWith("ref ")
  const param = machine.params.find((p) => isRef(p.type))
  if (param !== undefined) return `the parameter ${param.name}: ${param.type}`
  const variable = machine.vars.find((v) => isRef(v.type))
  if (variable !== undefined) return `the variable ${variable.name}: ${variable.type}`
  return stateMessaging(machine.root)
}

const stateMessaging = (state: State): string | undefined => {
  const effect = (effects: ReadonlyArray<Effect>): string | undefined => {
    for (const e of effects) {
      if (e.kind === "send") return `send ${e.to.name}.${e.event}`
      if (e.kind === "emit") return `emit ${e.event}`
    }
    return undefined
  }
  const own = effect(state.entry) ?? effect(state.exit)
  if (own !== undefined) return `${own} in state ${state.id}`
  if (state.invoke !== null && state.invoke.as !== null) {
    return `invoke ${state.invoke.src}(…) as ${state.invoke.as} in state ${state.id}`
  }
  for (const t of state.transitions) {
    if (t.trigger.kind === "emitted") return `on ${t.trigger.from.name}.${t.trigger.event} in state ${state.id}`
    const found = effect(t.effects)
    if (found !== undefined) return `${found} in state ${state.id}`
  }
  for (const child of state.states) {
    const found = stateMessaging(child)
    if (found !== undefined) return found
  }
  return undefined
}

const findParallel = (state: State): string | undefined => {
  if (state.kind === "parallel") return state.id
  for (const child of state.states) {
    const found = findParallel(child)
    if (found !== undefined) return found
  }
  return undefined
}

/** A parsed type reference: its base name and its suffixes, applied left to right. */
export interface TypeRef {
  readonly base: string
  readonly suffixes: ReadonlyArray<"[]" | "?">
}

export const parseTypeRef = (type: string): TypeRef => {
  const match = /^([A-Za-z][A-Za-z0-9]*)((?:\[\]|\?)*)$/.exec(type)
  if (match === null) throw new IrError(`not a type reference: ${JSON.stringify(type)}`)
  const suffixes: Array<"[]" | "?"> = []
  const rest = match[2] ?? ""
  for (let i = 0; i < rest.length;) {
    if (rest[i] === "?") {
      suffixes.push("?")
      i += 1
    } else {
      suffixes.push("[]")
      i += 2
    }
  }
  return { base: match[1] ?? "", suffixes }
}
