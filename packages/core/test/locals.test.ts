/**
 * Data local to a state (execution.md, "Data local to a state"): a state's parameters and
 * variables, bound on entry with `set` records, dropped after the microstep's effects and
 * arguments, kept by history records, saved and redacted. The IR here is written by hand.
 */
import { describe, expect, it } from "vitest"
import {
  checkSaved,
  type Command,
  type CoreRecord,
  type Effect,
  type Expr,
  type Field,
  type Fns,
  type Input,
  type Ir,
  loadIr,
  restore,
  type Saved,
  type Snapshot,
  type State,
  start,
  step,
  toSaved,
  type Transition,
  type Trigger,
  type Variable
} from "../src/index.ts"

const SPAN = { file: 0, line: 1, col: 1, endLine: 1, endCol: 1 }
const HASH = `sha256:${"0".repeat(64)}`
const noFns: Fns = (name) => ({ defect: { kind: "host", message: `unexpected call of ${name}` } })

const lit = (value: number | string, type = "int"): Expr => ({ kind: "lit", value, type, text: String(value) })
const ref = (name: string, type = "int", state: string | null = null, ...fields: Array<string>): Expr => ({
  kind: "ref",
  path: [name, ...fields],
  state,
  type,
  text: [name, ...fields].join(".")
})
const op = (o: "+" | "*", a: Expr, b: Expr): Expr => ({
  kind: "op",
  op: o,
  args: [a, b],
  type: "int",
  text: `${a.text} ${o} ${b.text}`
})
const set = (v: string, value: Expr, state: string | null = null): Effect => ({ kind: "set", var: v, state, value })
const param = (name: string, type = "int", sensitive = false): Field => ({ name, type, sensitive, doc: "" })
const variable = (name: string, type: string, init: Expr | null, sensitive = false): Variable => ({
  name,
  type,
  sensitive,
  init,
  doc: "",
  span: SPAN
})
const on = (event: string): Trigger => ({ kind: "event", event })
const to = (
  trigger: Trigger,
  targets: Array<string>,
  more: Partial<Pick<Transition, "args" | "effects" | "internal" | "guard">> = {}
): Transition => ({
  trigger,
  group: 0,
  guard: null,
  targets,
  internal: false,
  args: [],
  effects: [],
  span: SPAN,
  ...more
})
const state = (
  id: string,
  kind: State["kind"],
  more: Partial<Pick<State, "params" | "vars" | "initial" | "states" | "entry" | "exit" | "transitions" | "output">> = {}
): State => ({
  id,
  name: id.slice(id.lastIndexOf(".") + 1),
  kind,
  doc: "",
  tags: [],
  meta: {},
  span: SPAN,
  params: [],
  vars: [],
  initial: null,
  default: [],
  states: [],
  entry: [],
  exit: [],
  invokes: [],
  transitions: [],
  output: null,
  fail: null,
  ...more
})
const program = (root: State, events: Array<string>, more: { returns?: string; vars?: Array<Variable> } = {}): Ir =>
  loadIr({
    irVersion: "0.14",
    sources: [{ path: "m.road", package: null }],
    types: {},
    functions: {},
    actions: {},
    tasks: {},
    machines: {
      M: {
        doc: "",
        span: SPAN,
        strict: false,
        params: [],
        returns: more.returns ?? null,
        errors: [],
        vars: more.vars ?? [],
        events: Object.fromEntries(events.map((e) => [e, { fields: e === "Go" ? [param("n")] : [], doc: "", span: SPAN }])),
        internalEvents: {},
        emits: {},
        calls: {},
        children: {},
        root,
        hash: HASH
      }
    }
  })

const records = (commands: ReadonlyArray<Command>): Array<CoreRecord> =>
  commands.flatMap((c) => (c.kind === "trace" ? [c.record] : []))
const ev = (tag: string, fields: Record<string, number> = {}): Input => ({ kind: "event", value: { _tag: tag, ...fields } })

