//! Charts imported across crates (the fixtures in `tests/crates/`): a chain (`chain2` imports
//! `chain1`, which imports `chain0`), a chain whose error enums collide in the middle crate
//! (`errchain0` to `errchain2`), a diamond (`apex` imports `left` and `right`, which both
//! import `inventory`), a renamed dependency whose package name has a hyphen, two importable
//! charts at one crate root, and one chart included twice in a crate. (`inv` is the
//! `inventory` example crate.)

#![deny(warnings)]

use std::sync::{Arc, Mutex};

struct Host;

impl chain0::zero::Handlers for Host {}
impl chain0::again::zero::Handlers for Host {}
impl chain1::one::Handlers for Host {}
impl chain2::two::Handlers for Host {}

#[tokio::test]
async fn a_chain_of_three_crates() {
    let host = Arc::new(Host);
    let zero = chain0::zero::spawn(host.clone());
    let one = chain1::one::spawn(
        host.clone(),
        zero.handle(),
        chain0::Greeting { text: "hi".into() },
    );
    // Each crate re-exports the items of the crate that owns them: one Rust type each.
    let handle: chain2::one::Ref = one.handle();
    assert_eq!(chain2::one_of(&handle), one.id());
    let _: chain1::zero::Ref = zero.handle();
    let _: chain2::Greeting = chain0::Greeting {
        text: "same".into(),
    };
    let two = chain2::two::spawn(host.clone(), handle);
    two.send(chain2::two::Event::Go).await;
    two.result().await.unwrap();
    assert_eq!(one.result().await, Ok("hi".to_owned()));
    zero.result().await.unwrap();
}

/// Declines every charge.
struct Card;

impl errchain1::charge::Handlers for Card {
    async fn charge(&self) -> Result<(), errchain0::ChargeError> {
        Err(errchain0::Declined.into())
    }
}

impl errchain2::pay::Handlers for Card {
    async fn charge(&self) -> Result<(), errchain1::ChargeMachineError> {
        errchain1::charge::run(self).await
    }
}

#[tokio::test]
async fn a_chain_of_crates_names_the_error_enums_their_owners_chose() {
    // `errchain1` re-exports the task's `ChargeError` from `errchain0`, so its machine's is
    // `ChargeMachineError`; `errchain2` re-exports both under those names, although
    // `errchain1`'s chart alone would give `ChargeTaskError` and `ChargeMachineError`.
    let _: errchain2::ChargeError = errchain0::ChargeError::Declined(errchain0::Declined);
    let _: errchain2::ChargeMachineError =
        errchain1::ChargeMachineError::Declined(errchain2::Declined);
    assert_eq!(
        errchain1::charge::run(&Card).await,
        Err(errchain1::ChargeMachineError::Declined(errchain0::Declined))
    );
    assert!(!errchain2::pay::run(&Card).await);
}

#[tokio::test]
async fn a_chart_included_twice_in_a_crate_has_two_copies() {
    let zero = chain0::again::zero::spawn(Arc::new(Host));
    zero.send(chain0::again::zero::Event::Ping).await;
    zero.result().await.unwrap();
}

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
    async fn add_stock(&self, _: &str, sku: &str, _: i64) {
        self.0.lock().unwrap().push(sku.to_owned());
    }
}

impl right::right::Handlers for Warehouse {}
impl left::left::Handlers for Warehouse {}

impl apex::apex::Handlers for Warehouse {
    async fn left(&self, stock: &inv::Ref) {
        apex::left::run(self, stock.clone()).await
    }
}

#[tokio::test]
async fn a_diamond_of_crates() {
    let warehouse = Arc::new(Warehouse::default());
    let stock = inv::spawn(warehouse.clone(), "w1".into());
    // The diamond's `inventory` is the owner's, through every side.
    let handle: apex::inventory::Ref = stock.handle();
    let _: left::inventory::Ref = handle.clone();
    let _: right::inventory::Ref = handle.clone();
    let right = right::right::spawn(warehouse.clone(), handle.clone());
    let apex = apex::apex::spawn(warehouse.clone(), handle, right.handle());
    // Let the apex start its Left before telling it to go.
    tokio::task::yield_now().await;
    apex.send(apex::apex::Event::Go).await;
    apex.result().await.unwrap();
    right.result().await.unwrap();
    while warehouse.0.lock().unwrap().len() < 2 {
        tokio::task::yield_now().await;
    }
    let mut skus = warehouse.0.lock().unwrap().clone();
    skus.sort();
    assert_eq!(skus, ["left", "right"]);
}

impl apex::quoted::shelf::Handlers for Host {}
impl apex::quoted::filler::Handlers for Host {}
impl apex::bare::stocker::Handlers for Host {}

#[tokio::test]
async fn a_renamed_dependency_with_a_hyphen_in_its_package_name() {
    let host = Arc::new(Host);
    for bare in [false, true] {
        // `apex::quoted::shelf` and `apex::bare::shelf` are both `hyphen-stock`'s.
        let shelf = apex::bare::shelf::spawn(host.clone());
        let handle: apex::quoted::shelf::Ref = shelf.handle();
        if bare {
            let stocker = apex::bare::stocker::spawn(host.clone(), handle);
            stocker.send(apex::bare::stocker::Event::Go).await;
            stocker.result().await.unwrap();
        } else {
            let filler = apex::quoted::filler::spawn(host.clone(), handle);
            filler.send(apex::quoted::filler::Event::Go).await;
            filler.result().await.unwrap();
        }
        shelf.result().await.unwrap();
    }
}
