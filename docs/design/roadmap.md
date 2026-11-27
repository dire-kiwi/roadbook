# Roadmap

## Repository layout

One repository, a Cargo workspace and a pnpm workspace side by side.

```
spec/
  execution.md             how machines execute: the rules both runtimes follow
  ir.md, ir.schema.json    the IR, and its JSON Schema
  trace.md, trace.schema.json  trace records, and their JSON Schema
  snapshot.schema.json     JSON Schema for saved snapshots
  conformance/             charts, scripts and expected traces
crates/
  roadbook-syntax           lexer, parser, syntax tree, formatter
  roadbook-compiler         name resolution, type checking, analyses, IR, diagnostics
  roadbook-ir               IR types (serde) and schema
  roadbook-core             pure interpreter
  roadbook-macros           the include! macro and Rust code generation
  roadbook-gen-ts           the TypeScript generator behind `gen`
  roadbook                  driver, Actor, tracing; re-exports the macro
  roadbook-cli              check, fmt, gen, viz, export, paths, lsp
  roadbook-lsp              the language server
  roadbook-conformance      loads and checks the conformance cases
  roadbook-wasm             the compiler as WebAssembly, for packages/cli
packages/
  core                     @dire-kiwi/roadbook-core: pure interpreter, no dependencies
  effect                   @dire-kiwi/roadbook-effect: Effect v4 driver and API
  viewer                   the viewer, built to one script the CLI embeds
  cli                      @dire-kiwi/roadbook-cli: the npm distribution of the CLI, on the WebAssembly compiler
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
| `roadbook viz --coverage <traces> [--coverage <traces>…]` | Write the viewer shading each transition and state by how often the traces took it. Each `--coverage` takes one trace file or directory of them; repeat it for more. |
| `roadbook export mermaid\|scxml\|ir <file> [--machine Name] [-o out]` | Other formats. Mermaid renders inline on GitHub; SCXML is one W3C document per machine; `ir` (no `--machine`) is the compiled IR. |
| `roadbook paths <Machine> [file] [--json] [--max N]` | List the paths through a machine, from its initial state to each final state, each state with no way out and each point where going on would repeat a transition, each loop taken at most once, for test generation and coverage. |
| `roadbook lsp` | The language server, on standard input and output, for editors: diagnostics, go to definition, references, rename, hover, formatting, outline, and the `roadbook/ir` request the VS Code preview compiles charts with. |

The npm package [`@dire-kiwi/roadbook-cli`](../../packages/cli) runs `check`, `fmt`, `gen` and `export ir` on the compiler built as WebAssembly, printing what the native command does byte for byte, with no Rust toolchain; it also has an API for build tools.

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
| M6. Depth | Done, 26 November 2026 |
| M7. Review tooling | Done, 26 November 2026 |
| M8. Editors | Done, 26 November 2026 |

**M1. Language and IR.** Parser, name resolution, the structural checks, IR and its schema, `check` and `fmt`, in Rust. Covers atomic, compound and final states, events, guards, `invoke`, `after`, `always`. Type checking of expressions and arguments. The shop example compiles, and bad charts produce the diagnostics described in the language page.

*Done.* `crates/roadbook-syntax`, `roadbook-ir`, `roadbook-compiler` and `roadbook-cli` (`check [--json]`, `fmt [--check]`, `export ir`); `spec/ir.md` and `spec/ir.schema.json`. Every check on the language page has a failing chart and an expected diagnostic, except definite assignment, which stays in M6 as planned. `examples/shop` checks and formats clean. Four further charts were written from prose by an agent that had read only the language page (`examples/README.md`); all compiled clean on the first attempt, and what was unclear went back into the language page. Two independent reviews found edge-case bugs (non-finite floats, deep nesting, multi-file projects, a root-level `always` loop), which are fixed. The behavioural contract for M3 and M4 (`spec/execution.md`, the trace format and the conformance cases) was written during M1 as well.

**M2. Viewer.** `roadbook viz` in explore mode, built from the spike: layout, drill-down into child machines, details panel, project index. `export mermaid`. At this point the project already delivers its main promise for charts that are written but not yet run.

*Done.* `packages/viewer` (`@dire-kiwi/roadbook-viewer`) is built into one script that `roadbook viz` embeds; pages in one output directory share it, or `--self-contained` inlines it. Explore mode has pan, zoom, fit, collapse and expand, a details panel, drill-down into child machines with a trail, search, light and dark themes, and a project index of which machine invokes which. `roadbook viz --watch` serves the output locally and reloads, and `roadbook export mermaid` is checked against Mermaid's own parser. The spike's problems are handled (see the visualization page), and the 46-state `examples/large` opens with its deep levels collapsed. The pages were checked in a browser in both themes, and an independent review's findings (Mermaid syntax, link escaping, container widths, the watch server's host check) are fixed. The spike viewer and compiler are deleted.

**M3. TypeScript runtime.** `@dire-kiwi/roadbook-core`, `@dire-kiwi/roadbook-effect`, `gen`, the trace, and the first conformance cases. The shop example serves requests from Express.

*Done.* `@dire-kiwi/roadbook-core` (the pure interpreter, no dependencies) and `@dire-kiwi/roadbook-effect` (the Effect v4 driver and API), `roadbook gen [--check]` with the generator in `crates/roadbook-gen-ts`, and the shop served by Express. The behavioural contract (`spec/execution.md`, the trace format and 24 conformance cases) was written before both runtimes, reviewed, and checked by two throwaway independent implementations. The runtime passes every case byte for byte through its production driver, and `tsc`-checked tests keep the type-level guarantees of the API page. An independent review's findings (task cancellation waiting for the task, the shop's error handling, generated error fields that clash with `Error`) are fixed.

**M4. Rust runtime.** `roadbook-core`, the macro, the driver. The same example serves requests from axum and passes the same conformance cases.

*Done.* `roadbook-core`, `roadbook-macros` (`roadbook::include!`) and `roadbook` (the driver, `Actor`, `Timer`, `TraceSink`), and the shop served by axum in `examples/shop/axum`. Every conformance case passes byte for byte through the production driver, and `scripts/check-all.sh` diffs the two runtimes' traces directly; they are identical. Express and axum give the same answer to every request in `examples/shop/README.md`. Before M4 committed to a build setup, `spike/cross-crate` settled how the macro will get a chart from a dependency crate. An independent review's findings (aliases in operators, which made the runtimes diverge and is now a conformance case; non-finite floats; panics while dropping futures; the actor's lifetime) are fixed.

**M5. Messaging.** `send` and `emit` with an invoked child, `ref` parameters for peers, and imports across crates and packages, as specified in [actors and messaging](actors.md). The viewer draws message edges and the project index draws which machines can talk to which. The Rust macro gets a chart from a dependency crate through a hidden macro that crate exports, as a spike (`spike/cross-crate`, since deleted) recommended.

*Done.* The language, the compiler and IR 0.2: `emits`, `ref`, `invoke … as`, `on name.Event`, `send` and `emit`, with their checks, and imports from other packages (`[packages]` in `roadbook.toml`, `node_modules` with Node-style `exports`, and an import satisfied by another package's compiled IR). The semantics are in `spec/execution.md` with ten conformance cases that start several top-level runs; an independent throwaway implementation reproduced every trace. Both runtimes implement messaging and write identical traces for all 34 cases; both run `examples/upload` and `examples/orders`. In Rust, `examples/inventory` and `examples/orders` are two crates, and chains and diamonds of crates are tested. The TypeScript generator imports other packages' generated modules. The viewer draws messaging in each machine and, in the project index, which machines invoke, send to, hold references to and receive from which. Two independent reviews found lifecycle bugs in the Rust driver, a `$crate` hygiene bug that broke crate chains, and gaps in package resolution; all are fixed with tests.

**M6. Depth.** Definite assignment analysis. Parallel states. Saving and restoring snapshots (`resume`), with the IR hash check. Simulate and replay modes in the viewer, which need the TypeScript core and the trace.

*Done.* Definite assignment (`RB0403`): a forward analysis over the chart, conservative in a few documented places, about 100 ms on a 1,000-state chart; it found two real bugs in a compiler test chart, and the runtime check stays as a backstop. Parallel states and snapshots (`restore`, `save`, `suspend`, `resume`, with the IR hash check and migrations) are specified in `spec/execution.md` and `spec/snapshot.schema.json`, with conformance cases checked by an independent throwaway implementation, and built in both runtimes: they write identical traces and byte-identical snapshot files for all 49 cases, so a snapshot saved by one runtime restores in the other. The viewer has simulate mode (it runs `@dire-kiwi/roadbook-core` in the page and asks the reader for task outcomes and function results) and replay mode (`roadbook viz --trace`, or a trace dropped on the page). Independent reviews found a suspend/cancel race in each runtime, aliased snapshot objects, and several places the runtimes disagreed on restore; all are fixed, several as new conformance cases.

**M7. Review tooling.** Diff mode and coverage mode in the viewer. A CI recipe that attaches the diff viewer to a pull request.

*Done.* `roadbook viz --diff <range>` draws both revisions of each chart as one diagram, with added, removed, changed and moved states, transitions, machines and declarations marked in text and line style as well as colour, and writes `summary.md` and `summary.json`. Coverage (`roadbook viz --coverage`, or traces dropped on a page) shades each state and transition by how often the traces took it and flags the ones never taken. `roadbook paths` lists the paths through a machine for test generation. `docs/ci/roadbook-diff.yml` is a GitHub Actions recipe that builds the diff pages for a pull request, uploads them and comments a summary. It has run on GitHub, in the repository's first pull request (dire-kiwi/roadbook#1, a test that changed one chart): it drew the change, uploaded the pages and posted its comment, and on the next push updated that comment instead of adding another. It needed no changes beyond moving its actions to their current major versions. An independent review found an argument-parsing bug, unescaped chart text in the pull-request comment, and gaps in matching across files; all are fixed.

**M8. Editors.** Tree-sitter grammar, language server (diagnostics, go to definition, rename), VS Code extension with live preview.

*Done.* The tree-sitter grammar (`editors/tree-sitter-roadbook`) parses every chart in the repository, with highlight, locals, fold and indent queries for Neovim, Helix and Zed; `scripts/check-all.sh` checks it. The language server (`roadbook lsp`, `crates/roadbook-lsp`, on a symbol index added to the compiler without changing its output) gives diagnostics with the compiler's fixes as quick fixes, go to definition, references, hover, rename that refuses to capture or clash, formatting and an outline, across files and packages and for unsaved and untitled documents. The VS Code extension (`editors/vscode`) adds the language, highlighting, the server and a live preview that compiles through the server, so it sees every unsaved buffer; its integration test passed in a real VS Code (headless). An independent review found edge cases with packages under `node_modules`, the preview's compilation, rename scoping and workspace trust; all are fixed.

**Calls and strict machines.** *Done.* Every [decided question](#decided-questions) is built. Emitted events are delivered only to machines that handle them. `strict` machines are built, with `on Event ignore`, in IR 0.5. `call` declarations are built: `call reserve(…)`, `invoke inventory.reserve(…)`, `on reserve` and `reply`, with the built-in error `Unanswered` and IR 0.4, and `examples/orders` uses them.

**Retry sugar.** `invoke … retry N [on E | …] [after delay]`, with `attempt` in the delay, as the [language page](language.md#retrying-an-invoke) describes.

*Done.* The compiler writes a retry out as explicit states (the invoking state becomes compound, with `Attempt` and `Backoff`), a counter variable and guarded `invoke.fail` branches, so the IR gains nothing, and both runtimes, the viewer and the exporters are unchanged, and the sugar and the same states written by hand have the same machine hash. Conformance cases run the written-out form in both runtimes; the formatter, the diagnostics (`RB0120`, `RB0215`–`RB0218`), the tree-sitter grammar and the language server know the clause.

**History states.** `history Name` and `deep history Name`, as in SCXML, built after M8 the way the milestones were: the syntax and semantics first ([language](language.md#history-states), `spec/execution.md`, IR 0.7 and snapshot version 2), then six conformance cases, which a throwaway independent implementation written only from `spec/` reproduced byte for byte, as it did the 30 earlier cases in its scope.

*Done, 27 November 2026.* The compiler (with `RB0219` and `RB0910`), both runtimes (identical traces and saved snapshots for every case), the viewer's five modes, `export mermaid` and `scxml`, `roadbook paths`, `viz --diff`, the tree-sitter grammar, the VS Code grammar and the language server. Two choices differ from SCXML or extend it, and are written down: which states a transition to a history state leaves never depends on what was recorded, and a history state with no record enters its parent's default entry (SCXML requires a default transition). A machine with no history state keeps its hash, so its saved snapshots (version 1) still restore. An independent review found that the `always` cycle check missed loops back through a history state, and that the Rust runtime restored a snapshot naming a history state as active; both are fixed, the second as a conformance case.

**Durable driver (SM-14).** Running a chart on `effect/workflow`, so that a run survives a crash in the middle of a task without repeating completed work.

*Done, in TypeScript.* `Durable.workflow` (`@dire-kiwi/roadbook-effect/durable`) runs a machine as a workflow execution: tasks and actions are activities, each run journals the inputs it takes and replays them through the core after a crash, timers keep their deadlines, child runs share their root's execution, `ref`s between durable runs are sent again on replay and de-duplicated, and an idle run parks on a `DurableClock` ([durable runs](typescript.md#durable-runs)). It writes the same traces for 60 of the 64 conformance cases (the four with `suspend` steps do not apply), and crash tests on `ClusterWorkflowEngine` show each guarantee. `examples/import-job` crashes in the middle of a batch. The language did not change.

**Keyed spawn.** *Done, 27 November 2026.* A dynamic number of children, built as M5 was ([actors and messaging](actors.md#many-children)). The language: `children name: Machine by Key`, `spawn name[key] = Machine(args)`, `stop name[key]`, `send name[key].E` and `send name.*.E`, `on name.E`, `on name.done` and `on name.fail E` with the implicit name `key`, and their checks (`RB0121`, `RB0122`, `RB0220`, `RB0322`, `RB0911`, and `RB0402` through spawns); IR 0.7, which changes no hash of a machine that spawns nothing, and snapshot version 3. The semantics are in `spec/execution.md` ("Spawned children") with eight conformance cases; an independent throwaway implementation reproduced the first six byte for byte from the spec, and an independent review's findings (`int` keys in float form in Rust, collections named `text` missing from the hash, gaps in `RB0402`, a wrong claim about what a parent's exit can tell its children) are fixed, the first as the seventh case. The eighth, `redact-spawn`, covers redaction in the spawn records, and a sensitive key is `RB0320`. Both runtimes write identical traces and snapshots for all 72 cases, and both run `examples/upload/batch.road`, which spawns an upload per file. The viewer draws spawning and the project index lists it; the tree-sitter grammar and the language server know the new syntax. How the messaging decisions (supervision, bounded queues, who receives emitted events, calls) apply to children is in [actors and messaging](actors.md#how-the-messaging-decisions-apply-to-children).

**Maps and generic types.** *Done, 27 November 2026.* `map<K, V>` with string keys, and generic records and aliases (`type Page<T> { … }`, `type Dict<T> = map<string, T>`), as the [language page](language.md#maps-and-generic-types) describes: type arguments are given in types and inferred for record values, with the checks `RB0323` to `RB0327`. IR 0.7 keeps type parameters rather than writing out instances, and changes no hash of a machine that uses no generic type. A map is a JSON object and an instance takes the form of its declaration; redaction follows both. The generated TypeScript has a schema function per generic type, and the Rust macro generic structs and `BTreeMap`s. Three conformance cases (`generic-values`, `generic-snapshot`, `redact-generic`) pass byte for byte in both runtimes and the durable driver. The viewer, the tree-sitter grammar and the language server know the syntax.

**Later.** A durable driver in Rust, durable runs across several cluster runners and of machines that declare or make calls, a trigger for a peer that has ended, generated Rust state machines for hot paths, a Vite plugin that generates `.road.ts` files on the fly, bounded queues if load ever calls for them, `==` on records and lists, and a hard stop for a TypeScript run that skips its exit effects.

M3 and M4 are independent once M1 is done and can run in parallel. M2 depends only on M1.

## Risks

| Risk | Mitigation |
| --- | --- |
| The language is a poor fit for real programs. | Port two or three real request handlers during M1, before the runtimes exist. The viewer makes shortcomings obvious early. |
| The two runtimes diverge. | The conformance suite is the specification, and both run it in CI from M4 on. |
| Agents write the syntax badly. | Small grammar, canonical formatter, machine-readable diagnostics with fixes, and a bundled instructions file for agents. Measure it in M1 by having an agent write charts from prose. |
| Large charts are unreadable as diagrams. | Collapse compound states by default beyond a size, and push structure into child machines, which the language makes cheap. M2 tried the viewer on the 46-state `examples/large`, which opens with its deep levels collapsed. |
| Definite assignment is too strict or too slow. | Built in M6 and tuned against the example charts: it is conservative in a few documented places, and takes about 100 ms on a 1,000-state chart in a debug build. The runtime check stays as a backstop. |
| No editor support at first. | `roadbook viz --watch` gave live feedback from M2. M8 added the tree-sitter grammar, the language server and the VS Code extension. |

## Decided questions

The first draft left seven questions open, each with a default the design assumed. They were decided on 27 November 2026 and moved into the pages they belong to. Four of the decisions needed code, and all four are built: the `@dire-kiwi` npm scope, `call` declarations, the filter on emitted events and `strict` machines.

| Question | Decision | Built | Where |
| --- | --- | --- | --- |
| The name | Roadbook, published under the `@dire-kiwi` npm scope | Yes | [Below](#the-name) |
| Parameter style in TypeScript | One object, as now | Yes | [TypeScript](typescript.md#implementing-a-machine) |
| Strict events | An opt-in `strict` modifier on a machine, with `on Event ignore` to ignore an event on purpose; an undelivered send is not a defect | Yes, in IR 0.5 | [Language](language.md#strict-machines) |
| Exit actions on cancellation | They run, as now, and must be quick | Yes | [Semantics](semantics.md#cancelling-from-outside) |
| Equality on records | Scalars and enums only, as now; the future design is written down | Yes | [Language](language.md#equality) |
| Messaging details | `call` declarations; unbounded queues; emitted events only to machines that handle them; no supervision yet | Unbounded queues and no supervision are what exists; the filter on emitted events is built; `call` is built (the callee takes a call with `on reserve` and answers with `reply`, the pending call has type `call reserve`, and a callee that has ended or does not take the call gives `Unanswered`) | [Actors and messaging](actors.md#decided-after-the-milestones) |
| Generated files in git | Committed, and marked as generated for GitHub | Yes | [TypeScript](typescript.md#what-gets-generated) |

### The name

Roadbook stays. It replaced the first working name, Waymark, which another workflow engine already uses. Availability was checked on 26 and 27 November 2026; nothing has been reserved.

- **crates.io:** `roadbook` and every `roadbook-*` name the workspace uses are free. The crates keep their names.
- **npm:** the bare `roadbook` is free, but the `@roadbook` scope belongs to an npm user of that name, who has published nothing, and a user's scope is not handed over. The packages are published under the `@dire-kiwi` scope instead, which was unclaimed, each named `roadbook-` and what it is, like the crates: `@dire-kiwi/roadbook-cli` (the `roadbook` command as WebAssembly), `@dire-kiwi/roadbook-core` (the interpreter), `@dire-kiwi/roadbook-effect` (the runtime the generated modules import) and `@dire-kiwi/roadbook-viewer`, all still `private`; the shop example is `@dire-kiwi/roadbook-example-shop`. `@dire-kiwi/roadbook` itself is left for a package that gathers them, if one is wanted.
- **GitHub:** `roadbook` is a personal account, inactive since 2021. The repository is `github.com/dire-kiwi/roadbook`.
- **Domains:** `roadbook.com` (a UK travel publisher), `.dev`, `.io` and `.org` are registered; `roadbook.sh` was not.
- **Trademarks:** a web search, not a legal clearance, found no registration of the word alone. MOBILEYE ROADBOOK (US Reg. 6348237) is registered for autonomous-driving software, in the same class, and "roadbook" is a common word in rallying. A proper trademark search is due before a public 1.0.

Nothing was published, so renaming the scope moved no users. The packages were renamed wherever they appeared, including the runtime import that `roadbook-gen-ts` writes into every generated module, and every committed `.road.ts` file and the WebAssembly build of the CLI were regenerated with it. Neither the scope nor any name has been reserved on npm yet.
