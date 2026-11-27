/**
 * `pnpm --filter @dire-kiwi/roadbook-effect conformance [--durable] [case…]`: run the cases and print a
 * line per case; with `--durable`, through the durable driver.
 */
import { caseNames, isDurable, isSupported, runCase } from "./runner.ts"

const args = process.argv.slice(2)
const durable = args.includes("--durable")
const given = args.filter((arg) => arg !== "--durable")
const names = given.length > 0 ? given : caseNames()
let passed = 0
let skipped = 0
for (const name of names) {
  if (!isSupported(name) || (durable && !isDurable(name))) {
    skipped += 1
    console.log(`skip  ${name}`)
    continue
  }
  const result = await runCase(name, durable)
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
console.log(`conformance: ${passed}/${names.length - skipped} cases pass byte for byte, ${skipped} skipped`)
process.exitCode = passed === names.length - skipped ? 0 : 1
