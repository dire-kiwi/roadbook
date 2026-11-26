/**
 * `pnpm --filter @roadbook/example-import-job start`: import a 500-row file in five batches,
 * crash the process while batch 3 runs, start it again on the same storage, pause and resume
 * the job, and print what the database was asked to import.
 */
import { Context, Deferred, Effect, Exit, Layer, Scope } from "effect"
import { MessageStorage, ShardingConfig } from "effect/cluster"
import { WorkflowEngine } from "effect/workflow"
import { Database, ImportFlow, processLayer } from "./durable.ts"

export const demo = Effect.gen(function*() {
  const imported: Array<string> = []
  // Batch 3 waits until this is opened, so that the crash falls in the middle of it.
  const third = yield* Deferred.make<void>()
  const database = Layer.succeed(Database, {
    countRows: () => Effect.succeed(500),
    importRows: (jobId, batch, from, count) =>
      Effect.gen(function*() {
        imported.push(`${jobId} batch ${batch}: rows ${from}-${from + count - 1}`)
        if (batch === 2) yield* Deferred.await(third)
        yield* Effect.sleep(20)
      })
  })
  // The storage outlives each process, as a database would.
  const storage = yield* Layer.build(MessageStorage.layerMemory.pipe(Layer.provide(ShardingConfig.layer())))
  const startProcess = Effect.gen(function*() {
    const scope = yield* Scope.make()
    const context = yield* Layer.buildWithScope(Layer.provide(processLayer, database), scope).pipe(
      Effect.provideContext(storage)
    )
    return {
      run: <A, E>(effect: Effect.Effect<A, E, WorkflowEngine.WorkflowEngine>) =>
        Effect.provideContext(effect, context as Context.Context<WorkflowEngine.WorkflowEngine>),
      stop: Scope.close(scope, Exit.void)
    }
  })
  const upload = { jobId: "job-7", path: "/uploads/customers.csv" }

  let process = yield* startProcess
  const id = yield* process.run(ImportFlow.start({ upload }))
  while (imported.length < 3) yield* Effect.sleep(10)
  yield* Effect.log(`crash while ${imported.at(-1)}`)
  yield* process.stop

  process = yield* startProcess
  yield* Deferred.succeed(third, undefined)
  // Pausing takes effect after the running batch; resuming carries on with the next one.
  yield* process.run(ImportFlow.send(id, { _tag: "Pause" }))
  yield* process.run(ImportFlow.send(id, { _tag: "Resume" }))
  const summary = yield* process.run(ImportFlow.run({ upload }))
  yield* process.stop
  return { summary, imported }
}).pipe(Effect.scoped)

if (import.meta.main) {
  Effect.runPromise(demo).then(({ imported, summary }) => {
    console.log(imported.join("\n"))
    console.log(summary)
  })
}
