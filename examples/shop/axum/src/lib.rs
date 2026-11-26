//! The shop example's `POST /orders`, served by axum.
//!
//! The charts are `examples/shop/place_order.road` (the request), `checkout.road` (the
//! payment, run as a child machine) and `types.road`. `roadbook::include!` compiles them at
//! build time; this crate implements their host code with the fake services that
//! `examples/shop/README.md` describes, so that its answers can be compared with the
//! TypeScript application's.

use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde_json::json;

roadbook::include!("../place_order.road");

/// The application's dependencies: here, the fake services. One struct implements every
/// chart's handlers.
#[derive(Debug, Default)]
pub struct Shop {
    /// Log a line for each email and audit entry.
    pub log: bool,
}

impl Shop {
    fn log(&self, line: String) {
        if self.log {
            eprintln!("{line}");
        }
    }
}

impl checkout::Handlers for Shop {
    fn retry_delay(&self, attempt: i64) -> Duration {
        Duration::from_millis(10 << attempt.clamp(0, 32))
    }

    async fn charge_card(&self, order: &Order, _user: &User) -> Result<Receipt, ChargeCardError> {
        if order.items.iter().any(|i| i.sku == "declined") {
            return Err(CardDeclined {
                reason: "insufficient_funds".into(),
            }
            .into());
        }
        if order.items.iter().any(|i| i.sku == "flaky") {
            return Err(GatewayUnavailable.into());
        }
        Ok(Receipt {
            charge_id: format!("ch_{}", order.id),
            amount_cents: order.total_cents,
        })
    }
}

impl place_order::Handlers for Shop {
    fn confirmation(&self, order: &Order, receipt: &Receipt) -> ApiResponse {
        ApiResponse {
            status: 201,
            body: json!({
                "orderId": order.id,
                "chargeId": receipt.charge_id,
                "totalCents": order.total_cents,
            }),
        }
    }

    fn invalid(&self, issues: &[String]) -> ApiResponse {
        ApiResponse {
            status: 422,
            body: json!({ "code": "invalid", "issues": issues }),
        }
    }

    fn problem(&self, status: i64, code: &str) -> ApiResponse {
        ApiResponse {
            status,
            body: json!({ "code": code }),
        }
    }

    async fn send_confirmation_email(&self, user: &User, order: &Order, receipt: &Receipt) {
        self.log(format!(
            "email: order {} charged {} to {}",
            order.id, receipt.charge_id, user.id
        ));
    }

    async fn audit(&self, what: &str, order: &Order) {
        self.log(format!("audit: {what} for order {}", order.id));
    }

    async fn authenticate(&self, token: &str) -> Result<User, AuthenticateError> {
        let (id, role) = match token {
            "Bearer good" => ("u1", Role::Customer),
            "Bearer admin" => ("u2", Role::Admin),
            _ => return Err(Unauthenticated.into()),
        };
        Ok(User {
            id: id.into(),
            role,
        })
    }

    async fn validate_order(&self, draft: &OrderDraft) -> Result<Order, ValidateOrderError> {
        let issues: Vec<String> = if draft.items.is_empty() {
            vec!["order has no items".into()]
        } else {
            draft
                .items
                .iter()
                .filter(|i| i.qty < 1)
                .map(|i| format!("{}: quantity must be at least 1", i.sku))
                .collect()
        };
        if !issues.is_empty() {
            return Err(ValidationFailed { issues }.into());
        }
        Ok(Order {
            id: "ord_1".into(),
            items: draft.items.clone(),
            total_cents: 0,
        })
    }

    async fn price_order(&self, order: &Order, user: &User) -> Order {
        // Checked in debug and release builds alike: an overflow is a defect, answered 500.
        let total = order
            .items
            .iter()
            .try_fold(0_i64, |sum, i| i.qty.checked_mul(1250)?.checked_add(sum))
            .expect("the order's total overflows an int");
        let total_cents = match user.role {
            Role::Admin => total - total / 10,
            Role::Customer => total,
        };
        Order {
            total_cents,
            ..order.clone()
        }
    }

    async fn checkout(&self, order: &Order, user: &User) -> Result<Receipt, CheckoutError> {
        checkout::run(self, order.clone(), user.clone()).await
    }
}

/// The router: `POST /orders`.
pub fn app(shop: Arc<Shop>) -> Router {
    Router::new()
        .route("/orders", post(post_orders))
        .with_state(shop)
}

/// One `PlaceOrder` run per request. A request whose `Content-Type` is not
/// `application/json`, or whose body is not an `OrderDraft`, is answered
/// `400 {"code": "bad_request"}` without running the machine. A run that ends with a defect
/// is answered `500 {"code": "internal"}`.
async fn post_orders(State(shop): State<Arc<Shop>>, headers: HeaderMap, body: Bytes) -> Response {
    let draft = is_json(&headers)
        .then(|| serde_json::from_slice::<OrderDraft>(&body).ok())
        .flatten();
    let Some(draft) = draft else {
        return answer(StatusCode::BAD_REQUEST, "bad_request");
    };
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    match catch_unwind(place_order::run(&*shop, token, draft)).await {
        Ok(r) => respond(r),
        Err(payload) => {
            if let Some(d) = payload.downcast_ref::<roadbook::Defect>() {
                shop.log(format!("defect: {d}"));
            }
            answer(StatusCode::INTERNAL_SERVER_ERROR, "internal")
        }
    }
}

/// Whether the request says its body is JSON: `application/json`, with any parameters, as
/// Express's `express.json()` requires.
fn is_json(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|t| t.trim().eq_ignore_ascii_case("application/json"))
}

/// `status {"code": code}`.
fn answer(status: StatusCode, code: &str) -> Response {
    (status, axum::Json(json!({ "code": code }))).into_response()
}

/// Polls `fut` to its end, catching a panic: how a run that ends with a defect unwinds.
async fn catch_unwind<T>(fut: impl Future<Output = T>) -> std::thread::Result<T> {
    let mut fut = std::pin::pin!(fut);
    std::future::poll_fn(|cx| {
        match std::panic::catch_unwind(AssertUnwindSafe(|| fut.as_mut().poll(cx))) {
            Ok(Poll::Ready(v)) => Poll::Ready(Ok(v)),
            Ok(Poll::Pending) => Poll::Pending,
            Err(p) => Poll::Ready(Err(p)),
        }
    })
    .await
}

/// The HTTP response for the chart's `ApiResponse`.
pub fn respond(r: ApiResponse) -> Response {
    let status = u16::try_from(r.status)
        .ok()
        .and_then(|s| StatusCode::from_u16(s).ok())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, axum::Json(r.body)).into_response()
}
