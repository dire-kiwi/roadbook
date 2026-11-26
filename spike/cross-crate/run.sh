#!/usr/bin/env bash
# Runs every scenario of the cross-crate probe and prints a results table.
#
# Hermetic: everything goes under work/ (deleted first), with its own CARGO_HOME. Nothing
# is fetched from the network or published anywhere. The git dependencies are local
# file:// repositories, the "registry" dependencies are a cargo-vendor style directory
# source and a local-registry (whose sources Cargo unpacks into CARGO_HOME/registry/src,
# as it does for crates.io).
set -uo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
W=${WORK:-$HERE/work}
rm -rf "$W"
mkdir -p "$W/logs"
export CARGO_HOME=$W/cargo-home
export CARGO_TERM_COLOR=never
# Guard: any attempt to reach crates.io (or anything over HTTP) fails at once instead of
# fetching. file:// git repositories are unaffected.
export CARGO_HTTP_PROXY=http://127.0.0.1:9 CARGO_NET_RETRY=0
unset CARGO_TARGET_DIR RUSTFLAGS ROAD_PROBE_METADATA_ARGS
RESULTS=$W/results.tsv
: >"$RESULTS"
MECHS="metadata env toml callback"
GIT="git -c user.name=probe -c user.email=probe@example.invalid -c init.defaultBranch=main"

# ---- helpers ------------------------------------------------------------------------------

# record SCENARIO MECH LOG: one row of the table from a run's output.
record() {
    local scen=$1 mech=$2 log=$3 row
    if row=$(grep -m1 '^RESULT' "$log"); then
        local copy rev cons us crate note
        copy=$(sed -E 's/.* copy=([^ ]*).*/\1/' <<<"$row")
        rev=$(sed -E 's/.* rev=([^ ]*).*/\1/' <<<"$row")
        cons=$(sed -E 's/.* consistent=([^ ]*).*/\1/' <<<"$row")
        us=$(sed -E 's/.* us=([^ ]*).*/\1/' <<<"$row")
        crate=$(sed -E 's/.* crate=([^ ]*) .*/\1/' <<<"$row")
        note=$(grep -o 'WROTE-Cargo.lock-in-package-dir\|Blocking[^|]*' <<<"$row" | head -1)
        printf '%s\t%s\tok\tread=%s rev=%s same-as-compiled=%s first-expansion=%sus crate=%s %s\n' \
            "$scen" "$mech" "$copy" "$rev" "$cons" "$us" "$crate" "$note" >>"$RESULTS"
    else
        local err
        err=$(grep -m1 -E 'road-probe:|^error' "$log" | sed -E "s/^error(\[E[0-9]+\])?: //; s|$W|work|g" | cut -c1-170)
        printf '%s\t%s\tFAIL\t%s\n' "$scen" "$mech" "${err:-see $log}" >>"$RESULTS"
    fi
}

# run_all SCENARIO DIR [cargo args…]: build and run `orders`/`app` once per mechanism.
run_all() {
    local scen=$1 dir=$2
    shift 2
    for m in $MECHS; do
        (cd "$dir" && timeout 300 cargo run -q --features "$m" "$@") >"$W/logs/$scen-$m.log" 2>&1
        record "$scen" "$m" "$W/logs/$scen-$m.log"
    done
}

# copy_inventory DEST MARKER: a copy of `inventory` whose chart says which copy it is.
copy_inventory() {
    mkdir -p "$1"
    cp -r "$HERE/inventory/." "$1/"
    sed -i "s|^// copy: .*|// copy: $2|" "$1/inventory.road"
}

# orders_pkg DIR DEPENDENCY-LINES…: a standalone `orders` package with the given dependencies.
orders_pkg() {
    local dir=$1
    shift
    mkdir -p "$dir"
    cp -r "$HERE/orders/build.rs" "$HERE/orders/orders.road" "$HERE/orders/src" "$dir/"
    {
        printf '[package]\nname = "orders"\nversion = "0.1.0"\nedition = "2024"\nbuild = "build.rs"\n\n'
        printf '[features]\nmetadata = []\nenv = []\ntoml = []\ncallback = []\n\n[dependencies]\n'
        printf '%s\n' "$@"
        printf '\n[workspace]\n'
    } >"$dir/Cargo.toml"
    # The author's mapping: a sibling working copy of inventory, as a developer would have.
    printf '[packages]\ninventory = "%s"\n' "$(realpath --relative-to="$dir" "$W/src/inventory")" >"$dir/roadbook.toml"
}

# Registry form of a manifest: path dependencies become plain version requirements.
normalize() { sed -E -i 's|\{ version = "0.1", path = "[^"]*" \}|"0.1"|; /^publish = false/d' "$1"; }

sha() { sha256sum "$1" | cut -d' ' -f1; }

