// A type that contains itself with no `?` or `[]` on the way has no finite value, and no
// Rust type: an error at the macro call, not rustc's "recursive type has infinite size".

roadbook::include!("charts/endless.road");

fn main() {}
