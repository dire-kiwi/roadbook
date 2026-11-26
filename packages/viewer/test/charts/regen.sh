#!/bin/sh
# Regenerates the IR of the charts the viewer's tests lay out, from the repository root.
# `cargo test -p roadbook-cli` fails when one of these files differs from the compiler's output.
set -e
cd "$(dirname "$0")/../../../.."
out=packages/viewer/test/charts
for chart in \
  examples/shop/place_order.road \
  examples/large/ticket.road \
  examples/password-reset/confirm.road \
  examples/webhook/webhook.road \
  examples/import-job/import.road \
  packages/viewer/test/charts/stress.road
do
  name=$(basename "$(dirname "$chart")")-$(basename "$chart" .road)
  cargo run -q -p roadbook-cli -- export ir "$chart" -o "$out/$name.ir.json"
done