# publish_local NAME VERSION SRC-DIR DEPS-JSON FEATURES-JSON LINKS-JSON: add a crate to the
# local registry ($W/registry) and to the vendor directory ($W/vendor).
publish_local() {
    local name=$1 ver=$2 src=$3 deps=$4 feats=$5 links=$6 stage=$W/stage/$1-$2
    mkdir -p "$stage" "$W/registry/index" "$W/vendor"
    cp -r "$src/." "$stage/"
    rm -rf "$stage/target" "$stage/Cargo.lock"
    normalize "$stage/Cargo.toml"
    tar -czf "$W/registry/$name-$ver.crate" -C "$W/stage" "$name-$ver"
    local idx="$W/registry/index/${name:0:2}/${name:2:2}"
    mkdir -p "$idx"
    printf '{"name":"%s","vers":"%s","deps":%s,"cksum":"%s","features":%s,"yanked":false,"links":%s}\n' \
        "$name" "$ver" "$deps" "$(sha "$W/registry/$name-$ver.crate")" "$feats" "$links" >>"$idx/$name"
    if [ "$ver" = 0.1.0 ]; then
        cp -r "$stage" "$W/vendor/$name"
        printf '{"files":{},"package":"%s"}' "$(sha "$W/registry/$name-$ver.crate")" >"$W/vendor/$name/.cargo-checksum.json"
    fi
}
dep() { printf '{"name":"%s","req":"^0.1","features":[],"optional":false,"default_features":true,"target":null,"kind":"normal"}' "$1"; }

use_source() { # use_source DIR KIND PATH: replace crates.io for this scenario only
    mkdir -p "$1/.cargo"
    printf '[source.crates-io]\nreplace-with = "probe"\n\n[source.probe]\n%s = "%s"\n' "$2" "$3" >"$1/.cargo/config.toml"
}

# ---- sources --------------------------------------------------------------------------------

mkdir -p "$W/src"
cp -r "$HERE/road-probe-macro" "$W/src/road-probe-macro"
rm -rf "$W/src/road-probe-macro/target"
copy_inventory "$W/src/inventory" path

# git: repository A holds the macro and inventory; repository B holds orders (downstream test).
A=$W/git/inventory-repo
mkdir -p "$A"
cp -r "$W/src/road-probe-macro" "$A/"
copy_inventory "$A/inventory" git
(cd "$A" && $GIT init -q && $GIT add -A && $GIT commit -qm "inventory 0.1.0")

# vendor directory and local registry: the macro, inventory 0.1.0 and 0.2.0, orders.
copy_inventory "$W/stage-src/inventory" registry
publish_local road-probe-macro 0.1.0 "$W/src/road-probe-macro" '[]' '{}' null
publish_local inventory 0.1.0 "$W/stage-src/inventory" "[$(dep road-probe-macro)]" '{}' '"roadbook-inventory"'
sed -i 's|^// copy: .*|// copy: vendor|' "$W/vendor/inventory/inventory.road"
sed -i 's|^version = "0.1.0"|version = "0.2.0"|' "$W/stage-src/inventory/Cargo.toml"
publish_local inventory 0.2.0 "$W/stage-src/inventory" "[$(dep road-probe-macro)]" '{}' '"roadbook-inventory"'
orders_pkg "$W/stage-src/orders" 'road-probe-macro = { version = "0.1", path = "x" }' 'inventory = { version = "0.1", path = "x" }'
printf '[packages]\ninventory = "../inventory"\n' >"$W/stage-src/orders/roadbook.toml"
sed -i '/^\[workspace\]/d' "$W/stage-src/orders/Cargo.toml"
publish_local orders 0.1.0 "$W/stage-src/orders" "[$(dep road-probe-macro),$(dep inventory)]" \
    '{"metadata":[],"env":[],"toml":[],"callback":[]}' null

MACRO_PATH="road-probe-macro = { path = \"../src/road-probe-macro\" }"

# ---- scenarios ------------------------------------------------------------------------------

echo "== path dependency"
orders_pkg "$W/s-path" "$MACRO_PATH" 'inventory = { path = "../src/inventory" }'
run_all path "$W/s-path"

echo "== git dependency (file:// repository)"
orders_pkg "$W/s-git" "$MACRO_PATH" "inventory = { git = \"file://$A\" }"
run_all git "$W/s-git"

echo "== vendored directory source"
orders_pkg "$W/s-vendor" 'road-probe-macro = "0.1"' 'inventory = "0.1"'
use_source "$W/s-vendor" directory "$W/vendor"
run_all vendor "$W/s-vendor"

echo "== registry (local-registry, unpacked into CARGO_HOME like crates.io)"
orders_pkg "$W/s-registry" 'road-probe-macro = "0.1"' 'inventory = "0.1"'
use_source "$W/s-registry" local-registry "$W/registry"
run_all registry "$W/s-registry"

