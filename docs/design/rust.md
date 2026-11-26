# Rust runtime

The Rust side mirrors the TypeScript side: `roadbook-core` is the pure interpreter, and `roadbook` is the driver, the macro and the public API.

The API on this page is implemented by [`crates/roadbook-core`](../../crates/roadbook-core) (the interpreter), [`crates/roadbook-macros`](../../crates/roadbook-macros) (`include!`) and [`crates/roadbook`](../../crates/roadbook) (the driver, re-exporting the macro). The runtime passes every conformance case byte for byte, and [`examples/shop/axum`](../../examples/shop/axum) serves the shop example with it. [`examples/inventory`](../../examples/inventory) and [`examples/orders`](../../examples/orders) are two crates whose machines message each other.

## What gets generated

There is no separate generation step. A macro runs the compiler at build time:

```rust
roadbook::include!("../place_order.road");
```

The path is relative to the package's directory (`CARGO_MANIFEST_DIR`); the chart's imports and diagnostics are relative to its project root, the nearest directory above it that holds a `roadbook.toml`. The macro is called at module level, at most once per module, and expands, next to the call, to a module per machine plus the shared types:

- A `struct` or `enum` for every `type`, `enum` and `error`, with `serde` impls: fields in `snake_case`, renamed to the chart's names in JSON, and errors carrying `"_tag"` as the [value encoding](../../spec/execution.md#values) requires (decoding an error checks its `_tag`). `json` is `serde_json::Value` and `duration` is `std::time::Duration`. An `extern type` generates nothing: the application defines it next to the call, with `serde` impls.
- Numbers follow the value encoding, not serde's defaults. A JSON number's form does not matter, so an `int` (and a `duration`) accepts any integral number, `2.0` and `2e0` as well as `2`, and refuses `2.5`. A non-finite `float`, which `serde_json` would write as `null`, does not encode: from host code (a function's or task's result, a machine argument, an event) it is a `host` defect. An extern type's JSON is its own `Serialize`'s, so a non-finite float inside one is not caught.
- For every fallible task and machine, an error `enum` of exactly the errors it declared, with `From` impls so `?` converts into it. Rust has no anonymous unions, so `! CardDeclined | GatewayUnavailable` becomes `ChargeCardError`. If the name is taken, a task's is `<Name>TaskError` and a machine's `<Name>MachineError`.
- A `Handlers` trait: one method per function, action, task and child machine the chart uses.
- `State`, `Event` and `Emitted` enums: the states, the events the machine accepts and the events it `emits`. Compound states carry their active child: `State::Processing(Processing::Paying)`. A child enum whose name is taken is prefixed with the names of the states above it.
- `Ref`, a handle to a run of the machine (`roadbook::Ref<Event>`): the Rust type of `ref Machine`, in `run` and `spawn` parameters and in `Handlers` methods that take one (by reference, `&inventory::Ref`).
- `run` and `spawn` functions, and `run_with` and `spawn_with`, which take a `roadbook::Config` (trace sink, timer, root run id).
- The IR, embedded as a constant.

A chart error is a Rust compile error, reported at the macro call with the chart's file and line. Cargo rebuilds when any of the chart's files changes, or the `roadbook.toml` that made the project root: the expansion `include_bytes!`es each of them. Stable Rust has no way for a macro to track a file that does not exist, so creating one is not seen: a new `roadbook.toml` between the chart and its project root (or above a chart that has none), which would move the root, takes effect at the next rebuild for another reason (touch the chart). A missing import is a compile error, and Cargo retries a crate that failed on every build, so creating the imported file is seen. A `build.rs` route to the same generator is not built yet.

