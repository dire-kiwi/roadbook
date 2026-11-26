// The fixture both cores run: crates/roadbook-core/tests/data/redact-malformed.json. The Rust
// core's test (crates/roadbook-core/tests/redact.rs) compares the same expected records.
import * as fs from "node:fs"
import * as path from "node:path"
import { expect, it } from "vitest"
import { canonicalJson, type Fns, type Input, loadIr, start, step } from "../src/index.ts"

const repo = path.resolve(import.meta.dirname, "../../..")
const data = path.join(repo, "crates/roadbook-core/tests/data")
const noFns: Fns = (name) => ({ defect: { kind: "host", message: `unexpected call of ${name}` } })

it("redaction fails closed on malformed values, as the Rust core does", () => {
  const fixture = JSON.parse(fs.readFileSync(path.join(data, "redact-malformed.json"), "utf8"))
  const ir = loadIr(JSON.parse(fs.readFileSync(path.join(repo, fixture.ir), "utf8")))
  const lines: Array<string> = []
  const keep = (commands: ReadonlyArray<{ kind: string }>) => {
    for (const c of commands) if (c.kind === "trace") lines.push(canonicalJson((c as { record: unknown }).record))
  }
  let stepped = start(ir, fixture.machine, "r1", fixture.input, noFns)
  keep(stepped.commands)
  for (const input of fixture.inputs as Array<Input>) {
    stepped = step(ir, stepped.snapshot, input, noFns)
    keep(stepped.commands)
  }
  const actual = lines.map((l) => `${l}\n`).join("")
  const expected = path.join(data, "redact-malformed.jsonl")
  if (process.env["ROADBOOK_BLESS"]) fs.writeFileSync(expected, actual)
  expect(actual).toBe(fs.readFileSync(expected, "utf8"))
})
