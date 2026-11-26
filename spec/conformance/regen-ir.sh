#!/bin/sh
# Regenerates the IR of every chart a conformance case uses, then re-stamps
# the machine hashes in the expected traces from it (the `ir` member of each
# `started` record, and nothing else).
#
#   spec/conformance/regen-ir.sh
#
# Run from anywhere inside the repository after changing a chart or the
# compiler, and commit the result.
set -eu

root=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
cd "$root"

charts=$(sed -n 's/^ *"chart": *"\([^"]*\)".*/\1/p' spec/conformance/cases/*/case.json | sort -u)
for chart in $charts; do
  src="spec/conformance/$chart"
  out="${src%.road}.ir.json"
  echo "$src -> $out"
  cargo run -q -p roadbook-cli -- export ir "$src" > "$out.tmp"
  mv "$out.tmp" "$out"
done

ROADBOOK_BLESS=1 cargo test -q -p roadbook-conformance --test cases ir_when_present
cargo test -q -p roadbook-conformance
