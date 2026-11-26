//! The `Order` machine of `orders.road`, which imports `Inventory` from the `inventory` crate.
//!
//! The chart's `import { Inventory, Item, OutOfStock } from "inventory/inventory.road"` is
//! satisfied by the `inventory` crate itself: `roadbook::include!` asks that crate's hidden
//! macro for the chart's IR, so nothing here reads `examples/inventory`. The generated code
//! refers to that crate's types: `Item` and `OutOfStock` here are `inventory::Item` and
//! `inventory::OutOfStock`, re-exported, and the `ref Inventory` parameter of `order::run`
//! and `order::spawn` is an `inventory::inventory::Ref`.

roadbook::include!("orders.road");
