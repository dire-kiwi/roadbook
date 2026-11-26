# Visualization

`roadbook viz` turns charts into HTML with no agent and no service involved. It is a deterministic function of the IR: the same chart always gives the same page.

The viewer is [`packages/viewer`](../../packages/viewer) (`@roadbook/viewer`), built to one script that the CLI embeds: `crates/roadbook-cli/assets/roadbook-viewer.js` is committed, so building the CLI needs no Node. After changing `packages/viewer`, rebuild it with `pnpm --filter @roadbook/viewer build` and commit it; `pnpm test` and `cargo test` fail while it is stale (`cargo test` reads its header: the hash of the sources, the elkjs and esbuild versions in `pnpm-lock.yaml`, and the hash of the script, so a dependency bump or a hand edit is caught without Node). `roadbook viz examples/shop` draws the shop example.

![PlaceOrder drawn by the viewer](images/place-order.png)

## What it produces

One HTML page per entry chart, plus `index.html` for the project, in `roadbook-viz/` unless `-o` says otherwise. An entry chart is a file given (or found under a directory given, or in the project) that no other of those files imports; its page draws every machine of its compilation. Each page holds its IR. The viewer script, with the layout engine and the styles, is about 1.5 MB, almost all of it `elkjs`, so the pages of one directory share one copy, `roadbook-viewer.js`, loaded by a plain `<script src>` that also works from `file://`. With `--self-contained` every page inlines the script instead and is one file that works alone. Either way there are no network requests and no server, so the pages open from disk, attach to a pull request as a build artifact, and can be published as a static site from CI.

The index lists every machine in the project and draws which machine invokes which and which talk to which. This is the "big state machine that delegates to more specific ones" view: start at the top-level machine and click down. Because a machine can only reach another through an invoke or a `ref` it is given ([actors and messaging](actors.md)), the charts alone say who can talk to whom, and the index draws all of it, laid out by ELK with the labels as edge labels:

| Edge | Drawn as |
| --- | --- |
| A invokes B | A solid arrow from A to B, unlabelled |
| A holds a `ref B` (a parameter or variable, or an alias of one) | A dashed arrow from A to B labelled `ref inventory`, the names holding it |
| A sends B events | An arrow in the message colour with an open head, from A to B, labelled `sends Reserve, Release` |
| B emits events A handles (`on b.Event` in A) | A dotted arrow in the message colour with an open head, from B to A, labelled `emits Reserved, Rejected` |

A legend under the graph says the same, and the table of machines has a "Talks to" column with each relation in words (`ref Inventory (inventory)`, `sends Reserve, Release to Inventory`, `handles Reserved, Rejected from Inventory`). An emitted event no chart handles draws no edge, since only host code could be listening. A machine of another package is named by its package and its path in it, `inventory/inventory.road:19`, as imports write it.

## Reading a diagram

