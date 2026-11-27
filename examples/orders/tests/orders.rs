//! `Order` runs (from this crate's chart) against an `Inventory` peer (from the `inventory`
//! crate's), spawned and passed by handle, under a virtual clock: Tokio's paused time,
//! which the `Timer` below reads, so every `after` fires as soon as the runs are idle. An
//! order calls the inventory's `reserve` and takes the answer as its invoke's outcome, so the
//! host code here only sees each order succeed or fail.

use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use orders::{ChargeError, OrderDraft, OrderError, OutOfStock, PaymentDeclined, Receipt, order};
use roadbook::{BoxFuture, Config, MemorySink, Stopped, Timer, TraceRecord};
use serde_json::{Value, json};

/// Virtual milliseconds since the test started, on Tokio's paused clock.
struct VirtualTimer(tokio::time::Instant);

impl Timer for VirtualTimer {
    fn now(&self) -> u64 {
        self.0.elapsed().as_millis() as u64
    }

    fn start(&self, _run: &str, _timer: u64, delay: u64) -> BoxFuture<'static, ()> {
        Box::pin(tokio::time::sleep(Duration::from_millis(delay)))
    }
}

/// The warehouse: holds what it has after a second, or never answers.
#[derive(Default)]
struct Warehouse {
    silent: bool,
    stock: Mutex<i64>,
    log: Mutex<Vec<String>>,
}

impl Warehouse {
    fn log(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }
}

