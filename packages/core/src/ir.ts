/**
 * The Roadbook IR, version 0.14, as `spec/ir.md` describes it. The types mirror the JSON;
 * {@link loadIr} checks the version before anything else.
 */
import { aliasCycle } from "./typeref.ts"
import type { Json } from "./json.ts"

/** The IR version this package reads. While the major version is 0 only this exact one. */
export const IR_VERSION = "0.14"

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
  /**
   * The events the task emits while it runs (`emits Name { fields }`), keyed by name, `{}` if
   * none. They are in the task's own namespace: their type is `task.Name`.
   */
  readonly emits: Readonly<Record<string, EventDecl>>
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

/** A call the machine answers, `call name(params) -> T ! E`: the members of a task. */
export interface CallDecl {
  readonly params: ReadonlyArray<Field>
  readonly returns: string | null
  readonly errors: ReadonlyArray<string>
  readonly doc: string
  readonly span: Span
}

/** The built-in error every call can fail with besides its declared ones. */
export const UNANSWERED = "Unanswered"

/**
 * The built-in enum of how a peer's run ended, the type of `event.status` in `on name.ended`.
 * Whenever a document names it, it is in `types` as an ordinary `enum` with these variants.
 */
export const ENDED = "Ended"
export const ENDED_VARIANTS = ["Done", "Failed", "Cancelled", "Defect", "Suspended"] as const

export interface Machine {
  readonly doc: string
  readonly span: Span
  readonly strict: boolean
  readonly params: ReadonlyArray<Field>
  readonly returns: string | null
  readonly errors: ReadonlyArray<string>
  readonly vars: ReadonlyArray<Variable>
  readonly events: Readonly<Record<string, EventDecl>>
  /** Events only the machine raises itself (`internal event Name`); host code cannot send them. */
  readonly internalEvents: Readonly<Record<string, EventDecl>>
  readonly emits: Readonly<Record<string, EventDecl>>
  readonly calls: Readonly<Record<string, CallDecl>>
  readonly children: Readonly<Record<string, ChildrenDecl>>
  readonly root: State
  readonly hash: string
}

/**
 * `children name: Machine by Key` or `children name: task by Key`: a keyed collection of
 * spawned children, machines or tasks.
 */
export interface ChildrenDecl {
  /** Whether the children are machines or tasks. */
  readonly kind: "machine" | "task"
  /** What they run: a key of `machines` or of `tasks`, by `kind`. */
  readonly src: string
  /** The key type: `string`, `int` or an enum, or an alias of one. */
  readonly key: string
  readonly doc: string
  readonly span: Span
}

/**
 * `history` and `deepHistory` are history pseudo-states (ir.md, "History states"). A `choice` is
 * run as an atomic state whose transitions are all `always`, the last unguarded.
 */
export type StateKind = "atomic" | "compound" | "parallel" | "final" | "history" | "deepHistory" | "choice"

export interface State {
  readonly id: string
  readonly name: string
  readonly kind: StateKind
  readonly doc: string
  readonly span: Span
  /** The state's parameters, in order: `[]` for a state written without them. */
  readonly params: ReadonlyArray<Field>
  /** The state's variables, in order: `[]` for a state that declares none. */
  readonly vars: ReadonlyArray<Variable>
  readonly initial: string | null
  /**
   * For a history state, the ids of the states it enters when it has recorded nothing: `[]` for
   * one written without a default, and for every other state.
   */
  readonly default: ReadonlyArray<string>
  readonly states: ReadonlyArray<State>
  readonly entry: ReadonlyArray<Effect>
  readonly exit: ReadonlyArray<Effect>
  /** The invokes, in the order written. With several, each has an `as`, and no two the same. */
  readonly invokes: ReadonlyArray<Invoke>
  readonly transitions: ReadonlyArray<Transition>
  readonly output: Expr | null
  readonly fail: Expr | null
}

export interface Invoke {
  readonly kind: "task" | "machine" | "call"
  /** The task's or machine's name, or the call's. */
  readonly src: string
  /** For a call, the callee: an address of kind `ref`. `null` for a task or a machine. */
  readonly to: MachineAddress | null
  readonly as: string | null
  readonly args: ReadonlyArray<Expr>
  readonly span: Span
}

/**
 * Another running machine, or a running task that emits: an invoked child or task by its `as`
 * name, a `ref` parameter or variable, or the spawned children of a collection. It names its
 * other end by `machine` or by `task`, never both.
 */
export type Address = MachineAddress | TaskAddress

/** An address of a machine: an invoked child, a `ref`, or a collection of machines. */
export interface MachineAddress {
  readonly kind: "child" | "ref" | "children"
  readonly name: string
  readonly machine: string
  readonly task?: undefined
  readonly state: string | null
}

/**
 * An address of a task: one invoked with `as` (kind `task`), or a collection of tasks. It
 * appears only in `emitted` triggers, since a task takes no events.
 */
export interface TaskAddress {
  readonly kind: "task" | "children"
  readonly name: string
  readonly task: string
  readonly machine?: undefined
  readonly state: string | null
}

