# Rust API sketch

The proposed Rust API as shapes only. `src/lib.rs` has three modules:

- `roadbook`: what the runtime crate would export. Bodies are `todo!()`.
- `shop`: what `roadbook::include!("examples/shop/place_order.road")` would expand to, written by hand.
- `app`: what a developer writes.

```
cargo check
```
