/**
 * The TypeScript conformance runner (spec/conformance/README.md, "How a runner works").
 *
 * It runs a case through the production driver, `make(...).implement(...).spawn(...)`, and
 * replaces only what the README lets a runner replace: the clock and timer (a virtual `Timer`
 * that fires one timer at a time), and the host code (scripted tasks and stubbed machines, the
 * `fns` table, the `actions` table). Every other machine runs from the chart as a child run.
 *
 * Quiescence. Every fiber runs on a manual `Scheduler` that queues tasks instead of using
 * `setImmediate`, and nothing in a case waits on real time or I/O: scripted invokes wait on the
 * runner, timers on the virtual timer. So once the scheduler has no task left, every run's queue
 * is empty, no run is carrying out commands and every ended child's outcome has been taken. The
 * runner drains the scheduler after each action, synchronously, and that is "wait until
 * quiescent".
 */
import * as Core from "@roadbook/core"
import { Effect, Exit, type Fiber, References, Scheduler, Scope } from "effect"
import * as fs from "node:fs"
import * as path from "node:path"
import { Codec } from "../../src/internal/codec.ts"
import * as Roadbook from "../../src/index.ts"

// ---- the case files ---------------------------------------------------------------------------

type Step =
  | { readonly advance: number }
  | { readonly event: Core.Tagged }
  | {
    readonly task: string
    readonly run?: string
    readonly done?: Core.Json
    readonly fail?: Core.Tagged
    readonly defect?: string
  }
  | { readonly cancel: true }
  | { readonly together: ReadonlyArray<Step> }

export interface Case {
  readonly description: string
  readonly requires: ReadonlyArray<string>
  readonly chart: string
  readonly machine: string
  readonly input: { readonly [param: string]: Core.Json }
  readonly stub: ReadonlyArray<string>
  readonly fns: Readonly<Record<string, ReadonlyArray<{ readonly args: ReadonlyArray<Core.Json>; readonly returns: Core.Json }>>>
  readonly actions: Readonly<Record<string, ReadonlyArray<{ readonly args: Core.Json; readonly defect: string }>>>
  readonly script: ReadonlyArray<Step>
  readonly status: Core.Status
}

export const conformanceDir = path.resolve(import.meta.dirname, "../../../../spec/conformance")

/**
 * The features of case.schema.json's `requires` this runtime implements. Messaging (milestone 5)
 * is not one yet. A case that requires another is skipped, not failed.
 */
export const supported: ReadonlyArray<string> = []

/** Whether this runtime implements everything case `name` requires. */
export const isSupported = (name: string): boolean => {
  const spec = JSON.parse(fs.readFileSync(path.join(conformanceDir, "cases", name, "case.json"), "utf8")) as Case
  return spec.requires.every((feature) => supported.includes(feature))
}

export const caseNames = (): Array<string> =>
  fs.readdirSync(path.join(conformanceDir, "cases")).filter((name) =>
    fs.existsSync(path.join(conformanceDir, "cases", name, "case.json"))
  ).sort()

// ---- run order --------------------------------------------------------------------------------

/** trace.md's run order: the first segment as a string, the others as numbers, prefixes first. */
export const compareRunIds = (a: string, b: string): number => {
  const as = a.split(".")
  const bs = b.split(".")
  for (let i = 0; i < Math.min(as.length, bs.length); i++) {
    const x = as[i]!
    const y = bs[i]!
    const c = i === 0 ? (x < y ? -1 : x > y ? 1 : 0) : Number(x) - Number(y)
    if (c !== 0) return c
  }
  return as.length - bs.length
}

/** Records grouped by run, groups in run order, each line canonical JSON and a line feed. */
export const arrange = (records: ReadonlyArray<Core.TraceRecord>): string => {
  const groups = new Map<string, Array<Core.TraceRecord>>()
  for (const record of records) {
    let group = groups.get(record.run)
    if (group === undefined) groups.set(record.run, group = [])
    group.push(record)
  }
  return [...groups.keys()].sort(compareRunIds).flatMap((run) => groups.get(run)!)
    .map((record) => `${Core.canonicalJson(record)}\n`).join("")
}

