// The parts of the Roadbook IR (spec/ir.md, version 0.8) that the viewer reads.
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
  kind: 'task' | 'machine' | 'call'
  src: string
  /** For a call, the callee: a `ref` address. Null for a task or a machine. */
  to: Address | null
  /** `invoke Machine(…) as name`: the name the state sends to and receives from, or null. */
  as: string | null
  args: Expr[]
  span: Span
}

/** Another running machine: the receiver of a `send`, the sender of an `emitted` trigger. */
export interface Address {
  /**
   * `child`: an `invoke … as name`; `ref`: a parameter or variable of type `ref Machine`;
   * `children`: the spawned children of the collection `name` (which of them, the item says).
   */
  kind: 'child' | 'ref' | 'children'
  name: string
  /** The machine at the other end, a key of `machines`. */
  machine: string
  /** For a child, the id of the invoking state; null for a ref and for children. */
  state: string | null
}

/** A collection of spawned children (`children name: Machine by Key`), keyed by its name. */
export interface ChildrenDecl {
  /** The machine its children run, a key of `machines`. */
  machine: string
  /** The type of its keys. */
  key: string
  doc: string
  span: Span
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
  | { kind: 'call'; call: string }
  /** `on name.done`: a spawned child of the collection `children` ended with an output. */
  | { kind: 'spawn.done'; children: string }
  /** `on name.fail E`: a spawned child of the collection failed with `error`, or `*`. */
  | { kind: 'spawn.fail'; children: string; error: string }
  /** `on name.ended`: the run the ref `from` names closed, however it closed; `event.status`, of the built-in enum `Ended`, says how. */
  | { kind: 'ended'; from: Address }

export type Effect =
  | { kind: 'set'; var: string; value: Expr }
  | { kind: 'do'; action: string; args: Expr[] }
  /** `key`: for a `children` address, the key (`send name[key].E`), or null for every child (`send name.*.E`); null otherwise. */
  | { kind: 'send'; to: Address; key: Expr | null; event: string; value: Expr }
  | { kind: 'emit'; event: string; value: Expr }
  | { kind: 'reply'; call: Expr; outcome: 'done' | 'fail'; value: Expr | null }
  /** `spawn name[key] = Machine(args)` */
  | { kind: 'spawn'; children: string; machine: string; key: Expr; args: Expr[] }
  /** `stop name[key]` */
  | { kind: 'stop'; children: string; key: Expr }

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

/** `call name(params) [-> T] [! E]`: the members of a task. */
export interface CallDecl {
  params: Field[]
  returns: string | null
  errors: string[]
  doc: string
  span: Span
}

export interface Machine {
  doc: string
  span: Span
  /** Written `strict machine`: an event of its own, a message or a call that enables no transition is an `unhandled` defect, not dropped. */
  strict: boolean
  params: Field[]
  returns: string | null
  errors: string[]
  vars: Var[]
  events: Record<string, EventDecl>
  /** Events the machine emits (`emits Name { fields }`), to its invoker and every holder of a ref to it. */
  emits: Record<string, EventDecl>
  /** Calls the machine answers (`call name(params) -> T ! E`). */
  calls: Record<string, CallDecl>
  /** The collections of spawned children, keyed by name. */
  children: Record<string, ChildrenDecl>
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
export const IR_VERSION = '0.8'
