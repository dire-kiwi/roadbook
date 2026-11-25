# Visualization

`waymark viz` turns charts into HTML with no agent and no service involved. It is a deterministic function of the IR: the same chart always gives the same page.

The spike in [`spike/`](../../spike) already does this for the shop example. The pictures on this page are its output.

![PlaceOrder rendered by the spike](images/place-order.png)

## What it produces

One self-contained HTML file per entry chart, plus an index for the project. Each file inlines the IR, the viewer script, the layout engine and the styles. It has no network requests and no server, so it opens from disk, attaches to a pull request as a build artifact, and can be published as a static site from CI.

The index lists every machine in the project and draws which machine invokes which. This is the "big state machine that delegates to more specific ones" view: start at the top-level machine and click down.

## Reading a diagram

| Element | Drawn as |
| --- | --- |
| State | A rounded box with its name |
| Compound state | A container with its children laid out inside |
| Initial state | A filled dot with an arrow |
| Final state | A double border, with its `output` or `fail` |
| `invoke` of a task | A row inside the state: `invoke chargeCard(order, user)` |
| `invoke` of a machine | The same row with a badge. Clicking it opens that machine. |
| `entry`, `exit`, internal transitions | Rows inside the state |
| Transition on an event | A solid arrow labelled with the event |
| `done` outcome | A solid arrow labelled `done` |
| `fail` outcome | An arrow in the error colour labelled `fail CardDeclined` |
| `after` | A dashed arrow labelled with the delay |
| `always` | A dotted arrow |
| Guard | `[attempts < 3]` after the trigger |
| Effects | `set` and `do` lines under the label, in a quieter style |

Every distinction is made in text or line style as well as colour, so the diagram reads in greyscale and for colour-blind readers.

Transitions written on a compound state leave from the container's border, which is how a reader sees at a glance that `Cancel` and the 30 second budget apply to every step inside `Processing`.

![Checkout rendered by the spike](images/checkout.png)

## Layout

Layout is done by ELK's layered algorithm (through `elkjs`), which is the established choice for statecharts because it handles nested containers and edges that cross between levels. It runs in the browser when the page loads and again when a state is collapsed or expanded.

The drawing code is a pure function from the IR and a computed layout to SVG. The same function runs in Node to produce static, script-free SVG for places that cannot run scripts, such as a README or an email. The spike generates both forms.

## Interaction

- Pan, zoom and fit to view.
- Collapse and expand compound states. Large charts open with deep levels collapsed.
- Click a state or a transition for its details: doc comment, the invoked task's full signature, effects, and the source location as `file:line`.
- Click a machine invoke to open the child machine, with a trail back.
- Search by state, event, task or error name.
- Light and dark themes.

## Modes

Explore is the first milestone. The others arrive as the runtime pieces they depend on do.

**Explore.** The static chart, as above.

**Simulate.** Step through a chart by hand. The viewer shows the events and outcomes that are possible in the current state. The reader picks one (send `Cancel`, let `chargeCard` fail with `GatewayUnavailable`, let 30 seconds pass), and the viewer highlights the transition and the new state and shows the variables. Guards that call host functions ask the reader for the result. This runs `@waymark/core` in the page, so it behaves exactly as the real runtime would.

**Replay.** Load a [trace](semantics.md#the-trace) recorded by either runtime. The viewer draws the path the run took, steps forward and back through it, and shows the variables at each point. A failed request becomes a picture of where it went. If the trace's IR hash differs from the chart on screen, the viewer says so.

**Diff.** `waymark viz --diff main..HEAD` draws both versions as one chart with added, removed and changed states and transitions marked. This is the mode aimed most directly at the project's goal. When an agent changes a program, the reviewer sees the change in flow before reading any code.

**Coverage.** Load many traces, from a test run or from production, and shade each transition by how often it was taken. Transitions never taken stand out.

## Other outputs

- **Mermaid.** `waymark export mermaid` writes a `stateDiagram-v2` block. GitHub renders it inline, so a chart can sit in a README or a pull request description. It loses the detail panel and the drill-down, and its layout is coarser.
- **Static SVG.** For documentation.
- **Live.** `waymark viz --watch` serves the viewer and reloads as charts change. Later, a running application can stream its trace to the viewer for a live view of what it is doing.
