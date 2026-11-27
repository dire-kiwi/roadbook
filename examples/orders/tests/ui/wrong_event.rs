// Host code sends to an `Inventory` run through its handle: only `inventory::Event` goes.

async fn restock(stock: inventory::Ref) {
    // `reserve` is a call, not an event: an order makes it, a handle cannot send it.
    stock
        .send(inventory::Event::Reserve {
            order_id: "o1".into(),
            items: vec![],
        })
        .await;
    // Nor is an event it does not declare.
    stock.send(inventory::Event::Refund { order_id: "o1".into() }).await;
}

fn main() {
    drop(restock);
}