| Element | Drawn as |
| --- | --- |
| State | A rounded box with its name |
| Compound state | A container with its children laid out inside |
| Initial state | A filled dot with an arrow |
| Final state | A double border, with its `output` or `fail` |
| `invoke` of a task | A row inside the state: `invoke chargeCard(order, user)` |
| `invoke` of a machine | The same row with a badge. Clicking it opens that machine. |
| `invoke … as upload` | The badge ends with `as upload`, the name in italics |
| `ref Machine` parameter or variable | In the signature above the diagram and in the details panel, a link that opens that machine |
| `send upload.Pause { … }`, `emit Progress { … }` | In an edge label or a state's row, an effect line like the others, in the message colour with the keyword in bold. Clicking it opens the event in the machine that declares it: the receiver's for a `send`, the machine's own for an `emit`. |
| `entry`, `exit`, internal transitions | Rows inside the state |
| Collapsed compound state | A box with `+`, a bar under it and the row `12 states inside (collapsed)` |
| Transition on an event | A solid arrow labelled with the event (`on *` for any event) |
| Transition on an event another machine emitted (`on upload.Progress`) | A solid arrow in the message colour, labelled through the sender: `upload.Progress`. Clicking the name opens the event in the sender's machine. As a row inside a state, `on upload.Progress` with the name in the message colour. |
| Transition from a state hidden in a collapsed one | The same arrow, leaving the collapsed box from a small square |
| `done` outcome | A solid arrow labelled `done` |
| `fail` outcome | An arrow in the error colour labelled `fail CardDeclined` |
| `after` | A dashed arrow labelled with the delay |
| `always` | A dotted arrow |
| Guard | `[attempts < 3]` after the trigger |
| Effects | `set`, `do`, `send` and `emit` lines under the label, in a quieter style, one per line (also in a state's rows) |

Every distinction is made in text or line style as well as colour, so the diagram reads in greyscale and for colour-blind readers.

Transitions written on a compound state leave from the container's border, which is how a reader sees at a glance that `Cancel` and the 30 second budget apply to every step inside `Processing`.

![Checkout drawn by the viewer](images/checkout.png)

## Layout

Layout is done by ELK's layered algorithm (through `elkjs`), which is the established choice for statecharts because it handles nested containers and edges that cross between levels. It runs in the browser when the page loads and again when a state is collapsed or expanded. Transition labels are ELK edge labels placed on their own edge, so a label is never ambiguous about which arrow it belongs to.

The drawing code is a pure function from the IR and a computed layout to SVG. The same function runs in Node to produce static, script-free SVG for places that cannot run scripts, such as a README or an email (`packages/viewer/scripts/svg.ts`; the pictures on this page are its output, with text widths estimated rather than measured).

### What the spike showed, and what the viewer does about it

The viewer's tests lay out every example, every conformance chart and a stress chart (parallel regions, self-loops, deep nesting, many finals), opened and fully expanded, and check that no two state boxes overlap and no label overlaps a state or another label.

- **Width with many final states.** ELK put `PlaceOrder`'s six finals in one row, about 1450 units wide. The viewer adds invisible edges between the final states of a container, three to a row, so they stack: `PlaceOrder` is now about 1220 by 1220.
- **A return edge's label far from its source.** ELK's feedback-edge routing took `after retryDelay(attempts)` around the outside of `Checkout`. Without it, and with `centerLabelPlacementStrategy: TAIL_LAYER`, the label sits between `Backoff` and `Charging`, next to `Backoff`.
- **The ELK option that crashed `elkjs`.** Model order (`considerModelOrder`) crashed 0.11.1 in nested graphs; 0.12 refuses it there with an `UnsupportedConfigurationException`. The viewer sets it on the root graph only, and falls back to plain layered, then to ELK's layered defaults, if ELK throws; the tests force both fallbacks.
- **1.6 MB per file.** Pages share one copy of the script, as above; only `--self-contained` repeats it.
- **Large charts.** A machine with more than 32 states opens with the containers at its deepest levels collapsed, as few levels as bring it to 32 drawn states. The 46-state `examples/large` opens with seven collapsed.

## Interaction

- Pan (drag), zoom (wheel, `+`, `-`) and fit to view (`f`). A chart too large to read whole opens at its top at a readable size.
- Collapse and expand compound states with the `+`/`-` box in their title, a double click or the details panel; the layout is redone. Large charts open with deep levels collapsed; "Expand all" undoes it.
- Click a state or a transition for its details: doc comment, the invoked task's full signature, guard, effects, and the source location as `file:line`.
- Click a machine invoke to open the child machine, with a trail back.
- The machine's details list the events it accepts and the events it emits, which machines it talks to (invokes, holds a `ref` to, sends which events to, handles which emitted events from) and which machines of the chart talk to it.
- Click a `send`, an `emit` or the name in an `on upload.Progress` label to open the event in the machine that declares it: its declaration and fields, where that machine handles or emits it, and every place in the chart that sends it or handles it, each a link. The transitions it triggers are highlighted. A `ref` type in a signature opens its machine.
- Search (`/`) by state, event (declared, sent, emitted or handled), task, machine, error or action name. Matches are highlighted; choosing one opens its machine, expands what hides it and selects it.
- Light and dark themes, following the system, with a toggle that is remembered.

## Modes

Explore is the first milestone. The others arrive as the runtime pieces they depend on do.

**Explore.** The static chart, as above.

**Simulate.** Step through a chart by hand. The viewer shows the events and outcomes that are possible in the current state. The reader picks one (send `Cancel`, let `chargeCard` fail with `GatewayUnavailable`, let 30 seconds pass), and the viewer highlights the transition and the new state and shows the variables. Guards that call host functions ask the reader for the result. This runs `@roadbook/core` in the page, so it behaves exactly as the real runtime would.

**Replay.** Load a [trace](semantics.md#the-trace) recorded by either runtime. The viewer draws the path the run took, steps forward and back through it, and shows the variables at each point. A failed request becomes a picture of where it went. If the trace's IR hash differs from the chart on screen, the viewer says so.

**Diff.** `roadbook viz --diff main..HEAD` draws both versions as one chart with added, removed and changed states and transitions marked. This is the mode aimed most directly at the project's goal. When an agent changes a program, the reviewer sees the change in flow before reading any code.

**Coverage.** Load many traces, from a test run or from production, and shade each transition by how often it was taken. Transitions never taken stand out.

## Other outputs

- **Mermaid.** `roadbook export mermaid <file>` writes Markdown with a `stateDiagram-v2` block per machine; `--machine Name` writes one bare diagram. GitHub renders it inline, so a chart can sit in a README or a pull request description. States, nesting, parallel regions, initial and final states and labelled transitions survive, with invokes, `entry`, `exit`, internal transitions and results as state descriptions (a note beside a compound or parallel state, which Mermaid allows no description). Messaging is written as in the chart: `upload.Progress` as a trigger, `send upload.Pause` and `emit Progress { … }` as effects, `invoke machine Upload(file) as upload`; the source line of a machine from another package names the package (`inventory/inventory.road:19`). A state named like a Mermaid keyword gets an id ending in `_`, and the machine drawn as a state the id `<Machine>_machine`; the names shown are unchanged. A machine's doc comment is a `text` block, so nothing in it is read as Markdown. It loses the line styles, the detail panel and the drill-down, and its layout is coarser.
- **Static SVG.** For documentation, from `packages/viewer/scripts/svg.ts`. The CLI has no command for it yet, since it needs a JavaScript runtime to run ELK.
- **Live.** `roadbook viz --watch [--port N]` rebuilds when a chart changes and serves the output on `127.0.0.1` (port 7357 by default), and the pages reload themselves. It answers only requests whose `Host` is `localhost`, `127.0.0.1` or `[::1]` with its port, so another site cannot read the charts by pointing a DNS name at the machine. Later, a running application can stream its trace to the viewer for a live view of what it is doing.
