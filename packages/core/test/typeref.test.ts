import { describe, expect, it } from "vitest"
import {
  aliasCycle,
  checkSaved,
  expandType,
  fieldsOf,
  type Ir,
  IrError,
  isOptionalType,
  loadIr,
  parseTypeRef,
  substituteType,
  typeNames,
  typeRefText,
  valueIssue
} from "../src/index.ts"

const span = { file: 0, line: 1, col: 1, endLine: 1, endCol: 1 }
const field = (name: string, type: string) => ({ name, type, sensitive: false, doc: "" })

/** The declarations of crates/roadbook-compiler/tests/pass/generics.road, and an error. */
const ir = {
  irVersion: "0.9",
  sources: [],
  types: {
    UserId: { kind: "alias", params: [], type: "string", doc: "", span },
    Page: { kind: "record", params: ["T"], fields: [field("items", "T[]"), field("next", "string?")], doc: "", span },
    Pair: { kind: "record", params: ["A", "B"], fields: [field("first", "A"), field("second", "B")], doc: "", span },
    Tree: { kind: "record", params: ["T"], fields: [field("value", "T"), field("children", "Tree<T>[]")], doc: "", span },
    Dict: { kind: "alias", params: ["T"], type: "map<string, T>", doc: "", span },
    Scores: { kind: "alias", params: [], type: "map<UserId, int>", doc: "", span },
    Maybe: { kind: "alias", params: ["T"], type: "T?", doc: "", span },
    First: { kind: "alias", params: ["T"], type: "T", doc: "", span },
    OrderPage: { kind: "alias", params: [], type: "Page<Pair<string, int>>", doc: "", span },
    Oops: { kind: "error", fields: [field("why", "string")], doc: "", span }
  },
  functions: {},
  actions: {},
  tasks: {},
  machines: {}
} as unknown as Ir

describe("parseTypeRef", () => {
  it("reads every canonical form and writes it back the same", () => {
    for (
      const text of [
        "string",
        "json",
        "LineItem[]",
        "string?",
        "string[]?",
        "string?[]",
        "A1[][]",
        "ref Inventory",
        "ref Inventory[]?",
        "Upload.Progress",
        "upload.Progress",
        "upload.Progress[]?",
        "map<string, int>",
        "map<UserId, Order[]>?",
        "Page<Order>",
        "Pair<string, map<string, Tree<int>>>[]",
        "Page<int?>?",
        "map<string, ref M>",
        "call reserve"
      ]
    ) {
      expect(typeRefText(parseTypeRef(text))).toBe(text)
    }
  })

  it("builds a tree, suffixes applied left to right", () => {
    expect(parseTypeRef("string?[]")).toEqual({
      kind: "list",
      of: { kind: "optional", of: { kind: "builtin", name: "string" } }
    })
    expect(parseTypeRef("map<UserId, Page<Order>>")).toEqual({
      kind: "map",
      key: { kind: "named", name: "UserId", args: [] },
      value: { kind: "named", name: "Page", args: [{ kind: "named", name: "Order", args: [] }] }
    })
    expect(parseTypeRef("Upload.Progress")).toEqual({ kind: "event", machine: "Upload", event: "Progress" })
    // A task's emitted event: the sender is the task, whose name is lower-case.
    expect(parseTypeRef("upload.Progress")).toEqual({ kind: "event", machine: "upload", event: "Progress" })
    expect(parseTypeRef("ref Inventory[]")).toEqual({ kind: "list", of: { kind: "ref", machine: "Inventory" } })
  })

  it("refuses what is not a type reference", () => {
    for (
      const text of [
        "",
        "order",
        "Order []",
        "Order[",
        "[]",
        "?",
        "ref order",
        "ref  Order",
        "ref int",
        "ref Order.Event",
        "Order.",
        "Order.event",
        "A.B.C",
        "int!",
        "Or_der",
        "map",
        "map<string,int>",
        "map<string>",
        "map<string, int, int>",
        "Page<>",
        "Page<Order",
        "Page <Order>",
        "page<Order>",
        "ref Page<Order>",
        "Upload.Progress<int>",
        "upload.progress",
        "upload.Progress<int>",
        "ref upload.Progress",
        "up_load.Progress",
        "a.b.C"
      ]
    ) {
      expect(() => parseTypeRef(text), text).toThrow(IrError)
    }
    expect(() => parseTypeRef(`${"Page<".repeat(200)}int${">".repeat(200)}`)).toThrow(IrError)
  })

  it("lists the declared names a type mentions", () => {
    expect(typeNames(parseTypeRef("map<UserId, Page<Order>>"))).toEqual(["UserId", "Page", "Order"])
    expect(typeNames(parseTypeRef("ref M[]"))).toEqual([])
  })

  it("substitutes type parameters", () => {
    const t = substituteType(parseTypeRef("map<string, Tree<T>[]>"), ["T"], [parseTypeRef("Order?")])
    expect(typeRefText(t)).toBe("map<string, Tree<Order?>[]>")
  })
})

