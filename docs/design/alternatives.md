# Alternatives considered

## How machines are written

The task left the format open: JSON, YAML, or a custom language with a compiler. There is also a fourth option the task did not name, which is to define machines in host code and export a description. This is the decision everything else hangs on.

### The same transition, four ways

Roadbook:

```
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
```

YAML with a schema:

```yaml
Charging:
  invoke:
    src: chargeCard
    args: [order, user]
    onDone:
      - target: Charged
        set: { receipt: result }
    onError:
      CardDeclined:
        - target: Declined
          set: { declined: error }
      GatewayUnavailable:
        - guard: "attempts < 3"
          target: Backoff
        - target: GaveUp
```

JSON is the same tree with more punctuation and no comments.

TypeScript, in the style of xstate:

```ts
Charging: {
  invoke: {
    src: "chargeCard",
    input: ({ context }) => ({ order: context.order, user: context.user }),
    onDone: { target: "Charged", actions: assign({ receipt: ({ event }) => event.output }) },
    onError: [
      { guard: ({ event }) => event.error._tag === "CardDeclined", target: "Declined", actions: assign({ /* … */ }) },
      { guard: ({ context }) => context.attempts < 3, target: "Backoff" },
      { target: "GaveUp" }
    ]
  }
}
```

### Comparison

| | Custom language | YAML or JSON | Host code |
| --- | --- | --- | --- |
| Readable as a flow | Best. One line per transition. | Fair. The structure is a tree of keys, and the flow is in the values. | Depends on the author. Logic can hide in inline functions. |
| Same definition for Rust and TypeScript | Yes | Yes | No. Two authoring surfaces. |
| Diagram without running the program | Yes | Yes | Only by executing or statically analysing the code. Inline guards show as "anonymous function". |
| Structural validation | The parser | A JSON Schema | The host type checker |
| Semantic validation (targets exist, errors handled, variables assigned) | A custom checker | The same custom checker, with worse error locations | Partly the host type checker, partly nothing |
| Types for data | Native syntax | Needs a type language anyway: either embedded JSON Schema, which is verbose, or type expressions in strings, which is a custom language inside YAML | Native |
| Guards such as `attempts < 3` | Native syntax | Strings holding a custom language | Native |
| Editor support on day one | None until a grammar and language server exist | Good, from the schema | Complete |
| How well agents write it | Unknown syntax, but small, with precise compiler errors to correct against | Familiar syntax, schema to validate against | Best |
| Cost to build | Parser, formatter, editor grammar, language server | Schema and loader | Two builder APIs and two exporters |
| Typed host code | Code generation | Code generation | Inference, no generation |

### Why a custom language

The semantic checker is needed in every case. A JSON Schema can say that `target` is a string. It cannot say that the target exists, that every declared error is handled, or that `order` is assigned before `Pricing` reads it. That checker is most of the compiler. Once it exists, what a custom language adds on top is a parser, and what it buys is the thing the project is for: a file a person can read as a flow and review as a diff.

YAML does not avoid inventing a language. Types and guards have to be written somehow, and in YAML they end up as strings with their own grammar and no tooling.

Host code gives up the two hard requirements. It cannot be shared between Rust and TypeScript, and a diagram of it is only as complete as the author's discipline in not writing inline logic.

The costs are real. There is no editor support until it is built, people and agents have to learn a syntax, and the project owns a parser. They are bounded: the grammar fits on one page, the formatter is canonical, and diagnostics are designed to be acted on by an agent.

### The fallback

The compiler's back half does not care where its input came from. If the custom language turns out to be the wrong call, a YAML front end can produce the same IR, and nothing after the IR changes: not the runtimes, the code generators or the viewer. That is the reason for the hard line at the IR, and it makes this decision cheaper to get wrong than it looks.

## What is declared and what is implemented

A second choice sits inside the first: how much of a machine's data handling is in the chart.

