/**
 * Type references: the strings the IR uses wherever a type is named, read into a tree, and the
 * two questions the runtimes ask of one: what it is once aliases are expanded, and the fields of
 * a record or error type, generic instances substituted.
 */
import { type Field, type Ir, IrError, type TypeDecl } from "./ir.ts"

export type Builtin = "string" | "int" | "float" | "bool" | "duration" | "json"

const BUILTINS: ReadonlyArray<string> = ["string", "int", "float", "bool", "duration", "json"]

/**
 * A parsed type reference. `[]` and `?` wrap what they follow, left to right: `string?[]` is a
 * list of optional strings. A `named` type is a declared type (or, inside a generic declaration,
 * one of its parameters, or a bare event name of the machine it is used in); `args` is empty
 * unless it is an instance of a generic declaration, `Page<Order>`. An `event` is a dotted
 * `Machine.Event`. A `call` is a pending call of the machine it is used in, `call reserve`.
 */
export type TypeRef =
  | { readonly kind: "builtin"; readonly name: Builtin }
  | { readonly kind: "named"; readonly name: string; readonly args: ReadonlyArray<TypeRef> }
  | { readonly kind: "event"; readonly machine: string; readonly event: string }
  | { readonly kind: "ref"; readonly machine: string }
  | { readonly kind: "call"; readonly call: string }
  | { readonly kind: "map"; readonly key: TypeRef; readonly value: TypeRef }
  | { readonly kind: "list"; readonly of: TypeRef }
  | { readonly kind: "optional"; readonly of: TypeRef }

const UPPER = /^[A-Z][A-Za-z0-9]*$/
const LOWER = /^[a-z][A-Za-z0-9]*$/

/** The deepest nesting read, so that a hostile IR cannot exhaust the stack. */
const MAX_DEPTH = 128

/** The types read so far, by text: the IR names a few types many times over. */
const parsed = new Map<string, TypeRef>()
const MAX_PARSED = 10_000

/**
 * Read a type reference in canonical form (spec/ir.md): a base (a builtin, an upper-case name
 * with optional type arguments, `Machine.Event`, `ref Machine`, `call name` or `map<K, V>`, arguments
 * separated by `, `) and any number of `[]` and `?`. Throws an `IrError` for anything else.
 */
export const parseTypeRef = (text: string): TypeRef => {
  const known = parsed.get(text)
  if (known !== undefined) return known
  let at = 0
  const fail = (): never => {
    throw new IrError(`not a type reference: ${JSON.stringify(text)}`)
  }
  const eat = (s: string) => {
    if (!text.startsWith(s, at)) return false
    at += s.length
    return true
  }
  const word = () => {
    const start = at
    while (at < text.length && /[A-Za-z0-9.]/.test(text[at]!)) at += 1
    return text.slice(start, at)
  }
  const type = (depth: number): TypeRef => {
    if (depth > MAX_DEPTH) fail()
    let t: TypeRef
    if (eat("ref ")) {
      const machine = word()
      if (!UPPER.test(machine)) fail()
      t = { kind: "ref", machine }
    } else if (eat("call ")) {
      const call = word()
      if (!LOWER.test(call)) fail()
      t = { kind: "call", call }
    } else {
      const base = word()
      if (base === "map" && eat("<")) {
        const key = type(depth + 1)
        if (!eat(", ")) fail()
        const value = type(depth + 1)
        if (!eat(">")) fail()
        t = { kind: "map", key, value }
      } else if (BUILTINS.includes(base)) {
        t = { kind: "builtin", name: base as Builtin }
      } else if (UPPER.test(base)) {
        const args: Array<TypeRef> = []
        if (eat("<")) {
          do args.push(type(depth + 1))
          while (eat(", "))
          if (!eat(">")) fail()
        }
        t = { kind: "named", name: base, args }
      } else {
        const dot = base.indexOf(".")
        const machine = base.slice(0, dot)
        const event = base.slice(dot + 1)
        if (dot === -1 || !UPPER.test(machine) || !UPPER.test(event)) fail()
        t = { kind: "event", machine, event }
      }
    }
    // Each suffix is a level too, counted against the same bound.
    for (let level = depth; ; level++) {
      if (level > MAX_DEPTH) fail()
      if (eat("[]")) t = { kind: "list", of: t }
      else if (eat("?")) t = { kind: "optional", of: t }
      else return t
    }
  }
  const t = type(0)
  if (at !== text.length) fail()
  if (parsed.size >= MAX_PARSED) parsed.clear()
  parsed.set(text, t)
  return t
}

/** The canonical text of a type reference: `parseTypeRef(typeRefText(t))` is `t`. */
export const typeRefText = (t: TypeRef): string => {
  switch (t.kind) {
    case "builtin":
      return t.name
    case "named":
      return t.args.length === 0 ? t.name : `${t.name}<${t.args.map(typeRefText).join(", ")}>`
    case "event":
      return `${t.machine}.${t.event}`
    case "ref":
      return `ref ${t.machine}`
    case "call":
      return `call ${t.call}`
    case "map":
      return `map<${typeRefText(t.key)}, ${typeRefText(t.value)}>`
    case "list":
      return `${typeRefText(t.of)}[]`
    case "optional":
      return `${typeRefText(t.of)}?`
  }
}

