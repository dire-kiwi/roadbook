//! Every request of `examples/shop/README.md`, against the server on an ephemeral port, and
//! the retry path under a virtual timer.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use roadbook::{BoxFuture, Config, MemorySink, Record, Timer};
use serde_json::{Value, json};
use shop_axum::{OrderDraft, Shop, place_order};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;

/// Sends one request with a raw HTTP/1.1 client and gives the status and JSON body.
async fn post(addr: std::net::SocketAddr, auth: Option<&str>, body: &str) -> (u16, Value) {
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let auth = auth.map_or(String::new(), |a| format!("Authorization: {a}\r\n"));
    let req = format!(
        "POST /orders HTTP/1.1\r\nHost: shop\r\nContent-Type: application/json\r\n{auth}\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut resp = String::new();
    s.read_to_string(&mut resp).await.unwrap();
    let (head, body) = resp.split_once("\r\n\r\n").unwrap();
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    (status, serde_json::from_str(body).unwrap())
}

#[tokio::test(flavor = "multi_thread")]
async fn every_request_in_the_readme_gets_its_response() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, shop_axum::app(Arc::new(Shop::default())))
            .await
            .unwrap()
    });
    let apples = r#"{"items": [{"sku": "apple", "qty": 2}]}"#;
    let rows: [(Option<&str>, &str, u16, Value); 9] = [
        (
            Some("Bearer good"),
            apples,
            201,
            json!({"orderId": "ord_1", "chargeId": "ch_ord_1", "totalCents": 2500}),
        ),
        (
            Some("Bearer admin"),
            apples,
            201,
            json!({"orderId": "ord_1", "chargeId": "ch_ord_1", "totalCents": 2250}),
        ),
        (
            Some("Bearer nope"),
            apples,
            401,
            json!({"code": "unauthenticated"}),
        ),
        (None, apples, 401, json!({"code": "unauthenticated"})),
        (
            Some("Bearer good"),
            r#"{"items": []}"#,
            422,
            json!({"code": "invalid", "issues": ["order has no items"]}),
        ),
        (
            Some("Bearer good"),
            r#"{"items": [{"sku": "a", "qty": 0}, {"sku": "b", "qty": -1}]}"#,
            422,
            json!({"code": "invalid", "issues": [
                "a: quantity must be at least 1", "b: quantity must be at least 1"
            ]}),
        ),
        (
            Some("Bearer good"),
            r#"{"items": [{"sku": "declined", "qty": 1}]}"#,
            402,
            json!({"code": "payment_failed"}),
        ),
        (
            Some("Bearer good"),
            r#"{"items": [{"sku": "flaky", "qty": 1}]}"#,
            402,
            json!({"code": "payment_failed"}),
        ),
        (
            Some("Bearer good"),
            r#"{"items": "nope"}"#,
            400,
            json!({"code": "bad_request"}),
        ),
    ];
    for (auth, body, status, expected) in rows {
        let got = post(addr, auth, body).await;
        assert_eq!(got, (status, expected), "{auth:?} {body}");
    }
}

/// A timer that fires only when the test says: each start is handed to the test.
struct Manual {
    now: AtomicU64,
    started: Mutex<VecDeque<(String, u64, oneshot::Sender<()>)>>,
}

impl Timer for Manual {
    fn now(&self) -> u64 {
        self.now.load(Ordering::SeqCst)
    }

    fn start(&self, run: &str, _timer: u64, delay: u64) -> BoxFuture<'static, ()> {
        let (tx, rx) = oneshot::channel();
        self.started
            .lock()
            .unwrap()
            .push_back((run.to_owned(), delay, tx));
        Box::pin(async move {
            if rx.await.is_err() {
                // Never fired: the test dropped it.
                std::future::pending::<()>().await;
            }
        })
    }
}

#[tokio::test]
async fn a_flaky_gateway_is_retried_three_times_with_growing_delays() {
    let timer = Arc::new(Manual {
        now: AtomicU64::new(0),
        started: Mutex::new(VecDeque::new()),
    });
    let sink = Arc::new(MemorySink::new());
    let config = Config::new()
        .timer(timer.clone())
        .sink(sink.clone())
        .run_id("r1");
    let shop = Shop::default();
    let draft: OrderDraft =
        serde_json::from_str(r#"{"items": [{"sku": "flaky", "qty": 1}]}"#).unwrap();
    let run = place_order::run_with(&config, &shop, "Bearer good".into(), draft);
    tokio::pin!(run);

    let mut delays = Vec::new();
    let mut held = Vec::new();
    let response = loop {
        tokio::select! {
            r = &mut run => break r,
            () = tokio::task::yield_now() => {}
        }
        let next = timer.started.lock().unwrap().pop_front();
        if let Some((run_id, delay, fire)) = next {
            if run_id == "r1" {
                // PlaceOrder's 30 second budget: never reached in this test.
                assert_eq!(delay, 30_000);
                held.push(fire);
            } else {
                assert_eq!(run_id, "r1.4");
                delays.push(delay);
                timer.now.fetch_add(delay, Ordering::SeqCst);
                let _ = fire.send(());
            }
        }
    };
    assert_eq!(response.status, 402);
    assert_eq!(response.body, json!({"code": "payment_failed"}));
    assert_eq!(delays, [20, 40, 80]);

    // Four charge attempts in the child run, and its records on the virtual clock.
    let records = sink.records();
    let attempts = records
        .iter()
        .filter(|r| r.run == "r1.4")
        .filter(|r| matches!(&r.record, Record::InvokeStarted { src, .. } if src == "chargeCard"))
        .count();
    assert_eq!(attempts, 4);
    let failed = records
        .iter()
        .find(|r| r.run == "r1.4" && matches!(r.record, Record::Failed { .. }))
        .unwrap();
    assert_eq!(failed.t, 140);
    assert_eq!(failed.parent.as_deref(), Some("r1"));
}