impl inventory::Handlers for Warehouse {
    async fn reserve_stock(
        &self,
        warehouse: &str,
        order_id: &str,
        items: &[inventory::Item],
    ) -> Result<(), inventory::ReserveStockError> {
        if self.silent {
            std::future::pending::<()>().await;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
        let qty: i64 = items.iter().map(|i| i.qty).sum();
        let mut stock = self.stock.lock().unwrap();
        if qty > *stock {
            return Err(OutOfStock {
                reason: format!("only {stock} left"),
            }
            .into());
        }
        *stock -= qty;
        self.log
            .lock()
            .unwrap()
            .push(format!("reserve {warehouse} {order_id} {qty}"));
        Ok(())
    }

    async fn release_stock(&self, warehouse: &str, order_id: &str) {
        self.log
            .lock()
            .unwrap()
            .push(format!("release {warehouse} {order_id}"));
    }

    async fn add_stock(&self, warehouse: &str, sku: &str, qty: i64) {
        *self.stock.lock().unwrap() += qty;
        self.log
            .lock()
            .unwrap()
            .push(format!("add {warehouse} {sku} {qty}"));
    }
}

/// The payment service: declines one card.
struct Payments;

impl order::Handlers for Payments {
    async fn charge(&self, order_id: &str, draft: &OrderDraft) -> Result<Receipt, ChargeError> {
        if draft.card == "0000" {
            return Err(PaymentDeclined.into());
        }
        Ok(Receipt {
            order_id: order_id.to_owned(),
            total: 4200,
        })
    }
}

fn draft(card: &str) -> OrderDraft {
    // `Item` is the inventory crate's type, re-exported here: one type, not a copy.
    let item: inventory::Item = orders::Item {
        sku: "s1".into(),
        qty: 2,
    };
    OrderDraft {
        items: vec![item],
        card: card.into(),
    }
}

fn config(sink: &Arc<MemorySink>) -> Config {
    Config::new()
        .sink(sink.clone())
        .timer(Arc::new(VirtualTimer(tokio::time::Instant::now())))
}

/// A spawned `Inventory` run.
type Stock = roadbook::Actor<inventory::Event, inventory::inventory::State, (), Infallible>;

/// A warehouse holding `qty` of `s1`, restocked by host code through its handle.
async fn stocked(sink: &Arc<MemorySink>, warehouse: &Arc<Warehouse>, qty: i64) -> Stock {
    let stock = inventory::spawn_with(config(sink), warehouse.clone(), "w1".into());
    // An `inventory::Event`, checked by the compiler.
    stock
        .handle()
        .send(inventory::Event::Restock {
            sku: "s1".into(),
            qty,
        })
        .await;
    stock
}

/// `run`'s records, as JSON.
fn records(sink: &MemorySink, run: &str) -> Vec<Value> {
    sink.records()
        .iter()
        .filter(|r| r.run == run)
        .map(TraceRecord::to_json)
        .collect()
}

fn of_kind(records: &[Value], kind: &str) -> Vec<Value> {
    records
        .iter()
        .filter(|r| r["kind"] == kind)
        .cloned()
        .collect()
}

#[tokio::test(start_paused = true)]
async fn an_order_calls_a_spawned_inventory_and_pays() {
    let sink = Arc::new(MemorySink::new());
    let warehouse = Arc::new(Warehouse::default());
    let stock = stocked(&sink, &warehouse, 5).await;
    let order = order::spawn_with(
        config(&sink),
        Arc::new(Payments),
        "o1".into(),
        draft("4242"),
        stock.handle(),
    );
    let order_id = order.id().to_owned();
    let receipt = order.result().await;
    assert_eq!(
        receipt,
        Ok(Receipt {
            order_id: "o1".into(),
            total: 4200
        })
    );
    assert_eq!(warehouse.log(), ["add w1 s1 5", "reserve w1 o1 2"]);

    // The order called the inventory's run, which took the call and answered it.
    let ours = records(&sink, &order_id);
    let call = &of_kind(&ours, "invoke.started")[0];
    assert_eq!(call["src"], "inventory.reserve");
    assert_eq!(call["args"]["orderId"], "o1");
    let theirs = records(&sink, stock.id());
    let taken: Vec<Value> = of_kind(&theirs, "event")
        .into_iter()
        .filter(|r| r["event"]["kind"] == "call")
        .collect();
    assert_eq!(taken.len(), 1);
    assert_eq!(taken[0]["event"]["from"], order_id.as_str());
    assert_eq!(taken[0]["event"]["call"], "reserve");
    let replied = of_kind(&theirs, "replied");
    assert_eq!(replied.len(), 1);
    assert_eq!(replied[0]["to"], order_id.as_str());
    assert_eq!(replied[0]["outcome"], "done");
    // The answer is the order's alone: nothing was sent or emitted to carry it.
    assert!(of_kind(&ours, "sent").is_empty());
    assert!(of_kind(&theirs, "emitted").is_empty());

    stock.cancel().await;
    assert_eq!(stock.result().await, Err(Stopped::Cancelled));
}

#[tokio::test(start_paused = true)]
async fn an_order_the_warehouse_cannot_supply_is_out_of_stock() {
    let sink = Arc::new(MemorySink::new());
    let warehouse = Arc::new(Warehouse::default());
    let stock = stocked(&sink, &warehouse, 1).await;
    let result = order::run_with(
        &config(&sink),
        &Payments,
        "o1".into(),
        draft("4242"),
        stock.handle(),
    )
    .await;
    // The task's error, as the inventory replied it.
    assert_eq!(
        result,
        Err(OrderError::OutOfStock(OutOfStock {
            reason: "only 1 left".into()
        }))
    );
    assert_eq!(warehouse.log(), ["add w1 s1 1"]);
}

#[tokio::test(start_paused = true)]
async fn a_call_to_a_busy_inventory_is_turned_away() {
    let sink = Arc::new(MemorySink::new());
    let warehouse = Arc::new(Warehouse::default());
    let stock = stocked(&sink, &warehouse, 5).await;
    let order = |id: &str| {
        order::spawn_with(
            config(&sink),
            Arc::new(Payments),
            id.into(),
            draft("4242"),
            stock.handle(),
        )
    };
    // Two orders at once: whichever call comes second arrives while the inventory is still
    // reserving for the first, and is answered "busy" at once.
    let (a, b) = (order("a"), order("b"));
    let mut results = [a.result().await, b.result().await];
    results.sort_by_key(|r| r.is_err());
    let busy = OrderError::OutOfStock(OutOfStock {
        reason: "busy".into(),
    });
    match &results[..] {
        [Ok(paid), Err(Stopped::Failed(turned_away))] => {
            assert_eq!(paid.total, 4200);
            assert_eq!(*turned_away, busy);
        }
        other => panic!("one order is paid and one turned away, not {other:?}"),
    }
    assert_eq!(warehouse.log().len(), 2);
    // Each answer went to its caller alone: no order received, or dropped, the other's.
    let replied = of_kind(&records(&sink, stock.id()), "replied");
    assert_eq!(replied.len(), 2);
    assert_eq!(replied[0]["outcome"], "fail");
    assert_eq!(replied[1]["outcome"], "done");
    assert_ne!(replied[0]["to"], replied[1]["to"]);
    let dropped = sink
        .records()
        .iter()
        .filter(|r| r.to_json()["kind"] == "dropped")
        .count();
    assert_eq!(dropped, 0);
}

#[tokio::test(start_paused = true)]
async fn a_closed_inventory_leaves_the_call_unanswered() {
    let sink = Arc::new(MemorySink::new());
    let warehouse = Arc::new(Warehouse::default());
    let stock = stocked(&sink, &warehouse, 5).await;
    stock.handle().send(inventory::Event::Close).await;
    let handle = stock.handle();
    assert_eq!(stock.result().await, Ok(()));
    assert!(handle.is_closed());

    // The call fails at once with the built-in `Unanswered`, which the chart maps to
    // OutOfStock: the order does not wait for its timeout.
    let started = tokio::time::Instant::now();
    let result = order::run_with(
        &config(&sink).run_id("late"),
        &Payments,
        "o1".into(),
        draft("4242"),
        handle,
    )
    .await;
    assert_eq!(
        result,
        Err(OrderError::OutOfStock(OutOfStock {
            reason: "the warehouse is closed".into()
        }))
    );
    assert_eq!(started.elapsed(), Duration::ZERO);
    let failed: Vec<Value> = of_kind(&records(&sink, "late"), "event")
        .into_iter()
        .map(|r| r["event"].clone())
        .filter(|e| e["kind"] == "invoke.fail")
        .collect();
    assert_eq!(
        failed,
        [json!({ "kind": "invoke.fail", "invoke": 1, "error": { "_tag": "Unanswered" } })]
    );
}

#[tokio::test(start_paused = true)]
async fn an_order_gives_up_on_an_inventory_that_does_not_answer() {
    let sink = Arc::new(MemorySink::new());
    let warehouse = Arc::new(Warehouse {
        silent: true,
        ..Warehouse::default()
    });
    let stock = inventory::spawn_with(config(&sink), warehouse, "w1".into());
    let started = tokio::time::Instant::now();
    let result = order::run_with(
        &config(&sink).run_id("order-1"),
        &Payments,
        "o1".into(),
        draft("4242"),
        stock.handle(),
    )
    .await;
    assert_eq!(
        result,
        Err(OrderError::OutOfStock(OutOfStock {
            reason: "the warehouse did not answer".into()
        }))
    );
    assert_eq!(started.elapsed(), Duration::from_secs(5));
    let after = sink
        .records()
        .into_iter()
        .find(|r| r.run == "order-1" && r.to_json()["kind"] == "event")
        .unwrap();
    assert_eq!(after.t, 5000);
    // Leaving the state abandoned the call.
    let cancelled = of_kind(&records(&sink, "order-1"), "invoke.cancelled");
    assert_eq!(cancelled[0]["src"], "inventory.reserve");
}

#[tokio::test(start_paused = true)]
async fn a_declined_order_releases_its_stock() {
    let sink = Arc::new(MemorySink::new());
    let warehouse = Arc::new(Warehouse::default());
    let stock = stocked(&sink, &warehouse, 5).await;
    let result = order::run_with(
        &config(&sink),
        &Payments,
        "o1".into(),
        draft("0000"),
        stock.handle(),
    )
    .await;
    assert_eq!(result, Err(OrderError::PaymentDeclined(PaymentDeclined)));
    // The Release the order sent as it ended reaches the inventory.
    while warehouse.log().len() < 3 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        warehouse.log(),
        ["add w1 s1 5", "reserve w1 o1 2", "release w1 o1"]
    );
}

