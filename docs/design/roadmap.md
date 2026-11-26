# Roadmap

## Repository layout

One repository, a Cargo workspace and a pnpm workspace side by side.

```
spec/
  language.md              the language reference
  ir.schema.json           JSON Schema for the IR
  trace.schema.json        JSON Schema for trace records
  conformance/             charts, scripts and expected traces
crates/
  roadbook-syntax           lexer, parser, syntax tree, formatter
  roadbook-compiler         name resolution, type checking, analyses, IR, diagnostics
  roadbook-ir               IR types (serde) and schema
  roadbook-core             pure interpreter
  roadbook-macros           the include! macro and Rust code generation
  roadbook                  driver, Actor, tracing; re-exports the macro
  roadbook-cli              check, fmt, gen, viz, export, paths
  roadbook-wasm             the compiler as WebAssembly
packages/
  core                     @roadbook/core: pure interpreter, no dependencies
  effect                   @roadbook/effect: Effect v4 driver and API
  viewer                   the viewer, built to one script the CLI embeds
  cli                      the npm distribution of the CLI
examples/
  shop/                    the example used throughout these documents
editors/
  tree-sitter-roadbook      grammar for highlighting
  vscode                   extension: highlighting, diagnostics, live preview
```

## Command line

| Command | Does |
| --- | --- |
| `roadbook check [--json]` | Compile and report diagnostics. |
| `roadbook fmt [--check]` | Canonical formatting. |
| `roadbook gen [--check]` | Write the `.road.ts` files. |
| `roadbook viz [paths…] [-o dir] [--self-contained] [--watch]` | Write the HTML viewer, a page per entry chart, plus an index. |
| `roadbook viz --trace <run.jsonl>` | The same, opening the pages that have the trace's machine in replay mode. |
| `roadbook viz --diff <git range>` | Write a viewer showing what changed between two revisions (`main..HEAD`, `main...HEAD`, or one revision against the working tree), with `summary.md` and `summary.json`. |
| `roadbook viz --coverage <traces…>` | Write the viewer shading each transition and state by how often the traces took it. |
| `roadbook export mermaid\|scxml\|ir` | Other formats. Mermaid renders inline on GitHub. |
| `roadbook paths <Machine> [file] [--json] [--max N]` | List the paths through a machine, from its initial state to each final state and each state with no way out, each loop taken at most once, for test generation and coverage. |
| `roadbook lsp` | The language server, on standard input and output, for editors: diagnostics, go to definition, references, rename, hover, formatting, outline. |

## Milestones

Each milestone ends with something usable, and the order puts the riskiest assumptions first.

Progress is recorded on each milestone below as it finishes.

| Milestone | Status |
| --- | --- |
| M1. Language and IR | Done, 26 November 2026 |
| M2. Viewer | Done, 26 November 2026 |
| M3. TypeScript runtime | Done, 26 November 2026 |
| M4. Rust runtime | Done, 26 November 2026 |
| M5. Messaging | Done, 26 November 2026 |
| M6. Depth | In progress |
| M7. Review tooling | Not started |
| M8. Editors | Not started |

**M1. Language and IR.** Parser, name resolution, the structural checks, IR and its schema, `check` and `fmt`, in Rust. Covers atomic, compound and final states, events, guards, `invoke`, `after`, `always`. Type checking of expressions and arguments. The shop example compiles, and bad charts produce the diagnostics described in the language page.

*Done.* `crates/roadbook-syntax`, `roadbook-ir`, `roadbook-compiler` and `roadbook-cli` (`check [--json]`, `fmt [--check]`, `export ir`); `spec/ir.md` and `spec/ir.schema.json`. Every check on the language page has a failing chart and an expected diagnostic, except definite assignment, which stays in M6 as planned. `examples/shop` checks and formats clean. Four further charts were written from prose by an agent that had read only the language page (`examples/README.md`); all compiled clean on the first attempt, and what was unclear went back into the language page. Two independent reviews found edge-case bugs (non-finite floats, deep nesting, multi-file projects, a root-level `always` loop), which are fixed. The behavioural contract for M3 and M4 (`spec/execution.md`, the trace format and the conformance cases) was written during M1 as well.

**M2. Viewer.** `roadbook viz` in explore mode, built from the spike: layout, drill-down into child machines, details panel, project index. `export mermaid`. At this point the project already delivers its main promise for charts that are written but not yet run.

*Done.* `packages/viewer` (`@roadbook/viewer`) is built into one script that `roadbook viz` embeds; pages in one output directory share it, or `--self-contained` inlines it. Explore mode has pan, zoom, fit, collapse and expand, a details panel, drill-down into child machines with a trail, search, light and dark themes, and a project index of which machine invokes which. `roadbook viz --watch` serves the output locally and reloads, and `roadbook export mermaid` is checked against Mermaid's own parser. The spike's problems are handled (see the visualization page), and the 46-state `examples/large` opens with its deep levels collapsed. The pages were checked in a browser in both themes, and an independent review's findings (Mermaid syntax, link escaping, container widths, the watch server's host check) are fixed. The spike viewer and compiler are deleted.

**M3. TypeScript runtime.** `@roadbook/core`, `@roadbook/effect`, `gen`, the trace, and the first conformance cases. The shop example serves requests from Express.

