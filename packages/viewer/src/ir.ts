// The parts of the Roadbook IR (spec/ir.md, version 0.1) that the viewer reads.
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
  doc: string
}

/** An expression node. The viewer only shows `text`, the canonical source form. */
export interface Expr {
  kind: string
  type: string
  text: string
}

export type TypeDecl =
  | { kind: 'record'; fields: Field[]; doc: string; span: Span }
  | { kind: 'alias'; type: string; doc: string; span: Span }
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
  init: Expr | null
  doc: string
  span: Span
}

export interface EventDecl {
  fields: Field[]
  doc: string
  span: Span
}

export type StateKind = 'atomic' | 'compound' | 'parallel' | 'final'

export interface Invoke {
  kind: 'task' | 'machine'
  src: string
  args: Expr[]
  span: Span
}

export type Trigger =
  | { kind: 'event'; event: string }
  | { kind: 'any' }
  | { kind: 'after'; delay: Expr }
  | { kind: 'always' }
  | { kind: 'done' }
  | { kind: 'invoke.done' }
  | { kind: 'invoke.fail'; error: string }

export type Effect =
  | { kind: 'set'; var: string; value: Expr }
  | { kind: 'do'; action: string; args: Expr[] }

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
  root: State
  hash: string
}

export interface Ir {
  irVersion: string
  sources: { path: string }[]
  types: Record<string, TypeDecl>
  functions: Record<string, FunctionDecl>
  actions: Record<string, ActionDecl>
  tasks: Record<string, TaskDecl>
  machines: Record<string, Machine>
}

/** The IR version this viewer was written for. */
export const IR_VERSION = '0.1'
