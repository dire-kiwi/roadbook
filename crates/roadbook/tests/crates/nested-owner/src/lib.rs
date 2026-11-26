//! A fixture of roadbook's tests: a crate that includes its chart in a module, not at its crate
//! root, so another crate that imports the chart gets an error saying so
//! (`tests/ui/owner_in_module.rs`).

/// The chart, in a module.
pub mod inner {
    roadbook::include!("nested.road");
}