// ---- the manual scheduler -----------------------------------------------------------------------

class ManualScheduler implements Scheduler.Scheduler {
  readonly executionMode = "async" as const
  private tasks: Array<[priority: number, task: () => void]> = []

  shouldYield(fiber: Fiber.Fiber<unknown, unknown>): boolean {
    return fiber.currentOpCount >= fiber.cache.maxOpsBeforeYield
  }

  makeDispatcher(): Scheduler.SchedulerDispatcher {
    return {
      scheduleTask: (task, priority) => {
        this.tasks.push([priority, task])
      },
      flush: () => this.drain()
    }
  }

  /** Run tasks until there are none: the runs are then quiescent. */
  drain() {
    let rounds = 0
    while (this.tasks.length > 0) {
      if (++rounds > 1_000_000) throw new Error("the runs never became quiescent")
      const batch = this.tasks
      this.tasks = []
      batch.sort((a, b) => a[0] - b[0])
      for (const [, task] of batch) task()
    }
  }
}

// ---- running a case ------------------------------------------------------------------------------

export interface CaseResult {
  readonly name: string
  readonly expected: string
  readonly actual: string
  /** Why the case failed other than by its trace, if it did. */
  readonly errors: ReadonlyArray<string>
}

class CaseFailure extends Error {}

interface Scripted {
  readonly name: string
  readonly run: string
  readonly invoke: number
  resume: (effect: Effect.Effect<unknown, unknown>) => void
}

interface PendingTimer {
  readonly run: string
  readonly timer: number
  readonly due: number
  fire: () => void
}