// machine M() -> int {
//   var log: int = 0
//   initial Idle
//   state Idle { on Go -> Counting(log)  set log = event.n + 1 }
//   state Counting(n: int) {
//     var tries: int = n + 1
//     var note: string?
//     var later: int
//     on Again -> Counting(tries * 10)  set tries = 99   // a lost assignment, then re-entry
//     on Bump set tries = tries + 1
//     on Peek set log = later
//     on Swap -> .B(tries)
//     on Boom -> Counting(n * 9007199254740991)
//     on Leave -> Done(n + tries)
//     initial A
//     state A {}
//     state B(x: int) {}
//   }
//   final state Done(total: int) { output total }
// }
const C = "M.Counting"
const counter = program(
  state("M", "compound", {
    initial: "M.Idle",
    states: [
      state("M.Idle", "atomic", {
        transitions: [to(on("Go"), [C], {
          args: [{ state: C, values: [ref("log")] }],
          effects: [set("log", op("+", ref("event", "int", null, "n"), lit(1)))]
        })]
      }),
      state(C, "compound", {
        params: [param("n")],
        vars: [variable("tries", "int", op("+", ref("n", "int", C), lit(1))), variable("note", "string?", null), variable("later", "int", null)],
        initial: `${C}.A`,
        transitions: [
          to(on("Again"), [C], {
            args: [{ state: C, values: [op("*", ref("tries", "int", C), lit(10))] }],
            effects: [set("tries", lit(99), C)]
          }),
          to(on("Bump"), [], { effects: [set("tries", op("+", ref("tries", "int", C), lit(1)), C)] }),
          to(on("Peek"), [], { effects: [set("log", ref("later", "int", C))] }),
          to(on("Swap"), [`${C}.B`], { internal: true, args: [{ state: `${C}.B`, values: [ref("tries", "int", C)] }] }),
          to(on("Boom"), [C], { args: [{ state: C, values: [op("*", ref("n", "int", C), lit(9007199254740991))] }] }),
          to(on("Leave"), ["M.Done"], {
            args: [{ state: "M.Done", values: [op("+", ref("n", "int", C), ref("tries", "int", C))] }]
          })
        ],
        states: [state(`${C}.A`, "atomic"), state(`${C}.B`, "atomic", { params: [param("x")] })]
      }),
      state("M.Done", "final", { params: [param("total")], output: ref("total", "int", "M.Done") })
    ]
  }),
  ["Go", "Again", "Bump", "Peek", "Swap", "Boom", "Leave"],
  { returns: "int", vars: [variable("log", "int", lit(0))] }
)

const run = (ir: Ir, inputs: ReadonlyArray<Input>): { snapshot: Snapshot; steps: Array<Array<CoreRecord>> } => {
  let { commands, snapshot } = start(ir, "M", "r1", {}, noFns)
  const steps = [records(commands)]
  for (const input of inputs) {
    ;({ commands, snapshot } = step(ir, snapshot, input, noFns))
    steps.push(records(commands))
  }
  return { snapshot, steps }
}

