//! A fixture of roadbook's tests: the first crate of a chain, which `chain1` imports and
//! `chain2` imports in turn. Two importable charts sit at its crate root, and one of them is
//! included again in a module.

roadbook::include!("zero.road");
roadbook::include!("extra.road");

/// The same chart again, with types of its own. Only the first `include!` of a chart exports
/// it to other crates, so this one adds no second hidden macro.
pub mod again {
    roadbook::include!("zero.road");
}

/// A decoy: if `chain1` handed over `chain0` as its own crate, `chain2` would re-export this
/// instead of `chain1::one`.
pub mod one {
    /// Not a machine's module.
    pub struct Decoy;
}