export const runCase = (name: string): CaseResult => {
  const dir = path.join(conformanceDir, "cases", name)
  const spec = JSON.parse(fs.readFileSync(path.join(dir, "case.json"), "utf8")) as Case
  const expected = fs.readFileSync(path.join(dir, "trace.jsonl"), "utf8")
  const irPath = path.join(conformanceDir, spec.chart.replace(/\.road$/, ".ir.json"))
  const ir = Core.loadIr(JSON.parse(fs.readFileSync(irPath, "utf8")))
  if (!Object.hasOwn(ir.machines, spec.machine)) throw new Error(`${irPath} has no machine ${spec.machine}`)
  const codec = new Codec(ir, {})
  const errors: Array<string> = []
  const records: Array<Core.TraceRecord> = []
  const fail = (message: string): never => {
    errors.push(message)
    throw new CaseFailure(message)
  }

  // Virtual time and the timers the driver started.
  let now = 0
  const timers = new Set<PendingTimer>()
  const timer: Roadbook.Timer = {
    now: Effect.sync(() => now),
    sleep: ({ delay, run, timer: id }) =>
      Effect.callback<void>((resume) => {
        const pending: PendingTimer = { run, timer: id, due: now + delay, fire: () => resume(Effect.void) }
        timers.add(pending)
        return Effect.sync(() => {
          timers.delete(pending)
        })
      })
  }

  // Scripted invokes: tasks and stubbed machines wait until the script ends them.
  const scripted = new Set<Scripted>()
  const scriptedInvoke = (src: string) => () =>
    Effect.flatMap(References.CurrentLogAnnotations, (annotations) =>
      Effect.callback<unknown, unknown>((resume) => {
        const entry: Scripted = {
          name: src,
          run: String(annotations["roadbook.run"]),
          invoke: Number(annotations["roadbook.invoke"]),
          resume
        }
        scripted.add(entry)
        return Effect.sync(() => {
          scripted.delete(entry)
        })
      }))

  const fnImpls: Record<string, (args: Record<string, unknown>) => unknown> = {}
  for (const [fn, decl] of Object.entries(ir.functions)) {
    fnImpls[fn] = (named) => {
      const args = decl.params.map((p) => codec.encode(p.type, named[p.name], p.name))
      const key = Core.canonicalJson(args)
      const entry = (spec.fns[fn] ?? []).find((e) => Core.canonicalJson(e.args) === key)
      if (entry === undefined) {
        errors.push(`unscripted call ${fn}(${key})`)
        throw new CaseFailure(`unscripted call ${fn}(${key})`)
      }
      return codec.decode(decl.returns, entry.returns)
    }
  }
  const actionImpls: Record<string, (args: Record<string, unknown>) => Effect.Effect<void>> = {}
  for (const [action, decl] of Object.entries(ir.actions)) {
    actionImpls[action] = (named) => {
      const key = Core.canonicalJson(codec.encodeArgs(decl.params, named, action))
      const entry = (spec.actions[action] ?? []).find((e) => Core.canonicalJson(e.args) === key)
      return entry === undefined ? Effect.void : Effect.die(new Error(entry.defect))
    }
  }
  const taskImpls: Record<string, () => Effect.Effect<unknown, unknown>> = {}
  for (const task of Object.keys(ir.tasks)) taskImpls[task] = scriptedInvoke(task)
  const machineImpls: Record<string, unknown> = {}
  const implemented = new Map<string, Roadbook.Implemented<any, never>>()
  const handlers = { fns: fnImpls, actions: actionImpls, tasks: taskImpls, machines: machineImpls }
  for (const machine of Object.keys(ir.machines)) {
    machineImpls[machine] = spec.stub.includes(machine)
      ? scriptedInvoke(machine)
      : { run: (input: unknown) => Effect.suspend(() => implemented.get(machine)!.run(input)) }
  }
  const handleFor = (machine: string) => {
    let impl = implemented.get(machine)
    if (impl === undefined) {
      impl = Roadbook.make<any>(ir, machine).implement(handlers as never) as Roadbook.Implemented<any, never>
      implemented.set(machine, impl)
    }
    return impl
  }
  for (const machine of Object.keys(ir.machines)) {
    if (!spec.stub.includes(machine)) handleFor(machine)
  }

  // Every run, as the driver reports it.
  const runs = new Map<string, Roadbook.RunHandle>()
  const scheduler = new ManualScheduler()
  const exec = <A, E>(effect: Effect.Effect<A, E>): A => {
    const fiber = Effect.runFork(effect, { scheduler })
    scheduler.drain()
    const exit = fiber.pollUnsafe()
    if (exit === undefined) throw new Error("an effect of the runner did not complete")
    if (Exit.isFailure(exit)) throw new Error(`an effect of the runner failed: ${exit.cause}`)
    return exit.value
  }

  const scope = Scope.makeUnsafe()
  const root = handleFor(spec.machine)
  const actor = exec(
    root.spawn(spec.input).pipe(
      Scope.provide(scope),
      Effect.provideService(Roadbook.TraceSink, { write: (record) => records.push(record) }),
      Effect.provideService(Roadbook.Timer, timer),
      Effect.provideService(Roadbook.RootRunId, () => "r1"),
      Effect.provideService(Roadbook.RunObserver, { started: (run) => runs.set(run.id, run) })
    )
  )
  const rootRun = () => runs.get("r1") ?? fail("the root run was not named r1")
  const status = () => rootRun().snapshot().status

  /** The running scripted invoke a `task` step refers to. */
  const findScripted = (step: { readonly task: string; readonly run?: string }, taken: ReadonlySet<Scripted>) => {
    const matches = [...scripted].filter((s) =>
      s.name === step.task && !taken.has(s) && (step.run === undefined || s.run === step.run)
    )
    if (matches.length !== 1) {
      fail(`${matches.length === 0 ? "no" : "more than one"} running invoke of ${step.task}${step.run ? ` in ${step.run}` : ""}`)
    }
    return matches[0]!
  }

  const outcomeOf = (step: Extract<Step, { readonly task: string }>, entry: Scripted): Core.Input => {
    if (step.done !== undefined) return { kind: "invoke.done", invoke: entry.invoke, value: step.done }
    if (step.fail !== undefined) return { kind: "invoke.fail", invoke: entry.invoke, error: step.fail }
    return { kind: "invoke.defect", invoke: entry.invoke, defect: { kind: "host", message: step.defect ?? "" } }
  }

  const returnsOf = (src: string): string | null =>
    Object.hasOwn(ir.tasks, src) ? ir.tasks[src]!.returns : ir.machines[src]?.returns ?? null

  /** The timers due in `(…, end]`, in the order an advance fires them. */
  const dueBy = (end: number) =>
    [...timers].filter((t) => t.due <= end).sort((a, b) =>
      a.due - b.due || compareRunIds(a.run, b.run) || a.timer - b.timer
    )

  const decodeEvent = (event: Core.Tagged) => {
    const decl = ir.machines[spec.machine]?.events[event._tag]
    const fields: Record<string, unknown> = {}
    for (const field of decl?.fields ?? []) fields[field.name] = codec.decode(field.type, event[field.name] ?? null)
    return { ...fields, _tag: event._tag }
  }

  const carryOut = (step: Step) => {
    if ("advance" in step) {
      const end = now + step.advance
      for (let next = dueBy(end)[0]; next !== undefined; next = dueBy(end)[0]) {
        now = next.due
        timers.delete(next)
        next.fire()
        scheduler.drain()
      }
      now = end
    } else if ("event" in step) {
      exec(actor.send(decodeEvent(step.event)))
    } else if ("task" in step) {
      const entry = findScripted(step, new Set())
      scripted.delete(entry)
      const returns = returnsOf(entry.name)
      entry.resume(
        step.done !== undefined
          ? Effect.succeed(returns === null ? undefined : codec.decode(returns, step.done))
          : step.fail !== undefined
          ? Effect.fail(codec.decodeError(step.fail))
          : Effect.die(new Error(step.defect ?? ""))
      )
      scheduler.drain()
    } else if ("cancel" in step) {
      exec(Effect.exit(actor.cancel))
    } else {
      const inputs: Array<{ readonly run: string; readonly input: Core.Input }> = []
      const taken = new Set<Scripted>()
      for (const item of step.together) {
        if ("event" in item) inputs.push({ run: "r1", input: { kind: "event", value: item.event } })
        else if ("task" in item) {
          const entry = findScripted(item, taken)
          taken.add(entry)
          inputs.push({ run: entry.run, input: outcomeOf(item, entry) })
        } else if ("advance" in item) {
          const end = now + item.advance
          for (const t of dueBy(end)) {
            timers.delete(t)
            inputs.push({ run: t.run, input: { kind: "after", timer: t.timer } })
          }
          now = end
        } else fail("a together step may hold only event, task and advance steps")
      }
      // Never ended otherwise: the driver drops them when the core takes their outcomes.
      for (const entry of taken) scripted.delete(entry)
      const run = inputs[0]?.run
      if (run === undefined) fail("an empty together step")
      if (inputs.some((i) => i.run !== run)) fail("a together step with inputs for two runs")
      const handle = runs.get(run!) ?? fail(`no run ${run}`)
      exec(handle.offerAll(inputs.map((i) => i.input) as never))
    }
  }

  try {
    scheduler.drain()
    for (const step of spec.script) {
      if (status() !== "running") fail(`the root run had already ended (${status()}) before step ${JSON.stringify(step)}`)
      carryOut(step)
    }
    if (status() !== spec.status) fail(`the root run's status is ${status()}, not ${spec.status}`)
  } catch (error) {
    if (!(error instanceof CaseFailure)) errors.push(String((error as Error)?.stack ?? error))
  }

  const actual = arrange(records)
  const out = process.env["ROADBOOK_CONFORMANCE_OUT"]
  if (out) {
    fs.mkdirSync(out, { recursive: true })
    fs.writeFileSync(path.join(out, `${name}.jsonl`), actual)
  }
  return { name, expected, actual, errors }
}
