import { afterAll, describe, expect, it } from "vitest"
import { caseNames, runCase } from "./conformance/runner.ts"

const names = caseNames()
let passed = 0

describe("conformance", () => {
  it("finds the cases", () => {
    expect(names.length).toBeGreaterThan(0)
  })

  it.each(names)("%s", (name) => {
    const result = runCase(name)
    expect(result.errors).toEqual([])
    expect(result.actual).toBe(result.expected)
    passed += 1
  })

  afterAll(() => {
    console.log(`conformance: ${passed}/${names.length} cases pass byte for byte`)
  })
})
