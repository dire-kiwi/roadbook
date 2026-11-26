import { afterAll, describe, expect, it } from "vitest"
import { caseNames, isSupported, runCase } from "./conformance/runner.ts"

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

  it.each(names)("%s", (name) => {
    const result = runCase(name)
    expect(result.errors).toEqual([])
    expect(result.actual).toBe(result.expected)
    passed += 1
  })

  afterAll(() => {
    console.log(`conformance: ${passed}/${names.length} cases pass byte for byte, ${skipped.length} skipped`)
  })
})
