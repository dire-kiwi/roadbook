# Rust API sketch

The proposed Rust API as shapes only. `src/lib.rs` has three modules:

- `waymark`: what the runtime crate would export. Bodies are `todo!()`.
- `shop`: what `waymark::include!("examples/shop/place_order.way")` would expand to, written by hand.
- `app`: what a developer writes.

```
cargo check
```
