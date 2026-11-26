//! A chart that imports from a crate this crate depends on under another name: the
//! `inventory` package is the dependency `inv`, so `include!` is told with `crates(…)`.

use std::sync::{Arc, Mutex};

roadbook::include!("tests/charts/restock.road", crates(inventory = inv));

#[derive(Default)]
struct Warehouse(Mutex<Vec<String>>);

impl inv::Handlers for Warehouse {
    async fn reserve_stock(
        &self,
        _: &str,
        _: &str,
        _: &[inv::Item],
    ) -> Result<(), inv::ReserveStockError> {
        Ok(())
    }
    async fn release_stock(&self, _: &str, _: &str) {}
    async fn add_stock(&self, warehouse: &str, sku: &str, qty: i64) {
        self.0
            .lock()
            .unwrap()
            .push(format!("{warehouse} {sku} {qty}"));
    }
}

struct Host;
impl restocker::Handlers for Host {}

#[tokio::test]
async fn a_renamed_dependency_is_named_with_crates() {
    let warehouse = Arc::new(Warehouse::default());
    let stock = inv::spawn(warehouse.clone(), "w1".into());
    // The generated `ref Inventory` is the dependency's own handle type.
    let handle: inv::inventory::Ref = stock.handle();
    let restocker = restocker::spawn(Arc::new(Host), handle);
    restocker.send(restocker::Event::Go).await;
    restocker.result().await.unwrap();
    while warehouse.0.lock().unwrap().is_empty() {
        tokio::task::yield_now().await;
    }
    assert_eq!(*warehouse.0.lock().unwrap(), ["w1 s1 3"]);
}
