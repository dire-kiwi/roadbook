# Actors and messaging

Two questions came up in review of the first draft. Does Roadbook need xstate's actors? And can a machine send messages to a machine defined in another crate? This page answers both and specifies the design.

## Short answers

**Actors.** A running Roadbook machine already is an actor: it has private state, a queue, and handles one event at a time. What the first draft lacked is machines talking to each other while they run. That is needed, and this page adds it. What Roadbook should not copy is xstate's free-form version, where any actor can hold a reference to any other and send it anything. That makes "who talks to whom" a fact about a running program, which is exactly the kind of thing this project exists to make visible in a file. Roadbook's version is declared and typed, so the conversation between machines is part of the chart and part of the diagram.

**Other crates.** Not in the first draft. With the additions here, yes: a machine's events are a typed interface declared in its chart, another crate's chart imports that interface, and sending is a queue operation on a typed handle. The same design covers npm packages, and because events have schemas it extends to other processes.

## What xstate's actor model contains

| xstate | Roadbook | Status |
| --- | --- | --- |
| Machine actors with a mailbox, one event at a time | Every running machine | In the first draft |
| `invoke`: a child tied to a state | `invoke` | In the first draft |
| Promise actors | Tasks | In the first draft |
| `sendTo` a child, `sendParent`, `emit` | `send` and `emit`, checked against the receiver's declared events | **Added here** |
| References to other actors, passed around in context and events | `ref Machine` parameters: typed, and visible in the signature | **Added here** |
| `spawn`: any number of children, references kept in context | Keyed `spawn` | Sketched here, built later |
| Callback and observable actors (event sources) | Host code calls `send` on the handle. A `stream` task kind may follow. | Later |
| `system` and `systemId`: look up any actor by name | Not adopted. A machine reaches only what it is given. | Rejected |

The rejected row is the important one. A global registry lets any machine reach any other without that appearing in a signature. Passing references as parameters keeps the set of possible conversations readable from the charts alone.

## Talking to an invoked child

A machine's `event` declarations are what it accepts. A new `emits` declaration is what it sends out. Together with its parameters, result and errors they are its complete interface.

```
/// Upload a file in chunks. Can be paused.
machine Upload(file: File) -> Url ! UploadFailed {
  event Pause
  event Resume
  emits Progress { percent: int }

  initial Sending
  state Sending {
    invoke sendChunk(file, offset) {
      done -> Sending  set offset = result  emit Progress { percent: percentOf(file, offset) }
      fail UploadFailed -> Failed
    }
    on Pause -> Paused
  }
  state Paused { on Resume -> Sending }
  // …
}
```

The parent names the invoked child with `as` and can then send to it and react to what it emits:

```
state Uploading {
  invoke Upload(file) as upload {
    done              -> Published  set url = result
    fail UploadFailed -> Failed
  }
  on PauseClicked    send upload.Pause
  on ResumeClicked   send upload.Resume
  on upload.Progress set percent = event.percent
}
```

`send` and `emit` are effects, alongside `set` and `do`. The name `upload` is in scope only inside `Uploading`, which is exactly while the child is alive, so a chart cannot send to a child that has stopped. `send upload.Stop` is a compile error if `Upload` declares no `Stop`.

## Talking to a peer

A long-lived machine that is not a child is reached through a reference passed in as a parameter.

```
machine Order(id: string, draft: OrderDraft, inventory: ref Inventory) -> Receipt ! OutOfStock {
  state Reserving {
    entry send inventory.Reserve { orderId: id, items: draft.items }
    on inventory.Reserved if event.orderId == id -> Paying
    on inventory.Rejected if event.orderId == id -> Unavailable
    after 5s -> Unavailable
  }
  // …
}
```

`ref Inventory` is a type: a handle to some running `Inventory` machine. Which instance is decided by whoever starts `Order`, in host code or in a parent chart. Holding a reference lets a machine `send` the peer's declared events and handle the peer's emitted events with `on inventory.Name`.

