/**
 * The CSV import run durably on `effect/workflow`: each job is a workflow execution keyed by
 * its job id, each batch a task, and so an activity. A process that crashes in the middle of
 * batch 3 imports batch 3 again after it restarts, and never batches 1 and 2.
 */
import { Durable } from "@dire-kiwi/roadbook-effect/durable"
import { Context, Effect, Layer } from "effect"
import {
  ClusterWorkflowEngine,
  type MessageStorage,
  RunnerHealth,
  Runners,
  RunnerStorage,
  Sharding,
  ShardingConfig
} from "effect/cluster"
import { ImportJob } from "../import.road.js"
import { InvalidFile, type TransientBatchError } from "../types.road.js"

/** Where the rows go. Importing a batch is keyed by job and batch, so doing it twice is harmless. */
export class Database extends Context.Service<Database, {
  /** How many rows the file holds. */
  countRows(path: string): Effect.Effect<number>
  /** Import rows `[from, from + count)` of the job's file as batch `batch`. */
  importRows(jobId: string, batch: number, from: number, count: number): Effect.Effect<void, TransientBatchError>
}>()("import-job/Database") {}

const BATCH_SIZE = 100

export const ImportJobLive = ImportJob.implement({
  tasks: {
    validateUpload: ({ upload }) =>
      Effect.gen(function*() {
        const totalRows = yield* (yield* Database).countRows(upload.path)
        if (totalRows === 0) {
          return yield* new InvalidFile({ rowErrors: [{ row: 0, message: "the file has no rows" }] })
        }
        return { totalRows, batchSize: BATCH_SIZE }
      }),
    processBatch: ({ upload, plan, batch }) =>
      Effect.gen(function*() {
        const db = yield* Database
        const from = batch * plan.batchSize
        const count = Math.min(plan.batchSize, plan.totalRows - from)
        yield* db.importRows(upload.jobId, batch, from, count)
        return plan.totalRows - from - count
      })
  }
})

/** The import job as a durable machine: starting a job id that is already running joins it. */
export const ImportFlow = Durable.workflow(ImportJobLive, {
  idempotencyKey: ({ upload }) => upload.jobId
})

/**
 * One process: a single cluster runner with the import job registered, on `MessageStorage`
 * (`SqlMessageStorage` in production, so that the runs outlive the process). It needs the
 * `Database`, which the batches use.
 */
export const processLayer: Layer.Layer<
  never,
  never,
  Database | MessageStorage.MessageStorage
> = ImportFlow.layer.pipe(
  Layer.provideMerge(ClusterWorkflowEngine.layer),
  Layer.provideMerge(Sharding.layer),
  Layer.provide(Runners.layerNoop),
  Layer.provide([RunnerStorage.layerMemory, RunnerHealth.layerNoop]),
  // No grace period on shutdown, so that stopping the process stands for a crash.
  Layer.provide(ShardingConfig.layer({ entityTerminationTimeout: 0, entityMessagePollInterval: 20 }))
)
