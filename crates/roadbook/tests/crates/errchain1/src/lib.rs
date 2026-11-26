//! A fixture of roadbook's tests: the middle crate of a chain whose error enums collide here:
//! the task `charge` keeps `errchain0`'s `ChargeError`, so the machine `Charge` gets
//! `ChargeMachineError`.

roadbook::include!("charge.road");