/** The name of the machine or task at the other end of `address`. */
export const peerOf = (address: Address): string =>
  address.machine !== undefined ? address.machine : address.task

export type Trigger =
  | { readonly kind: "event"; readonly event: string }
  | { readonly kind: "emitted"; readonly from: Address; readonly event: string }
  | { readonly kind: "any" }
  | { readonly kind: "after"; readonly delay: Expr }
  | { readonly kind: "always" }
  | { readonly kind: "done" }
  /** `as` names the invoke of the state it belongs to; null for a state's one invoke without `as`. */
  | { readonly kind: "invoke.done"; readonly as: string | null }
  | { readonly kind: "invoke.fail"; readonly as: string | null; readonly error: string }
  | { readonly kind: "call"; readonly call: string }
  | { readonly kind: "spawn.done"; readonly children: string }
  | { readonly kind: "spawn.fail"; readonly children: string; readonly error: string }
  | { readonly kind: "ended"; readonly from: MachineAddress }

export interface Transition {
  readonly trigger: Trigger
  readonly group: number
  readonly guard: Expr | null
  /** Target state ids, in the order written: `[]` for a transition with no target. */
  readonly targets: ReadonlyArray<string>
  /** An internal transition (`-> .X`): its domain is its own state, which it does not leave. */
  readonly internal: boolean
  /** The arguments of each state with parameters it enters, in the order they are entered. */
  readonly args: ReadonlyArray<StateArgs>
  readonly effects: ReadonlyArray<Effect>
  readonly span: Span
}

/** The arguments a transition passes to one state with parameters that it enters. */
export interface StateArgs {
  readonly state: string
  /** One per parameter, in parameter order. */
  readonly values: ReadonlyArray<Expr>
}

export type Effect =
  /** `state`: the id of the state whose variable `var` is, or `null` for the machine's. */
  | { readonly kind: "set"; readonly var: string; readonly state: string | null; readonly value: Expr }
  | { readonly kind: "do"; readonly action: string; readonly args: ReadonlyArray<Expr> }
  | {
    readonly kind: "send"
    readonly to: MachineAddress
    /** For a collection, the child's key, or `null` for every running child. */
    readonly key: Expr | null
    readonly event: string
    readonly value: Expr
  }
  | { readonly kind: "emit"; readonly event: string; readonly value: Expr }
  | {
    readonly kind: "reply"
    /** A `ref` expression of type `call c`: the implicit `call` or a variable. */
    readonly call: Expr
    readonly outcome: "done" | "fail"
    readonly value: Expr | null
  }
  | {
    readonly kind: "spawn"
    readonly children: string
    /** The collection's machine or task; which of the two is the collection's `kind`. */
    readonly src: string
    readonly key: Expr
    readonly args: ReadonlyArray<Expr>
  }
  | { readonly kind: "stop"; readonly children: string; readonly key: Expr }
  /** `raise E`, on the internal queue, or `raise E after d [as name]`, a delayed raise. */
  | {
    readonly kind: "raise"
    readonly event: string
    readonly value: Expr
    readonly delay: Expr | null
    readonly name: string | null
  }
  /** `cancel name`: the pending delayed raise of that name, if any. */
  | { readonly kind: "cancel"; readonly name: string }

interface ExprBase {
  readonly type: string
  readonly text: string
}

export type Expr =
  | (ExprBase & { readonly kind: "lit"; readonly value: Json })
  /** `state`: the id of the state whose parameter or variable `path[0]` is, or `null` for the machine's or an implicit name. */
  | (ExprBase & { readonly kind: "ref"; readonly path: ReadonlyArray<string>; readonly state: string | null })
  | (ExprBase & { readonly kind: "field"; readonly of: Expr; readonly name: string })
  | (ExprBase & { readonly kind: "call"; readonly fn: string; readonly args: ReadonlyArray<Expr> })
  | (ExprBase & { readonly kind: "op"; readonly op: Op; readonly args: ReadonlyArray<Expr> })
  | (ExprBase & {
    readonly kind: "record"
    readonly fields: ReadonlyArray<{ readonly name: string; readonly value: Expr }>
  })
  | (ExprBase & { readonly kind: "enum"; readonly variant: string })
  /** Whether state `state`, an id, is active when the expression is evaluated. Its `type` is `bool`. */
  | (ExprBase & { readonly kind: "in"; readonly state: string })

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

/** The machine's own event `name`: one of its `events` or `internalEvents`. */
export const ownEvent = (m: Machine, name: string): EventDecl | undefined =>
  Object.hasOwn(m.events, name)
    ? m.events[name]
    : Object.hasOwn(m.internalEvents, name)
    ? m.internalEvents[name]
    : undefined

/** The names of `m`'s delayed raises (`raise … after … as name`), each once, in document order. */
export const raiseNames = (m: Machine): ReadonlyArray<string> => {
  const out: Array<string> = []
  const walk = (s: State) => {
    for (const e of [...s.entry, ...s.exit, ...s.transitions.flatMap((t) => t.effects)]) {
      if (e.kind === "raise" && e.name !== null && !out.includes(e.name)) out.push(e.name)
    }
    s.states.forEach(walk)
  }
  walk(m.root)
  return out
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