A chart that imports from another crate gets that crate's chart from the crate itself. Each `include!` also exports a hidden macro that hands its IR and `$crate` to the importer's macro, so nothing has to find files in Cargo's caches, and the importer re-exports the owning crate's types and machine modules instead of generating its own ([across crates](actors.md#across-crates-and-packages)). A renamed dependency is named in the call: `roadbook::include!("orders.road", crates(inventory = inv))`. [`examples/inventory`](../../examples/inventory) and [`examples/orders`](../../examples/orders) are two crates that work this way. A build script cannot call macros, so the `build.rs` route covers charts without cross-crate imports. Supporting them there would need `links` metadata from the owning crate's build script.

## Implementing a machine

```rust
pub struct App { users: Users, payments: Payments }

impl checkout::Handlers for App {
    fn retry_delay(&self, attempt: i64) -> Duration {
        Duration::from_millis(200 * 2u64.pow(attempt as u32))
    }
    async fn charge_card(&self, order: &Order, user: &User) -> Result<Receipt, ChargeCardError> {
        self.payments.charge(order, user).await
    }
}

impl place_order::Handlers for App {
    fn problem(&self, status: i64, code: &str) -> ApiResponse {
        ApiResponse { status, body: serde_json::json!({ "code": code }) }
    }
    async fn authenticate(&self, token: &str) -> Result<User, AuthenticateError> {
        Ok(self.users.by_token(token).await.ok_or(Unauthenticated)?)
    }
    async fn checkout(&self, order: &Order, user: &User) -> Result<Receipt, CheckoutError> {
        checkout::run(self, order.clone(), user.clone()).await
    }
    // … the rest of the trait
}
```

The contract is enforced by the trait. A missing method is a compile error, a task can only return its own error enum, and removing something from the chart removes the trait method, so a stale implementation stops compiling.

A child machine is one trait method. Delegating to the real chart is one line (`checkout::run`), and a test implements the same method with a canned result. The method's body has no parent run id, trace sink, clock or cancellation to pass on, and needs none: the driver polls the method's future inside an ambient invoke context, and a `run` that finds one becomes the parent's child run (see [child runs](#child-runs-and-cancellation) below).

Functions are plain `fn`. Actions and tasks are `async fn`, declared in the trait as returning `impl Future + Send` so machines can run on a multi-threaded runtime.

## Running

```rust
// One run per request. With axum this is a handler body.
pub async fn post_orders(app: Arc<App>, token: String, draft: OrderDraft) -> ApiResponse {
    place_order::run(&*app, token, draft).await
}

// Long-lived: send events, watch state.
let mut actor = place_order::spawn(app.clone(), token.clone(), draft.clone());
actor.send(place_order::Event::Cancel).await;
let state = actor.changed().await;
let response = actor.result().await;       // Result<ApiResponse, roadbook::Stopped<Infallible>>

// Or cancel one from outside: its exit effects run, and its trace ends with `cancelled`.
let actor = place_order::spawn(app.clone(), token.clone(), draft.clone());
actor.cancel().await;
assert_eq!(actor.result().await, Err(roadbook::Stopped::Cancelled));

// Peers: pass a spawned run's handle as the chart's `ref` argument. Host code can send through
// a handle too.
let stock = inventory::spawn(app.clone(), "w1".into());
let order = order::spawn(app.clone(), id, draft, stock.handle());
stock.handle().send(inventory::Event::Restock { sku, qty: 5 }).await;

// With a trace and a timer of your own (a test's virtual clock, say).
let config = roadbook::Config::new().sink(Arc::new(roadbook::JsonLines::new(std::io::stderr()))).timer(timer);
place_order::run_with(&config, &*app, token, draft).await;
```

A machine that declares no errors returns its output directly from `run`. One that does returns `Result<Output, MachineError>`. `spawn` runs the machine on a Tokio task (the default `tokio` feature); dropping the actor stops it like dropping `run`'s future. An actor's `result()` returns `Result<Output, roadbook::Stopped<MachineError>>`, where `Stopped` is `Failed(error)` or `Cancelled`. It takes the actor, so dropping its future before it is ready drops the actor and stops the run too: `tokio::time::timeout(d, actor.result())` that times out leaves nothing running. A run that ended with a defect resumes unwinding with its `roadbook::Defect` there, and so does an output or error that its Rust type refuses to decode (an extern type's `Deserialize`, say), as a `host` defect.

## How the driver maps onto Rust

| Statechart concept | Rust |
| --- | --- |
| A machine instance | One future that owns everything the machine started |
| The event queue | One FIFO per run (a `VecDeque`) that every input is pushed onto, fed by a shared inbox (`Actor::send`, `RunHandle::push`) that the loop drains |
| A state's `invoke` | A future held in a set keyed by invoke id. Dropping it cancels it, except a child run's, which is cancelled through its token. |
| `after` timers | Sleep futures from the `Timer`, in a set keyed by timer id |
| The loop | `select` over the channel, the invoke set and the timers, pushing what completed onto the FIFO; the core is given the FIFO's front |
| A child machine run from its chart | The trait method's future, polled inside an invoke context that its `run` claims |
| A `ref` to a run | A `Ref` handle (`roadbook::Ref<Event>`) around the run's shared inbox, which encodes as the run id |
| `send`, `emit` | A push onto the receiver's shared inbox, refused once the receiver has closed; a run's subscribers are kept beside its inbox |
| A task's declared errors | The generated error enum, matched to the `fail` branch |
| A defect | A panic, caught with `catch_unwind` around functions, actions and invoke futures; a run that ends with one resumes unwinding with a `roadbook::Defect` payload |
| Host code | The erased `roadbook::dynamic::Host` (called by name with JSON values), which the generated `Handlers` are adapted to |
| Trace output | A `TraceSink` trait object, with a no-op default, `JsonLines` and `MemorySink` |
| Tracing | `tracing` spans per machine and per state: not built yet |

Rust futures are cancelled by dropping them, so "leaving a state cancels its work" needs no extra machinery for a task: the driver drops the invoke's future. Because the driver holds futures and never spawns tasks, `run` borrows the handlers (`&H`) and works on any executor. Only timers need a runtime, behind a small `Timer` trait with a Tokio implementation enabled by default; the conformance runner implements it with a virtual clock.

A completed invoke future or timer is never handled directly: the loop pushes its outcome onto the run's FIFO, behind whatever arrived first, so an outcome can be stale by the time it is taken ([one queue per run](../../spec/execution.md#one-queue-per-run)).

### Child runs and cancellation

The rules are in [execution.md](../../spec/execution.md#child-runs); in Rust they come out like this.

- **The invoke context.** When the driver starts a machine invoke it sets a thread-local invoke context around each `poll` of the method's future (and around the call that creates it), restoring the previous one afterwards, so it works on any executor. It holds the trace sink, the clock and timer, the parent's run id, the invoke id, a cancel token, the invoke's `as` name and the parent's inbox. The token is also the child's inbox: what the parent sends before a run has claimed it waits there, and the child takes it first. `checkout::run` finds it, claims the token and runs as child run `r1.4`, writing to the parent's sink. A canned test implementation never calls `run`, never claims the token, and is treated as a task. Functions, actions and tasks are polled with no context.
- **Cancelling a child.** Dropping a child's future would skip its exit effects. So when its state is left the parent signals the token and keeps polling the future while the child runs its cancel step (exit effects, the `cancelled` record), and drops it once the child reports through the token that it has finished. Leaving the state waits for that, like an action. A task's or a stub's future is dropped at once.
- **Defects.** The driver wraps functions, actions and invoke futures in `catch_unwind`, around each call, each poll and each drop. A panic whose payload is a `&str` or `String` becomes a host defect with that message, and any other payload one with the message `Box<dyn Any>`. A run that ends with a defect resumes unwinding with a `roadbook::Defect` payload, which the parent's driver recognises and passes on unchanged, so a child's defect reaches the root as it was. A defect in a child's cancel step reaches the parent through the token and ends the parent with that defect.
- **Panics while dropping.** What a cancelled invoke does is ignored, so a panic while the driver drops a cancelled task's or stub's future (or a child run's, once its cancel step is done), one whose outcome was queued from outside and taken, or anything a run holds when it is stopped hard, is caught and ignored. A future that panics while it is dropped after it completed is host code failing before its invoke ended: the invoke's outcome is that `host` defect in place of the one it gave, as if the panic had happened in its last poll. The same goes for an action's future: the action fails with the defect.
- **Encoding.** A machine argument that does not encode (a non-finite `float`) is a `host` defect before the run starts: `run` resumes unwinding with it, with no trace records, and `spawn` gives an actor whose `result()` does. An event that does not encode is queued as a `defect` input, so the run ends with that `host` defect when it takes it.
- **Cancelling from outside.** Dropping the future of a root `run` is a hard stop: nothing more runs, no exit effects and no final trace record, and everything the run held is dropped with it. Graceful cancellation goes through the actor: `spawn` the machine and call `actor.cancel().await`, which returns once the exit effects have run and the trace has its `cancelled` record.

### Messages

The rules are in [execution.md](../../spec/execution.md#messages-between-runs). Each run has a small shared value beside its future (its inbox, cancel flag and finish report, and now whether it is open and which runs are subscribed to it), and every message is a push onto the receiver's inbox, which wakes the receiver's task. Nothing is delivered by calling the receiver.

- **Handles.** `actor.handle()` gives a `Ref` (`inventory::Ref` is `roadbook::Ref<inventory::Event>`), a cloneable `Send` handle to the run. It serialises as the run id, and while the generated `run` or `spawn` encodes the arguments it collects every handle it meets, so the driver gets the handles beside the ids: a run reaches exactly the runs it was given, with no registry. While the driver calls host code (a function, an action, an invoke's creation) it sets the run's handles as an ambient scope, so a `ref` argument decodes back into a `Ref`. In the erased interface a `ref` is a run id and the handles travel beside the values: `dynamic::run`, `run_with` and `start` take them, and an `Invocation` carries those its arguments name.
- **Host code sends** through a handle, `stock.handle().send(inventory::Event::Restock { … }).await`, as through the actor: an ordinary event with no sender. Host code cannot receive what a run emits through a handle; that is not specified yet.
- **Open and closed.** The driver closes a run as soon as its core returns a snapshot that is not `running`, before it carries out that step's commands; closing and delivering take the same lock, so a push either lands before the close or is refused. A refused `send` gets the driver's `undelivered` record.
- **Subscriptions.** Before `start` the driver subscribes the run to every peer named in its arguments, once each; closing unsubscribes it. `emit` pushes onto the invoker's inbox when the invoke had `as`, and onto every subscriber's.
- **Order.** A run's loop drains its inbox, polls its invokes, and drains the inbox again before it queues the outcome of an invoke that ended in that poll, so what a child emitted in its last step comes before its outcome.

`roadbook-core` has no async and no I/O. Keeping it `no_std` with `alloc` is a goal to hold onto if it stays cheap, since it would let charts run on embedded targets, but it is not a first-version requirement, and the first version uses `std` (as `roadbook-ir` does).

The conformance runner (`crates/roadbook/tests/conformance.rs`) runs every case through this driver: it implements `dynamic::Host` from the case's script and `Timer` with a virtual clock, and is its own executor, polling the future of every top-level run until a round of polls leaves no wake-up behind, which is quiescence, since every child run is polled inside its top-level run's future and every delivery wakes its receiver.

## Interpreter first, generated code later

The first version interprets the embedded IR, like the TypeScript runtime. That keeps the two cores line-for-line comparable and puts all the effort into one set of semantics. If a hot machine ever needs it, the macro can instead generate a `match`-based state machine with no interpretation overhead. The conformance suite would hold that generated code to the same traces.
