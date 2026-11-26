//! `examples/orders` and `examples/inventory`: `Order` runs that message a long-lived
//! `Inventory` peer, which answers each by emitting an event to every holder of a reference
//! to it.

use std::sync::Arc;

use orders::{ChargeError, OrderDraft, Receipt, order};
use roadbook::Config;
use serde_json::{Value, json};

use crate::{Outcome, Scenario, Script, event, object};

/// Orders in one run of [`run`], and reservations in [`inventory_script`].
pub const ORDERS: usize = 20;

/// The warehouse: every reservation succeeds at once.
pub struct Warehouse;

impl inventory::Handlers for Warehouse {
    async fn reserve_stock(
        &self,
        _: &str,
        _: &str,
        _: &[inventory::Item],
    ) -> Result<(), inventory::ReserveStockError> {
        Ok(())
    }
    async fn release_stock(&self, _: &str, _: &str) {}
    async fn add_stock(&self, _: &str, _: &str, _: i64) {}
}

/// The payment service: every charge succeeds at once.
pub struct Payments;

impl order::Handlers for Payments {
    async fn charge(&self, order_id: &str, _: &OrderDraft) -> Result<Receipt, ChargeError> {
        Ok(Receipt {
            order_id: order_id.to_owned(),
            total: 4200,
        })
    }
}

pub fn draft() -> OrderDraft {
    OrderDraft {
        items: vec![
            orders::Item {
                sku: "sku_1".into(),
                qty: 2,
            },
            orders::Item {
                sku: "sku_2".into(),
                qty: 1,
            },
        ],
        card: "4242".into(),
    }
}

/// [`ORDERS`] `Order` runs one after another against one spawned `Inventory`, which is then
/// closed.
pub async fn run(config: &Config) {
    let stock = inventory::spawn_with(config.clone(), Arc::new(Warehouse), "w1".into());
    for i in 0..ORDERS {
        order::run_with(config, &Payments, format!("o_{i}"), draft(), stock.handle())
            .await
            .expect("the order is paid");
    }
    stock.send(inventory::Event::Close).await;
    stock.result().await.expect("the warehouse closes");
}

fn items() -> Value {
    json!([{ "sku": "sku_1", "qty": 2 }, { "sku": "sku_2", "qty": 1 }])
}

fn no_functions(name: &str, _: &[Value]) -> Value {
    panic!("no function {name}")
}

/// One `Order` on the core alone, its peer's answer scripted.
pub fn order_script() -> Script {
    Script::record(Scenario {
        chart: "examples/orders/orders.road",
        machine: "Order",
        input: object(
            json!({ "id": "o_1", "draft": { "items": items(), "card": "4242" }, "inventory": "r2" }),
        ),
        outside: vec![roadbook_core::Input::Emitted {
            from: "r2".into(),
            invoke: None,
            message: 1,
            value: json!({ "_tag": "Reserved", "orderId": "o_1" }),
        }],
        respond: |_, args, _| Outcome::Done(json!({ "orderId": args["orderId"], "total": 4200 })),
        functions: no_functions,
        fire_timers: false,
    })
}

/// `Inventory` on the core alone: [`ORDERS`] reservations, each with a restock, then `Close`.
pub fn inventory_script() -> Script {
    let mut outside = Vec::new();
    for i in 0..ORDERS {
        outside.push(event(
            json!({ "_tag": "Reserve", "orderId": format!("o_{i}"), "items": items() }),
        ));
        outside.push(event(
            json!({ "_tag": "Restock", "sku": "sku_1", "qty": 5 }),
        ));
    }
    outside.push(event(json!({ "_tag": "Close" })));
    Script::record(Scenario {
        chart: "examples/inventory/inventory.road",
        machine: "Inventory",
        input: object(json!({ "warehouse": "w1" })),
        outside,
        respond: |_, _, _| Outcome::Done(Value::Null),
        functions: no_functions,
        fire_timers: false,
    })
}
