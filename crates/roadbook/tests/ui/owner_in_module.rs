// A chart whose crate includes it in a module, not at its crate root, cannot be imported: the
// error says so, and how to fix it.

roadbook::include!("charts/uses_nested.road");

fn main() {}
