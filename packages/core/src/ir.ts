/**
 * The Roadbook IR, version 0.6, as `spec/ir.md` describes it. The types mirror the JSON;
 * {@link loadIr} checks the version before anything else.
 */
import { aliasCycle } from "./typeref.ts"
import type { Json } from "./json.ts"

/** The IR version this package reads. While the major version is 0 only this exact one. */
export const IR_VERSION = "0.6"

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
  /**
   * A type reference in canonical source form: `LineItem[]`, `string?`, `ref Inventory`,
   * `map<string, int>`, `Page<Order>`. {@link parseTypeRef} reads it.
   */
  readonly type: string
  /** Marked `sensitive`: trace records show its value as `"<redacted>"`. */
  readonly sensitive: boolean
  readonly doc: string
}

/**
 * A declared type. A record or an alias is generic when it has `params`: inside it, a bare name
 * that is one of them refers to that parameter.
 */
export type TypeDecl =
  | {
    readonly kind: "record"
    readonly params: ReadonlyArray<string>
    readonly fields: ReadonlyArray<Field>
    readonly doc: string
    readonly span: Span
  }
  | {
    readonly kind: "alias"
    readonly params: ReadonlyArray<string>
    readonly type: string
    readonly doc: string
    readonly span: Span
  }
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
  /** Marked `sensitive`: trace records show its value as `"<redacted>"`. */
  readonly sensitive: boolean
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
  readonly children: Readonly<Record<string, ChildrenDecl>>
  readonly root: State
  readonly hash: string
}

/** `children name: Machine by Key`: a keyed collection of spawned children. */
export interface ChildrenDecl {
  readonly machine: string
  /** The key type: `string`, `int` or an enum, or an alias of one. */
  readonly key: string
  readonly doc: string
  readonly span: Span
}

/** `history` and `deepHistory` are history pseudo-states (ir.md, "History states"). */
export type StateKind = "atomic" | "compound" | "parallel" | "final" | "history" | "deepHistory"

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

/**
 * Another running machine: an invoked child by its `as` name, a `ref` parameter or variable, or
 * the spawned children of a collection.
 */
export interface Address {
  readonly kind: "child" | "ref" | "children"
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
  | { readonly kind: "spawn.done"; readonly children: string }
  | { readonly kind: "spawn.fail"; readonly children: string; readonly error: string }

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
  | {
    readonly kind: "send"
    readonly to: Address
    /** For a collection, the child's key, or `null` for every running child. */
    readonly key: Expr | null
    readonly event: string
    readonly value: Expr
  }
  | { readonly kind: "emit"; readonly event: string; readonly value: Expr }
  | {
    readonly kind: "spawn"
    readonly children: string
    readonly machine: string
    readonly key: Expr
    readonly args: ReadonlyArray<Expr>
  }
  | { readonly kind: "stop"; readonly children: string; readonly key: Expr }

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

/** Whether `state` is a history pseudo-state: never active, and never one of its parent's children. */
export const isHistory = (state: State): boolean => state.kind === "history" || state.kind === "deepHistory"

/**
 * The children of `state` in execution.md's sense: its `states` that are not history states,
 * in order. A parallel state's regions are these.
 */
export const childrenOf = (state: State): ReadonlyArray<State> =>
  state.states.some(isHistory) ? state.states.filter((c) => !isHistory(c)) : state.states

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
  const cycle = aliasCycle(json as Ir)
  if (cycle !== undefined) {
    throw new IrError(`type alias ${JSON.stringify(cycle)} refers to itself, so it cannot be expanded`)
  }
  return json as Ir
}

/** The machine `name` of `ir`. Throws an `IrError` if there is none. */
export const getMachine = (ir: Ir, name: string): Machine => {
  const machine = Object.hasOwn(ir.machines, name) ? ir.machines[name] : undefined
  if (machine === undefined) {
    throw new IrError(`the IR has no machine ${JSON.stringify(name)}`)
  }
  return machine
}

