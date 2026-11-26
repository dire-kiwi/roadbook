# Roadbook

Roadbook is a statechart language with two runtimes. A program's flow lives in a `.road` file that a person can read, check and see as a diagram; the code around it is a set of small typed functions the chart calls. A Rust compiler checks charts and turns them into one JSON document, the IR, which everything else reads: a diagram viewer, a TypeScript runtime on Effect v4, and a Rust runtime. The two runtimes are held to the same behaviour by a shared conformance suite and write byte-identical traces.

```
/// Charge the customer's card, retrying while the payment gateway is down.
machine Checkout(order: Order, user: User) -> Receipt ! CardDeclined | GatewayUnavailable {
  var attempts: int = 0
  var receipt:  Receipt
  var declined: CardDeclined

  initial Charging

  state Charging {
    invoke chargeCard(order, user) {
      done                    -> Charged   set receipt = result
      fail CardDeclined       -> Declined  set declined = error
      fail GatewayUnavailable {
        if attempts < 3       -> Backoff
        else                  -> GaveUp
      }
    }
  }
  …
}
```

The design, its decisions and what was built are in [`docs/design/`](docs/design/README.md); progress per milestone is in the [roadmap](docs/design/roadmap.md).

## What is here

| Path | What |
| --- | --- |
| `crates/roadbook-syntax` | Lexer, parser with error recovery, syntax tree, diagnostics, the canonical formatter. |
| `crates/roadbook-compiler` | Projects and imports (including other packages), name resolution, type checking, analyses (definite assignment, `always` cycles, recursion), lowering to the IR, a symbol index. |
| `crates/roadbook-ir` | The IR's Rust types, canonical JSON (RFC 8785) and the machine hash. |
| `crates/roadbook-cli` | The `roadbook` command line: `check`, `fmt`, `gen`, `viz`, `export`, `paths`, `lsp`. |
| `crates/roadbook-core`, `crates/roadbook`, `crates/roadbook-macros` | The Rust runtime: the pure interpreter, the driver and API, and `roadbook::include!`, which turns a chart into types and a `Handlers` trait at build time. |
| `crates/roadbook-gen-ts` | The generator behind `roadbook gen`, which writes a typed `.road.ts` module next to each chart. |
| `crates/roadbook-lsp` | The language server. |
| `crates/roadbook-conformance` | Loads and checks the conformance cases. |
| `packages/core`, `packages/effect` | The TypeScript runtime: `@roadbook/core`, the pure interpreter, and `@roadbook/effect`, the Effect v4 driver and API, with durable runs on `effect/workflow` in `@roadbook/effect/durable`. |
| `crates/roadbook-wasm`, `packages/cli` | The compiler as WebAssembly, and `@roadbook/cli`, a Node `roadbook` (`check`, `fmt`, `gen`, `export ir`) that gives the same output as the native one without a Rust toolchain. |
| `packages/viewer` | The diagram viewer (explore, simulate, replay, diff and coverage), built into the script `roadbook viz` embeds. |
| `spec/` | The contract every tool and runtime is built against: the IR, the execution rules, the trace and snapshot formats, and the conformance cases. |
| `examples/` | Charts used throughout, including the shop example served by both Express and axum. |
| `editors/` | The tree-sitter grammar and the VS Code extension. |

## Building and checking

You need Rust (stable), Node and pnpm.

```sh
cargo build -p roadbook-cli              # the `roadbook` command, in target/debug
pnpm install

cargo run -p roadbook-cli -- check                      # every chart in the project
cargo run -p roadbook-cli -- viz examples/shop -o out   # diagrams; open out/place_order.html
cargo run -p roadbook-cli -- gen examples/shop          # typed TypeScript modules next to the charts

scripts/check-all.sh                     # every check: formatting, clippy, tests, charts, both
                                         # runtimes' conformance traces compared, the viewer bundle
```

`.github/workflows/ci.yml` runs `scripts/check-all.sh` on GitHub ([dire-kiwi/roadbook](https://github.com/dire-kiwi/roadbook)) for every push and pull request. Nothing is published to crates.io or npm.
