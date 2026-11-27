//! Calls between runs through `roadbook::include!`: Orders and a Counter call an Inventory
//! peer through a `ref`. A call invoke is no `Handlers` method (the driver makes the call
//! itself), so `Order` and `Counter` have empty `Handlers`, and the pending-call variable of
//! the Inventory is in no host-facing type.

#![deny(warnings)]

use std::sync::Arc;
use std::time::Duration;

use roadbook::{BoxFuture, Config, MemorySink, Stopped, Timer};

mod calls {
    roadbook::include!("../../spec/conformance/charts/calls.road");
}

use calls::{Item, OutOfStock, Reservation};

/// Virtual milliseconds on Tokio's paused clock.
struct VirtualTimer(tokio::time::Instant);

impl Timer for VirtualTimer {
    fn now(&self) -> u64 {
        self.0.elapsed().as_millis() as u64
    }

    fn start(&self, _run: &str, _timer: u64, delay: u64) -> BoxFuture<'static, ()> {
        Box::pin(tokio::time::sleep(Duration::from_millis(delay)))
    }
}

fn config(sink: &Arc<MemorySink>) -> Config {
    Config::new()
        .sink(sink.clone())
        .timer(Arc::new(VirtualTimer(tokio::time::Instant::now())))
}

/// A warehouse that holds up to five units an order, or one that never answers.
struct Stock {
    never: bool,
}

impl calls::inventory::Handlers for Stock {
    async fn reserve_stock(
        &self,
        _warehouse: &str,
        _order_id: &str,
        items: &[Item],
    ) -> Result<i64, calls::ReserveStockError> {
        if self.never {
            std::future::pending::<()>().await;
        }
        let n: i64 = items.iter().map(|i| i.qty).sum();
        if n > 5 {
            return Err(OutOfStock {
                reason: format!("{n} is too many"),
            }
            .into());
        }
        Ok(n)
    }
}

// A run that only makes calls has nothing for host code to do.
impl calls::order::Handlers for Stock {}
impl calls::counter::Handlers for Stock {}

fn items(qty: i64) -> Vec<Item> {
    vec![Item {
        sku: "s1".into(),
        qty,
    }]
}

#[tokio::test(start_paused = true)]
async fn orders_call_an_inventory_and_get_its_answers() {
    let sink = Arc::new(MemorySink::new());
    let stock = Arc::new(Stock { never: false });
    let inventory = calls::inventory::spawn_with(config(&sink), stock.clone(), "w1".into());

    let order = calls::order::spawn_with(
        config(&sink),
        stock.clone(),
        "o1".into(),
        items(2),
        inventory.handle(),
    );
    assert_eq!(
        order.result().await,
        Ok(Reservation {
            order_id: "o1".into(),
            count: 2
        })
    );
    // The declared error the task failed with is the call's answer.
    let big = calls::order::spawn_with(
        config(&sink),
        stock.clone(),
        "o2".into(),
        items(9),
        inventory.handle(),
    );
    assert_eq!(
        big.result().await,
        Err(Stopped::Failed(calls::OrderError::OutOfStock(OutOfStock {
            reason: "9 is too many".into()
        })))
    );
    let counter = calls::counter::spawn_with(config(&sink), stock.clone(), inventory.handle());
    assert_eq!(counter.result().await, Ok(1));

    // Closed, the Inventory answers nothing: its callers fail at once with `Unanswered`.
    inventory.send(calls::inventory::Event::Close).await;
    assert_eq!(inventory.result().await, Ok(()));
    let counter = calls::counter::spawn_with(config(&sink), stock.clone(), {
        // A handle outlives its actor.
        let sink = Arc::new(MemorySink::new());
        let closed = calls::inventory::spawn_with(config(&sink), stock.clone(), "w2".into());
        closed.send(calls::inventory::Event::Close).await;
        let handle = closed.handle();
        closed.result().await.unwrap();
        handle
    });
    assert_eq!(counter.result().await, Ok(0));
    let replied = sink
        .records()
        .iter()
        .filter(|r| r.to_json()["kind"] == "replied")
        .count();
    assert_eq!(replied, 3);
}

#[tokio::test(start_paused = true)]
async fn a_callee_stopped_hard_answers_its_calls_unanswered() {
    let sink = Arc::new(MemorySink::new());
    let stock = Arc::new(Stock { never: true });
    let inventory = calls::inventory::spawn_with(config(&sink), stock.clone(), "w1".into());
    let order = calls::order::spawn_with(
        config(&sink),
        stock.clone(),
        "o1".into(),
        items(2),
        inventory.handle(),
    );
    let pending = || {
        sink.records()
            .iter()
            .any(|r| r.run == inventory.id() && r.to_json()["kind"] == "invoke.started")
    };
    for _ in 0..1000 {
        if pending() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(pending(), "the Inventory never took the call");
    // Dropping the actor stops the run at once, with no record: the call it was answering
    // is answered `Unanswered`, before the Order's own 5s timeout.
    drop(inventory);
    assert_eq!(
        order.result().await,
        Err(Stopped::Failed(calls::OrderError::OutOfStock(OutOfStock {
            reason: "unanswered".into()
        })))
    );
}