describe("state locals", () => {
  it("binds parameters from arguments that see the transition's sets, then variables, each with a set record", () => {
    const { snapshot, steps } = run(counter, [ev("Go", { n: 4 })])
    expect(steps[1]!).toEqual([
      { kind: "event", event: { kind: "event", value: { _tag: "Go", n: 4 } } },
      { kind: "transition", from: "M.Idle", index: 0, to: [C] },
      { kind: "exited", state: "M.Idle" },
      { kind: "set", var: "log", value: 5 },
      { kind: "entered", state: C },
      { kind: "set", var: `${C}.n`, value: 5 },
      { kind: "set", var: `${C}.tries`, value: 6 },
      { kind: "set", var: `${C}.note`, value: null },
      { kind: "entered", state: `${C}.A` }
    ])
    expect(snapshot.locals).toEqual({ [C]: { n: 5, tries: 6, note: null } })
    expect(Object.keys(snapshot)).toEqual([
      "machine", "run", "ir", "status", "active", "input", "vars", "locals", "invokes", "calls", "spawned", "timers",
      "scheduled", "nextInvoke", "nextTimer", "nextMessage", "history", "historyLocals", "result"
    ])
  })

  it("assigns a local with a qualified set record, and reads it in an argument after the state is exited", () => {
    const { snapshot, steps } = run(counter, [ev("Go", { n: 4 }), ev("Bump"), ev("Again")])
    expect(steps[2]!).toContainEqual({ kind: "set", var: `${C}.tries`, value: 7 })
    // The lost assignment runs after the exits, the argument after it: it sees 99.
    expect(steps[3]!.slice(1)).toEqual([
      { kind: "transition", from: C, index: 0, to: [C] },
      { kind: "exited", state: `${C}.A` },
      { kind: "exited", state: C },
      { kind: "set", var: `${C}.tries`, value: 99 },
      { kind: "entered", state: C },
      { kind: "set", var: `${C}.n`, value: 990 },
      { kind: "set", var: `${C}.tries`, value: 991 },
      { kind: "set", var: `${C}.note`, value: null },
      { kind: "entered", state: `${C}.A` }
    ])
    expect(snapshot.locals).toEqual({ [C]: { n: 990, tries: 991, note: null } })
  })

  it("keeps the source's locals on an internal transition, and gives the entered child its own, in document order", () => {
    const { snapshot, steps } = run(counter, [ev("Go", { n: 1 }), ev("Swap")])
    expect(steps[2]!.slice(1)).toEqual([
      { kind: "transition", from: C, index: 3, to: [`${C}.B`] },
      { kind: "exited", state: `${C}.A` },
      { kind: "entered", state: `${C}.B` },
      { kind: "set", var: `${C}.B.x`, value: 3 }
    ])
    expect(snapshot.locals).toEqual({ [C]: { n: 2, tries: 3, note: null }, [`${C}.B`]: { x: 3 } })
    expect(Object.keys(snapshot.locals)).toEqual([C, `${C}.B`])
  })

  it("passes a final state its argument as the output, and clears the locals when the run ends", () => {
    const { snapshot, steps } = run(counter, [ev("Go", { n: 1 }), ev("Leave")])
    expect(steps[2]!).toContainEqual({ kind: "set", var: "M.Done.total", value: 5 })
    expect(steps[2]!.at(-1)).toEqual({ kind: "done", state: "M.Done", output: 5 })
    expect(snapshot.status).toBe("done")
    expect(snapshot.locals).toEqual({})
  })

  it("clears the locals when the run is cancelled", () => {
    const { snapshot } = run(counter, [ev("Go", { n: 1 }), { kind: "cancel" }])
    expect(snapshot.locals).toEqual({})
  })

  it("reads a local with no value as unassigned, named by its state, and leaves the locals as they were", () => {
    const { snapshot, steps } = run(counter, [ev("Go", { n: 1 }), ev("Peek")])
    expect(steps[2]!).toEqual([{ kind: "defect", defect: { kind: "unassigned", var: `${C}.later` } }])
    expect(snapshot.locals).toEqual({ [C]: { n: 2, tries: 3, note: null } })
  })

  it("makes a step atomic when an argument overflows", () => {
    const { snapshot, steps } = run(counter, [ev("Go", { n: 1 }), ev("Boom")])
    expect(steps[2]!).toEqual([{ kind: "defect", defect: { kind: "overflow", expr: "n * 9007199254740991" } }])
    expect(snapshot.active).toEqual(["M", C, `${C}.A`])
    expect(snapshot.locals).toEqual({ [C]: { n: 2, tries: 3, note: null } })
  })
})

// machine M() {
//   initial Work
//   state Work {
//     initial A
//     history H
//     deep history D
//     state A { var k: int = 0  on Inc set k = k + 1  on Next -> B(k) }
//     state B(v: int) { initial C  state C { var z: int = 5  on Zap set z = 7 } }
//     on Pause -> Paused
//   }
//   state Paused { on Resume -> Work.H  on Deep -> Work.D }
// }
const W = "M.Work"
const historyIr = program(
  state("M", "compound", {
    initial: W,
    states: [
      state(W, "compound", {
        initial: `${W}.A`,
        transitions: [to(on("Pause"), ["M.Paused"])],
        states: [
          state(`${W}.A`, "atomic", {
            vars: [variable("k", "int", lit(0))],
            transitions: [
              to(on("Inc"), [], { effects: [set("k", op("+", ref("k", "int", `${W}.A`), lit(1)), `${W}.A`)] }),
              to(on("Next"), [`${W}.B`], { args: [{ state: `${W}.B`, values: [ref("k", "int", `${W}.A`)] }] })
            ]
          }),
          state(`${W}.B`, "compound", {
            params: [param("v")],
            initial: `${W}.B.C`,
            states: [
              state(`${W}.B.C`, "atomic", {
                vars: [variable("z", "int", lit(5))],
                transitions: [to(on("Zap"), [], { effects: [set("z", lit(7), `${W}.B.C`)] })]
              })
            ]
          }),
          state(`${W}.H`, "history"),
          state(`${W}.D`, "deepHistory")
        ]
      }),
      state("M.Paused", "atomic", {
        transitions: [to(on("Resume"), [`${W}.H`]), to(on("Deep"), [`${W}.D`])]
      })
    ]
  }),
  ["Inc", "Next", "Zap", "Pause", "Resume", "Deep"]
)

