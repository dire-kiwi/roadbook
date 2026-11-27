//! The `Inventory` machine of `inventory.road`, for other crates to use.
//!
//! `roadbook::include!` generates the chart's types (`Item`, `OutOfStock`), the error enum of
//! its task (`ReserveStockError`) and the module `inventory` with `Event`, `Ref`, `Handlers`,
//! `run` and `spawn`. The `reserve` call is in none of them: an `Order` makes it from its
//! chart, and the driver delivers it and its answer. It also exports a hidden macro that hands
//! the chart's IR to the `roadbook::include!` of a crate whose chart imports
//! `"inventory/inventory.road"`, as `examples/orders` does: that crate's generated code then
//! uses these types instead of copies of them. A chart that other crates import is included
//! at the root of its crate, where their generated code looks for it.

roadbook::include!("inventory.road");

pub use self::inventory::{Event, Handlers, Ref, spawn, spawn_with};