A machine cannot conjure a reference. It gets one as a parameter, from `invoke … as`, or (later) from `spawn`. So the project index can draw, from signatures alone, which machines can talk to which.

## Many children

`invoke` is one child per state. A dynamic number (one machine per connection, per order, per player) needs `spawn`. The intended shape is a keyed collection declared on the machine, so the children remain typed and visible:

```
children sessions: Session by SessionId

on Connected     spawn sessions[event.id] = Session(event.socket)
on Disconnected  stop sessions[event.id]
on Broadcast     send sessions.*.Notify { text: event.text }
```

This is a sketch. It is planned after the first two forms have been used in real charts.

## Semantics

The precise rules are in [`spec/execution.md`](../../spec/execution.md#messages-between-runs), the records in [`spec/trace.md`](../../spec/trace.md#messages).

- **Sending is queuing.** `send` puts the event in the receiver's queue and continues. The receiver handles it later, by the ordinary one-event-at-a-time rule. A sender never runs the receiver's transitions and never waits for them. A sent event is one of the receiver's own events, so `on Pause` takes it exactly as it would take `Pause` from host code.
- **Order is preserved per pair.** Two events sent from A to B arrive in the order sent, and so does everything else A queues for B: A's emitted events, and a child's outcome, which comes after everything the child emitted. Nothing orders what two different machines send to a third.
- **Emitted events arrive as events.** They are queued for the invoker, if it named the child with `as`, and for every machine holding a reference, and are handled by `on name.Event`. Only a machine that has an `on` for an event, in any state, is sent it ([who receives emitted events](#who-receives-emitted-events)): an invoker without `as` has no name to handle them under, and a holder that only sends to the peer is sent nothing. One that is sent an event and has no matching transition when it takes it drops it, traced like any unhandled event.
- **Holding a reference** means having it among the machine's arguments, from start to end: a machine gets a `ref` only as an argument, so the machines it can hear from are fixed when it starts. `on inventory.Reserved` is taken for an event from whichever machine the parameter or variable `inventory` holds.
- **A child's emitted events belong to its invoke.** One that arrives after the invoking state was left, or re-entered with a new child, is stale, like an outcome that arrives late: a child cancelled by its parent can still emit from its exit effects, and its parent ignores what it said.
- **Sending to a machine that is not running** is dropped and traced as `undelivered`: a peer that has ended, or a child that has not started yet or has already finished. The compiler rules out the plain cases for invoked children (a send from the invoking state's own `entry`, `exit` or outcomes, or from a transition that leaves it); what it cannot see, a send from a state inside the invoking state in the step that enters it, before the child starts, or after the child's outcome was taken by a transition that stays, is undelivered at run time. Whether a [`strict` machine](language.md#strict-machines), which is decided but not built, also fails on an undelivered send is left to its implementation.
- **Endings are not messages.** A machine that ends, or is cancelled, does not tell those that hold a reference to it ([supervision](#supervision)). What is still queued for it is discarded, and its exit effects can still send and emit like any effects.
- **References are run ids.** In snapshots, traces and conformance scripts a `ref` is the id of the run it refers to, such as `"r2"`; host code holds a typed handle instead. References are not compared: `==` is not defined on them.
- **Everything is in the trace.** A `sent` record carries the receiver's run id, an `emitted` record the event, and the receiver's record of the input carries the sender's run id and the sender's message id, so the viewer can draw a sequence view across machines as well as each machine's own path. A send that reaches no machine is an `undelivered` record.

## Across crates and packages

Three things have to hold for a machine in one crate to message a machine in another: the charts have to see each other, the generated types have to be the same types, and delivery has to work at run time.

**Charts.** The crate that owns a machine ships its `.road` file. Another crate's chart imports from it by package name:

```
import { Inventory } from "inventory/inventory.road"
```

A path that does not start with `./`, `../` or `/` names a file in another package: its first segment is the package (its first two for `@scope/name`), the rest the file in it. The command line finds the package in the `[packages]` table of `roadbook.toml` or, as Node does, in `node_modules`, and follows the package's `exports` ([projects](language.md#projects)); two packages may import each other, and a file reached both by a relative path and as a package's file is loaded once; [`examples/orders`](../../examples/orders) imports [`examples/inventory`](../../examples/inventory) through the root `roadbook.toml`. The compiled IR stays self-contained: the imported machine and everything it uses are in it, each file of the other package named in `sources` by its package and its path in the package, which is also what marks its declarations as the other package's ([`spec/ir.md`](../../spec/ir.md#sources)).

**Types.** The importing crate does not generate its own copy of `Inventory`'s types. Its generated code refers to the owning crate's: for every declaration from the other package it re-exports the owning crate's item instead of generating one (`pub use ::inventory::Item;`, and `pub use ::inventory::inventory;` for the machine's module, with its `Event`, `Emitted` and `Ref`). There is one definition of each type, so the Rust compiler checks every cross-crate send: `order::run`'s `inventory: ref Inventory` is an `inventory::inventory::Ref`, which is `roadbook::Ref<inventory::inventory::Event>`, and a handle to any other machine, or an event `Inventory` does not accept, does not compile ([`examples/orders/tests/ui`](../../examples/orders/tests/ui)).

**Delivery.** A `Ref` is a cloneable, `Send` handle around the target's queue.

```rust
// crate `inventory`, at its root
roadbook::include!("inventory.road");      // generates inventory::inventory::{Event, Emitted, Ref, Handlers, spawn, …}
pub use self::inventory::{Emitted, Event, Handlers, Ref, spawn, spawn_with};   // optional: inventory::Event

// crate `orders`, whose chart imports Inventory
roadbook::include!("orders.road");
let stock = inventory::spawn(warehouse, "w1".into());
let order = order::spawn(app, id, draft, stock.handle());   // the chart's `inventory: ref Inventory`

// host code in any crate can send too
stock.handle().send(inventory::Event::Restock { sku, qty }).await;
```

Host code cannot receive what a run emits through its handle: which of a run's emitted events host code would see, and how, is not specified yet.

TypeScript works the same way with packages: the chart imports `"@shop/inventory/inventory.road"`, and the generated file imports types from that package's generated file.

The Rust macro does not look for the dependency's `.road` file at all. The compiler's loader takes an import satisfied by an already compiled IR (`Loaded::Compiled`), whose declarations enter the importing chart's scope as a parsed file's would and are copied into its IR unchanged, so their hashes are the dependency's. A spike (`spike/cross-crate`, now deleted) tried four mechanisms against path, git, vendored and registry dependencies, and against a crate that is itself a dependency, and `roadbook::include!` is built on the one that worked everywhere:

1. Each `include!` embeds its chart's IR and also exports a hidden `#[macro_export] macro_rules!`, named after the chart's path in its package, every character but ASCII letters and digits written as `_`, its hex code and `_` so that no two paths share a name (`__roadbook_chart_inventory_2e_road` for `inventory.road`).
2. When the importing chart's `include!` compiles and its loader is asked for `inventory/inventory.road`, which no crate has handed over yet, it expands instead to a call of `::inventory::__roadbook_chart_inventory_2e_road!`, passing its own arguments and a continuation, the hidden proc macro `roadbook::__private::include_continue`.
3. That macro calls the continuation back with the arguments, the IR, `$crate`, and the names the owning crate gave the chart's error enums. The continuation compiles the chart again, its loader now answering the import with `Loaded::Compiled(ir)`, and generates code that names the dependency's items through `$crate`. A chart importing from several packages goes round once per package. The `$crate` that a hidden macro hands over carries the span of its own crate's `include!` path literal, not the macro's call site, which in a chain lies inside the previous crate's hidden macro; so it always names its own crate, and chains (`c2` imports `c1` imports `c0`) and diamonds (`apex` imports `left` and `right`, which both import `inventory`) work ([`crates/roadbook/tests/include_crates.rs`](../../crates/roadbook/tests/include_crates.rs)).

This works for every kind of dependency, always sees the version that is actually compiled, needs no build script or configuration, and Cargo's normal dependency tracking rebuilds the importer when the chart changes: editing `examples/inventory/inventory.road` rebuilds `inventory`, then `orders`, whose embedded `Inventory` has the new hash. The limits, as built:

- The package's name is the crate's name (`-` read as `_`), unless the call names the crate: `crates(my-stock = stock)`, or quoted, `crates("@scope/x" = x)`, for a name that is not one.
- Only a file that the owning crate itself `include!`s can be imported, by its path relative to the crate's directory, since only those have a hidden macro.
- The owning crate's `include!` must be at its crate root, since the importer names its items as `$crate::Name`. The hidden macro checks this: an importer of a chart included only in a module fails with an error that says to move the `include!` to the crate root ([`crates/roadbook/tests/ui/owner_in_module.rs`](../../crates/roadbook/tests/ui/owner_in_module.rs)).
- A crate can include the same chart more than once, in different modules, each with types of its own. Only the first `include!` of it to be expanded exports the hidden macro (a crate cannot export two macros of one name), so one of them must be at the crate root for other crates to import it.
- Several importable charts can sit at one crate root: their hidden items are named after their paths (`__ROADBOOK_IR_inventory_2e_road`). They must not declare a name in common, as any two charts included in one module.
- A declaration that the importer gets through a chain of packages (`a` imports from `b`, and the importer from `a`) is re-exported from `a`, which re-exports it from `b`.
- An error enum's name depends on the names already taken, and the owning crate takes those of the crates it imports from first, so the importer cannot work it out from the IR: the hidden macro hands the owner's names over. In a chain where `c0` has `task charge() ! Declined` and `c1` imports it and declares `machine Charge() ! Declined`, `c1` keeps `c0`'s `ChargeError` for the task and so names the machine's `ChargeMachineError`, and `c2`, importing `Charge` from `c1`, re-exports both under those names (`errchain0` to `errchain2` in [`crates/roadbook/tests/crates`](../../crates/roadbook/tests/crates)).

The other three mechanisms were rejected:

- Running `cargo metadata` from the macro misses the project's Cargo configuration when the importing crate is itself a dependency. It can then reach the network, write a `Cargo.lock` into Cargo's caches, or read a different version from the one being compiled.
- Passing the path through build scripts works, but it needs `links` and a `build.rs` in every crate on both sides, and `links` forbids two semver-incompatible versions of the owning crate in one graph.
- A `roadbook.toml` mapping works only for path dependencies.

The caveat is renamed dependencies. The macro calls the owning crate by its package name, so `inv = { package = "inventory" }` needs an alias in the call: `roadbook::include!("orders.road", crates(inventory = inv))` ([`crates/roadbook/tests/include_renamed.rs`](../../crates/roadbook/tests/include_renamed.rs)). Without it the error is that no crate `inventory` can be found. The package is named as the import writes it, bare (`crates(my-stock = stock)`, or `my_stock`) or quoted (`crates("my-stock" = stock)`).

**Handles at run time.** Both runtimes check a handle against the `ref M` it is passed for: TypeScript by the machine's name, Rust by its name and, since a handle knows the program its run runs, its machine hash, so a handle to a run of an older build of `M` is refused too. The typed `Ref`s of the generated Rust code cannot mismatch; the check is for the erased `dynamic` interface. A mismatch is a host defect before the run starts. A child run's handle, which only Rust's `Config::on_run` hands out (for tests that feed a child inputs directly), cannot be passed as a `ref` at all, since nothing but its parent may reach a child run.

### Across processes and languages

Every event has a generated schema and one JSON encoding, identical in both runtimes because both come from the same chart. A Rust machine can therefore receive an event produced by a TypeScript machine over any transport. The first versions leave the transport to host code: an adapter decodes a message and calls `send` on a handle. A `remote ref` that does this declaratively is possible later and needs no change to the language's types.

## Compared with rustate

[rustate](https://crates.io/crates/rustate) is an xstate-inspired Rust crate that lists cross-crate integration and actors as features. Its latest release is 0.3.0, from April 2025.

| | rustate 0.3.0 | Roadbook |
| --- | --- | --- |
| How machines are defined | A Rust builder API. Events and states are strings. | A chart, with generated enums |
| Cross-crate mechanism | Shared memory: the machine in an `Arc<Mutex<…>>`, or a shared `serde_json::Value` context in an `Arc<RwLock<…>>` | A typed handle around the receiver's queue |
| What a send does | Locks the other machine and runs its transition inside the sender's action | Queues the event and returns |
| Checked at compile time | No. A misspelled event name is a runtime no-op. | Yes. Events are enum variants from the owning crate. |
| Visible in a diagram | No. Forwarding happens inside closures. | Yes. `send`, `emit` and `ref` are in the chart. |
| Across processes | Listed on its roadmap as research | Events have schemas. Transport is left to the host at first. |
| Actors | An `Actor` trait, `ActorRef` and `spawn`, described as basic | Machines are actors. References are typed by machine. |

rustate's README describes message passing as planned and shared memory as the current approach. Shared memory has two costs worth avoiding. Running the receiver inside the sender breaks one-event-at-a-time: a machine can be re-entered halfway through its own step, and as the README's example is written, two machines that forward to each other would deadlock on the mutex. And a shared JSON context is the hidden shared state that this design removes everywhere else.

## Grammar additions

Implemented in milestone 5 in the language, the compiler and IR 0.2 (the [language page](language.md#messages-between-machines) has the rules and checks, [`spec/ir.md`](../../spec/ir.md) the IR). What the runtimes do with it is specified in [`spec/execution.md`](../../spec/execution.md#messages-between-runs) and [`spec/trace.md`](../../spec/trace.md), and held to it by the conformance cases that `require` `messaging`. Both runtimes implement it and pass those cases byte for byte ([TypeScript](typescript.md#messages), [Rust](rust.md#messages)).

```ebnf
machineItem = … | "emits" Upper [ fields ] ;
type        = … | "ref" Upper ;
stateItem   = … | "invoke" name "(" … ")" [ "as" lower ] "{" outcome { outcome } "}" ;
commonItem  = … | "on" lower "." Upper branches ;
effect      = … | "send" lower "." Upper [ recordBody ]
                | "emit" Upper [ recordBody ] ;
recordBody  = "{" [ lower ":" expr { "," lower ":" expr } ] "}" ;
```

`emits`, `send` and `emit` are reserved words; `ref` and `as` are not, since they mean something only where no name can appear. `on name.Event` is a common item, so a machine can handle a `ref` parameter's events in its body.

### Decided while implementing

- **Two namespaces of triggers.** The events a machine accepts (`event`) are its own and are triggered as `on Name`. The events it receives from others are named through their sender, `on upload.Progress`, a different trigger from `on Progress` and from `on other.Progress`. `on *` matches only the machine's own events, never an emitted one, so a catch-all does not swallow a child's progress reports.
- **One namespace of declarations.** A machine's `event` and `emits` names are one set: no name is both accepted and emitted. Like events, emitted events cannot be named like a type, error or machine.
- **A child is addressed only while it runs.** `upload` is in scope in the invoking state and its descendants. On top of that, the compiler rejects a `send` to it from the invoking state's own `entry` (the child starts after the step settles), `exit` (it is stopped before the exit effects run) and invoke outcomes (it has ended), and from a transition that leaves the invoking state (leaving stops it before the transition's effects run). Sends from the `entry` of a state inside the invoking state, in the step that enters the invoking state, happen before the child has started: they are undelivered, not queued for it ([execution](../../spec/execution.md#exiting-and-entering-a-state)). A compile-time rule would have to forbid sends to the child in every `entry`, `exit`, `always` and `done` inside the invoking state, since all of them can run in that step, and that also forbids them in later steps where the child runs; and a send after the child's outcome was taken by a transition that stays in the state is undelivered all the same.
- **Where a `ref` can be.** Machine parameters, variables, and the parameters of functions, actions and tasks, so a `ref` can be passed to an invoked machine and to host code. Not in fields of records, errors or events, nor in results: a machine still cannot conjure a reference, and events cannot carry references around. `==` is not defined on `ref`s, and a `ref` is not `json`.
- **An `as` name is not a value.** It is used only in `send name.E` and `on name.E`, so it cannot be stored in a variable that outlives the child. It shares the machine's namespace with parameters and variables, and cannot shadow an enclosing `as`.
- **A machine does not receive its own emitted events.**
- **Effect order.** Every `set` of a list runs first, then the `do`, `send` and `emit` effects in the order written, so a message carries the values its transition assigns.
- **Warnings.** An `emits` its machine never emits, an `on name.E` for an event the sender never emits, and an `emits` nothing in the checked charts receives (host code holding a handle may still).
- **Hash.** A machine that holds a `ref Inventory` includes only `Inventory`'s interface, its `events` and `emits`, in its hash: a change to how the peer works inside does not invalidate the snapshots of the machines that talk to it.

## Decided after the milestones

The first draft left four questions about messaging open. They were decided on 27 November 2026, after milestone 5 had built the rest of this page. Two of the decisions are what already existed, and the filter on emitted events has been built since; `call` needs code that is not written yet.

### Asking a peer

**Decided: `call` declarations. Not built.** Until they are, a request with a reply is written as in [talking to a peer](#talking-to-a-peer): a `send`, an `on` whose guard matches an id, and an `after`. Besides being verbose, that pattern has a cost: the reply is an emitted event, so it reaches every machine holding a reference to the peer. With N orders in flight, each reply is delivered N times and N − 1 orders drop it, which the [filter on emitted events](#who-receives-emitted-events) does not prevent, since every order has an `on` for it.

A long-lived machine declares the calls it answers, and a holder of a reference invokes one like a task:

```
machine Inventory(warehouse: string) {
  call reserve(items: Item[]) -> Reservation ! OutOfStock
  // … takes the call, and replies to it now or in a later step
}

machine Order(id: string, draft: OrderDraft, inventory: ref Inventory) -> Receipt ! OutOfStock {
  state Reserving {
    invoke inventory.reserve(draft.items) {
      done            -> Paying       set reservation = result
      fail OutOfStock -> Unavailable
    }
    after 5s -> Unavailable
  }
}
```

What is decided:

- **The caller's side is an invoke.** Its `done` and `fail` branches are checked against the call's declaration as a task's are, and it is tied to its state: leaving the state abandons the call, and a reply that arrives later is stale, like a late outcome. A timeout is an ordinary `after`.
- **Replies are directed.** Only the caller receives the reply, so there is no fan-out and no guard on an id.
- **The callee takes a call as an input** from its queue, one at a time like any other, so order per pair still holds. It must be able to reply in a later step (Inventory replies once its own work is done), so it keeps the pending call as a value until it replies. That is a new kind of value, and like a `ref` it cannot leave the machine: not in a record, an event or a result.
- **A peer that has ended fails the call.** A call to a peer that is not running, and a call still pending when the callee ends, fail on the caller's side. This is the one place where a peer's end reaches another machine before [supervision](#supervision) is designed.
- **The existing pattern keeps working.** Moving to `call` is optional; [`examples/orders`](../../examples/orders) should be its first user.

Left to the implementation: the syntax by which the callee takes a call and replies to it, the type of the pending-call value, and the error the caller sees when the callee has ended.

What it changes: the language (the `call` item, the addressed invoke, the callee's trigger and reply effect, the pending-call value, and their checks, formatter, tree-sitter grammar and language server), the IR and its version, so every hash changes; new inputs and commands in both cores and both drivers; new trace records; a callee's snapshot, which holds its pending calls; new conformance cases; and in the viewer, a call edge in the project index and a call that simulate mode treats like a task. Holding a reply address is close to what keyed `spawn` needs to hold its children, so whichever is built second should reuse the first's design.

### Full queues

**Decided: queues are unbounded.** This is what both runtimes do, and [`spec/execution.md`](../../spec/execution.md#one-queue-per-run) states it as a rule. There is no evidence of load that needs a bound. If one is added later, the principle is settled now:

- **It is a setting of the driver, not chart syntax.** No chart changes.
- **It covers messages and emitted events only**, and perhaps events from host code. Losing a task's outcome or a timer's expiry would leave a state waiting forever.
- **A run that sends to a full queue does not wait: the message is dropped** and written as `undelivered`, with a reason (`full`, beside today's ended receiver). A send from one run to another that waited could deadlock. A parent cancelling a child waits for the child's cancel step, and that step may emit into the parent's full queue; two peers sending into each other's full queues wait for each other; and a conformance runner cannot tell such a wait from being idle.
- **Host code that sends to a full queue waits.** Its `send` is already asynchronous in both runtimes.

Making an overflow a failure was rejected: one full queue would end a long-lived peer.

### Who receives emitted events

**Decided: only machines that handle them. Built.** A run receives an emitted event `E` from a run of machine `M` only if its own machine has an `on x.E` where `x` is an `as` name, parameter or variable of machine `M` ([`spec/execution.md`](../../spec/execution.md#messages-between-runs)). Before it was built, every holder of a reference received every emitted event and wrote `dropped` when it had no enabled transition.

The first draft called this an optimisation the compiler could make without changing the language. The language does not change, but traces do, so it is a rule of the semantics: a holder with no `on` for an event writes no `dropped` record for it. A holder that has one and cannot take it (its guard fails, or it is in a state with no such transition) still writes `dropped`.

- **The filter is by machine, not by name.** One run can be held under two names, as a parameter and a variable, and an `on` under either name is enough.
- **It is decided statically**, from the chart, when the run subscribes. Filtering by the transitions enabled in the current state was rejected: the state when an event is queued is not the state when it is taken, so events would be lost depending on timing.
- **It extends an existing rule.** An invoker without `as` is already not sent its child's emitted events, since it cannot have an `on` for them.
- **Nothing in the language, the IR, the hashes or snapshots changes**, and no chart needs editing. Every `on x.E` is in the IR with the machine `x` names, so each runtime works out, when it loads the IR, which events each machine hears from which (`Program::heard` in Rust, `heard` in `@roadbook/core`).

What it changed: the rule in [`spec/execution.md`](../../spec/execution.md#messages-between-runs); in each runtime, the set of events kept with each subscription, and with an invoke's `as`, and a check before delivering; and the conformance cases, where `child-undelivered` lost its `dropped` record and `peer-send-only` covers a holder that only sends. `peer-two-holders` stays as it was, since its holder has an `on` and fails on the guard. The viewer already drew emitted events from `on` triggers.

### Supervision

**Decided: none yet.** A child's failure reaches its parent as a `fail` branch on the invoke. A peer has no parent in the chart, and when it ends, holders of its reference are not told: they learn of it through their own timeouts, which they need anyway for a peer that is slow. Rust host code can ask a handle whether its run has closed (`Ref::is_closed`); no chart can. A pending [`call`](#asking-a-peer) is the exception, since it fails when the callee ends.

The intended shape is a trigger, `on inventory.ended`, possibly with the status the peer ended with. The lower case after the dot keeps it apart from `on name.Event`, and with the [filter on emitted events](#who-receives-emitted-events) writing it is the subscription, so no `monitor` declaration is needed. It is to be designed with keyed `spawn`, whose children need the same "a member has ended" event, so that one design covers both.

Erlang-style links, where a peer's end ends the machines linked to it, were rejected. `Inventory` outlives every `Order`, and one machine's failure would end another without that showing in either chart.
