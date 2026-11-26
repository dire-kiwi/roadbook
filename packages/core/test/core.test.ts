import * as fs from "node:fs"
import * as path from "node:path"
import { describe, expect, it } from "vitest"
import {
  canonicalJson,
  checkSaved,
  compareCodePoints,
  type Fns,
  getMachine,
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
    expect(() => loadIr({ irVersion: "0.5" })).toThrow(/"0\.5".*0\.6/)
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

  it("saves version 3 with history, and restores it", () => {
    const saved = helped()
    expect(saved.snapshotVersion).toBe(3)
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
    expect(reason({ ...v2, snapshotVersion: 4, history, spawned })).toBe("invalid")
  })

  it("restores a version 1 snapshot with no records, traces it as given and saves it as version 3", () => {
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
    expect(resaved.snapshotVersion).toBe(3)
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
