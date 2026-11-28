// The parts of the Roadbook IR (spec/ir.md, version 0.14) that the viewer reads.
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
  /** Events the task emits while it runs (`emits Name { fields }`), to the run that invoked or spawned it. */
  emits: Record<string, EventDecl>
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
export type StateKind = 'atomic' | 'compound' | 'parallel' | 'final' | 'history' | 'deepHistory' | 'choice'

export interface Invoke {
  kind: 'task' | 'machine' | 'call'
  src: string
  /** For a call, the callee: a `ref` address. Null for a task or a machine. */
  to: Address | null
  /**
   * `invoke Machine(…) as name` or `invoke task(…) as name` (a task that emits): the name the
   * state sends to and receives from, and its outcomes' triggers name it by, or null. Always
   * set in a state with several invokes.
   */
  as: string | null
  args: Expr[]
  span: Span
}

/**
 * Another running machine, or a running task that emits: the receiver of a `send`, the sender
 * of an `emitted` trigger. It names its other end by `machine` or by `task`, never both.
 */
export interface Address {
  /**
   * `child`: an `invoke Machine(…) as name`; `task`: an `invoke task(…) as name`; `ref`: a
   * parameter or variable of type `ref Machine`; `children`: the spawned children of the
   * collection `name`, machines or tasks (which of them, the item says).
   */
  kind: 'child' | 'task' | 'ref' | 'children'
  name: string
  /** The machine at the other end, a key of `machines`: for `child`, `ref`, and `children` of machines. */
  machine?: string
  /** The task at the other end, a key of `tasks`: for `task`, and `children` of tasks. Only `emitted` triggers name one. */
  task?: string
  /** For a child or a task, the id of the invoking state; null for a ref and for children. */
  state: string | null
}

/** A collection of spawned children (`children name: Machine by Key`) or tasks (`children name: task by Key`), keyed by its name. */
export interface ChildrenDecl {
  /** What its children are: machines or tasks. */
  kind: 'machine' | 'task'
  /** The machine its children run, a key of `machines`, or the task, a key of `tasks`. */
  src: string
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
  /** `as` names the invoke of the state it belongs to; null for a state's one invoke without `as`. */
  | { kind: 'invoke.done'; as: string | null }
  | { kind: 'invoke.fail'; as: string | null; error: string }
  | { kind: 'call'; call: string }
  /** `on name.done`: a spawned child of the collection `children` ended with an output. */
  | { kind: 'spawn.done'; children: string }
  /** `on name.fail E`: a spawned child of the collection failed with `error`, or `*`. */
  | { kind: 'spawn.fail'; children: string; error: string }
  /** `on name.ended`: the run the ref `from` names closed, however it closed; `event.status`, of the built-in enum `Ended`, says how. */
  | { kind: 'ended'; from: Address }

export type Effect =
  /** `state`: the id of the state whose variable `var` is, or null for the machine's. */
  | { kind: 'set'; var: string; state: string | null; value: Expr }
  | { kind: 'do'; action: string; args: Expr[] }
  /** `key`: for a `children` address, the key (`send name[key].E`), or null for every child (`send name.*.E`); null otherwise. */
  | { kind: 'send'; to: Address; key: Expr | null; event: string; value: Expr }
  | { kind: 'emit'; event: string; value: Expr }
  | { kind: 'reply'; call: Expr; outcome: 'done' | 'fail'; value: Expr | null }
  /** `spawn name[key] = Machine(args)` or `spawn name[key] = task(args)`: `src` is the collection's machine or task. */
  | { kind: 'spawn'; children: string; src: string; key: Expr; args: Expr[] }
  /** `stop name[key]` */
  | { kind: 'stop'; children: string; key: Expr }
  /**
   * `raise E { … }`, on the internal queue (`delay` null), or `raise E { … } after d [as name]`,
   * a delayed raise: a timer of the run, not of a state. `event` is one of the machine's own
   * events, accepted or internal; `value`'s text is the event as written after `raise`.
   */
  | { kind: 'raise'; event: string; value: Expr; delay: Expr | null; name: string | null }
  /** `cancel name`: the pending delayed raise of that name, if any. */
  | { kind: 'cancel'; name: string }

export interface Transition {
  trigger: Trigger
  group: number
  guard: Expr | null
  /** Target state ids, in the order written: `[]` for a transition with no target. */
  targets: string[]
  /** An internal transition (`-> .X`): its own state is not left. */
  internal: boolean
  /** The arguments of each state with parameters it enters, in the order they are entered. */
  args: StateArgs[]
  effects: Effect[]
  span: Span
}

/** The arguments a transition passes to one state with parameters that it enters. */
export interface StateArgs {
  state: string
  /** One per parameter, in parameter order. */
  values: Expr[]
}

export interface State {
  id: string
  name: string
  kind: StateKind
  doc: string
  span: Span
  /** The state's parameters, in order: `[]` for a state written without them. */
  params: Field[]
  /** The state's variables, in order: `[]` for a state that declares none. */
  vars: Var[]
  initial: string | null
  /** For a history state, the states it enters when it has recorded nothing: `[]` for none. */
  default: string[]
  states: State[]
  entry: Effect[]
  exit: Effect[]
  /** The invokes, in the order written. With several, each has an `as`, and no two the same. */
  invokes: Invoke[]
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
  /** Events only the machine raises itself (`internal event Name { fields }`): host code and other machines cannot send them. */
  internalEvents: Record<string, EventDecl>
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
export const IR_VERSION = '0.14'
