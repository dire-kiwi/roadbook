# Cross-crate chart lookup probe

Throwaway. It answers one question from [actors and messaging](../../docs/design/actors.md#across-crates-and-packages): how does `roadbook::include!`, expanding in crate `orders`, find the `.road` file of a dependency `inventory`, so that `import { Inventory } from "inventory/inventory.road"` works and the generated code uses `inventory::Event` rather than a copy? Delete this directory when real cross-crate support lands.

There is no compiler here. `road-probe-macro` is a stand-in that reads the chart, finds package-qualified imports, resolves each package by one of four mechanisms and expands to consts (`PATH`, `SOURCE`, `MICROS`, …) plus `pub type Event = <dependency>::Event`. `orders` builds one module per mechanism, each behind a feature, and prints a `RESULT` line. `same-as-compiled` compares the chart text the macro read with the text `inventory` itself was compiled from.

```sh
spike/cross-crate/run.sh     # about 15 s; deletes and recreates spike/cross-crate/work/
```

The script needs no network and publishes nothing. It uses its own `CARGO_HOME` under `work/` and points `CARGO_HTTP_PROXY` at a closed port, so any attempt to reach crates.io fails at once. Dependency kinds: a path dependency; a git dependency on a local `file://` repository; a directory source as `cargo vendor` makes; and a local registry, whose sources Cargo unpacks into `CARGO_HOME/registry/src` as it does for crates.io. Checked with Cargo and rustc 1.99.1.

## Mechanisms

1. **metadata**: the macro runs `$CARGO metadata --format-version 1 --manifest-path $CARGO_MANIFEST_DIR/Cargo.toml`, finds its own resolve node, and takes the dependency's `manifest_path` and its extern name (`deps[].name`, which includes renames). The result is cached per crate in a `OnceLock`.
2. **env**: `inventory` declares `links = "roadbook-inventory"` and its `build.rs` prints `cargo::metadata=road_dir=$CARGO_MANIFEST_DIR`. `orders`'s generic `build.rs` turns every `DEP_*_ROAD_DIR` into `cargo::rustc-env=ROADBOOK_DIR_<package>=…`, which the macro reads with `std::env::var`.
3. **toml**: `roadbook.toml` next to `orders/Cargo.toml` maps package names to directories. The macro also `include_bytes!`s the file so that editing it re-expands.
4. **callback** (found while probing): no file lookup at all. `inventory`'s own include macro embeds its chart and exports a hidden `macro_rules! __roadbook_chart_inventory_road`. The dependent's macro expands to `::inventory::__roadbook_chart_inventory_road!{ ::road_probe_macro::__include_road_continue { <original input> } }`. The dependency's macro calls back with `dep { package = "inventory", krate = $crate, path = …, source = … }` appended. `$crate` arrives as a token and names the dependency however the dependent refers to it.

Mechanisms 1 to 3 make Cargo rebuild by expanding to `include_str!("<absolute path>")`. `proc_macro::tracked::path` exists in 1.99 but is still unstable (`proc_macro_tracked_path`, rust-lang/rust#99515). Mechanism 4 needs nothing extra: when the chart changes, `inventory` is rebuilt, and Cargo then rebuilds `orders`.

## Results

| | path | git | vendor | registry | downstream: `app` → `orders` (git or registry) → `inventory` | renamed dependency | cost per crate |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 metadata | ok | ok | ok | ok | **git: reads the wrong revision** once the dependency has moved, and writes a `Cargo.lock` into Cargo's checkout. **registry: fails**, because it tries crates.io; with the replacement in global config it works but writes a `Cargo.lock` into `registry/src`. With `--locked` it fails. | ok (`crate=inv`) | 25–45 ms cold, 8 ms warm on this graph; 100 ms and 190 ms for `cargo metadata` on local 274- and 614-package graphs |
| 2 env (`links`) | ok | ok | ok | ok | ok | fails | ≈ 0, plus a build script run |
| 3 toml | ok | reads the sibling working copy, not the compiled one | same | same | fails: the relative path does not exist in the checkout | fails | ≈ 0 |
| 4 callback | ok | ok | ok | ok | ok, including after the dependency has moved | fails | ≈ 0 |

Also checked by `run.sh`:

- **Rebuilds.** For every mechanism, a no-op build does not recompile `orders`, and editing `inventory.road` (path dependency) recompiles it with the new text. After a new commit and `cargo update -p inventory`, mechanisms 1, 2 and 4 see the new revision. Mechanism 3 still reads the local copy.
- **Locks.** Nothing blocked, and `cargo metadata` never printed "Blocking waiting for file lock". This held under `cargo build`, `cargo check`, and 8 crates expanding in parallel with `-j8`. The outer `--offline`, `--locked` and `--frozen` all work, including `--frozen` from an empty target directory. The inner `cargo metadata` does not receive those flags, but it needs nothing in these cases.
- **`--config` and config discovery.** The inner `cargo metadata` does not inherit `--config` flags from the outer command line: with source replacement given only by `--config`, mechanism 1 fails and the others work. rustc, and therefore the macro, runs with its cwd set to the package root for non-workspace dependencies. Cargo discovers config from the cwd, so a downstream crate's inner `cargo metadata` misses the project's `.cargo/config.toml`. This is why the downstream registry case went to crates.io.
- **Resolution drift.** In a downstream crate there is no lock file next to the manifest, so the inner `cargo metadata` resolves on its own. `app`'s `Cargo.lock` pinned inventory rev 1 while the macro read rev 2 (`same-as-compiled=NO`).
- **`links` is exclusive.** Depending on `inventory` 0.1 and 0.2 together fails to resolve: "package `inventory` links to the native library `roadbook-inventory`, but it conflicts with a previous package". Mechanism 2 therefore rules out two semver-incompatible versions of a chart-owning crate in one graph. It also needs a build script in every chart owner and in every crate that imports from one. `DEP_*` variables reach only the build scripts of direct dependents, never the macro (`DEP_*-visible-to-macro=false`). `cargo::metadata=` needs Cargo 1.77 or later; older Cargo only accepts `cargo:road_dir=…`, which fits Roadbook's MSRV of 1.85.
- **Renames.** With `inv = { package = "inventory", … }`, only mechanism 1 learns the name `inv`. The others emit `::inventory::…` and fail to compile. A proc macro runs inside rustc and can read rustc's `--extern inv=…/libinventory-….rmeta` flags with `std::env::args()` (shown in `externs=[…]`), but that is not a supported interface.

## Recommendation for `roadbook::include!`

Use the callback (mechanism 4). Every `roadbook::include!` already embeds the IR, and it also exports a hidden `#[macro_export] macro_rules! __roadbook_chart_<mangled path>` that passes the IR and `$crate` to a callback. An importing chart's macro expands to a call into the owning crate's macro and finishes in the continuation. It reads no files and needs no build script, `links` value, Cargo subprocess or configuration. It works for every source kind and for downstream builds, it always sees exactly the version that is compiled and linked, and Cargo's normal dependency tracking rebuilds it.

**Main caveat:** the first call must name the dependency's crate. The macro uses the package name with `-` replaced by `_`. A renamed dependency needs an explicit alias in the call, such as `roadbook::include!("orders.road", crates(inventory = inv))`. Otherwise the error is "cannot find `inventory` in the list of imported crates", which the docs should explain. Two smaller points:

- The owning crate must itself use `roadbook::include!`, which is the design anyway.
- The IR passed between crates must carry its format version, so that a dependency built with a different Roadbook version is rejected with a clear message.

Not the macro's job:

- **Tools outside a build** (CLI, viewer, project index, language server) need the dependency's files on disk. They should use `cargo metadata`, which is safe outside a build.
- **The `build.rs` route** cannot see a dependency's macros. There, mechanism 2 is the natural fit, because `DEP_*` arrives at the dependent's build script directly. It brings the `links` cost: a build script in the owner and only one version of it per graph. Simplest is to make `build.rs` generation cover charts without cross-crate imports and keep cross-crate imports on the macro.

Mechanism 1 inside a macro should not be used. It can reach the network, write into Cargo's caches and read a different version from the one being compiled. Mechanism 3 only suits path dependencies and silently reads the wrong file otherwise.

## TypeScript (reasoning only)

The code generator resolves `"@shop/inventory/inventory.road"` the way Node would from the importing file, with `createRequire(importingFile).resolve(spec)` or `import.meta.resolve`. This gives the same copy of the package that the importing package loads at run time, including with pnpm symlinks, workspaces and duplicate versions, so the drift seen with mechanism 1 cannot happen. If the owning package has an `exports` map, the map must expose its charts and generated modules, for example `"./*.road": "./*.road"` and `"./*.road.js": …`. Otherwise resolution fails with `ERR_PACKAGE_PATH_NOT_EXPORTED`. The generated file then imports `Event` and `Ref` types from the owning package's generated module, which TypeScript resolves through the same `exports` under `moduleResolution: node16` or `bundler`. Yarn Plug'n'Play needs the generator to run under Yarn's resolver (`yarn node` or the `pnpapi` module).

## Files

- `road-probe-macro/`: `include_road!`, `__include_road_continue!` and `export_road!`, plus just enough JSON parsing for `cargo metadata`, with no dependencies.
- `inventory/`: owns `inventory.road`. Its `build.rs` and `links` exist for mechanism 2.
- `orders/`: `orders.road` imports `Inventory`. It has a forwarding `build.rs` for mechanism 2 and a `roadbook.toml` for mechanism 3.
- `app/`: the downstream consumer.
- `run.sh`: builds copies of these under `work/` for every scenario.
