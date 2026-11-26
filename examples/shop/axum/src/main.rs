//! Serves the shop example on `127.0.0.1:3000` (or `$PORT`).

use std::sync::Arc;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let port = std::env::var("PORT").unwrap_or_else(|_| "3000".into());
    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}")).await?;
    eprintln!("shop: POST http://{}/orders", listener.local_addr()?);
    axum::serve(
        listener,
        shop_axum::app(Arc::new(shop_axum::Shop { log: true })),
    )
    .await
}
