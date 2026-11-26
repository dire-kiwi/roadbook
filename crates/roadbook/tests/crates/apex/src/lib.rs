//! A fixture of roadbook's tests: the top of a diamond (`left` and `right` both import
//! `inventory`, and this crate imports all three), and charts importing from a dependency
//! renamed in `Cargo.toml` whose package name has a hyphen (`stock` is `hyphen-stock`).

roadbook::include!("apex.road");

/// The renamed dependency named by its package name, quoted.
pub mod quoted {
    roadbook::include!("quoted.road", crates("hyphen-stock" = stock));
}

/// The renamed dependency named by its package name, bare.
pub mod bare {
    roadbook::include!("bare.road", crates(hyphen - stock = stock));
}
