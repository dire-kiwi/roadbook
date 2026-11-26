// The parts of the Roadbook IR (spec/ir.md, version 0.5) that the viewer reads.
// The viewer reads the IR and nothing else: every word on the diagram comes from here.

export interface Span {
  file: number
  line: number
  col: number
  endLine: number
  endCol: number
}

export interface Field {
  name: string
  type: string
  /** Marked `sensitive`: traces show its value as "<redacted>". */
  sensitive: boolean
  doc: string
}

/** An expression node. The viewer only shows `text`, the canonical source form. */
export interface Expr {
  kind: string
  type: string
  text: string
}

/** A declared type. A record or an alias is generic when it has `params`. */
export type TypeDecl =
  | { kind: 'record'; params: string[]; fields: Field[]; doc: string; span: Span }
  | { kind: 'alias'; params: string[]; type: string; doc: string; span: Span }
  | { kind: 'enum'; variants: string[]; doc: string; span: Span }
  | { kind: 'error'; fields: Field[]; doc: string; span: Span }
  | { kind: 'extern'; doc: string; span: Span }

export interface FunctionDecl {
  params: Field[]
  returns: string
  doc: string
  span: Span
}

export interface ActionDecl {
  params: Field[]
  doc: string
  span: Span
}

export interface TaskDecl {
  params: Field[]
  returns: string | null
  errors: string[]
  doc: string
  span: Span
}

export interface Var {
  name: string
  type: string
  /** Marked `sensitive`: traces show its value as "<redacted>". */
  sensitive: boolean
  init: Expr | null
  doc: string
  span: Span
}

export interface EventDecl {
  fields: Field[]
  doc: string
  span: Span
}

/**
 * `history` and `deepHistory` are history pseudo-states (`history Name`, `deep history Name`):
 * in their parent's `states`, never active, only ever a transition's target. A state's
 * children and a parallel state's regions are its other `states`.
 */
export type StateKind = 'atomic' | 'compound' | 'parallel' | 'final' | 'history' | 'deepHistory'

export interface Invoke {
  kind: 'task' | 'machine'
  src: string
  /** `invoke Machine(…) as name`: the name the state sends to and receives from, or null. */
  as: string | null
  args: Expr[]
  span: Span
}

/** Another running machine: the receiver of a `send`, the sender of an `emitted` trigger. */
export interface Address {
  /** `child`: an `invoke … as name`; `ref`: a parameter or variable of type `ref Machine`. */
  kind: 'child' | 'ref'
  name: string
  /** The machine at the other end, a key of `machines`. */
  machine: string
  /** For a child, the id of the invoking state; null for a ref. */
  state: string | null
}

export type Trigger =
  | { kind: 'event'; event: string }
  | { kind: 'emitted'; from: Address; event: string }
  | { kind: 'any' }
  | { kind: 'after'; delay: Expr }
  | { kind: 'always' }
  | { kind: 'done' }
  | { kind: 'invoke.done' }
  | { kind: 'invoke.fail'; error: string }

export type Effect =
  | { kind: 'set'; var: string; value: Expr }
  | { kind: 'do'; action: string; args: Expr[] }
  | { kind: 'send'; to: Address; event: string; value: Expr }
  | { kind: 'emit'; event: string; value: Expr }

export interface Transition {
  trigger: Trigger
  group: number
  guard: Expr | null
  target: string | null
  effects: Effect[]
  span: Span
}

export interface State {
  id: string
  name: string
  kind: StateKind
  doc: string
  span: Span
  initial: string | null
  states: State[]
  entry: Effect[]
  exit: Effect[]
  invoke: Invoke | null
  transitions: Transition[]
  output: Expr | null
  fail: Expr | null
}

export interface Machine {
  doc: string
  span: Span
  params: Field[]
  returns: string | null
  errors: string[]
  vars: Var[]
  events: Record<string, EventDecl>
  /** Events the machine emits (`emits Name { fields }`), to its invoker and every holder of a ref to it. */
  emits: Record<string, EventDecl>
  root: State
  hash: string
}

export interface Ir {
  irVersion: string
  /** `package`: null for a file of the project, else the package's name as imports write it. */
  sources: { path: string; package: string | null }[]
  types: Record<string, TypeDecl>
  functions: Record<string, FunctionDecl>
  actions: Record<string, ActionDecl>
  tasks: Record<string, TaskDecl>
  machines: Record<string, Machine>
}

/**
 * `map[key]` if `map` has its own member `key`. IR maps are plain JSON objects, and a key
 * that comes from a URL, a click or another declaration must not reach `Object.prototype`
 * (`#constructor`, `toString`).
 */
export function own<T>(map: Record<string, T>, key: string): T | undefined {
  return Object.prototype.hasOwnProperty.call(map, key) ? map[key] : undefined
}

/** The IR version this viewer was written for. */
export const IR_VERSION = '0.5'
