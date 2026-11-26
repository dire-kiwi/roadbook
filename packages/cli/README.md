# @roadbook/cli

The Roadbook compiler for Node: the `roadbook` command and an API for build tools, on the compiler built as WebAssembly from the Rust crates ([`crates/roadbook-wasm`](../../crates/roadbook-wasm)). Installing it needs no Rust toolchain: the built module is committed in `wasm/`.

The package is `private` until its name is settled; nothing is published.

## The command

```
roadbook check [--json] [--warn invoke-timeout] [paths…]
roadbook fmt [--check] [paths…]
roadbook gen [--check] [paths…]
roadbook export ir <file> [-o <file>]
```

These print what the native command line (`crates/roadbook-cli`) prints, byte for byte, on standard output and standard error, and exit with the same codes: 0 for no errors, 1 when the input has errors (or for `--check`, is not formatted or generated), 2 for bad usage or an I/O error. The project is found and read the same way: the nearest `roadbook.toml` upward from the first path, its `exclude`, `gen-exclude`, `allow-unassigned` and `[packages]`, and packages in `node_modules` with their `exports` (see "Projects" in [the language page](../../docs/design/language.md#projects)).

`viz`, `paths`, `lsp` and `export mermaid` are only in the native command line. Messages for bad arguments are worded like clap's but are not identical.

## The API

```ts
import { check, compile, format, gen, RoadbookError } from '@roadbook/cli'

const out = compile('charts/orders.road')        // finds roadbook.toml upward from the file
if (out.errors > 0) console.error(out.rendered)  // the diagnostics as the command prints them
else use(out.ir)                                 // the IR (spec/ir.md); out.irJson is the committed-file text

check(['charts']).report                         // what `roadbook check --json` prints, parsed
gen('charts/orders.road').code                   // the `.road.ts` module, or null (see `error`)
format(source)                                   // { formatted, errors: [] } or { formatted: null, errors }
```

Every function is synchronous. Each takes `{ cwd, files, disk }`: `files` holds files in memory, by path, in front of the disk (unsaved editor buffers); with `disk: false` they are the only files, the project root is `cwd` and no `roadbook.toml` is read. A path that does not exist or an entry that cannot be read throws a `RoadbookError`; everything else is a diagnostic. Types are in [`src/api.d.ts`](src/api.d.ts).

## How it is built

`crates/roadbook-wasm` compiles the compiler, the formatter and the TypeScript generator for `wasm32-unknown-unknown`, with `wasm-bindgen` for the glue. The compiler reads files through the same `FsLoader` the native command line uses, over a file system the host supplies: a `readFile`/`isDir`/`realpath` object (Node's `fs`, in [`src/host.js`](src/host.js)) and a map of files held in memory. Its functions take and return JSON text; the crate's documentation lists them. The rest of the command line (finding the project, walking directories, printing, writing files) is a port of `crates/roadbook-cli` in [`src/workspace.js`](src/workspace.js) and [`src/cli.js`](src/cli.js), plain JavaScript checked by `tsc`, so the package runs from `node_modules` as it is.

The module is 937 kB (326 kB gzipped), built for size (`opt-level = "s"`, LTO, no name section); its glue is 11 kB.

Rebuild and commit `wasm/` whenever a compiler crate changes:

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.130   # the version crates/roadbook-wasm pins
pnpm --filter @roadbook/cli build
```

`wasm/SOURCES` records what the module was built from: a hash of the sources (every file under `src/` of `roadbook-wasm`, `roadbook-compiler`, `roadbook-syntax`, `roadbook-ir` and `roadbook-gen-ts`, their `Cargo.toml`, the workspace's, and `build.mjs`), the crates.io packages the build used, and the hashes of the module and its glue. `node build.mjs --check`, which `scripts/check-all.sh` and the tests run, fails if any of them no longer matches; it needs no Rust.

## Tests

`pnpm --filter @roadbook/cli test`:

- `test/parity.test.ts` runs this command and the native one (`$ROADBOOK_BIN`, or built with `cargo build -p roadbook-cli`; skipped without cargo) and compares what they print, their exit codes and the files they write: `check --json`, `check`, `export ir`, `fmt --check` and `gen --check` on every chart under `examples/` and `spec/conformance/charts/`, the whole project, `fmt` and `gen` writing into copies of it, projects importing packages, and the error cases.
- `test/staleness.test.ts`: the committed module is current, and the check fails in a copy of the sources where a compiler source changed, a file was added, Cargo.lock changed or the module was edited.
- `test/api.test.ts`: the API, on disk and in memory.
