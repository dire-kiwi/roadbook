import * as fs from "node:fs"
import * as path from "node:path"
import { describe, expect, it } from "vitest"
import {
  canonicalJson,
  checkSaved,
  type Command,
  compareCodePoints,
  type Fns,
  getMachine,
  heard,
  IrError,
  isRunId,
  loadIr,
  RUN_ID_PATTERN,
  start,
  step,
  toSaved
} from "../src/index.ts"

const charts = path.resolve(import.meta.dirname, "../../../spec/conformance/charts")
const readIr = (file: string) => loadIr(JSON.parse(fs.readFileSync(path.join(charts, file), "utf8")))
const noFns: Fns = (name) => ({ defect: { kind: "host", message: `unexpected call of ${name}` } })

describe("canonicalJson", () => {
  it("sorts keys by UTF-16 code units and writes numbers as JavaScript does", () => {
    expect(canonicalJson({ b: 1, a: [2.0, -0, 1e21, "é"], "€": null, "😀": true })).toBe(
      "{\"a\":[2,0,1e+21,\"é\"],\"b\":1,\"€\":null,\"😀\":true}"
    )
  })
  it("refuses non-finite numbers", () => {
    expect(() => canonicalJson(Number.NaN)).toThrow()
  })
})

describe("run ids", () => {
  it("are the snapshot schema's", () => {
    const schema = JSON.parse(fs.readFileSync(path.join(charts, "../../snapshot.schema.json"), "utf8"))
    expect(RUN_ID_PATTERN).toBe(schema.$defs.runId.pattern)
    for (const id of ["r1", "r1.4.10", "order-7_x"]) expect(isRunId(id)).toBe(true)
    for (const id of ["", "r1.0", "r1.", "a b", "order:42", 7]) expect(isRunId(id)).toBe(false)
  })
})

describe("compareCodePoints", () => {
  it("orders by Unicode scalar value, not UTF-16 code unit", () => {
    // U+FF61 is a single code unit above the surrogates; U+1F600 is a surrogate pair.
    expect("｡" < "😀").toBe(false)
    expect(compareCodePoints("｡", "😀")).toBeLessThan(0)
    expect(compareCodePoints("ab", "abc")).toBeLessThan(0)
    expect(compareCodePoints("b", "abc")).toBeGreaterThan(0)
    expect(compareCodePoints("x", "x")).toBe(0)
  })
})

describe("loadIr", () => {
  it("refuses another IR version, naming both", () => {
    expect(() => loadIr({ irVersion: "0.1" })).toThrow(/"0\.1".*0\.5/)
    expect(() => loadIr([])).toThrow(IrError)
  })
  it("refuses a machine the IR does not have", () => {
    expect(() => getMachine(readIr("nest.ir.json"), "Nope")).toThrow(/no machine "Nope"/)
  })
})

describe("messaging", () => {
  it("sends through a ref parameter and a ref variable, in order", () => {
    const ir = readIr("pair.ir.json")
    const started = start(ir, "Sender", "r2", { peer: "r1" }, noFns)
    expect(started.snapshot.run).toBe("r2")
    expect(started.snapshot.vars).toEqual({ target: "r1" })
    const go = step(ir, started.snapshot, { kind: "event", value: { _tag: "Go" } }, noFns)
    expect(go.commands.filter((c) => c.kind === "send")).toEqual([
      { kind: "send", to: "r1", invoke: null, message: 1, value: { _tag: "First" } },
      { kind: "send", to: "r1", invoke: null, message: 2, value: { _tag: "Second" } }
    ])
    expect(go.snapshot.nextMessage).toBe(3)
  })

  it("takes an emitted event only from the child its address names, and a stale one from none", () => {
    const ir = readIr("upload.ir.json")
    const started = start(ir, "Publish", "r1", { size: 200 }, noFns)
    expect(started.commands.at(-1)).toMatchObject({ kind: "invoke.start", invoke: 1, as: "upload" })
    const progress = { _tag: "Progress", offset: 100 }
    const taken = step(ir, started.snapshot, { kind: "emitted", from: "r1.1", invoke: 1, message: 1, value: progress }, noFns)
    expect(taken.snapshot.vars).toMatchObject({ shown: 100 })
    const stale = step(ir, started.snapshot, { kind: "emitted", from: "r1.7", invoke: 7, message: 1, value: progress }, noFns)
    expect(stale.commands).toEqual([
      {
        kind: "trace",
        record: { kind: "stale", event: { kind: "emitted", from: "r1.7", invoke: 7, message: 1, value: progress } }
      }
    ])
    const peer = step(ir, started.snapshot, { kind: "emitted", from: "r2", invoke: null, message: 1, value: progress }, noFns)
    expect(peer.commands.map((c) => c.kind === "trace" && c.record.kind)).toEqual(["dropped"])
  })
})