echo "== renamed dependency: inv = { package = \"inventory\" }"
orders_pkg "$W/s-rename" "$MACRO_PATH" 'inv = { package = "inventory", path = "../src/inventory" }'
run_all rename "$W/s-rename"

echo "== downstream: app -> orders (git) -> inventory (git); no Cargo.lock in the checkout"
B=$W/git/orders-repo
orders_pkg "$B/orders" "road-probe-macro = { git = \"file://$A\" }" "inventory = { git = \"file://$A\" }"
printf '[packages]\ninventory = "../inventory"\n' >"$B/orders/roadbook.toml"
sed -i '/^\[workspace\]/d' "$B/orders/Cargo.toml"
(cd "$B" && $GIT init -q && $GIT add -A && $GIT commit -qm "orders 0.1.0")
mkdir -p "$W/d-git/src"
cp "$HERE/app/src/main.rs" "$W/d-git/src/"
sed -E "s|^orders = .*|orders = { git = \"file://$B\" }|" "$HERE/app/Cargo.toml" >"$W/d-git/Cargo.toml"
printf '\n[workspace]\n' >>"$W/d-git/Cargo.toml"
run_all downstream-git "$W/d-git"

echo "== downstream: app -> orders (registry) -> inventory (registry)"
mkdir -p "$W/d-registry/src"
cp "$HERE/app/src/main.rs" "$W/d-registry/src/"
sed -E 's|^orders = .*|orders = "0.1"|' "$HERE/app/Cargo.toml" >"$W/d-registry/Cargo.toml"
printf '\n[workspace]\n' >>"$W/d-registry/Cargo.toml"
use_source "$W/d-registry" local-registry "$W/registry"
run_all downstream-registry "$W/d-registry"

echo "== downstream registry again, with the source replacement in CARGO_HOME/config.toml"
cp "$W/d-registry/.cargo/config.toml" "$CARGO_HOME/config.toml"
(cd "$W/d-registry" && cargo clean -q -p orders && timeout 300 cargo run -q --features metadata) >"$W/logs/down-global.log" 2>&1
record "downstream-registry, global config" metadata "$W/logs/down-global.log"
locks=$(find "$CARGO_HOME/git/checkouts" "$CARGO_HOME/registry/src" -name Cargo.lock 2>/dev/null | sed "s|$CARGO_HOME|\$CARGO_HOME|" | tr '\n' ' ')
printf 'downstream\tmetadata\tnote\tCargo.lock files the macro left in CARGO_HOME: %s\n' "${locks:-none}" >>"$RESULTS"
# The same with the macro's `cargo metadata` made read-only (--locked), so it cannot write
# a lock file into Cargo's source cache.
find "$CARGO_HOME/registry/src" -name Cargo.lock -delete
(cd "$W/d-registry" && cargo clean -q -p orders && ROAD_PROBE_METADATA_ARGS="--locked" timeout 300 cargo run -q --features metadata) \
    >"$W/logs/down-locked.log" 2>&1
record "downstream-registry, macro passes --locked" metadata "$W/logs/down-locked.log"
rm "$CARGO_HOME/config.toml"

echo "== two semver-incompatible versions of inventory (needs links to be unique)"
orders_pkg "$W/s-links" 'road-probe-macro = "0.1"' 'inventory = "0.1"' 'inventory02 = { package = "inventory", version = "0.2" }'
use_source "$W/s-links" local-registry "$W/registry"
(cd "$W/s-links" && timeout 300 cargo build -q --features callback) >"$W/logs/links.log" 2>&1
if grep -q 'links to the native library' "$W/logs/links.log"; then
    printf 'links\tany\tFAIL\t%s\n' "$(grep -m1 'links to the native library' "$W/logs/links.log" | cut -c1-150)" >>"$RESULTS"
else
    printf 'links\tany\tok\tboth versions built\n' >>"$RESULTS"
fi

echo "== rebuilds: no-op build, then edit inventory.road (path dependency)"
mkdir -p "$W/rebuild"
cp -r "$W/src/road-probe-macro" "$W/rebuild/"
copy_inventory "$W/rebuild/inventory" path
orders_pkg "$W/rebuild/orders" 'road-probe-macro = { path = "../road-probe-macro" }' 'inventory = { path = "../inventory" }'
printf '[packages]\ninventory = "../inventory"\n' >"$W/rebuild/orders/roadbook.toml"
for m in $MECHS; do
    road=$W/rebuild/inventory/inventory.road
    sed -i 's|^// rev: .*|// rev: 1|' "$road"
    (cd "$W/rebuild/orders" && cargo run -q --features "$m") >/dev/null 2>&1
    noop=$(cd "$W/rebuild/orders" && cargo build -v --features "$m" 2>&1 | grep -c 'Running `.*rustc --crate-name orders')
    sleep 1 # make the edit's mtime visibly newer
    sed -i 's|^// rev: .*|// rev: 2|' "$road"
    (cd "$W/rebuild/orders" && cargo run -q --features "$m") >"$W/logs/rebuild-$m.log" 2>&1
    rev=$(grep -o ' rev=[^ ]*' "$W/logs/rebuild-$m.log" | head -1)
    printf 'rebuild\t%s\t%s\tno-op build recompiled orders %s times; after edit:%s\n' "$m" \
        "$([ "$noop" = 0 ] && [ "$rev" = " rev=2" ] && echo ok || echo FAIL)" "$noop" "${rev:- (no output)}" >>"$RESULTS"
