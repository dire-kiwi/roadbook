//! A fixture of roadbook's tests: the last crate of a chain, importing `chain1`'s chart, which
//! imports `chain0`'s.

roadbook::include!("two.road");

/// Only compiles if `one` is `chain1`'s module, not `chain0`'s decoy.
pub fn one_of(o: &one::Ref) -> &str {
    o.id()
}