describe("start and step", () => {
  const ir = readIr("shop/checkout.ir.json")
  const input = {
    order: { id: "o1", items: [{ sku: "apple", qty: 2 }], totalCents: 4200 },
    user: { id: "u1", role: "Customer" }
  }

  it("asks the driver to start the invoke, then finishes on its outcome", () => {
    const started = start(ir, "Checkout", "r1", input, noFns)
    expect(started.snapshot.active).toEqual(["Checkout", "Checkout.Charging"])
    expect(started.snapshot.invokes).toEqual([{ id: 1, state: "Checkout.Charging", src: "chargeCard" }])
    expect(started.commands.at(-1)).toEqual({
      kind: "invoke.start",
      invoke: 1,
      state: "Checkout.Charging",
      src: "chargeCard",
      srcKind: "task",
      to: null,
      as: null,
      args: input
    })
    const receipt = { chargeId: "c1", amountCents: 4200 }
    const done = step(ir, started.snapshot, { kind: "invoke.done", invoke: 1, value: receipt }, noFns)
    expect(done.snapshot.status).toBe("done")
    expect(done.snapshot.result).toEqual({ output: receipt })
    expect(done.snapshot.active).toEqual([])
    expect(started.snapshot.status).toBe("running")
  })

  it("is atomic: a host defect in a function discards the step's commands", () => {
    const started = start(ir, "Checkout", "r1", input, noFns)
    const failed = step(ir, started.snapshot, { kind: "invoke.fail", invoke: 1, error: { _tag: "GatewayUnavailable" } }, noFns)
    expect(failed.snapshot.status).toBe("defect")
    expect(failed.snapshot.result).toEqual({ defect: { kind: "host", message: "unexpected call of retryDelay" } })
    expect(failed.commands).toEqual([
      { kind: "invoke.cancel", invoke: 1 },
      { kind: "trace", record: { kind: "defect", defect: { kind: "host", message: "unexpected call of retryDelay" } } }
    ])
  })

  it("traces a stale outcome and changes nothing else", () => {
    const started = start(ir, "Checkout", "r1", input, noFns)
    const stale = step(ir, started.snapshot, { kind: "invoke.done", invoke: 9, value: null }, noFns)
    expect(stale.commands).toEqual([
      { kind: "trace", record: { kind: "stale", event: { kind: "invoke.done", invoke: 9, value: null } } }
    ])
    expect(stale.snapshot).toEqual(started.snapshot)
  })
})

describe("heard", () => {
  it("is the emitted events of a sender's machine that a machine has an `on` for", () => {
    const ir = readIr("orders.ir.json")
    expect([...heard(ir, "Order", "Inventory")].sort()).toEqual(["Rejected", "Reserved"])
    // A Supplier holds a `ref Inventory` only to send to it.
    expect(heard(ir, "Supplier", "Inventory").size).toBe(0)
    expect(heard(ir, "Inventory", "Order").size).toBe(0)
  })
})

