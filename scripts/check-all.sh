#!/usr/bin/env bash
# Runs every check the repository has, in the order CI runs them, and stops at the
# first failure. Needs Rust (stable), Node and pnpm.
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
ROADBOOK_CONFORMANCE_OUT="$out/ts" pnpm --filter @roadbook/effect exec vitest run test/conformance >/dev/null
ROADBOOK_CONFORMANCE_OUT="$out/rust" cargo test -q -p roadbook --test conformance >/dev/null
test "$(ls "$out/ts" | wc -l)" -eq "$(ls spec/conformance/cases | wc -l)"
diff -r "$out/ts" "$out/rust"
rm -rf "$out"

step "Viewer: the bundle the CLI embeds is current"
node packages/viewer/build.mjs --check

printf '\nAll checks passed.\n'
