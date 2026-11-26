/** `pnpm --filter @roadbook/effect conformance [case…]`: run the cases and print a line per case. */
import { caseNames, runCase } from "./runner.ts"

const names = process.argv.length > 2 ? process.argv.slice(2) : caseNames()
let passed = 0
for (const name of names) {
  const result = runCase(name)
  const ok = result.errors.length === 0 && result.actual === result.expected
  if (ok) passed += 1
  console.log(`${ok ? "pass" : "FAIL"}  ${name}`)
  for (const error of result.errors) console.log(`      ${error}`)
  if (!ok && process.env["VERBOSE"]) {
    const a = result.actual.split("\n")
    const e = result.expected.split("\n")
    for (let i = 0; i < Math.max(a.length, e.length); i++) {
      if (a[i] !== e[i]) {
        console.log(`      line ${i + 1}\n      expected ${e[i]}\n      actual   ${a[i]}`)
        break
      }
    }
  }
}
console.log(`conformance: ${passed}/${names.length} cases pass byte for byte`)
process.exitCode = passed === names.length ? 0 : 1
