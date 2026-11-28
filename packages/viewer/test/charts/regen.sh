#!/bin/sh
# Regenerates, from the repository root, what the viewer's tests read from the compiler:
# - test/charts/*.ir.json: the IR of the charts the tests lay out;
# - test/mermaid/*.md: `roadbook export mermaid` of every chart under examples/,
#   spec/conformance/charts/ and test/charts/ that declares a machine, which the tests run
#   through Mermaid's parser.
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
  examples/upload/publish.road \
  examples/orders/orders.road \
  packages/viewer/test/charts/stress.road \
  packages/viewer/test/charts/edges.road \
  packages/viewer/test/charts/escape.road \
  packages/viewer/test/charts/history.road \
  packages/viewer/test/charts/spawn.road \
  packages/viewer/test/charts/tasks.road
do
  name=$(basename "$(dirname "$chart")")-$(basename "$chart" .road)
  cargo run -q -p roadbook-cli -- export ir "$chart" -o "$out/$name.ir.json"
done

mermaid=packages/viewer/test/mermaid
rm -rf "$mermaid"
mkdir -p "$mermaid"
for chart in $(find examples spec/conformance/charts packages/viewer/test/charts -name '*.road' | sort)
do
  name=$(echo "${chart%.road}" | tr / -)
  cargo run -q -p roadbook-cli -- export mermaid "$chart" -o "$mermaid/$name.md" 2>/dev/null
  # A file of declarations only has no diagram.
  [ -s "$mermaid/$name.md" ] || rm "$mermaid/$name.md"
done
