// `order::run`'s `inventory: ref Inventory` takes a handle to an Inventory run, and a handle
// to any other machine is another type.

fn inventory_of(order: roadbook::Ref<orders::order::Event>) -> inventory::Ref {
    order
}

fn main() {
    drop(inventory_of);
}
