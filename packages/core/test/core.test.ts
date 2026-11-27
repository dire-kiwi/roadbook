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
  type Input,
  type Ir,
  IrError,
  isRunId,
  loadIr,
  restore,
  RUN_ID_PATTERN,
  type Saved,
  type Snapshot,
  start,
  step,
  type Stepped,
  toSaved,
  withChildren,
  withoutChildren
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
    expect(() => loadIr({ irVersion: "0.7" })).toThrow(/"0\.7".*0\.8/)
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
      args: input,
      saved: null
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

describe("a peer's end", () => {
  const ir = readIr("ended.ir.json")
  const kinds = (commands: ReadonlyArray<Command>) => commands.flatMap((c) => (c.kind === "trace" ? [c.record.kind] : []))

  it("is heard by a machine with an `on x.ended` for a run of the peer's machine, in any state", () => {
    expect([...heard(ir, "Order", "Inventory")]).toEqual(["ended"])
    expect([...heard(ir, "Picker", "Inventory")]).toEqual(["ended"])
    expect(heard(ir, "Clerk", "Inventory").size).toBe(0)
  })

  it("is taken through the run an address names when it is taken, with `event.status`, and never stale", () => {
    const started = start(ir, "Switcher", "r3", { a: "r1", b: "r2" }, noFns)
    const ended = (from: string): Input => ({ kind: "ended", from, status: "Failed" })
    expect(kinds(step(ir, started.snapshot, ended("r9"), noFns).commands)).toEqual(["dropped"])
    const taken = step(ir, started.snapshot, ended("r1"), noFns)
    expect(taken.commands).toContainEqual({ kind: "action", action: "note", args: { status: "Failed" } })
    expect(taken.snapshot.status).toBe("done")
    const swapped = step(ir, started.snapshot, { kind: "event", value: { _tag: "Swap" } }, noFns)
    expect(kinds(step(ir, swapped.snapshot, ended("r1"), noFns).commands)).toEqual(["dropped"])
    expect(kinds(step(ir, swapped.snapshot, ended("r2"), noFns).commands)).toContain("event")
  })

  it("is dropped, not unhandled, by a strict machine with no transition for it", () => {
    const started = start(ir, "Guard", "r2", { inventory: "r1" }, noFns)
    const dropped = step(ir, started.snapshot, { kind: "ended", from: "r1", status: "Defect" }, noFns)
    expect(dropped.snapshot.status).toBe("running")
    expect(kinds(dropped.commands)).toEqual(["dropped"])
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
      args: { orderId: "o1", items },
      saved: null
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

describe("history states", () => {
  const ir = readIr("history.ir.json")
  const regions = readIr("history-regions.ir.json")
  const send = (from: Stepped, tag: string, chart: Ir = ir) =>
    step(chart, from.snapshot, { kind: "event", value: { _tag: tag } }, noFns)
  const steps = (chart: Ir, machine: string, ...tags: Array<string>): Stepped => {
    let at = start(chart, machine, "r1", {}, noFns)
    for (const tag of tags) at = send(at, tag, chart)
    return at
  }
  /** The `entered` and `exited` records of a step, in order. */
  const moves = (stepped: Stepped) =>
    stepped.commands.flatMap((c) =>
      c.kind === "trace" && (c.record.kind === "entered" || c.record.kind === "exited")
        ? [`${c.record.kind} ${c.record.state}`]
        : []
    )
  const recorded = { "Editor.Work.Deep": ["Editor.Work.Review.Read"], "Editor.Work.Shallow": ["Editor.Work.Review"] }

  it("records when a microstep exits the parent, and only then", () => {
    const started = steps(ir, "Editor")
    expect(started.snapshot.history).toEqual({})
    // Leaving Draft for Review stays inside Work: nothing is recorded.
    const review = send(started, "Next")
    expect(review.snapshot.history).toEqual({})
    const help = send(review, "Help")
    expect(help.snapshot.history).toEqual(recorded)
    // Going back records nothing (Work is entered, not exited); leaving again replaces the records.
    const comment = send(send(send(help, "Back"), "Deeper"), "Edit")
    expect(comment.snapshot.history).toEqual(recorded)
    expect(send(comment, "Help").snapshot.history).toEqual({
      "Editor.Work.Deep": ["Editor.Work.Draft"],
      "Editor.Work.Shallow": ["Editor.Work.Draft"]
    })
  })

  it("records nothing on cancel", () => {
    const comment = steps(ir, "Editor", "Next", "Help", "BackDeep", "Deeper")
    expect(comment.snapshot.active).toEqual(["Editor", "Editor.Work", "Editor.Work.Review", "Editor.Work.Review.Comment"])
    const cancelled = step(ir, comment.snapshot, { kind: "cancel" }, noFns)
    expect(cancelled.snapshot.status).toBe("cancelled")
    expect(moves(cancelled)).toContain("exited Editor.Work")
    expect(cancelled.snapshot.history).toEqual(recorded)
  })

  it("does not record when a run finishes: what it holds is what the last microstep recorded", () => {
    const done = steps(ir, "Steps", "Pause", "Ready", "Resume", "Finish")
    expect(done.snapshot.status).toBe("done")
    expect(done.snapshot.history).toEqual({ "Steps.Flow.H": ["Steps.Flow.S2"] })
  })

  it("enters the parent's default entry with no record, from inside a compound parent", () => {
    const jumped = send(steps(ir, "Editor"), "Jump")
    // The domain is Work: it is neither exited nor entered, and nothing is recorded.
    expect(jumped.commands[1]).toEqual({
      kind: "trace",
      record: { kind: "transition", from: "Editor.Work.Draft", index: 1, to: "Editor.Work.Shallow" }
    })
    expect(moves(jumped)).toEqual(["exited Editor.Work.Draft", "entered Editor.Work.Draft"])
    expect(jumped.snapshot.active).toEqual(["Editor", "Editor.Work", "Editor.Work.Draft"])
    expect(jumped.snapshot.history).toEqual({})
  })

  it("enters, from inside, what the parent recorded when it was last exited", () => {
    const draft = steps(ir, "Editor", "Next", "Help", "Back", "Edit")
    expect(draft.snapshot.active).toEqual(["Editor", "Editor.Work", "Editor.Work.Draft"])
    const jumped = send(draft, "Jump")
    expect(moves(jumped)).toEqual([
      "exited Editor.Work.Draft",
      "entered Editor.Work.Review",
      "entered Editor.Work.Review.Read"
    ])
  })

  it("enters the parent's default entry with no record, from outside", () => {
    // A version 1 snapshot in Help has no records.
    const saved = { ...toSaved(steps(ir, "Editor", "Help").snapshot, () => 0), snapshotVersion: 1 } as Record<string, unknown>
    delete saved["history"]
    delete saved["spawned"]
    const checked = checkSaved(ir, "Editor", saved)
    if (!checked.ok) throw new Error(checked.message)
    const restored = restore(ir, checked.saved, "r2", 0, null, noFns)
    expect(restored.snapshot.history).toEqual({})
    for (const tag of ["Back", "BackDeep"]) {
      const back = send(restored, tag)
      expect(moves(back)).toEqual(["exited Editor.Help", "entered Editor.Work", "entered Editor.Work.Draft"])
    }
  })

  it("enters shallow and deep records from outside", () => {
    const help = steps(ir, "Editor", "Next", "Deeper", "Help")
    expect(help.snapshot.history).toEqual({
      "Editor.Work.Deep": ["Editor.Work.Review.Comment"],
      "Editor.Work.Shallow": ["Editor.Work.Review"]
    })
    expect(moves(send(help, "Back")).slice(1)).toEqual([
      "entered Editor.Work",
      "entered Editor.Work.Review",
      "entered Editor.Work.Review.Read"
    ])
    const deep = send(help, "BackDeep")
    expect(moves(deep).slice(1)).toEqual([
      "entered Editor.Work",
      "entered Editor.Work.Review",
      "entered Editor.Work.Review.Comment"
    ])
    // A restored state starts its work again.
    expect(deep.snapshot.invokes).toEqual([{ id: 2, state: "Editor.Work.Review.Comment", src: "fetch" }])
  })

  it("restores a parallel state's regions, deep and shallow", () => {
    const out = steps(regions, "Panels", "Left", "Right", "Away")
    expect(out.snapshot.history).toEqual({
      "Panels.Both.Deep": ["Panels.Both.L.L2", "Panels.Both.R.R2.R3"],
      "Panels.Both.Shallow": ["Panels.Both.L", "Panels.Both.R"]
    })
    const deep = send(out, "Back", regions)
    expect(deep.snapshot.active).toEqual([
      "Panels",
      "Panels.Both",
      "Panels.Both.L",
      "Panels.Both.L.L2",
      "Panels.Both.R",
      "Panels.Both.R.R2",
      "Panels.Both.R.R2.R3"
    ])
    const shallow = send(out, "BackShallow", regions)
    expect(shallow.snapshot.active).toEqual([
      "Panels",
      "Panels.Both",
      "Panels.Both.L",
      "Panels.Both.L.L1",
      "Panels.Both.R",
      "Panels.Both.R.R1"
    ])
  })

  it("fills the regions a deep record below one region does not reach with their default entry", () => {
    const away = steps(regions, "Region", "Go", "Out")
    expect(away.snapshot.history).toEqual({ "Region.P.A.H": ["Region.P.A.A2"] })
    const back = send(away, "Back", regions)
    expect(moves(back)).toEqual([
      "exited Region.Away",
      "entered Region.P",
      "entered Region.P.A",
      "entered Region.P.A.A2",
      "entered Region.P.B",
      "entered Region.P.B.B1"
    ])
  })

  it("a deep record restores a final state, which completes its parent", () => {
    const paused = steps(ir, "Steps", "Finish", "Pause")
    expect(paused.snapshot.history).toEqual({ "Steps.Flow.H": ["Steps.Flow.S2"] })
    const ready = send(paused, "Ready")
    expect(send(ready, "Resume").snapshot.status).toBe("done")
  })
})

describe("restoring history", () => {
  const ir = readIr("history.ir.json")
  const helped = (): Saved => {
    let at = start(ir, "Editor", "r1", {}, noFns)
    for (const tag of ["Next", "Help"]) {
      at = step(ir, at.snapshot, { kind: "event", value: { _tag: tag } } satisfies Input, noFns)
    }
    return toSaved(at.snapshot, () => 0)
  }
  const reason = (json: unknown) => {
    const checked = checkSaved(ir, "Editor", json)
    return checked.ok ? "ok" : checked.reason
  }
  const withHistory = (history: unknown) => ({ ...helped(), history })

  it("saves version 4 with history, and restores it", () => {
    const saved = helped()
    expect(saved.snapshotVersion).toBe(4)
    expect(saved.history).toEqual({
      "Editor.Work.Deep": ["Editor.Work.Review.Read"],
      "Editor.Work.Shallow": ["Editor.Work.Review"]
    })
    expect(reason(saved)).toBe("ok")
  })

  it("refuses a key that is not a history state's id", () => {
    expect(reason(withHistory({ "Editor.Work.Draft": ["Editor.Work.Review"] }))).toBe("invalid")
    expect(reason(withHistory({ "Editor.Nope": ["Editor.Work.Review"] }))).toBe("invalid")
    expect(reason(withHistory({ "not an id": ["Editor.Work.Review"] }))).toBe("invalid")
  })

  it("refuses a shallow record its history state could not have made", () => {
    const shallow = (record: unknown) => reason(withHistory({ "Editor.Work.Shallow": record }))
    expect(shallow(["Editor.Work.Draft"])).toBe("ok")
    expect(shallow(["Editor.Work.Review.Read"])).toBe("invalid")
    expect(shallow(["Editor.Help"])).toBe("invalid")
    expect(shallow(["Editor.Work.Draft", "Editor.Work.Review"])).toBe("invalid")
    expect(shallow(["Editor.Work.Deep"])).toBe("invalid")
    expect(shallow([])).toBe("invalid")
    expect(shallow("Editor.Work.Draft")).toBe("invalid")
  })

  it("refuses a deep record that is not the leaves of a configuration below the parent", () => {
    const deep = (record: unknown) => reason(withHistory({ "Editor.Work.Deep": record }))
    expect(deep(["Editor.Work.Draft"])).toBe("ok")
    expect(deep(["Editor.Work.Review.Comment"])).toBe("ok")
    expect(deep(["Editor.Work.Review"])).toBe("invalid")
    expect(deep(["Editor.Work.Draft", "Editor.Work.Review.Read"])).toBe("invalid")
    expect(deep(["Editor.Help"])).toBe("invalid")
    expect(deep(["Editor.Work.Review.Read", "Editor.Work.Review.Read"])).toBe("invalid")
  })

  it("checks a parallel parent's records: every region, in document order", () => {
    const regions = readIr("history-regions.ir.json")
    const out = start(regions, "Panels", "r1", {}, noFns)
    const away = step(regions, out.snapshot, { kind: "event", value: { _tag: "Away" } }, noFns)
    const saved = toSaved(away.snapshot, () => 0)
    const check = (history: unknown) => {
      const checked = checkSaved(regions, "Panels", { ...saved, history })
      return checked.ok ? "ok" : checked.reason
    }
    expect(check(saved.history)).toBe("ok")
    expect(check({ "Panels.Both.Shallow": ["Panels.Both.L"] })).toBe("invalid")
    expect(check({ "Panels.Both.Shallow": ["Panels.Both.R", "Panels.Both.L"] })).toBe("invalid")
    expect(check({ "Panels.Both.Deep": ["Panels.Both.L.L2"] })).toBe("invalid")
    expect(check({ "Panels.Both.Deep": ["Panels.Both.R.R1", "Panels.Both.L.L1"] })).toBe("invalid")
    expect(check({ "Panels.Both.Deep": ["Panels.Both.L.L2", "Panels.Both.R.R2.R3"] })).toBe("ok")
    // A history state is never active.
    expect(checkSaved(regions, "Panels", { ...saved, active: ["Panels", "Panels.Out", "Panels.Both.Deep"] }).ok).toBe(false)
  })

  it("reads version 1 without history, and refuses version 1 with it and version 2 without it", () => {
    const { history, spawned, ...v2 } = helped()
    expect(history).toBeDefined()
    expect(spawned).toEqual([])
    expect(reason({ ...v2, snapshotVersion: 1 })).toBe("ok")
    expect(reason({ ...v2, snapshotVersion: 1, history })).toBe("invalid")
    expect(reason({ ...v2, snapshotVersion: 2 })).toBe("invalid")
    expect(reason({ ...v2, snapshotVersion: 2, history })).toBe("ok")
    expect(reason({ ...v2, snapshotVersion: 2, history, spawned })).toBe("invalid")
    expect(reason({ ...v2, snapshotVersion: 3, history })).toBe("invalid")
    expect(reason({ ...v2, snapshotVersion: 3, history, spawned })).toBe("ok")
    expect(reason({ ...v2, snapshotVersion: 4, history })).toBe("invalid")
    expect(reason({ ...v2, snapshotVersion: 4, history, spawned })).toBe("ok")
    expect(reason({ ...v2, snapshotVersion: 5, history, spawned })).toBe("invalid")
  })

  it("restores a version 1 snapshot with no records, traces it as given and saves it as version 4", () => {
    const { history: _, spawned: __, ...v2 } = helped()
    const v1 = { ...v2, snapshotVersion: 1 }
    const checked = checkSaved(ir, "Editor", v1)
    if (!checked.ok) throw new Error(checked.message)
    const restored = restore(ir, checked.saved, "r2", 0, null, noFns)
    expect(restored.commands[0]).toEqual({
      kind: "trace",
      record: { kind: "restored", machine: "Editor", ir: v1.ir, migrated: null, snapshot: v1 }
    })
    expect(restored.snapshot.history).toEqual({})
    const resaved = toSaved(restored.snapshot, () => 0)
    expect(resaved.snapshotVersion).toBe(4)
    expect(resaved.history).toEqual({})
    expect(resaved.spawned).toEqual([])
    expect(reason(resaved)).toBe("ok")
    // A run restored from version 2 keeps its records.
    const fromV2 = checkSaved(ir, "Editor", helped())
    if (!fromV2.ok) throw new Error(fromV2.message)
    const kept: Snapshot = restore(ir, fromV2.saved, "r3", 0, null, noFns).snapshot
    expect(kept.history).toEqual(helped().history)
  })
})

describe("child runs in a saved snapshot (version 4)", () => {
  const ir = readIr("kept.ir.json")
  const reasonOf = (json: unknown, migrate?: Parameters<typeof checkSaved>[3]) => {
    const checked = checkSaved(ir, "Desk", json, migrate)
    return checked.ok ? "ok" : checked.reason
  }
  /** A desk saved with its tally's own snapshot nested in its invoke. */
  const tree = () => {
    const desk = start(ir, "Desk", "r1", { start: 1 }, noFns)
    const tally = start(ir, "Tally", "r1.1", { start: 1 }, noFns)
    const child = toSaved(tally.snapshot, () => 5000)
    return withChildren(toSaved(desk.snapshot, () => 0), (id) => (id === 1 ? child : undefined))
  }
  type Mutable = { -readonly [K in keyof Saved]: any }
  const changed = (f: (top: Mutable, child: Mutable) => void) => {
    const s = JSON.parse(JSON.stringify(tree()))
    f(s, s.invokes[0].child)
    return s
  }

  it("nests the child's snapshot in its invoke, and restores it through invoke.start", () => {
    const saved = tree()
    expect(saved.snapshotVersion).toBe(4)
    expect(saved.invokes[0]!.child).toMatchObject({ snapshotVersion: 4, machine: "Tally", run: "r1.1" })
    const checked = checkSaved(ir, "Desk", saved)
    if (!checked.ok) throw new Error(checked.message)
    const restored = restore(ir, checked.saved, "r2", 1000, null, noFns)
    const record = restored.commands[0]
    expect(record).toMatchObject({ kind: "trace", record: { kind: "restored", snapshot: withoutChildren(saved) } })
    expect(JSON.stringify(record)).not.toContain("child")
    const command = restored.commands.find((c) => c.kind === "invoke.start")
    expect(command).toMatchObject({ kind: "invoke.start", invoke: 1, saved: saved.invokes[0]!.child })
    expect(restored.snapshot.invokes).toEqual([{ id: 1, state: "Desk.Open", src: "Tally" }])
    // The child restores from it as its own run, its timer for the time left.
    const child = restore(ir, (command as { saved: Saved }).saved, "r2.1", 1000, null, noFns)
    expect(child.commands.map((c) => c.kind === "trace" ? c.record.kind : c.kind)).toEqual([
      "restored",
      "timer.started",
      "timer.start"
    ])
    expect(child.commands.at(-1)).toEqual({ kind: "timer.start", timer: 1, delay: 4000 })
    // start and step never restore a child.
    expect(start(ir, "Desk", "r3", { start: 1 }, noFns).commands.find((c) => c.kind === "invoke.start"))
      .toMatchObject({ saved: null })
  })

  it("strips nested snapshots only where there are some", () => {
    const plain = toSaved(start(ir, "Desk", "r1", { start: 1 }, noFns).snapshot, () => 0)
    expect(withoutChildren(plain)).toBe(plain)
    expect(withoutChildren(tree())).toEqual(plain)
  })

  it("checks the whole tree: shape, hashes, then the fit of each nested snapshot", () => {
    expect(reasonOf(tree())).toBe("ok")
    expect(reasonOf(changed((_, c) => (c.ir = ir.machines["Office"]!.hash)))).toBe("ir")
    expect(reasonOf(changed((_, c) => (c.machine = "Desk")))).toBe("invalid")
    expect(reasonOf(changed((_, c) => (c.run = "r1.2")))).toBe("invalid")
    expect(reasonOf(changed((_, c) => {
      c.status = "done"
      c.active = []
      c.timers = []
      c.result = { output: 1 }
    }))).toBe("invalid")
    expect(reasonOf(changed((_, c) => (c.calls = [{ from: "r9", invoke: 1, call: "ask" }])))).toBe("invalid")
    expect(reasonOf(changed((t) => (t.snapshotVersion = 3)))).toBe("invalid")
    expect(reasonOf(changed((_, c) => (c.snapshotVersion = 3)))).toBe("invalid")
    expect(reasonOf(changed((_, c) => (c.vars = { n: "one" })))).toBe("invalid")
    // A schema refusal comes before a hash one anywhere in the tree.
    expect(reasonOf(changed((t, c) => {
      t.ir = ir.machines["Office"]!.hash
      c.snapshotVersion = 3
    }))).toBe("invalid")
  })

  it("refuses a child on a task's invoke", () => {
    const errand = start(ir, "Errand", "r1", {}, noFns)
    const fetcher = start(ir, "Fetcher", "r1.1", { n: 3 }, noFns)
    const tally = toSaved(start(ir, "Tally", "r1.1.1", { start: 1 }, noFns).snapshot, () => 0)
    const nested = withChildren(toSaved(fetcher.snapshot, () => 0), () => tally)
    const saved = withChildren(toSaved(errand.snapshot, () => 0), () => nested)
    expect(checkSaved(ir, "Errand", withChildren(toSaved(errand.snapshot, () => 0), () => toSaved(fetcher.snapshot, () => 0))).ok)
      .toBe(true)
    expect(checkSaved(ir, "Errand", saved)).toMatchObject({ ok: false, reason: "invalid" })
  })

  it("migrates the whole tree once when a nested hash differs", () => {
    const stale = changed((t, c) => {
      t.ir = `sha256:${"0".repeat(64)}`
      c.ir = `sha256:${"1".repeat(64)}`
    })
    let calls = 0
    const checked = checkSaved(ir, "Desk", stale, () => {
      calls += 1
      return { value: tree() as never }
    })
    expect(calls).toBe(1)
    expect(checked).toMatchObject({ ok: true, migrated: `sha256:${"0".repeat(64)}` })
    // A migration that leaves a nested hash stale is refused as `ir`.
    const half = checkSaved(ir, "Desk", stale, () => ({ value: changed((_, c) => (c.ir = stale.ir)) as never }))
    expect(half).toMatchObject({ ok: false, reason: "ir" })
  })
})
