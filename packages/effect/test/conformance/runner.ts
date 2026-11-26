/**
 * The TypeScript conformance runner (spec/conformance/README.md, "How a runner works").
 *
 * It runs a case through the production driver, `make(...).implement(...).spawn(...)`, and
 * replaces only what the README lets a runner replace: the clock and timer (a virtual `Timer`
 * that fires one timer at a time), and the host code (scripted tasks and stubbed machines, the
 * `fns` table, the `actions` table). Every other machine runs from the chart as a child run.
 * `start` steps spawn more top-level runs the same way, with the handles of the runs started
 * before them as their `ref` arguments, and `restore` steps restore saved snapshots through
 * `restore`; `snapshot` and `suspend` steps use the actors' `save` and `suspend`.
 *
 * Quiescence. Every fiber runs on a manual `Scheduler` that queues tasks instead of using
 * `setImmediate`, and nothing in a case waits on real time or I/O: scripted invokes wait on the
 * runner, timers on the virtual timer. So once the scheduler has no task left, every run's queue
 * is empty, no run is carrying out commands and every ended child's outcome has been taken. The
 * runner drains the scheduler after each action, synchronously, and that is "wait until
 * quiescent".
 */
import * as Core from "@roadbook/core"
import { Cause, Effect, Exit, type Fiber, References, Scheduler, Scope } from "effect"
import * as fs from "node:fs"
import * as path from "node:path"
import { Codec } from "../../src/internal/codec.ts"
import * as Roadbook from "../../src/index.ts"

// ---- the case files ---------------------------------------------------------------------------

type Step =
  | { readonly advance: number }
  | { readonly event: Core.Tagged; readonly run?: string }
  | {
    readonly task: string
    readonly run?: string
    readonly done?: Core.Json
    readonly fail?: Core.Tagged
    readonly defect?: string
  }
  | { readonly cancel: true; readonly run?: string }
  | {
    readonly start: string
    readonly run: string
    readonly input: { readonly [param: string]: Core.Json }
    readonly status: Core.Status
  }
  | { readonly together: ReadonlyArray<Step> }
  | { readonly snapshot: string; readonly run?: string }
  | {
    readonly restore: string
    readonly run?: string
    readonly migrate?: string
    readonly status?: Core.Status
    readonly refused?: Core.RefusalReason
  }
  | { readonly suspend: string; readonly run?: string }

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
 * The features of case.schema.json's `requires` this runtime implements. A case that requires
 * another is skipped, not failed.
 */
