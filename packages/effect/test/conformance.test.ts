import { afterAll, describe, expect, it } from "vitest"
import { caseNames, isDurable, isSupported, runCase } from "./conformance/runner.ts"

const all = caseNames()
const names = all.filter(isSupported)
const skipped = all.filter((name) => !isSupported(name))
let passed = 0

describe("conformance", () => {
  it("finds the cases", () => {
    expect(names.length).toBeGreaterThan(0)
  })

  // Cases that require features this runtime does not implement yet (`requires`).
  it.skip.each(skipped)("%s", () => {})

  it.each(names)("%s", async (name) => {
    const result = await runCase(name)
    expect(result.errors).toEqual([])
    expect(result.actual).toBe(result.expected)
    passed += 1
  })

  afterAll(() => {
    console.log(`conformance: ${passed}/${names.length} cases pass byte for byte, ${skipped.length} skipped`)
  })
})

// The same cases through the durable driver, `Durable.workflow` on `WorkflowEngine.layerMemory`:
// while nothing crashes it must write the same traces. Cases with `suspend` steps are skipped,
// since a durable run parks rather than suspends.
const durableNames = names.filter(isDurable)
let durablePassed = 0

describe("conformance through the durable driver", () => {
  it.skip.each(names.filter((name) => !isDurable(name)))("%s", () => {})

  it.each(durableNames)("%s", async (name) => {
    const result = await runCase(name, true)
    expect(result.errors).toEqual([])
    expect(result.actual).toBe(result.expected)
    durablePassed += 1
  })

  afterAll(() => {
    console.log(`durable conformance: ${durablePassed}/${durableNames.length} cases pass byte for byte`)
  })
})
