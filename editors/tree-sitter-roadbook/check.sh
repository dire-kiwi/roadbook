#!/usr/bin/env bash
# Checks the tree-sitter grammar: the committed parser is what grammar.js generates, the corpus
# tests pass, every chart in the repository parses with no ERROR or MISSING node (except the
# compiler's fixtures that fail parsing on purpose), and the queries load. Needs the
# tree-sitter CLI (0.26) and a C compiler.
#
#   editors/tree-sitter-roadbook/check.sh
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$(git -C "$here" rev-parse --show-toplevel)"
cd "$here"

step() { printf '\n--> %s\n' "$*"; }

# A private tree-sitter configuration, so that the CLI neither warns about a missing one nor
# reads the user's.
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
printf '{"parser-directories": ["%s"]}\n' "$(dirname "$here")" >"$work/config.json"
export TREE_SITTER_DIR="$work"
export TREE_SITTER_LIBDIR="$work/lib"
mkdir -p "$work/lib"

step "the committed parser is what grammar.js generates"
tree-sitter generate --output "$work/src"
if ! diff -r -q src "$work/src"; then
  echo "src/ is stale: run \`tree-sitter generate\` in editors/tree-sitter-roadbook and commit it" >&2
  exit 1
fi

step "corpus tests"
tree-sitter test

step "every chart in the repository parses cleanly"
# A chart whose expected diagnostics include a syntax error (RB00xx) fails parsing on purpose.
: >"$work/charts"
skipped=0
while IFS= read -r f; do
  if grep -q '// expect: RB00' "$root/$f"; then
    skipped=$((skipped + 1))
  else
    printf '%s\n' "$root/$f" >>"$work/charts"
  fi
done < <(git -C "$root" ls-files '*.road')
total=$(wc -l <"$work/charts")
if ! out="$(tree-sitter parse -p . -q --paths "$work/charts" 2>&1)" \
  || grep -E 'ERROR|MISSING' <<<"$out"; then
  printf '%s\n' "$out" >&2
  echo "some charts do not parse cleanly" >&2
  exit 1
fi
echo "$total charts parse with no ERROR or MISSING node ($skipped skipped: they fail parsing on purpose)"

step "the queries load, and highlight the shop example"
for q in queries/*.scm; do
  tree-sitter query -p . -q "$q" "$root/examples/shop/checkout.road" >/dev/null
done
html="$(tree-sitter highlight -p . --html --css-classes "$root"/examples/shop/*.road)"
for class in keyword type function comment string number operator; do
  if ! grep -q "class='$class'" <<<"$html"; then
    echo "highlighting the shop example gives no \`$class\`" >&2
    exit 1
  fi
done
echo "ok"