describe("history states and locals", () => {
  it("restores the locals of the states a record re-enters, and starts the rest afresh", () => {
    const paused = run(historyIr, [ev("Inc"), ev("Pause")])
    expect(paused.snapshot.historyLocals).toEqual({ [`${W}.H`]: { [`${W}.A`]: { k: 1 } }, [`${W}.D`]: { [`${W}.A`]: { k: 1 } } })
    expect(paused.snapshot.locals).toEqual({})
    const resumed = step(historyIr, paused.snapshot, ev("Resume"), noFns)
    expect(records(resumed.commands).slice(-3)).toEqual([
      { kind: "entered", state: W },
      { kind: "entered", state: `${W}.A` },
      { kind: "set", var: `${W}.A.k`, value: 1 }
    ])
  })

  it("keeps a parameter for a shallow record and every state below for a deep one", () => {
    const paused = run(historyIr, [ev("Inc"), ev("Next"), ev("Zap"), ev("Pause")])
    expect(paused.snapshot.history).toEqual({ [`${W}.H`]: [`${W}.B`], [`${W}.D`]: [`${W}.B.C`] })
    expect(paused.snapshot.historyLocals).toEqual({
      [`${W}.H`]: { [`${W}.B`]: { v: 1 } },
      [`${W}.D`]: { [`${W}.B`]: { v: 1 }, [`${W}.B.C`]: { z: 7 } }
    })
    const shallow = records(step(historyIr, paused.snapshot, ev("Resume"), noFns).commands)
    expect(shallow.slice(-4)).toEqual([
      { kind: "entered", state: `${W}.B` },
      { kind: "set", var: `${W}.B.v`, value: 1 },
      { kind: "entered", state: `${W}.B.C` },
      { kind: "set", var: `${W}.B.C.z`, value: 5 }
    ])
    const deep = step(historyIr, paused.snapshot, ev("Deep"), noFns)
    expect(records(deep.commands).slice(-3)).toEqual([
      { kind: "set", var: `${W}.B.v`, value: 1 },
      { kind: "entered", state: `${W}.B.C` },
      { kind: "set", var: `${W}.B.C.z`, value: 7 }
    ])
    expect(deep.snapshot.locals).toEqual({ [`${W}.B`]: { v: 1 }, [`${W}.B.C`]: { z: 7 } })
  })

  it("replaces a record's locals with the next record's", () => {
    const again = run(historyIr, [ev("Inc"), ev("Pause"), ev("Resume"), ev("Inc"), ev("Pause")])
    expect(again.snapshot.historyLocals[`${W}.H`]).toEqual({ [`${W}.A`]: { k: 2 } })
  })
})

