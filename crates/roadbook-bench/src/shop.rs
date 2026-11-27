//! `examples/shop`: `PlaceOrder`, which invokes three tasks and the `Checkout` machine, and
//! `Checkout` alone with three failed attempts before the card is charged.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use roadbook::Config;
use serde_json::{Value, json};

use crate::{Outcome, Scenario, Script, object};

roadbook::include!("../../examples/shop/place_order.road");

/// Handlers that answer at once. The gateway is down for the first `down` attempts.
pub struct App {
    pub down: u32,
    pub attempts: AtomicU32,
}

impl App {
    pub fn new(down: u32) -> App {
        App {
            down,
            attempts: AtomicU32::new(0),
        }
    }
}

impl checkout::Handlers for App {
    fn retry_delay(&self, attempt: i64) -> Duration {
        Duration::from_millis(100 * attempt as u64)
    }
    async fn charge_card(&self, order: &Order, _user: &User) -> Result<Receipt, ChargeCardError> {
        let attempt = self.attempts.fetch_add(1, Ordering::Relaxed) + 1;
        if attempt <= self.down {
            return Err(GatewayUnavailable.into());
        }
        Ok(Receipt {
            charge_id: format!("ch_{attempt}"),
            amount_cents: order.total_cents,
        })
    }
}

impl place_order::Handlers for App {
    fn confirmation(&self, order: &Order, receipt: &Receipt) -> ApiResponse {
        ApiResponse {
            status: 201,
            body: json!({ "orderId": order.id, "chargeId": receipt.charge_id }),
        }
    }
    fn invalid(&self, issues: &[String]) -> ApiResponse {
        ApiResponse {
            status: 422,
            body: json!({ "issues": issues }),
        }
    }
    fn problem(&self, status: i64, code: &str) -> ApiResponse {
        ApiResponse {
            status,
            body: json!({ "code": code }),
        }
    }
    async fn send_confirmation_email(&self, _: &User, _: &Order, _: &Receipt) {}
    async fn audit(&self, _: &str, _: &Order) {}
    async fn authenticate(&self, _: &str) -> Result<User, AuthenticateError> {
        Ok(user())
    }
    async fn validate_order(&self, draft: &OrderDraft) -> Result<Order, ValidateOrderError> {
        Ok(Order {
            items: draft.items.clone(),
            ..order(0)
        })
    }
    async fn price_order(&self, order: &Order, _: &User) -> Order {
        let qty: i64 = order.items.iter().map(|i| i.qty).sum();
        Order {
            total_cents: 1000 * qty,
            ..order.clone()
        }
    }
    async fn checkout(&self, order: &Order, user: &User) -> Result<Receipt, CheckoutError> {
        checkout::run(self, order.clone(), user.clone()).await
    }
}

pub fn draft() -> OrderDraft {
    OrderDraft {
        items: vec![
            LineItem {
                sku: "sku_1".into(),
                qty: 2,
            },
            LineItem {
                sku: "sku_2".into(),
                qty: 1,
            },
        ],
        coupon: None,
    }
}

fn user() -> User {
    User {
        id: "u_1".into(),
        role: Role::Customer,
    }
}

fn order(total_cents: i64) -> Order {
    Order {
        id: "ord_1".into(),
        items: draft().items,
        total_cents,
    }
}

/// One `PlaceOrder` run to `Confirmed`, through `Checkout` as a child run.
pub async fn place_order(config: &Config, app: &App) -> ApiResponse {
    place_order::run_with(config, app, "t_1".into(), draft()).await
}

/// One `Checkout` run with `app.down` failed attempts first (its timers must fire).
pub async fn checkout(config: &Config, app: &App) -> Receipt {
    checkout::run_with(config, app, order(3000), user())
        .await
        .expect("the card is charged")
}

fn functions(name: &str, args: &[Value]) -> Value {
    match name {
        "confirmation" => {
            json!({ "status": 201, "body": { "orderId": args[0]["id"], "chargeId": args[1]["chargeId"] } })
        }
        "problem" => json!({ "status": args[0], "body": { "code": args[1] } }),
        "invalid" => json!({ "status": 422, "body": { "issues": args[0] } }),
        "retryDelay" => json!(100 * args[0].as_i64().unwrap_or(0)),
        _ => panic!("no function {name}"),
    }
}

fn encode<T: roadbook::__private::serde::Serialize>(v: T) -> Value {
    serde_json::to_value(v).unwrap()
}

/// `PlaceOrder` on the core alone: the `Checkout` invoke is answered like a task.
pub fn place_order_script() -> Script {
    Script::record(Scenario {
        chart: "examples/shop/place_order.road",
        machine: "PlaceOrder",
        input: object(json!({ "token": "t_1", "draft": encode(draft()) })),
        outside: Vec::new(),
        respond: |src, _, _| match src {
            "authenticate" => Outcome::Done(encode(user())),
            "validateOrder" => Outcome::Done(encode(order(0))),
            "priceOrder" => Outcome::Done(encode(order(3000))),
            "Checkout" => Outcome::Done(json!({ "chargeId": "ch_1", "amountCents": 3000 })),
            _ => panic!("no task {src}"),
        },
        functions,
        fire_timers: false,
    })
}

/// `Checkout` on the core alone, the gateway down for three attempts.
pub fn checkout_script() -> Script {
    Script::record(Scenario {
        chart: "examples/shop/checkout.road",
        machine: "Checkout",
        input: object(json!({ "order": encode(order(3000)), "user": encode(user()) })),
        outside: Vec::new(),
        respond: |_, _, nth| {
            if nth < 3 {
                Outcome::Fail(json!({ "_tag": "GatewayUnavailable" }))
            } else {
                Outcome::Done(json!({ "chargeId": "ch_4", "amountCents": 3000 }))
            }
        },
        functions,
        fire_timers: true,
    })
}
