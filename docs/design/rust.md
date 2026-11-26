# Rust runtime

The Rust side mirrors the TypeScript side: `roadbook-core` is the pure interpreter, and `roadbook` is the driver, the macro and the public API.

The API on this page is not implemented yet, but its shapes compile. [`spike/api-rust`](../../spike/api-rust) contains a hand-written version of what the macro would generate plus a full example, and passes `cargo check`.

## What gets generated

There is no separate generation step. A macro runs the compiler at build time:

```rust
roadbook::include!("examples/shop/place_order.road");
```

It expands to a module per machine plus the shared types:

- A `struct` or `enum` for every `type`, `enum` and `error`, with `serde` derives.
- For every fallible task and machine, an error `enum` of exactly the errors it declared, with `From` impls so `?` converts into it. Rust has no anonymous unions, so `! CardDeclined | GatewayUnavailable` becomes `ChargeCardError`.
- A `Handlers` trait: one method per function, action, task and child machine the chart uses.
- `State` and `Event` enums. Compound states carry their active child: `State::Processing(Processing::Paying)`.
- `run` and `spawn` functions.
- The IR, embedded as a constant.

A chart error is a Rust compile error, reported at the macro call with the chart's file and line. Cargo rebuilds when the `.road` file changes. For builds that prefer it, the same generator is available from `build.rs`.

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
        ApiResponse { status, body: code.to_owned() }
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

A child machine is one trait method. Delegating to the real chart is one line (`checkout::run`), and a test implements the same method with a canned result.

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
let response = actor.result().await;
```

A machine that declares no errors returns its output directly. One that does returns `Result<Output, MachineError>`.

## How the driver maps onto Rust

| Statechart concept | Rust |
| --- | --- |
| A machine instance | One future that owns everything the machine started |
| The event queue | An `mpsc` channel (only needed for `spawn`) |
| A state's `invoke` | A future held in a set keyed by state id. Dropping it cancels it. |
| `after` timers | Sleep futures in the same set |
| The loop | `select` over the queue, the invoke set and the timers |
| A task's declared errors | The generated error enum, matched to the `fail` branch |
| A defect | A panic, which unwinds through `run` like any other |
| Trace output | A `TraceSink` trait object, with a no-op default |
| Tracing | `tracing` spans per machine and per state |

Rust futures are cancelled by dropping them, so "leaving a state cancels its work" needs no extra machinery: the driver drops the invoke's future. Because the driver holds futures and never spawns tasks, `run` borrows the handlers (`&H`) and works on any executor. Only timers need a runtime, behind a small `Timer` trait with a Tokio implementation enabled by default.

`roadbook-core` has no async and no I/O. Keeping it `no_std` with `alloc` is a goal to hold onto if it stays cheap, since it would let charts run on embedded targets, but it is not a first-version requirement.

## Interpreter first, generated code later

The first version interprets the embedded IR, like the TypeScript runtime. That keeps the two cores line-for-line comparable and puts all the effort into one set of semantics. If a hot machine ever needs it, the macro can instead generate a `match`-based state machine with no interpretation overhead. The conformance suite would hold that generated code to the same traces.
