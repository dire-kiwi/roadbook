# Rust runtime

The Rust side mirrors the TypeScript side: `roadbook-core` is the pure interpreter, and `roadbook` is the driver, the macro and the public API.

The API on this page is implemented by [`crates/roadbook-core`](../../crates/roadbook-core) (the interpreter), [`crates/roadbook-macros`](../../crates/roadbook-macros) (`include!`) and [`crates/roadbook`](../../crates/roadbook) (the driver, re-exporting the macro). The runtime passes every conformance case byte for byte, and [`examples/shop/axum`](../../examples/shop/axum) serves the shop example with it.

## What gets generated

There is no separate generation step. A macro runs the compiler at build time:

```rust
roadbook::include!("../place_order.road");
```

The path is relative to the package's directory (`CARGO_MANIFEST_DIR`); the chart's imports and diagnostics are relative to its project root, the nearest directory above it that holds a `roadbook.toml`. The macro is called at module level, at most once per module, and expands, next to the call, to a module per machine plus the shared types:

- A `struct` or `enum` for every `type`, `enum` and `error`, with `serde` derives: fields in `snake_case`, renamed to the chart's names in JSON, and errors carrying `"_tag"` as the [value encoding](../../spec/execution.md#values) requires. `json` is `serde_json::Value` and `duration` is `std::time::Duration`. An `extern type` generates nothing: the application defines it next to the call, with `serde` impls.
- For every fallible task and machine, an error `enum` of exactly the errors it declared, with `From` impls so `?` converts into it. Rust has no anonymous unions, so `! CardDeclined | GatewayUnavailable` becomes `ChargeCardError`. If the name is taken, a task's is `<Name>TaskError` and a machine's `<Name>MachineError`.
- A `Handlers` trait: one method per function, action, task and child machine the chart uses.
- `State` and `Event` enums. Compound states carry their active child: `State::Processing(Processing::Paying)`. A child enum whose name is taken is prefixed with the names of the states above it.
- `run` and `spawn` functions, and `run_with` and `spawn_with`, which take a `roadbook::Config` (trace sink, timer, root run id).
- The IR, embedded as a constant.

A chart error is a Rust compile error, reported at the macro call with the chart's file and line. Cargo rebuilds when any of the chart's files changes: the expansion `include_bytes!`es each of them. A `build.rs` route to the same generator is not built yet.

A chart that imports from another crate gets that crate's chart from the crate itself. Each `include!` also exports a hidden macro that hands its IR to the importer's macro, so nothing has to find files in Cargo's caches ([cross-crate probe](../../spike/cross-crate)). A build script cannot call macros, so the `build.rs` route covers charts without cross-crate imports. Supporting them there would need `links` metadata from the owning crate's build script.

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
let mut actor = place_order::spawn(app, token, draft);
actor.send(place_order::Event::Cancel).await;
let state = actor.changed().await;
let response = actor.result().await;       // Result<ApiResponse, roadbook::Stopped<Infallible>>
// Or cancel it from outside: its exit effects run, and its trace ends with `cancelled`.
actor.cancel().await;                       // then `result()` is Err(Stopped::Cancelled)

