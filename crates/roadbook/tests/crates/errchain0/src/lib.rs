//! A fixture of roadbook's tests: the first crate of a chain whose error enums collide one
//! crate along (`errchain2` imports `errchain1` imports `errchain0`).

roadbook::include!("charge.road");