/**
 * Every name `t` mentions as a declared type (or a type parameter, or an event type, dotted),
 * outermost first: `map<UserId, Page<Order>>` gives `UserId`, `Page` and `Order`. The machine of
 * a `ref` is not a type and is not given, nor is the call of a `call`.
 */
export const typeNames = (t: TypeRef): Array<string> => {
  switch (t.kind) {
    case "builtin":
    case "ref":
    case "call":
      return []
    case "named":
      return [t.name, ...t.args.flatMap(typeNames)]
    case "event":
      return [`${t.machine}.${t.event}`]
    case "map":
      return [...typeNames(t.key), ...typeNames(t.value)]
    case "list":
    case "optional":
      return typeNames(t.of)
  }
}

/**
 * `t` with each of `params` replaced by the argument at the same index: a member type of the
 * instance `G<args>` of a generic declaration with these parameters.
 */
export const substituteType = (t: TypeRef, params: ReadonlyArray<string>, args: ReadonlyArray<TypeRef>): TypeRef => {
  if (params.length === 0) return t
  switch (t.kind) {
    case "named": {
      const i = t.args.length === 0 ? params.indexOf(t.name) : -1
      if (i !== -1) return args[i] ?? t
      return t.args.length === 0 ? t : { ...t, args: t.args.map((a) => substituteType(a, params, args)) }
    }
    case "map":
      return { kind: "map", key: substituteType(t.key, params, args), value: substituteType(t.value, params, args) }
    case "list":
    case "optional":
      return { kind: t.kind, of: substituteType(t.of, params, args) }
    default:
      return t
  }
}

/** What these functions read of an IR: its declared types. */
export type IrTypes = Pick<Ir, "types">

const declOf = (ir: IrTypes, name: string): TypeDecl | undefined => Object.hasOwn(ir.types, name) ? ir.types[name] : undefined

/**
 * `t` with aliases at the top expanded, the arguments of a generic alias substituted, until it
 * is not an alias: `type Dict<T> = map<string, T>` makes `Dict<Order>` `map<string, Order>`,
 * `type First<T> = T` makes `First<int>` `int`. What is inside a list, an optional or a map is
 * left as it is.
 */
export const expandType = (ir: IrTypes, type: TypeRef | string): TypeRef => {
  let t = typeof type === "string" ? parseTypeRef(type) : type
  for (let i = 0; i < 64 && t.kind === "named"; i++) {
    const decl = declOf(ir, t.name)
    // A bare generic alias (which the compiler never writes) is left as it is, as in Rust.
    if (decl?.kind !== "alias" || decl.params.length !== t.args.length) break
    t = substituteType(parseTypeRef(decl.type), decl.params, t.args)
  }
  return t
}

/**
 * The fields of `type` if it is, through aliases, a record or error type, the arguments of an
 * instance of a generic record substituted in their types: its declared name, whether it is an
 * error, and the fields. `undefined` for any other type.
 */
export const fieldsOf = (
  ir: IrTypes,
  type: TypeRef | string
): { readonly name: string; readonly error: boolean; readonly fields: ReadonlyArray<Field> } | undefined => {
  const t = expandType(ir, type)
  if (t.kind !== "named") return undefined
  const decl = declOf(ir, t.name)
  if (decl?.kind === "error") return { name: t.name, error: true, fields: decl.fields }
  if (decl?.kind !== "record") return undefined
  const params = decl.params
  if (params.length === 0) return { name: t.name, error: false, fields: decl.fields }
  const fields = decl.fields.map((f) => ({
    ...f,
    type: typeRefText(substituteType(parseTypeRef(f.type), params, t.args))
  }))
  return { name: t.name, error: false, fields }
}

/** Whether a type reference is optional once aliases are expanded (`type Note = string?`). */
export const isOptionalType = (ir: IrTypes, type: TypeRef | string): boolean => expandType(ir, type).kind === "optional"

/**
 * An alias that refers to itself, directly or through other aliases (anywhere in its type: in
 * a type argument, a map, a list), if there is one. The compiler never writes one; `loadIr`
 * refuses an IR that has one, since expanding it would not end.
 */
export const aliasCycle = (ir: IrTypes): string | undefined => {
  const aliasesIn = (name: string): Array<string> => {
    const decl = declOf(ir, name)
    if (decl?.kind !== "alias") return []
    return typeNames(parseTypeRef(decl.type)).filter((n) => declOf(ir, n)?.kind === "alias")
  }
  const done = new Set<string>()
  const onPath = new Set<string>()
  const visit = (name: string): string | undefined => {
    if (onPath.has(name)) return name
    if (done.has(name)) return undefined
    onPath.add(name)
    for (const next of aliasesIn(name)) {
      const found = visit(next)
      if (found !== undefined) return found
    }
    onPath.delete(name)
    done.add(name)
    return undefined
  }
  for (const name of Object.keys(ir.types)) {
    const found = visit(name)
    if (found !== undefined) return found
  }
  return undefined
}
