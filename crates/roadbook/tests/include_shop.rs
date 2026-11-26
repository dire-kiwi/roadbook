//! `roadbook::include!` on the shop chart: the generated types and their JSON, one struct
//! implementing several charts' `Handlers`, and real runs on a multi-threaded Tokio runtime.

#![deny(warnings)]

use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

roadbook::include!("../../examples/shop/place_order.road");

/// A second chart, whose `Handlers` the same `App` implements.
mod greet {
    roadbook::include!("tests/charts/greet.road");
}

/// What a developer writes: the application's dependencies, behind both charts' handlers.
mod app {
    use super::*;

    /// How the fake payment gateway answers.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum Gateway {
        Charges,
        Declines,
        /// Down for this many attempts, then charges.
        DownFor(u32),
    }

    pub struct App {
        pub gateway: Gateway,
        /// How long pricing takes.
        pub pricing: Duration,
        pub attempts: Mutex<u32>,
        pub audit: Mutex<Vec<String>>,
        pub emails: Mutex<Vec<String>>,
    }

    impl App {
        pub fn new(gateway: Gateway) -> App {
            App {
                gateway,
                pricing: Duration::ZERO,
                attempts: Mutex::new(0),
                audit: Mutex::new(Vec::new()),
                emails: Mutex::new(Vec::new()),
            }
        }

        async fn charge(&self, order: &Order) -> Result<Receipt, ChargeCardError> {
            tokio::time::sleep(Duration::from_millis(1)).await;
            let attempt = {
                let mut a = self.attempts.lock().unwrap();
                *a += 1;
                *a
            };
            match self.gateway {
                Gateway::Declines => Err(CardDeclined {
                    reason: "expired".into(),
                })?,
                Gateway::DownFor(n) if attempt <= n => Err(GatewayUnavailable)?,
                _ => Ok(Receipt {
                    charge_id: format!("ch_{attempt}"),
                    amount_cents: order.total_cents,
                }),
            }
        }
    }

    impl checkout::Handlers for App {
        fn retry_delay(&self, attempt: i64) -> Duration {
            Duration::from_millis(attempt as u64)
        }
        async fn charge_card(
            &self,
            order: &Order,
            _user: &User,
        ) -> Result<Receipt, ChargeCardError> {
            self.charge(order).await
        }
    }

    impl place_order::Handlers for App {
        fn confirmation(&self, order: &Order, receipt: &Receipt) -> ApiResponse {
            ApiResponse {
                status: 201,
                body: serde_json::json!({ "orderId": order.id, "chargeId": receipt.charge_id }),
            }
        }
        fn invalid(&self, issues: &[String]) -> ApiResponse {
            ApiResponse {
                status: 422,
                body: serde_json::json!({ "issues": issues }),
            }
        }
        fn problem(&self, status: i64, code: &str) -> ApiResponse {
            ApiResponse {
                status,
                body: serde_json::json!({ "code": code }),
            }
        }
        async fn send_confirmation_email(&self, user: &User, order: &Order, _receipt: &Receipt) {
            self.emails
                .lock()
                .unwrap()
                .push(format!("{} {}", user.id, order.id));
        }
        async fn audit(&self, what: &str, order: &Order) {
            self.audit
                .lock()
                .unwrap()
                .push(format!("{what} {}", order.id));
        }
        async fn authenticate(&self, token: &str) -> Result<User, AuthenticateError> {
            match token {
                "t_admin" => Ok(User {
                    id: "u_1".into(),
                    role: Role::Admin,
                }),
                "t_customer" => Ok(User {
                    id: "u_2".into(),
                    role: Role::Customer,
                }),
                _ => Err(Unauthenticated)?,
            }
        }
        async fn validate_order(&self, draft: &OrderDraft) -> Result<Order, ValidateOrderError> {
            if draft.items.is_empty() {
                return Err(ValidationFailed {
                    issues: vec!["order has no items".into()],
                }
                .into());
            }
            Ok(Order {
                id: "ord_1".into(),
                items: draft.items.clone(),
                total_cents: 0,
            })
        }
        async fn price_order(&self, order: &Order, user: &User) -> Order {
            tokio::time::sleep(self.pricing).await;
            let each = if user.role == Role::Admin { 900 } else { 1000 };
            let qty: i64 = order.items.iter().map(|i| i.qty).sum();
            Order {
                total_cents: each * qty,
                ..order.clone()
            }
        }
        async fn checkout(&self, order: &Order, user: &User) -> Result<Receipt, CheckoutError> {
            checkout::run(self, order.clone(), user.clone()).await
        }
    }

    // The same struct implements the second chart; its `problem` has the same Rust name as
    // `place_order`'s with another signature.
    impl greet::greet::Handlers for App {
        fn problem(&self, code: &str) -> String {
            format!("hello, {code}")
        }
        fn steps(&self, delay: Duration) -> Vec<Duration> {
            vec![delay, delay * 2]
        }
        fn no_extra(&self) -> Option<Duration> {
            None
        }
        fn greeting(
            &self,
            text: &str,
            waited: Duration,
            steps: &[Duration],
            extra: Option<Duration>,
        ) -> greet::Greeting {
            greet::Greeting {
                text: text.to_owned(),
                waited,
                steps: steps.to_vec(),
                extra,
            }
        }
        async fn wait(
            &self,
            delay: Duration,
            steps: &[Duration],
            extra: Option<Duration>,
        ) -> Duration {
            assert_eq!(steps, [delay, delay * 2]);
            assert_eq!(extra, None);
            tokio::time::sleep(delay).await;
            delay + Duration::from_millis(1)
        }
    }

    /// One machine run per request. With axum this is the body of a handler.
    pub async fn post_orders(app: Arc<App>, token: String, draft: OrderDraft) -> ApiResponse {
        place_order::run(&*app, token, draft).await
    }

    fn assert_send<T: Send>(_: &T) {}

    /// The futures are `Send`, so handlers can run on a multi-threaded runtime.
    #[allow(dead_code)]
    fn futures_are_send(app: Arc<App>, order: Order, user: User) {
        let draft = OrderDraft {
            items: vec![],
            coupon: None,
        };
        assert_send(&place_order::run(&*app, String::new(), draft.clone()));
        assert_send(&checkout::run(&*app, order, user));
        assert_send(&post_orders(app, String::new(), draft));
    }
}

