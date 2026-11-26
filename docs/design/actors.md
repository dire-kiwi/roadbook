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

- **Sending is queuing.** `send` puts the event in the receiver's queue and continues. The receiver handles it later, by the ordinary one-event-at-a-time rule. A sender never runs the receiver's transitions and never waits for them.
- **Order is preserved per pair.** Two events sent from A to B arrive in the order sent.
- **Emitted events arrive as events.** They are queued for the invoker and for every machine holding a reference, and are handled by `on name.Event`. With no matching transition they are dropped and traced like any unhandled event.
- **Sending to a stopped machine** is dropped and traced, or a failure in a `strict` machine. For invoked children the compiler rules it out.
- **Everything is in the trace.** A `sent` and a `received` record carry both run ids, so the viewer can draw a sequence view across machines as well as each machine's own path.

## Across crates and packages

Three things have to hold for a machine in one crate to message a machine in another: the charts have to see each other, the generated types have to be the same types, and delivery has to work at run time.

**Charts.** The crate that owns a machine ships its `.road` file. Another crate's chart imports from it by package name:

```
import { Inventory } from "inventory/inventory.road"
```

**Types.** The importing crate does not generate its own copy of `Inventory`'s types. Its generated code refers to the owning crate's: `inventory::Event`, `inventory::Ref`. There is one definition of each type, so the Rust compiler checks every cross-crate send.

**Delivery.** A `Ref` is a cloneable, `Send` handle around the target's queue.

```rust
// crate `inventory`
roadbook::include!("inventory.road");      // exports inventory::{Event, Emitted, Ref, Handlers, spawn}

// crate `orders`, whose chart imports Inventory
let stock = inventory::spawn(warehouse);
let order = order::spawn(app, id, draft, stock.handle());   // the chart's `inventory: ref Inventory`

// host code in any crate can send too
stock.handle().send(inventory::Event::Restock { sku, qty }).await;
```

TypeScript works the same way with packages: the chart imports `"@shop/inventory/inventory.road"`, and the generated file imports types from that package's generated file.

One part is not yet proven: how the Rust macro locates a dependency's `.road` file at build time. Cargo can pass a path from a dependency's build script to a dependent's, and failing that a mapping in `roadbook.toml` works. This needs a prototype before the Rust runtime milestone commits to it.

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

```ebnf
machineItem = … | "emits" Upper [ fields ] ;
type        = … | "ref" Upper ;
stateItem   = … | "invoke" name "(" … ")" [ "as" lower ] "{" outcome { outcome } "}"
                | "on" lower "." Upper branches ;
effect      = … | "send" lower "." Upper [ recordBody ]
                | "emit" Upper [ recordBody ] ;
```

## Open questions

1. **Asking a peer.** A request to a peer with a reply is written above as a `send`, an `on` with a guard that matches an id, and a timeout. That is explicit but verbose. The alternative is to let a long-lived machine declare calls (`call reserve(items) -> Reservation ! OutOfStock`) that others `invoke` like a task, with the reply routed back automatically.
2. **Full queues.** Queues are unbounded in this draft. A bound would need a choice between making the sender wait, which leaks the receiver's speed into the sender's flow, and dropping.
3. **Who receives emitted events.** Every holder of a reference receives every emitted event and filters with guards. A subscription limited to the events a chart has an `on` for is an optimisation the compiler can make without changing the language.
4. **Supervision.** A child's failure reaches its parent as a `fail` branch on the invoke. A peer has no parent in the chart. If a peer dies, holders of its reference currently learn of it only through their own timeouts.