// With a trace and a timer of your own (a test's virtual clock, say).
let config = roadbook::Config::new().sink(Arc::new(roadbook::JsonLines::new(std::io::stderr()))).timer(timer);
place_order::run_with(&config, &*app, token, draft).await;
```

A machine that declares no errors returns its output directly from `run`. One that does returns `Result<Output, MachineError>`. `spawn` runs the machine on a Tokio task (the default `tokio` feature); dropping the actor stops it like dropping `run`'s future. An actor's `result()` returns `Result<Output, roadbook::Stopped<MachineError>>`, where `Stopped` is `Failed(error)` or `Cancelled`.

## How the driver maps onto Rust

| Statechart concept | Rust |
| --- | --- |
| A machine instance | One future that owns everything the machine started |
| The event queue | One FIFO per run (a `VecDeque`) that every input is pushed onto, fed by a shared inbox (`Actor::send`, `RunHandle::push`) that the loop drains |
| A state's `invoke` | A future held in a set keyed by invoke id. Dropping it cancels it, except a child run's, which is cancelled through its token. |
| `after` timers | Sleep futures from the `Timer`, in a set keyed by timer id |
| The loop | `select` over the channel, the invoke set and the timers, pushing what completed onto the FIFO; the core is given the FIFO's front |
| A child machine run from its chart | The trait method's future, polled inside an invoke context that its `run` claims |
| A task's declared errors | The generated error enum, matched to the `fail` branch |
| A defect | A panic, caught with `catch_unwind` around functions, actions and invoke futures; a run that ends with one resumes unwinding with a `roadbook::Defect` payload |
| Host code | The erased `roadbook::dynamic::Host` (called by name with JSON values), which the generated `Handlers` are adapted to |
| Trace output | A `TraceSink` trait object, with a no-op default, `JsonLines` and `MemorySink` |
| Tracing | `tracing` spans per machine and per state: not built yet |

Rust futures are cancelled by dropping them, so "leaving a state cancels its work" needs no extra machinery for a task: the driver drops the invoke's future. Because the driver holds futures and never spawns tasks, `run` borrows the handlers (`&H`) and works on any executor. Only timers need a runtime, behind a small `Timer` trait with a Tokio implementation enabled by default; the conformance runner implements it with a virtual clock.

A completed invoke future or timer is never handled directly: the loop pushes its outcome onto the run's FIFO, behind whatever arrived first, so an outcome can be stale by the time it is taken ([one queue per run](../../spec/execution.md#one-queue-per-run)).

### Child runs and cancellation

The rules are in [execution.md](../../spec/execution.md#child-runs); in Rust they come out like this.

- **The invoke context.** When the driver starts a machine invoke it sets a thread-local invoke context around each `poll` of the method's future (and around the call that creates it), restoring the previous one afterwards, so it works on any executor. It holds the trace sink, the clock and timer, the parent's run id, the invoke id and a cancel token. `checkout::run` finds it, claims the token and runs as child run `r1.4`, writing to the parent's sink. A canned test implementation never calls `run`, never claims the token, and is treated as a task. Functions, actions and tasks are polled with no context.
- **Cancelling a child.** Dropping a child's future would skip its exit effects. So when its state is left the parent signals the token and keeps polling the future while the child runs its cancel step (exit effects, the `cancelled` record), and drops it once the child reports through the token that it has finished. Leaving the state waits for that, like an action. A task's or a stub's future is dropped at once.
- **Defects.** The driver wraps functions, actions and invoke futures in `catch_unwind`. A panic whose payload is a `&str` or `String` becomes a host defect with that message, and any other payload one with the message `Box<dyn Any>`. A run that ends with a defect resumes unwinding with a `roadbook::Defect` payload, which the parent's driver recognises and passes on unchanged, so a child's defect reaches the root as it was. A defect in a child's cancel step reaches the parent through the token and ends the parent with that defect.
- **Cancelling from outside.** Dropping the future of a root `run` is a hard stop: nothing more runs, no exit effects and no final trace record, and everything the run held is dropped with it. Graceful cancellation goes through the actor: `spawn` the machine and call `actor.cancel().await`, which returns once the exit effects have run and the trace has its `cancelled` record.

`roadbook-core` has no async and no I/O. Keeping it `no_std` with `alloc` is a goal to hold onto if it stays cheap, since it would let charts run on embedded targets, but it is not a first-version requirement, and the first version uses `std` (as `roadbook-ir` does).

The conformance runner (`crates/roadbook/tests/conformance.rs`) runs every case through this driver: it implements `dynamic::Host` from the case's script and `Timer` with a virtual clock, and is its own executor, polling the root run's future until a poll leaves no wake-up behind, which is quiescence, since every child run is polled inside it.

## Interpreter first, generated code later

The first version interprets the embedded IR, like the TypeScript runtime. That keeps the two cores line-for-line comparable and puts all the effort into one set of semantics. If a hot machine ever needs it, the macro can instead generate a `match`-based state machine with no interpretation overhead. The conformance suite would hold that generated code to the same traces.
