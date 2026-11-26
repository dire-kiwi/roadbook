/** The demo end to end: the crash falls in batch 3, which is the only one imported twice. */
import { Effect } from "effect"
import { describe, expect, it } from "vitest"
import { demo } from "../src/demo.ts"

describe("the durable import job", () => {
  it("imports the batches finished before a crash once, and the one cut short again", async () => {
    const { imported, summary } = await Effect.runPromise(demo)
    expect(imported).toEqual([
      "job-7 batch 0: rows 0-99",
      "job-7 batch 1: rows 100-199",
      "job-7 batch 2: rows 200-299",
      "job-7 batch 2: rows 200-299",
      "job-7 batch 3: rows 300-399",
      "job-7 batch 4: rows 400-499"
    ])
    expect(summary).toEqual({ jobId: "job-7", totalRows: 500, batches: 5, retries: 0 })
  }, 20_000)
})