done

echo "== git dependency moves to a new commit (cargo update)"
sed -i 's|^// rev: .*|// rev: 2|' "$A/inventory/inventory.road"
(cd "$A" && $GIT commit -qam "inventory rev 2")
(cd "$W/s-git" && cargo update -q -p inventory) >/dev/null 2>&1
run_all git-updated "$W/s-git"

echo "== downstream git: app's lock pins inventory rev 1, repository A has moved to rev 2"
# What a fresh machine sees: no Cargo.lock in orders' checkout yet, so the macro's own
# `cargo metadata` resolves the branch head while the build compiles the pinned commit.
find "$CARGO_HOME/git/checkouts" -path '*orders-repo*' -name Cargo.lock -delete
for m in metadata callback; do
    (cd "$W/d-git" && cargo clean -q -p orders && timeout 300 cargo run -q --features "$m") >"$W/logs/down-stale-$m.log" 2>&1
    record "downstream-git, dependency moved" "$m" "$W/logs/down-stale-$m.log"
done

echo "== cargo flags on the outer build (git dependency, mechanism 1)"
for flag in --offline --locked --frozen; do
    (cd "$W/s-git" && cargo clean -q -p orders && timeout 300 cargo run -q --features metadata $flag) >"$W/logs/flag$flag.log" 2>&1
    record "git $flag" metadata "$W/logs/flag$flag.log"
done
(cd "$W/s-git" && cargo clean -q -p orders && timeout 300 cargo check -q --features metadata) >"$W/logs/check.log" 2>&1
printf 'git, cargo check\tmetadata\t%s\tno lock contention with the running cargo check\n' "$([ $? = 0 ] && echo ok || echo FAIL)" >>"$RESULTS"
(cd "$W/s-git" && rm -rf target && timeout 300 cargo run -q --features metadata --frozen) >"$W/logs/flag-frozen-clean.log" 2>&1
record "git --frozen, empty target" metadata "$W/logs/flag-frozen-clean.log"

echo "== source replacement given only on the command line (--config)"
orders_pkg "$W/s-cliconfig" 'road-probe-macro = "0.1"' 'inventory = "0.1"'
for m in $MECHS; do
    (cd "$W/s-cliconfig" && CARGO_NET_OFFLINE=true timeout 300 cargo run -q --features "$m" \
        --config 'source.crates-io.replace-with="probe"' --config "source.probe.directory=\"$W/vendor\"") \
        >"$W/logs/cliconfig-$m.log" 2>&1
    record "vendor via --config" "$m" "$W/logs/cliconfig-$m.log"
done

echo "== cost: 8 crates in one workspace expanding in parallel"
P=$W/s-parallel
mkdir -p "$P"
members=""
for i in 1 2 3 4 5 6 7 8; do
    orders_pkg "$P/o$i" 'road-probe-macro = { path = "../../src/road-probe-macro" }' 'inventory = { path = "../../src/inventory" }'
    sed -i "s|^name = \"orders\"|name = \"o$i\"|; /^\[workspace\]/d" "$P/o$i/Cargo.toml"
    rm "$P/o$i/src/main.rs"
    members="$members\"o$i\", "
done
printf '[workspace]\nresolver = "3"\nmembers = [%s]\n' "$members" >"$P/Cargo.toml"
(cd "$P" && cargo build -q --workspace) >/dev/null 2>&1 # build dependencies once
for m in $MECHS; do
    (cd "$P" && cargo clean -q $(printf -- '-p o%s ' 1 2 3 4 5 6 7 8))
    start=$(date +%s%N)
    (cd "$P" && timeout 300 cargo build -q --workspace --features "$m") >"$W/logs/parallel-$m.log" 2>&1
    status=$?
    ms=$((($(date +%s%N) - start) / 1000000))
    printf 'parallel x8\t%s\t%s\tbuilding the 8 crates took %sms\n' "$m" "$([ $status = 0 ] && echo ok || echo FAIL)" "$ms" >>"$RESULTS"
done

# ---- table --------------------------------------------------------------------------------

echo
echo "Results (work directory: $W)"
sed "s|$W|work|g" "$RESULTS" | column -t -s $'\t' 