describe("saving locals", () => {
  const due = () => 0
  const counting = run(counter, [ev("Go", { n: 1 })]).snapshot
  const saved = toSaved(counting, due)

  it("writes locals and historyLocals only when they are not empty", () => {
    expect(saved.locals).toEqual({ [C]: { n: 2, tries: 3, note: null } })
    expect(Object.keys(saved).indexOf("locals")).toBe(Object.keys(saved).indexOf("vars") + 1)
    expect(Object.hasOwn(saved, "historyLocals")).toBe(false)
    const idle = toSaved(start(counter, "M", "r1", {}, noFns).snapshot, due)
    expect(Object.hasOwn(idle, "locals")).toBe(false)
    const paused = toSaved(run(historyIr, [ev("Pause")]).snapshot, due)
    expect(paused.historyLocals).toEqual({ [`${W}.H`]: { [`${W}.A`]: { k: 0 } }, [`${W}.D`]: { [`${W}.A`]: { k: 0 } } })
    expect(Object.keys(paused).indexOf("historyLocals")).toBe(Object.keys(paused).indexOf("history") + 1)
  })

  it("restores them and goes on with them", () => {
    const checked = checkSaved(counter, "M", saved)
    if (!checked.ok) throw new Error(checked.message)
    const restored = restore(counter, checked.saved, "r9", 0, null, noFns)
    expect(restored.snapshot.locals).toEqual(counting.locals)
    expect(restored.snapshot.historyLocals).toEqual({})
    const left = step(counter, restored.snapshot, ev("Leave"), noFns)
    expect(left.snapshot.result).toEqual({ output: 5 })
    const paused = toSaved(run(historyIr, [ev("Inc"), ev("Pause")]).snapshot, due)
    const back = checkSaved(historyIr, "M", paused)
    if (!back.ok) throw new Error(back.message)
    const resumed = step(historyIr, restore(historyIr, back.saved, "r9", 0, null, noFns).snapshot, ev("Resume"), noFns)
    expect(resumed.snapshot.locals).toEqual({ [`${W}.A`]: { k: 1 } })
  })

  const refused = (ir: Ir, json: unknown) => {
    const checked = checkSaved(ir, "M", json)
    return checked.ok ? "ok" : `${checked.reason}: ${checked.message}`
  }
  const without = (s: Saved, key: string) => Object.fromEntries(Object.entries(s).filter(([k]) => k !== key))

  it("refuses locals that do not fit the active states", () => {
    expect(refused(counter, without(saved, "locals"))).toMatch(/^invalid: .*locals has no M\.Counting/)
    expect(refused(counter, { ...saved, locals: { [C]: { tries: 3, note: null } } })).toMatch(/^invalid: .*has no n/)
    expect(refused(counter, { ...saved, locals: { [C]: { n: 2, note: null, other: 1 } } })).toMatch(/^invalid: .*other/)
    expect(refused(counter, { ...saved, locals: { [C]: { n: 2 } } })).toMatch(/^invalid: .*note, which is optional/)
    expect(refused(counter, { ...saved, locals: { [C]: { n: "two", note: null } } })).toMatch(/^invalid: .*not an int/)
    expect(refused(counter, { ...saved, locals: { ...saved.locals, "M.Done": { total: 1 } } })).toMatch(
      /^invalid: .*M\.Done, which is not an active state/
    )
    expect(refused(counter, { ...saved, locals: { [C]: { n: 2, note: null } } })).toBe("ok")
    expect(refused(counter, { ...saved, locals: { "m.x": {} } })).toMatch(/^invalid: not a saved snapshot/)
  })

  it("refuses history locals that its record does not re-enter", () => {
    const paused = toSaved(run(historyIr, [ev("Inc"), ev("Pause")]).snapshot, due)
    const kept = paused.historyLocals!
    expect(refused(historyIr, { ...paused, historyLocals: { ...kept, [`${W}.H`]: {} } })).toMatch(/^invalid: not a saved/)
    expect(refused(historyIr, { ...paused, historyLocals: { ...kept, [`${W}.H`]: { [`${W}.B`]: { v: 1 } } } })).toMatch(
      /^invalid: .*has no M\.Work\.A/
    )
    expect(refused(historyIr, { ...paused, history: {}, historyLocals: kept })).toMatch(/^invalid: .*not a history state with a record/)
    expect(refused(historyIr, { ...paused, historyLocals: { [`${W}.H`]: { [`${W}.A`]: { k: "x" } } } })).toMatch(
      /^invalid: .*not an int/
    )
  })
})

describe("redacting locals", () => {
  // state Idle { on Go -> Secret(event.n) }  state Secret(sensitive pin: int) { sensitive var copy: int = pin  var shown: int = 1 }
  const S = "M.Secret"
  const ir = program(
    state("M", "compound", {
      initial: "M.Idle",
      states: [
        state("M.Idle", "atomic", {
          transitions: [to(on("Go"), [S], { args: [{ state: S, values: [ref("event", "int", null, "n")] }] })]
        }),
        state(S, "atomic", {
          params: [param("pin", "int", true)],
          vars: [variable("copy", "int", ref("pin", "int", S), true), variable("shown", "int", lit(1))]
        })
      ]
    }),
    ["Go"]
  )

  it("redacts a sensitive parameter and variable in set records and in a restored snapshot", () => {
    const { snapshot, steps } = run(ir, [ev("Go", { n: 1234 })])
    expect(steps[1]!.filter((r) => r.kind === "set")).toEqual([
      { kind: "set", var: `${S}.pin`, value: "<redacted>" },
      { kind: "set", var: `${S}.copy`, value: "<redacted>" },
      { kind: "set", var: `${S}.shown`, value: 1 }
    ])
    expect(snapshot.locals).toEqual({ [S]: { pin: 1234, copy: 1234, shown: 1 } })
    const saved = toSaved(snapshot, () => 0)
    const checked = checkSaved(ir, "M", saved)
    if (!checked.ok) throw new Error(checked.message)
    const [first] = records(restore(ir, checked.saved, "r2", 0, null, noFns).commands)
    expect(first?.kind === "restored" && first.snapshot.locals).toEqual({
      [S]: { pin: "<redacted>", copy: "<redacted>", shown: 1 }
    })
  })
})
