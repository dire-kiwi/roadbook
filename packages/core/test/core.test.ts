import * as fs from "node:fs"
import * as path from "node:path"
import { describe, expect, it } from "vitest"
import {
  canonicalJson,
  compareCodePoints,
  type Fns,
  getMachine,
  IrError,
  isRunId,
  loadIr,
  RUN_ID_PATTERN,
  start,
  step
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
    expect(() => loadIr({ irVersion: "0.1" })).toThrow(/"0\.1".*0\.3/)
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