describe("aliases and instances", () => {
  it("expands aliases at the top, substituting a generic alias's arguments", () => {
    const expand = (t: string) => typeRefText(expandType(ir, t))
    expect(expand("Dict<Page<int>>")).toBe("map<string, Page<int>>")
    expect(expand("First<int>")).toBe("int")
    expect(expand("First<Dict<int>>")).toBe("map<string, int>")
    expect(expand("Scores")).toBe("map<UserId, int>")
    expect(expand("Scores[]")).toBe("Scores[]")
    expect(isOptionalType(ir, "Maybe<int>")).toBe(true)
    expect(isOptionalType(ir, "First<int>")).toBe(false)
  })

  it("gives the fields of an instance, substituted", () => {
    expect(fieldsOf(ir, "OrderPage")).toEqual({
      name: "Page",
      error: false,
      fields: [field("items", "Pair<string, int>[]"), field("next", "string?")]
    })
    expect(fieldsOf(ir, "Tree<duration>")?.fields.map((f) => f.type)).toEqual(["duration", "Tree<duration>[]"])
    expect(fieldsOf(ir, "Oops")).toEqual({ name: "Oops", error: true, fields: [field("why", "string")] })
    expect(fieldsOf(ir, "Dict<int>")).toBeUndefined()
  })
})

describe("valueIssue", () => {
  const issue = (type: string, value: unknown) => valueIssue(ir, type, value as never, "v")

  it("checks a map's values, whatever its keys", () => {
    expect(issue("map<string, int>", { a: 1, b: 2 })).toBeUndefined()
    expect(issue("Scores", JSON.parse(`{"__proto__": 1, "constructor": 2}`))).toBeUndefined()
    expect(issue("map<string, int>", { a: 1, b: "2" })).toBe(`v["b"] is not an int within ±(2^53 - 1)`)
    expect(issue("map<string, int>", [1])).toBe("v is not a map")
    expect(issue("map<string, int>", null)).toBe("v is not a map")
    expect(issue("map<string, int>?", null)).toBeUndefined()
    expect(issue("Dict<duration>", { a: -1 })).toBe(`v["a"] is not a duration in whole milliseconds`)
    expect(issue("map<string, ref M>", { a: "r1.2" })).toBeUndefined()
  })

  it("checks an instance of a generic record against its substituted fields", () => {
    expect(issue("Page<int>", { items: [1, 2], next: null })).toBeUndefined()
    expect(issue("Page<int>", { items: ["x"], next: null })).toBe("v.items[0] is not an int within ±(2^53 - 1)")
    expect(issue("OrderPage", { items: [{ first: "a", second: 1 }], next: "p2" })).toBeUndefined()
    expect(issue("OrderPage", { items: [{ first: "a" }], next: "p2" })).toBe("v.items[0] has no field second")
    expect(issue("Tree<Maybe<int>>", { value: null, children: [{ value: 3, children: [] }] })).toBeUndefined()
    expect(issue("Page<int>", { items: [], next: null, extra: 1 })).toBe("v has extra, which is not a field")
    expect(issue("First<bool>", true)).toBeUndefined()
  })
})

describe("checkSaved", () => {
  it("refuses a parameter that holds refs in a map", () => {
    const hash = `sha256:${"0".repeat(64)}`
    const withRefs = {
      ...ir,
      machines: {
        M: {
          params: [field("peers", "Dict<ref M>")],
          hash,
          root: { id: "M" }
        }
      }
    } as unknown as Ir
    const saved = {
      snapshotVersion: 1,
      machine: "M",
      run: "r1",
      ir: hash,
      status: "running",
      active: ["M"],
      input: { peers: {} },
      vars: {},
      invokes: [],
      timers: [],
      nextInvoke: 1,
      nextTimer: 1,
      nextMessage: 1,
      result: null
    }
    expect(checkSaved(withRefs, "M", saved)).toMatchObject({ ok: false, reason: "ref" })
  })
})

describe("hostile IR", () => {
  it("bounds suffixes like type arguments", () => {
    expect(() => parseTypeRef(`int${"[]".repeat(100_000)}`)).toThrow(IrError)
    expect(typeRefText(parseTypeRef(`int${"[]".repeat(100)}`))).toBe(`int${"[]".repeat(100)}`)
  })

  it("leaves a bare generic alias as it is, as Rust does", () => {
    const types = { Dict: { kind: "alias", params: ["T"], type: "map<string, T>", doc: "", span } }
    expect(typeRefText(expandType({ types } as unknown as Ir, "Dict"))).toBe("Dict")
  })

  it("refuses an alias that refers to itself", () => {
    const alias = (params: Array<string>, type: string) => ({ kind: "alias", params, type, doc: "", span })
    const types = {
      Pair: { kind: "record", params: ["A", "B"], fields: [field("a", "A"), field("b", "B")], doc: "", span },
      Boom: alias(["T"], "Boom<Pair<T, T>>")
    }
    expect(aliasCycle({ types } as unknown as Ir)).toBe("Boom")
    expect(aliasCycle({ types: { Loop: alias([], "map<string, Twice>[]"), Twice: alias([], "Loop") } } as unknown as Ir)).toBeDefined()
    expect(aliasCycle({ types: { A: alias([], "int"), B: alias([], "A[]") } } as unknown as Ir)).toBeUndefined()
    const doc = { irVersion: "0.10", sources: [], types, functions: {}, actions: {}, tasks: {}, machines: {} }
    expect(() => loadIr(doc)).toThrow(/refers to itself/)
  })
})
