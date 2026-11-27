/**
 * The codec on maps and generic types: a map is a plain object with a member per key (any key,
 * `__proto__` too), a `ref` in a map's values is a handle like one in a list, and an instance of
 * a generic record or alias is its declaration with the arguments substituted.
 */
import { assert, describe, it } from "@effect/vitest"
import * as Core from "@dire-kiwi/roadbook-core"
import { Duration, Effect, Queue } from "effect"
import { Codec, CodecError } from "../src/internal/codec.ts"
import { registerHandle, type Wake } from "../src/internal/port.ts"

const span = { file: 0, line: 1, col: 1, endLine: 1, endCol: 1 }
const field = (name: string, type: string) => ({ name, type, sensitive: false, doc: "" })
const hash = `sha256:${"1".repeat(64)}`

const ir = {
  irVersion: "0.8",
  sources: [],
  types: {
    UserId: { kind: "alias", params: [], type: "string", doc: "", span },
    Box: { kind: "record", params: ["T"], fields: [field("value", "T"), field("count", "int")], doc: "", span },
    Pair: { kind: "record", params: ["A", "B"], fields: [field("first", "A"), field("second", "B")], doc: "", span },
    Dict: { kind: "alias", params: ["T"], type: "map<string, T>", doc: "", span },
    First: { kind: "alias", params: ["T"], type: "T", doc: "", span },
    Scores: { kind: "alias", params: [], type: "map<UserId, int>", doc: "", span }
  },
  functions: {},
  actions: {},
  tasks: {},
  machines: { Peer: { hash } }
} as unknown as Core.Ir

const codec = () => new Codec(ir, {}, new Map())

/** A handle to a run `id` of `Peer`, as the driver registers one. */
const handle = (id: string) =>
  registerHandle({ id }, {
    id,
    machine: "Peer",
    hash,
    queue: Effect.runSync(Queue.unbounded<Core.Input | Wake>()),
    open: true,
    subscribers: new Map(),
    closedAs: undefined
  })

/** An object with an own member named `__proto__`, as JSON.parse makes one. */
const withProto = (value: unknown, rest: Record<string, unknown> = {}) => {
  const object: Record<string, unknown> = {}
  Object.defineProperty(object, "__proto__", { value, enumerable: true, writable: true, configurable: true })
  return Object.assign(object, rest)
}

describe("maps", () => {
  it("encode a plain object member by member, values in their JSON form", () => {
    const json = codec().encode("map<string, duration>", { a: Duration.seconds(1), b: Duration.millis(5) }, "v")
    assert.deepStrictEqual(json, { a: 1000, b: 5 })
    assert.deepStrictEqual(codec().encode("Scores", Object.create(null), "v"), {})
  })

  it("keep `__proto__` and `constructor` as members, both ways", () => {
    const value = withProto(Duration.seconds(2), { constructor: Duration.seconds(3) })
    const json = codec().encode("Dict<duration>", value, "v")
    assert.strictEqual(Object.getPrototypeOf(json), Object.prototype)
    assert.isTrue(Object.hasOwn(json as object, "__proto__"))
    assert.strictEqual(Core.canonicalJson(json), `{"__proto__":2000,"constructor":3000}`)
    assert.strictEqual(JSON.stringify(json), `{"__proto__":2000,"constructor":3000}`)
    assert.strictEqual(Core.canonicalJson(Core.copyJson(json)), `{"__proto__":2000,"constructor":3000}`)

    const host = codec().decode("Dict<duration>", JSON.parse(`{"__proto__": 2000, "constructor": 3000}`)) as Record<
      string,
      unknown
    >
    assert.strictEqual(Object.getPrototypeOf(host), Object.prototype)
    assert.deepStrictEqual(Object.keys(host), ["__proto__", "constructor"])
    assert.isTrue(Duration.equals(host["__proto__"] as Duration.Duration, Duration.seconds(2)))
    assert.isTrue(Duration.equals(host["constructor"] as Duration.Duration, Duration.seconds(3)))
  })

  it("refuse what is not a plain object", () => {
    class Bag {
      a = 1
    }
    for (const value of [[1], null, undefined, new Bag(), new Map([["a", 1]]), "a", 1]) {
      assert.throws(() => codec().encode("map<string, int>", value, "v"), CodecError)
    }
    assert.strictEqual(codec().encode("map<string, int>?", null, "v"), null)
    assert.throws(() => codec().encode("map<string, int>", { a: "1" }, "v"), /v\["a"\] is not an int/)
  })

  it("encode a `ref` among the values to its run id and decode it to the handle", () => {
    const c = codec()
    const a = handle("r1")
    const b = handle("r2")
    const json = c.encode("map<string, ref Peer>", withProto(a, { b }), "v")
    assert.strictEqual(Core.canonicalJson(json), `{"__proto__":"r1","b":"r2"}`)
    assert.deepStrictEqual([...c.refs!.keys()], ["r1", "r2"])
    const back = c.decode("Dict<ref Peer[]>", { x: ["r2", "r1"] }) as Record<string, ReadonlyArray<object>>
    assert.strictEqual(back["x"]![0], b)
    assert.strictEqual(back["x"]![1], a)
    assert.throws(() => c.decode("map<string, ref Peer>", { x: "r9" }), /no handle to the run "r9"/)
  })
})

describe("generic instances", () => {
  it("substitute the arguments in a record's fields, durations made Duration", () => {
    const value = { value: Duration.minutes(1), count: 2 }
    const json = codec().encode("Box<duration>", value, "v")
    assert.deepStrictEqual(json, { value: 60000, count: 2 })
    const host = codec().decode("Box<duration>", json) as { value: Duration.Duration; count: number }
    assert.isTrue(Duration.equals(host.value, Duration.minutes(1)))
    assert.strictEqual(host.count, 2)
    assert.throws(() => codec().encode("Box<int>", value, "v"), /v\.value is not an int/)
  })

  it("nest through records, lists, maps and generic aliases", () => {
    const type = "Pair<First<int>, Dict<Box<duration>[]>>"
    const value = { first: 7, second: { a: [{ value: Duration.seconds(1), count: 0 }] } }
    const json = codec().encode(type, value, "v")
    assert.deepStrictEqual(json, { first: 7, second: { a: [{ value: 1000, count: 0 }] } })
    const host = codec().decode(type, json) as { first: number; second: { a: Array<{ value: Duration.Duration }> } }
    assert.strictEqual(host.first, 7)
    assert.isTrue(Duration.equals(host.second.a[0]!.value, Duration.seconds(1)))
    assert.throws(() => codec().encode(type, { first: 1.5, second: {} }, "v"), /v\.first is not an int/)
  })
})