use app::{App, Gateway};

fn draft() -> OrderDraft {
    OrderDraft {
        items: vec![LineItem {
            sku: "sku_1".into(),
            qty: 2,
        }],
        coupon: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_order_end_to_end() {
    let app = Arc::new(App::new(Gateway::DownFor(2)));
    let response = app::post_orders(app.clone(), "t_admin".into(), draft()).await;
    assert_eq!(
        response,
        ApiResponse {
            status: 201,
            body: serde_json::json!({ "orderId": "ord_1", "chargeId": "ch_3" }),
        }
    );
    assert_eq!(*app.attempts.lock().unwrap(), 3);
    assert_eq!(*app.emails.lock().unwrap(), ["u_1 ord_1"]);
    assert!(app.audit.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn final_states_give_their_responses() {
    let app = App::new(Gateway::Declines);
    let r = place_order::run(&app, "t_customer".into(), draft()).await;
    assert_eq!(r.status, 402);
    assert_eq!(*app.audit.lock().unwrap(), ["card_declined ord_1"]);

    let app = App::new(Gateway::DownFor(10));
    let r = place_order::run(&app, "t_customer".into(), draft()).await;
    assert_eq!(r.status, 402);
    assert_eq!(*app.audit.lock().unwrap(), ["gateway_down ord_1"]);
    assert_eq!(*app.attempts.lock().unwrap(), 4);

    let r = place_order::run(&app, "nobody".into(), draft()).await;
    assert_eq!(r.status, 401);
    assert_eq!(r.body, serde_json::json!({ "code": "unauthenticated" }));

    let empty = OrderDraft {
        items: vec![],
        coupon: Some("FREE".into()),
    };
    let r = place_order::run(&app, "t_admin".into(), empty).await;
    assert_eq!(r.status, 422);
    assert_eq!(
        r.body,
        serde_json::json!({ "issues": ["order has no items"] })
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_machine_that_fails_with_a_declared_error() {
    let order = Order {
        id: "ord_9".into(),
        items: vec![],
        total_cents: 500,
    };
    let user = User {
        id: "u_1".into(),
        role: Role::Customer,
    };
    let app = App::new(Gateway::Declines);
    let r = checkout::run(&app, order.clone(), user.clone()).await;
    assert_eq!(
        r,
        Err(CheckoutError::CardDeclined(CardDeclined {
            reason: "expired".into()
        }))
    );
    assert_eq!(
        r.unwrap_err().to_string(),
        r#"CardDeclined { reason: "expired" }"#
    );

    let app = App::new(Gateway::DownFor(10));
    let r = checkout::run(&app, order.clone(), user.clone()).await;
    assert_eq!(
        r,
        Err(CheckoutError::GatewayUnavailable(GatewayUnavailable))
    );

    let app = App::new(Gateway::Charges);
    let r = checkout::run(&app, order, user).await;
    assert_eq!(
        r,
        Ok(Receipt {
            charge_id: "ch_1".into(),
            amount_cents: 500,
        })
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn run_with_a_config() {
    let sink = Arc::new(roadbook::MemorySink::new());
    let config = roadbook::Config::new().sink(sink.clone());
    let app = App::new(Gateway::Charges);
    let r = place_order::run_with(&config, &app, "t_admin".into(), draft()).await;
    assert_eq!(r.status, 201);
    assert!(!sink.records().is_empty());
}

#[test]
fn json_forms() {
    let d: OrderDraft = serde_json::from_value(serde_json::json!({
        "items": [{ "sku": "a", "qty": 1 }]
    }))
    .unwrap();
    assert_eq!(
        d,
        OrderDraft {
            items: vec![LineItem {
                sku: "a".into(),
                qty: 1
            }],
            coupon: None
        }
    );
    let order = Order {
        id: "o".into(),
        items: vec![],
        total_cents: 5,
    };
    assert_eq!(
        serde_json::to_value(&order).unwrap(),
        serde_json::json!({ "id": "o", "items": [], "totalCents": 5 })
    );

    let declined = CardDeclined {
        reason: "expired".into(),
    };
    let json = serde_json::json!({ "_tag": "CardDeclined", "reason": "expired" });
    assert_eq!(serde_json::to_value(&declined).unwrap(), json);
    assert_eq!(
        serde_json::from_value::<CardDeclined>(json.clone()).unwrap(),
        declined
    );
    let json_str = serde_json::to_string(&declined).unwrap();
    assert_eq!(json_str, r#"{"_tag":"CardDeclined","reason":"expired"}"#);

    let down = serde_json::json!({ "_tag": "GatewayUnavailable" });
    assert_eq!(serde_json::to_value(GatewayUnavailable).unwrap(), down);
    assert_eq!(
        serde_json::from_value::<GatewayUnavailable>(down.clone()).unwrap(),
        GatewayUnavailable
    );
    assert!(
        serde_json::from_value::<GatewayUnavailable>(serde_json::json!({ "_tag": "Other" }))
            .is_err()
    );

    // An error enum carries the error's own JSON, and decodes by `_tag`.
    let e = ChargeCardError::from(declined.clone());
    assert_eq!(serde_json::to_value(&e).unwrap(), json);
    assert_eq!(serde_json::from_value::<ChargeCardError>(json).unwrap(), e);
    assert_eq!(
        serde_json::from_value::<ChargeCardError>(down).unwrap(),
        ChargeCardError::GatewayUnavailable(GatewayUnavailable)
    );
    assert!(
        serde_json::from_value::<ChargeCardError>(serde_json::json!({ "_tag": "Unauthenticated" }))
            .is_err()
    );
    assert_eq!(GatewayUnavailable.to_string(), "GatewayUnavailable");

    assert_eq!(
        serde_json::to_value(Role::Admin).unwrap(),
        serde_json::json!("Admin")
    );
    assert_eq!(
        serde_json::to_value(place_order::Event::Cancel).unwrap(),
        serde_json::json!({ "_tag": "Cancel" })
    );
    assert_eq!(
        serde_json::from_value::<place_order::Event>(serde_json::json!({ "_tag": "Cancel" }))
            .unwrap(),
        place_order::Event::Cancel
    );

    // Durations are whole milliseconds, in fields and events.
    let g = greet::Greeting {
        text: "hi".into(),
        waited: Duration::from_millis(1500),
        steps: vec![Duration::from_millis(1), Duration::from_millis(2)],
        extra: None,
    };
    let json = serde_json::json!({ "text": "hi", "waited": 1500, "steps": [1, 2], "extra": null });
    assert_eq!(serde_json::to_value(&g).unwrap(), json);
    assert_eq!(serde_json::from_value::<greet::Greeting>(json).unwrap(), g);
    let without_extra = serde_json::json!({ "text": "hi", "waited": 1500, "steps": [1, 2] });
    assert_eq!(
        serde_json::from_value::<greet::Greeting>(without_extra).unwrap(),
        g
    );
    let hurry = greet::greet::Event::Hurry {
        by: Duration::from_millis(7),
        note: Some("now".into()),
    };
    assert_eq!(
        serde_json::to_value(&hurry).unwrap(),
        serde_json::json!({ "_tag": "Hurry", "by": 7, "note": "now" })
    );
}

#[test]
fn checkout_accepts_no_events() {
    fn uninhabited(e: checkout::Event) -> ! {
        match e {}
    }
    let _ = uninhabited;
}

#[tokio::test(flavor = "multi_thread")]
async fn one_struct_implements_two_charts() {
    let app = App::new(Gateway::Charges);
    let g = greet::greet::run(&app, "Ada".into(), Duration::from_millis(2)).await;
    assert_eq!(
        g,
        greet::Greeting {
            text: "hello, Ada".into(),
            waited: Duration::from_millis(3),
            steps: vec![Duration::from_millis(3), Duration::from_millis(6)],
            extra: None,
        }
    );
    let r = place_order::run(&app, "t_admin".into(), draft()).await;
    assert_eq!(r.status, 201);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_event_with_fields() {
    let app = Arc::new(App::new(Gateway::Charges));
    let actor = greet::greet::spawn(app, "Bo".into(), Duration::from_secs(60));
    assert_eq!(actor.state(), greet::greet::State::Waiting);
    actor
        .send(greet::greet::Event::Hurry {
            by: Duration::from_millis(5),
            note: None,
        })
        .await;
    let g = actor.result().await.unwrap();
    assert_eq!(g.waited, Duration::from_millis(5));
    assert_eq!(g.text, "hello, Bo");
}

#[tokio::test(flavor = "multi_thread")]
async fn spawn_send_and_watch() {
    let mut app = App::new(Gateway::Charges);
    app.pricing = Duration::from_secs(60);
    let mut actor = place_order::spawn(Arc::new(app), "t_admin".into(), draft());
    assert_eq!(actor.state(), place_order::State::Authenticating);
    // Wait until pricing, the slow task, is under way.
    loop {
        let s = actor.changed().await;
        if s == place_order::State::Processing(place_order::Processing::Pricing) {
            break;
        }
        assert_ne!(s, place_order::State::Cancelled, "ended early");
    }
    actor.send(place_order::Event::Cancel).await;
    let mut last = actor.changed().await;
    while last != place_order::State::Cancelled {
        last = actor.changed().await;
    }
    assert_eq!(actor.state(), place_order::State::Cancelled);
    let response = actor.result().await.unwrap();
    assert_eq!(response.status, 499);
    assert_eq!(response.body, serde_json::json!({ "code": "cancelled" }));
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_from_outside() {
    let mut app = App::new(Gateway::Charges);
    app.pricing = Duration::from_secs(60);
    let actor = place_order::spawn(Arc::new(app), "t_admin".into(), draft());
    tokio::time::sleep(Duration::from_millis(20)).await;
    actor.cancel().await;
    assert!(matches!(
        actor.state(),
        place_order::State::Processing(place_order::Processing::Pricing)
    ));
    assert_eq!(actor.result().await, Err(roadbook::Stopped::Cancelled));
}
