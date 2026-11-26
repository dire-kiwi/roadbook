// Host code sends to an `Inventory` run through its handle: only `inventory::Event` goes.

async fn restock(stock: inventory::Ref) {
    // What Inventory emits is not what it accepts.
    stock
        .send(inventory::Emitted::Reserved {
            order_id: "o1".into(),
        })
        .await;
    // Nor is an event it does not declare.
    stock.send(inventory::Event::Refund { order_id: "o1".into() }).await;
}

fn main() {
    drop(restock);
}
