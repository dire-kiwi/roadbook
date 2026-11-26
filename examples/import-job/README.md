# The import job

A long-lived CSV import (`import.road`): validate the file, then import it batch by batch, with `Pause`, `Resume` and `Cancel` from the operator and a timed retry for a batch that fails for a passing reason. `types.road` holds its data shapes, errors and tasks.

## Run durably

`src/durable.ts` runs the chart on `effect/workflow` with `@roadbook/effect/durable` ([durable runs](../../docs/design/typescript.md#durable-runs)):

```ts
export const ImportFlow = Durable.workflow(ImportJobLive, {
  idempotencyKey: ({ upload }) => upload.jobId
})
```

Each job is one workflow execution, keyed by its job id; each batch is a task, so an activity whose result is stored once it finishes. `processLayer` is one process: a single cluster runner (`ClusterWorkflowEngine`) with the job registered, on a `MessageStorage`. In production that is `SqlMessageStorage`, so runs outlive the process; the demo uses `MessageStorage.layerMemory` and keeps it across a simulated crash.

```sh
pnpm --filter @roadbook/example-import-job start
```

`src/demo.ts` imports a 500-row file in five batches, stops the process while batch 3 (`batch 2`, counting from 0) runs, starts a new one on the same storage, pauses and resumes the job, and prints what the database was asked to import:

```
job-7 batch 0: rows 0-99
job-7 batch 1: rows 100-199
job-7 batch 2: rows 200-299
job-7 batch 2: rows 200-299
job-7 batch 3: rows 300-399
job-7 batch 4: rows 400-499
{ jobId: 'job-7', totalRows: 500, batches: 5, retries: 0 }
```

The two batches finished before the crash are not imported again. The one cut short is (a task runs at least once), which is why `importRows` is keyed by job and batch. `pnpm --filter @roadbook/example-import-job test` checks exactly this.