#[test]
fn a_cross_crate_send_is_checked_against_the_owning_crates_event() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/*.rs");
}

/// The IR this crate embeds has the `inventory` crate's `Inventory` exactly as that crate
/// compiled it, from the chart that crate was built from, with its declarations marked as
/// the package's.
#[test]
fn the_imported_machine_is_the_dependencys() {
    let ours = roadbook::Program::from_json(orders::__ROADBOOK_IR_orders_2e_road).unwrap();
    let theirs = roadbook::Program::from_json(inventory::__ROADBOOK_IR_inventory_2e_road).unwrap();
    let hash = |p: &roadbook::Program| p.ir().machines["Inventory"].hash.clone();
    assert_eq!(hash(&ours), hash(&theirs));
    let interface = |p: &roadbook::Program| {
        let m = &p.ir().machines["Inventory"];
        (
            m.events.keys().cloned().collect::<Vec<_>>(),
            m.calls.keys().cloned().collect::<Vec<_>>(),
        )
    };
    assert_eq!(interface(&ours), interface(&theirs));
    assert_eq!(interface(&ours).1, ["reserve"]);
    let source = |p: &roadbook::Program, name: &str| {
        let file = p.ir().types[name].span.file as usize;
        p.ir().sources[file].clone()
    };
    let item = source(&ours, "Item");
    assert_eq!(item.package.as_deref(), Some("inventory"));
    assert_eq!(item.path, "inventory.road");
    assert_eq!(source(&ours, "OrderDraft").package, None);
}