describe("calls", () => {
  const ir = readIr("calls.ir.json")
  const items = [{ sku: "s1", qty: 2 }]
  const reserve = { kind: "call", from: "r2", invoke: 1, call: "reserve", args: { orderId: "o1", items } } as const
  const records = (commands: ReadonlyArray<Command>) => commands.flatMap((c) => (c.kind === "trace" ? [c.record.kind] : []))

  it("makes a call as an invoke: the peer read as `to`, the call as `src`, Unanswered declared", () => {
    const started = start(ir, "Order", "r2", { id: "o1", items, inventory: "r1" }, noFns)
    expect(started.snapshot.invokes).toEqual([{ id: 1, state: "Order.Reserving", src: "inventory.reserve" }])
    expect(started.commands).toContainEqual({
      kind: "invoke.start",
      invoke: 1,
      state: "Order.Reserving",
      src: "reserve",
      srcKind: "call",
      to: "r1",
      as: null,
      args: { orderId: "o1", items }
    })
    const unanswered = step(ir, started.snapshot, { kind: "invoke.fail", invoke: 1, error: { _tag: "Unanswered" } }, noFns)
    expect(unanswered.snapshot.status).toBe("failed")
    expect(unanswered.snapshot.result).toEqual({ error: { _tag: "OutOfStock", reason: "unanswered" } })
  })

  it("keeps a taken call pending until a reply answers it", () => {
    const started = start(ir, "Inventory", "r1", { warehouse: "w1" }, noFns)
    const taken = step(ir, started.snapshot, reserve, noFns)
    expect(taken.snapshot.calls).toEqual([{ from: "r2", invoke: 1, call: "reserve" }])
    expect(taken.snapshot.vars["pending"]).toEqual({ from: "r2", invoke: 1 })
    const done = step(ir, taken.snapshot, { kind: "invoke.done", invoke: 1, value: 2 }, noFns)
    expect(done.snapshot.calls).toEqual([])
    const reply = { to: "r2", invoke: 1, call: "reserve", outcome: "done", value: { orderId: "o1", count: 2 } } as const
    expect(done.commands).toContainEqual({ kind: "trace", record: { kind: "replied", ...reply } })
    expect(done.commands).toContainEqual({ kind: "reply", ...reply })
  })

  it("answers a call that enables no transition at once, with Unanswered", () => {
    const started = start(ir, "Inventory", "r1", { warehouse: "w1" }, noFns)
    const paused = step(ir, started.snapshot, { kind: "event", value: { _tag: "Pause" } }, noFns)
    const dropped = step(ir, paused.snapshot, reserve, noFns)
    const reply = { to: "r2", invoke: 1, call: "reserve", outcome: "fail", value: { _tag: "Unanswered" } } as const
    expect(dropped.commands).toEqual([
      { kind: "trace", record: { kind: "dropped", event: reserve } },
      { kind: "trace", record: { kind: "replied", ...reply } },
      { kind: "reply", ...reply }
    ])
    expect(dropped.snapshot.calls).toEqual([])
  })

  it("makes a second reply to one call the runtime error answered, discarding the step", () => {
    const started = start(ir, "Inventory", "r1", { warehouse: "w1" }, noFns)
    const taken = step(ir, started.snapshot, reserve, noFns)
    const twice = step(ir, taken.snapshot, { kind: "event", value: { _tag: "Twice" } }, noFns)
    expect(twice.snapshot.result).toEqual({ defect: { kind: "answered", call: "reserve" } })
    expect(records(twice.commands)).toEqual(["defect"])
    expect(twice.commands.some((c) => c.kind === "reply")).toBe(false)
  })

  it("saves pending calls only when there are some, and refuses to restore them (calls)", () => {
    const started = start(ir, "Inventory", "r1", { warehouse: "w1" }, noFns)
    expect(Object.hasOwn(toSaved(started.snapshot, () => 0), "calls")).toBe(false)
    const taken = step(ir, started.snapshot, reserve, noFns)
    const saved = toSaved(taken.snapshot, () => 0)
    expect(saved.calls).toEqual([{ from: "r2", invoke: 1, call: "reserve" }])
    expect(checkSaved(ir, "Inventory", saved)).toMatchObject({ ok: false, reason: "calls" })
    // `calls` is written only when not empty: an empty list is not a saved snapshot.
    const answered = step(ir, taken.snapshot, { kind: "invoke.done", invoke: 1, value: 2 }, noFns)
    const restorable = toSaved(answered.snapshot, () => 0)
    expect(checkSaved(ir, "Inventory", restorable)).toMatchObject({ ok: true })
    expect(checkSaved(ir, "Inventory", { ...restorable, calls: [] })).toMatchObject({ ok: false, reason: "invalid" })
    // A pending-call value in `vars` must be one.
    const bad = { ...restorable, vars: { ...restorable.vars, pending: { from: "r2" } } }
    expect(checkSaved(ir, "Inventory", bad)).toMatchObject({ ok: false, reason: "invalid" })
  })
})