*Done.* `@roadbook/core` (the pure interpreter, no dependencies) and `@roadbook/effect` (the Effect v4 driver and API), `roadbook gen [--check]` with the generator in `crates/roadbook-gen-ts`, and the shop served by Express. The behavioural contract (`spec/execution.md`, the trace format and 24 conformance cases) was written before both runtimes, reviewed, and checked by two throwaway independent implementations. The runtime passes every case byte for byte through its production driver, and `tsc`-checked tests keep the type-level guarantees of the API page. An independent review's findings (task cancellation waiting for the task, the shop's error handling, generated error fields that clash with `Error`) are fixed.

**M4. Rust runtime.** `roadbook-core`, the macro, the driver. The same example serves requests from axum and passes the same conformance cases.

*Done.* `roadbook-core`, `roadbook-macros` (`roadbook::include!`) and `roadbook` (the driver, `Actor`, `Timer`, `TraceSink`), and the shop served by axum in `examples/shop/axum`. Every conformance case passes byte for byte through the production driver, and `scripts/check-all.sh` diffs the two runtimes' traces directly; they are identical. Express and axum give the same answer to every request in `examples/shop/README.md`. Before M4 committed to a build setup, `spike/cross-crate` settled how the macro will get a chart from a dependency crate. An independent review's findings (aliases in operators, which made the runtimes diverge and is now a conformance case; non-finite floats; panics while dropping futures; the actor's lifetime) are fixed.

**M5. Messaging.** `send` and `emit` with an invoked child, `ref` parameters for peers, and imports across crates and packages, as specified in [actors and messaging](actors.md). The viewer draws message edges and the project index draws which machines can talk to which. The Rust macro gets a chart from a dependency crate through a hidden macro that crate exports, as a spike (`spike/cross-crate`, since deleted) recommended.

*Done.* The language, the compiler and IR 0.2: `emits`, `ref`, `invoke … as`, `on name.Event`, `send` and `emit`, with their checks, and imports from other packages (`[packages]` in `roadbook.toml`, `node_modules` with Node-style `exports`, and an import satisfied by another package's compiled IR). The semantics are in `spec/execution.md` with ten conformance cases that start several top-level runs; an independent throwaway implementation reproduced every trace. Both runtimes implement messaging and write identical traces for all 34 cases; both run `examples/upload` and `examples/orders`. In Rust, `examples/inventory` and `examples/orders` are two crates, and chains and diamonds of crates are tested. The TypeScript generator imports other packages' generated modules. The viewer draws messaging in each machine and, in the project index, which machines invoke, send to, hold references to and receive from which. Two independent reviews found lifecycle bugs in the Rust driver, a `$crate` hygiene bug that broke crate chains, and gaps in package resolution; all are fixed with tests.

**M6. Depth.** Definite assignment analysis. Parallel states. Saving and restoring snapshots (`resume`), with the IR hash check. Simulate and replay modes in the viewer, which need the TypeScript core and the trace.

**M7. Review tooling.** Diff mode and coverage mode in the viewer. A CI recipe that attaches the diff viewer to a pull request.

**M8. Editors.** Tree-sitter grammar, language server (diagnostics, go to definition, rename), VS Code extension with live preview.

**Later.** History states, keyed `spawn`, retry sugar, a durable driver, generated Rust state machines for hot paths.

M3 and M4 are independent once M1 is done and can run in parallel. M2 depends only on M1.

## Risks

| Risk | Mitigation |
| --- | --- |
| The language is a poor fit for real programs. | Port two or three real request handlers during M1, before the runtimes exist. The viewer makes shortcomings obvious early. |
| The two runtimes diverge. | The conformance suite is the specification, and both run it in CI from M4 on. |
| Agents write the syntax badly. | Small grammar, canonical formatter, machine-readable diagnostics with fixes, and a bundled instructions file for agents. Measure it in M1 by having an agent write charts from prose. |
| Large charts are unreadable as diagrams. | Collapse compound states by default beyond a size, and push structure into child machines, which the language makes cheap. The spike should be tried on a 40-state chart during M2. |
| Definite assignment is too strict or too slow. | Built in M6 and tuned against the example charts: it is conservative in a few documented places, and takes about 100 ms on a 1,000-state chart in a debug build. The runtime check stays as a backstop. |
| No editor support at first. | `roadbook viz --watch` gives live feedback from M2. Highlighting needs only the tree-sitter grammar, which can be pulled forward. |

## Open questions

These do not block starting. Each has a default the design assumes.

1. **The name.** Roadbook replaced the first working name, Waymark, which another workflow engine already uses. The bare name `roadbook` was free on crates.io and npm in November 2026, and a web search found no workflow or state machine tool using it. The npm scope, GitHub organisation, domain and trademarks have not been checked, and nothing has been reserved.
2. **Parameter style in TypeScript.** Implementations take one object (`({ order, user }) => …`), which is idiomatic and survives parameter reordering. Positional arguments would match the chart's call syntax and the Rust side.
3. **Strict events.** Unhandled events are dropped and traced by default. They could instead be an error by default, with an explicit `on * ` to opt out.
4. **Exit actions on cancellation.** When a machine is cancelled from outside, should the `exit` effects of its active states run? The default is yes, since they are the natural place for cleanup, but they then run during interruption and must be quick.
5. **Equality on records.** `==` is defined for scalars and enums. Comparing records and lists is left out until something needs it.
6. **Messaging details.** Asking a peer for a reply, bounded queues and supervision of peers are open. They are listed on the [actors and messaging](actors.md#open-questions) page.
7. **Generated files in git.** The default is to commit `.road.ts` files. Ignoring them and generating in a build step is the alternative.
