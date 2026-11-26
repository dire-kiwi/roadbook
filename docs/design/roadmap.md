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
| `roadbook viz [-o dir] [--watch]` | Write the HTML viewer for every machine, plus an index. |
| `roadbook viz --diff <git range>` | Write a viewer showing what changed between two revisions. |
| `roadbook export mermaid\|scxml\|ir` | Other formats. Mermaid renders inline on GitHub. |
| `roadbook paths <Machine>` | List the paths through a machine, for test generation and coverage. |

## Milestones

Each milestone ends with something usable, and the order puts the riskiest assumptions first.

**M1. Language and IR.** Parser, name resolution, the structural checks, IR and its schema, `check` and `fmt`, in Rust. Covers atomic, compound and final states, events, guards, `invoke`, `after`, `always`. Type checking of expressions and arguments. The shop example compiles, and bad charts produce the diagnostics described in the language page.

**M2. Viewer.** `roadbook viz` in explore mode, built from the spike: layout, drill-down into child machines, details panel, project index. `export mermaid`. At this point the project already delivers its main promise for charts that are written but not yet run.

**M3. TypeScript runtime.** `@roadbook/core`, `@roadbook/effect`, `gen`, the trace, and the first conformance cases. The shop example serves requests from Express.

**M4. Rust runtime.** `roadbook-core`, the macro, the driver. The same example serves requests from axum and passes the same conformance cases.

**M5. Messaging.** `send` and `emit` with an invoked child, `ref` parameters for peers, and imports across crates and packages, as specified in [actors and messaging](actors.md). The viewer draws message edges and the project index draws which machines can talk to which. How the Rust macro finds a chart in a dependency crate has been prototyped in [`spike/cross-crate`](../../spike/cross-crate).

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
| Definite assignment is too strict or too slow. | It is M6, after real charts exist to tune it against. Until then, unassigned reads are a runtime error. |
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
