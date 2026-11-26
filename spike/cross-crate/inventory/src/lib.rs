//! Owns `inventory.road`. In the real design `roadbook::include!("inventory.road")` would
//! generate `Event`, `Ref`, `Handlers`, `spawn`; here `export_road!` embeds the chart and
//! exports the hidden callback macro, and `Event` is written by hand.

road_probe_macro::export_road!("inventory.road");

#[derive(Debug)]
pub enum Event {
    Restock { sku: String, qty: i64 },
    Reserve { sku: String, qty: i64 },
}

pub fn accepts(e: Event) -> &'static str {
    match e {
        Event::Restock { .. } => "Restock",
        Event::Reserve { .. } => "Reserve",
    }
}