| Option | Description | Verdict |
| --- | --- | --- |
| Names only | The chart names guards and actions. Host code receives the whole context and does what it likes, as in xstate. | Simplest compiler. Data flow is invisible, and variables are optional fields that every reader has to assert. |
| Wiring in the chart | The chart declares types and passes explicit arguments. Host functions see only their arguments. A small expression language covers comparison and counting. | **Chosen.** The chart is a complete picture of control and data. Costs a type checker and the assignment analysis. |
| A full expression language | Adopt something like CEL so that most logic can live in the chart. | More semantics to keep identical across two runtimes, and logic in a language with no debugger. Left open for later. |

## Where the compiler lives

The compiler is written once, in Rust, and shipped three ways: a library the Rust macro calls, a native CLI, and WebAssembly for the npm package and a browser playground. TypeScript users install an npm package and never see Rust, which is how esbuild, SWC and Biome are distributed.

Writing it in TypeScript instead would force every Rust build to depend on Node. Writing it twice would give two parsers to keep in agreement, for no benefit.

The two interpreter cores are written twice, natively, on purpose. The alternative, compiling the Rust core to WebAssembly for TypeScript, would mean every guard call crosses the boundary, values are serialised in and out, and Effect users debug into a binary. The core is a few hundred lines and the conformance suite keeps the two honest.

## Existing libraries

Checked in November 2026.

| | What it is | Why not just use it |
| --- | --- | --- |
| **XState** v5, with v6 in beta | The reference statechart library for TypeScript. Machines are code, with implementations referenced by name or written inline. Stately's hosted studio visualises them. | TypeScript only. Visualisation of code needs their tooling or a running program. Context is untyped across states. |
| **`@xstate/effect`** (alpha) | Stately's bridge between XState v6 actors and Effect v4. | Same as XState, and not yet stable. |
| **`@typeonce/effect-machine`**, **`@humanlayer/effect-machine`**, **`effstate`** | Community statechart libraries built on Effect v4, schema-first, defined in TypeScript. | TypeScript only, code-first, no static diagram. Good references for the Effect driver. |
| **`effect/workflow`** | Effect's durable execution module: activities, durable clocks. | Not a statechart. Flow is ordinary code and is not visible. A possible future back end, see below. |
| **statig**, **smlang**, **rust-fsm** | Rust state machine crates built on macros. statig supports hierarchy and async. | Rust only, code-first. rust-fsm can emit a diagram, the others cannot. |
| **rustate** | An xstate-inspired Rust crate with hierarchy, parallel states and a basic actor trait. Last released in April 2025. | Rust only, a string-keyed builder API, and machines in different crates communicate through shared memory. Compared in detail on the [actors and messaging](actors.md#compared-with-rustate) page. |
| **SCXML** | The W3C standard, in XML, with a precisely specified algorithm. | Roadbook adopts its semantics. XML is not something people want to write or review, and it has no types. An SCXML exporter exists (`roadbook export scxml`). |
| **Amazon States Language**, **Serverless Workflow** | JSON and YAML workflow definitions, validated and visualised by their platforms. | Proof that a declared, validated, visualised flow works at scale. Both are tied to a workflow service, are flat rather than hierarchical, and suffer the string-embedded-expression problem described above. |
| **Lucy**, **state-machine-cat**, **Sismic** | Earlier textual statechart languages: Lucy compiled to XState, state-machine-cat renders diagrams, Sismic interprets YAML in Python. | Prior art for the syntax. None has types, two runtimes or tasks with declared errors. |

Using XState's JSON machine format as the IR was considered. A Rust interpreter for it is feasible, and it would inherit Stately's tooling. It was rejected because the format belongs to another project and is changing between v5 and v6, it has no place for types or declared errors, and implementations referenced by name receive the whole context, which is the model this design moves away from.

## Durable execution

Out of scope for the first versions, but the design keeps the door open. A snapshot is serialisable because every type has a schema. A task is a named unit with typed input and output, which is exactly what a durable engine calls an activity. A later driver could run a chart on `effect/workflow`, persisting the snapshot at each step, without changing the language.