export const supported: ReadonlyArray<string> = ["messaging", "parallel", "snapshots"]

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

  // The top-level runs: the root, then one per `start` step, each spawned with the others'
  // clock, timer, trace sink and observer.
  const scope = Scope.makeUnsafe()
  const actors = new Map<string, Roadbook.Actor<any>>()
  const machineOf = new Map<string, string>()
  const spawnTop = (machine: string, run: string, input: { readonly [param: string]: Core.Json }) => {
    const decl = ir.machines[machine] ?? fail(`the chart has no machine ${machine}`)
    // A `ref` argument is the id of a top-level run started before: pass its handle.
    const handles = new Map<string, object>([...actors].map(([id, actor]) => [id, actor.handle]))
    const args = new Codec(ir, {}, handles).decodeArgs(decl.params, input)
    const actor = exec(
      handleFor(machine).spawn(args).pipe(
        Scope.provide(scope),
        Effect.provideService(Roadbook.TraceSink, { write: (record) => records.push(record) }),
        Effect.provideService(Roadbook.Timer, timer),
        Effect.provideService(Roadbook.RootRunId, () => run),
        Effect.provideService(Roadbook.RunObserver, { started: (handle) => runs.set(handle.id, handle) })
      )
    )
    if (actor.handle.id !== run) fail(`the run of ${machine} was not named ${run}`)
    actors.set(run, actor)
    machineOf.set(run, machine)
  }
  // Saved snapshots: the files of the case, and the suspend steps still waiting for their run.
  const readSnapshot = (name: string): string => {
    const file = path.join(dir, `${name}.snapshot.json`)
    if (!fs.existsSync(file)) fail(`no snapshot file ${name}.snapshot.json`)
    return fs.readFileSync(file, "utf8")
  }
  const compareSaved = (name: string, saved: Core.Saved) => {
    const actual = `${Core.canonicalJson(saved)}\n`
    if (actual !== readSnapshot(name)) fail(`snapshot ${name} differs: ${actual}`)
  }
  const pendingSuspends: Array<{ readonly name: string; readonly run: string; readonly fiber: Fiber.Fiber<Core.Saved> }> = []
  /** Runs being suspended or suspended: no event or cancel step may give them an input. */
  const suspending = new Set<string>()
  const suspended = new Set<string>()
  const checkSuspends = () => {
    for (const pending of [...pendingSuspends]) {
      const exit = pending.fiber.pollUnsafe()
      if (exit === undefined) continue
      pendingSuspends.splice(pendingSuspends.indexOf(pending), 1)
      if (Exit.isFailure(exit)) fail(`suspending ${pending.run} failed: ${exit.cause}`)
      else {
        if (exit.value.status === "running") suspended.add(pending.run)
        compareSaved(pending.name, exit.value)
      }
    }
  }
  const nextRun = () => `r${actors.size + 1}`

  const restoreTop = (step: Extract<Step, { readonly restore: string }>) => {
    const text = readSnapshot(step.restore)
    const json = JSON.parse(text) as Core.Json
    const named = typeof json === "object" && json !== null && !Array.isArray(json)
      ? (json as { readonly [key: string]: Core.Json })["machine"]
      : undefined
    const machine = typeof named === "string" && Object.hasOwn(ir.machines, named) && !spec.stub.includes(named)
      ? named
      : spec.machine
    let calls = 0
    const options: Roadbook.RestoreOptions = step.migrate === undefined ? {} : {
      migrate: (old) => {
        calls += 1
        if (Core.canonicalJson(old) !== Core.canonicalJson(json)) fail("the migration was not given the snapshot being restored")
        return JSON.parse(readSnapshot(step.migrate!)) as Core.Saved
      }
    }
    const before = records.length
    const run = step.run ?? nextRun()
    const exit = exec(Effect.exit(
      handleFor(machine).restore(json as never, options).pipe(
        Scope.provide(scope),
        Effect.provideService(Roadbook.TraceSink, { write: (record) => records.push(record) }),
        Effect.provideService(Roadbook.Timer, timer),
        Effect.provideService(Roadbook.RootRunId, () => run),
        Effect.provideService(Roadbook.RunObserver, { started: (handle) => runs.set(handle.id, handle) })
      )
    ))
    if (step.migrate !== undefined && calls !== 1) fail(`the migration was called ${calls} times, not once`)
    if (step.refused !== undefined) {
      if (Exit.isSuccess(exit)) fail(`restoring ${step.restore} was not refused`)
      const error = Exit.isFailure(exit) ? exit.cause.reasons.find(Cause.isFailReason)?.error : undefined
      if (!(error instanceof Roadbook.RestoreError)) fail(`restoring ${step.restore} failed otherwise: ${String(exit)}`)
      if (error!.reason !== step.refused) fail(`restoring ${step.restore} was refused for ${error!.reason}, not ${step.refused}`)
      if (records.length !== before) fail(`a refused restore wrote ${records.length - before} records`)
      return
    }
    if (Exit.isFailure(exit)) return fail(`restoring ${step.restore} failed: ${exit.cause}`)
    const actor = exit.value
    if (actor.handle.id !== run) fail(`the restored run was not named ${run}`)
    if (actors.has(run)) fail(`a second run named ${run}`)
    actors.set(run, actor)
    machineOf.set(run, machine)
    statuses.set(run, step.status ?? "running")
  }

  const statusOf = (run: string) => (runs.get(run) ?? fail(`no run ${run}`)).snapshot().status
  const actorOf = (run: string) => actors.get(run) ?? fail(`no top-level run ${run}`)
  const statuses = new Map<string, Core.Status>([["r1", spec.status]])

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

  const decodeEvent = (run: string, event: Core.Tagged) => {
    const decl = ir.machines[machineOf.get(run) ?? ""]?.events[event._tag]
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
      const run = step.run ?? "r1"
      exec(actorOf(run).send(decodeEvent(run, step.event)))
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
      exec(Effect.exit(actorOf(step.run ?? "r1").cancel))
    } else if ("snapshot" in step) {
      compareSaved(step.snapshot, exec(actorOf(step.run ?? "r1").save))
    } else if ("restore" in step) {
      restoreTop(step)
    } else if ("suspend" in step) {
      const run = step.run ?? "r1"
      suspending.add(run)
      const fiber = Effect.runFork(actorOf(run).suspend, { scheduler })
      pendingSuspends.push({ name: step.suspend, run, fiber })
      scheduler.drain()
    } else if ("start" in step) {
      if (actors.has(step.run)) fail(`a second run named ${step.run}`)
      spawnTop(step.start, step.run, step.input)
      statuses.set(step.run, step.status)
    } else {
      const inputs: Array<{ readonly run: string; readonly input: Core.Input }> = []
      const taken = new Set<Scripted>()
      for (const item of step.together) {
        if ("event" in item) inputs.push({ run: item.run ?? "r1", input: { kind: "event", value: item.event } })
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

  /** The top-level runs a step gives an input to directly. */
  const targets = (step: Step): Array<string> =>
    "together" in step
      ? step.together.flatMap(targets)
      : "event" in step || "cancel" in step
      ? [step.run ?? "r1"]
      : []

  /** Whether the step gives an input to some top-level run, and so needs one that is live. */
  const givesInput = (step: Step) => "event" in step || "task" in step || "cancel" in step || "together" in step

  try {
    spawnTop(spec.machine, "r1", spec.input)
    scheduler.drain()
    for (const step of spec.script) {
      const what = JSON.stringify(step)
      for (const run of targets(step)) {
        if (statusOf(run) !== "running") fail(`${run} had already ended (${statusOf(run)}) before step ${what}`)
        if (suspending.has(run)) fail(`${run} is suspended or being suspended before step ${what}`)
      }
      if (
        givesInput(step) && [...actors.keys()].every((run) => statusOf(run) !== "running" || suspended.has(run))
      ) {
        fail(`every top-level run had ended or been suspended before step ${what}`)
      }
      carryOut(step)
      checkSuspends()
    }
    for (const pending of pendingSuspends) fail(`suspending ${pending.run} never gave snapshot ${pending.name}`)
    for (const [run, status] of statuses) {
      if (statusOf(run) !== status) fail(`the status of ${run} is ${statusOf(run)}, not ${status}`)
    }
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
