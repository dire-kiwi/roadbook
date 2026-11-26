//! `Order` runs (from this crate's chart) against an `Inventory` peer (from the `inventory`
//! crate's), spawned and passed by handle, under a virtual clock: Tokio's paused time,
//! which the `Timer` below reads, so every `after` fires as soon as the runs are idle.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use orders::{ChargeError, OrderDraft, OrderError, OutOfStock, PaymentDeclined, Receipt, order};
use roadbook::{BoxFuture, Config, MemorySink, Stopped, Timer};

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

/// The warehouse: reserves at once, or never answers.
#[derive(Default)]
struct Warehouse {
    silent: bool,
    log: Mutex<Vec<String>>,
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
        let qty: i64 = items.iter().map(|i| i.qty).sum();
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

/// The kinds of `run`'s records, in order.
fn kinds(sink: &MemorySink, run: &str) -> Vec<String> {
    sink.records()
        .iter()
        .filter(|r| r.run == run)
        .map(|r| r.to_json()["kind"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test(start_paused = true)]
async fn an_order_reserves_with_a_spawned_inventory_and_pays() {
    let sink = Arc::new(MemorySink::new());
    let warehouse = Arc::new(Warehouse::default());
    let stock = inventory::spawn_with(config(&sink), warehouse.clone(), "w1".into());
    // Host code sends through the handle: an `inventory::Event`, checked by the compiler.
    stock
        .handle()
        .send(inventory::Event::Restock {
            sku: "s1".into(),
            qty: 5,
        })
        .await;
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
    assert_eq!(
        *warehouse.log.lock().unwrap(),
        ["add w1 s1 5", "reserve w1 o1 2"]
    );
    // The order sent to the inventory's run, and took what it emitted.
    let sent = sink
        .records()
        .into_iter()
        .find(|r| r.run == order_id && r.to_json()["kind"] == "sent")
        .expect("the order sent Reserve");
    assert_eq!(sent.to_json()["to"], stock.id());
    assert_eq!(sent.to_json()["value"]["_tag"], "Reserve");
    let k = kinds(&sink, &order_id);
    assert!(k.contains(&"event".to_owned()) && k.last().unwrap() == "done");
    assert!(kinds(&sink, stock.id()).contains(&"emitted".to_owned()));
    stock.cancel().await;
    assert_eq!(stock.result().await, Err(Stopped::Cancelled));
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
}

#[tokio::test(start_paused = true)]
async fn a_declined_order_releases_its_stock_and_a_send_to_an_ended_peer_is_undelivered() {
    let sink = Arc::new(MemorySink::new());
    let warehouse = Arc::new(Warehouse::default());
    let stock = inventory::spawn_with(config(&sink), warehouse.clone(), "w1".into());
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
    while warehouse.log.lock().unwrap().len() < 2 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        *warehouse.log.lock().unwrap(),
        ["reserve w1 o1 2", "release w1 o1"]
    );

    // Close the inventory, then start an order against it: its Reserve is undelivered.
    stock.handle().send(inventory::Event::Close).await;
    let handle = stock.handle();
    assert_eq!(stock.result().await, Ok(()));
    assert!(handle.is_closed());
    let result = order::run_with(
        &config(&sink).run_id("late"),
        &Payments,
        "o2".into(),
        draft("4242"),
        handle,
    )
    .await;
    assert!(matches!(result, Err(OrderError::OutOfStock(_))));
    let k = kinds(&sink, "late");
    let sent = k.iter().position(|k| k == "sent").unwrap();
    assert_eq!(k[sent + 1], "undelivered");
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
    let ours = roadbook::Program::from_json(orders::__ROADBOOK_IR).unwrap();
    let theirs = roadbook::Program::from_json(inventory::__ROADBOOK_IR).unwrap();
    let hash = |p: &roadbook::Program| p.ir().machines["Inventory"].hash.clone();
    assert_eq!(hash(&ours), hash(&theirs));
    let events = |p: &roadbook::Program| {
        let m = &p.ir().machines["Inventory"];
        (
            m.events.keys().cloned().collect::<Vec<_>>(),
            m.emits.keys().cloned().collect::<Vec<_>>(),
        )
    };
    assert_eq!(events(&ours), events(&theirs));
    let source = |p: &roadbook::Program, name: &str| {
        let file = p.ir().types[name].span.file as usize;
        p.ir().sources[file].clone()
    };
    let item = source(&ours, "Item");
    assert_eq!(item.package.as_deref(), Some("inventory"));
    assert_eq!(item.path, "inventory.road");
    assert_eq!(source(&ours, "OrderDraft").package, None);
}
