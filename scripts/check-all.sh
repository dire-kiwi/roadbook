#!/usr/bin/env bash
# Runs every check the repository has, in the order CI runs them, and stops at the
# first failure. Needs Rust (stable), Node and pnpm, and the tree-sitter CLI for the grammar's
# checks, which are skipped without it.
#
#   scripts/check-all.sh
set -euo pipefail
cd "$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"

step() { printf '\n==> %s\n' "$*"; }
roadbook() { cargo run -q -p roadbook-cli -- "$@"; }

step "Rust: format"
cargo fmt --all --check

step "Rust: clippy, warnings denied"
cargo clippy --workspace --all-targets -- -D warnings

step "Rust: tests"
cargo test --workspace

step "Charts: check every chart in the project (roadbook.toml at the root)"
roadbook check

step "Charts: canonical formatting of the examples and the conformance charts"
roadbook fmt --check examples spec/conformance/charts

step "Charts: every generated TypeScript module is current, and none is left over"
roadbook gen --check

step "Conformance: the committed IR and hashes are what the compiler produces now"
sh spec/conformance/regen-ir.sh >/dev/null
git diff --exit-code -- spec/conformance

step "TypeScript: install"
pnpm install --frozen-lockfile

step "TypeScript: typecheck"
pnpm -r typecheck

step "TypeScript: tests"
pnpm -r test

step "Conformance: the TypeScript and Rust runtimes write identical traces"
out=$(mktemp -d)
mkdir -p "$out/ts" "$out/rust"
ROADBOOK_CONFORMANCE_OUT="$out/ts" pnpm --filter @dire-kiwi/roadbook-effect exec vitest run test/conformance >/dev/null
ROADBOOK_CONFORMANCE_OUT="$out/rust" cargo test -q -p roadbook --test conformance >/dev/null
# A runtime skips the cases that require a feature it does not implement yet (`requires`).
# Each must run every case whose features it shows it supports, by running some case that
# requires them, and the two must write the same trace for every case both ran.
node - "$out" <<'JS'
const fs = require("node:fs")
const path = require("node:path")
const [out] = process.argv.slice(2)
const dir = "spec/conformance/cases"
const cases = fs.readdirSync(dir).filter((c) => fs.existsSync(path.join(dir, c, "case.json")))
const requires = (c) => JSON.parse(fs.readFileSync(path.join(dir, c, "case.json"), "utf8")).requires
let failed = false
const ran = {}
for (const runtime of ["ts", "rust"]) {
  ran[runtime] = new Set(
    fs.readdirSync(path.join(out, runtime)).filter((f) => f.endsWith(".jsonl")).map((f) => f.replace(/\.jsonl$/, ""))
  )
  const supported = new Set([...ran[runtime]].flatMap(requires))
  for (const c of cases) {
    if (!ran[runtime].has(c) && requires(c).every((f) => supported.has(f))) {
      console.error(`${runtime} skipped ${c}, which needs only features it supports`)
      failed = true
    }
  }
  console.log(`${runtime}: ran ${ran[runtime].size} of ${cases.length} cases`)
}
const both = cases.filter((c) => ran.ts.has(c) && ran.rust.has(c))
for (const c of both) {
  const [a, b] = ["ts", "rust"].map((r) => fs.readFileSync(path.join(out, r, `${c}.jsonl`), "utf8"))
  if (a !== b) {
    console.error(`the runtimes write different traces for ${c}`)
    failed = true
  }
}
console.log(`identical traces for the ${both.length} cases both ran`)
// The TypeScript durable driver (`durable/`) must write the same traces as both, for the cases
// it runs: all but those with `suspend` steps.
const durableDir = path.join(out, "ts", "durable")
const durable = fs.readdirSync(durableDir).filter((f) => f.endsWith(".jsonl")).map((f) => f.replace(/\.jsonl$/, ""))
for (const c of durable) {
  if (!ran.rust.has(c)) continue
  if (fs.readFileSync(path.join(durableDir, `${c}.jsonl`), "utf8") !== fs.readFileSync(path.join(out, "rust", `${c}.jsonl`), "utf8")) {
    console.error(`the durable driver writes a different trace for ${c}`)
    failed = true
  }
}
const withoutSuspend = cases.filter((c) => !fs.readFileSync(path.join(dir, c, "case.json"), "utf8").includes('"suspend"'))
if (durable.length !== withoutSuspend.length) {
  console.error(`the durable driver ran ${durable.length} cases, not the ${withoutSuspend.length} without suspend steps`)
  failed = true
}
console.log(`identical traces from the durable driver for its ${durable.length} cases`)
process.exit(failed ? 1 : 0)
JS
rm -rf "$out"

step "Viewer: the bundle the CLI embeds is current"
node packages/viewer/build.mjs --check

step "npm CLI: the committed WebAssembly compiler (packages/cli/wasm) is current"
node packages/cli/build.mjs --check

step "Editors: the tree-sitter grammar (generated parser, corpus, every chart, queries)"
if command -v tree-sitter >/dev/null; then
  editors/tree-sitter-roadbook/check.sh
else
  echo "skipped: the tree-sitter CLI is not installed (npm install -g tree-sitter-cli@0.26.9)"
fi

printf '\nAll checks passed.\n'
