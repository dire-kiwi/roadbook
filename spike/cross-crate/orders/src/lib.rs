//! Imports `Inventory` from the `inventory` crate's chart. One module per mechanism, each
//! behind a feature so a failing mechanism does not hide the others. Each expands twice to
//! show the per-crate cache of mechanism 1.

macro_rules! via {
    ($feature:literal, $name:ident, $via:literal) => {
        #[cfg(feature = $feature)]
        pub mod $name {
            road_probe_macro::include_road!("orders.road", via = $via);
            pub mod again {
                road_probe_macro::include_road!("orders.road", via = $via);
            }
        }
    };
}
via!("metadata", via_metadata, "metadata");
via!("env", via_env, "env");
via!("toml", via_toml, "toml");
via!("callback", via_callback, "callback");

fn field<'a>(src: &'a str, key: &str) -> &'a str {
    src.lines().find_map(|l| l.trim().strip_prefix(key)).map(str::trim).unwrap_or("?")
}

macro_rules! line {
    ($m:path) => {{
        use $m as m;
        // A value of the dependency's own type, built through the generated alias.
        let sent = m::inventory::deliver(m::inventory::Event::Reserve { sku: "A1".into(), qty: 1 });
        format!(
            "RESULT via={} copy={} rev={} consistent={} us={} cached_us={} crate={} sent={} path={} note={} externs=[{}]",
            m::inventory::VIA,
            field(m::inventory::SOURCE, "// copy:"),
            field(m::inventory::SOURCE, "// rev:"),
            if m::inventory::SOURCE == m::inventory::COMPILED_SOURCE { "yes" } else { "NO(compiled-from-other-file)" },
            m::inventory::MICROS,
            m::again::inventory::MICROS,
            m::inventory::CRATE,
            sent,
            m::inventory::PATH,
            m::inventory::NOTE,
            m::RUSTC_EXTERNS,
        )
    }};
}

pub fn report() -> Vec<String> {
    #[allow(unused_mut)]
    let mut v = Vec::new();
    #[cfg(feature = "metadata")]
    v.push(line!(via_metadata));
    #[cfg(feature = "env")]
    v.push(line!(via_env));
    #[cfg(feature = "toml")]
    v.push(line!(via_toml));
    #[cfg(feature = "callback")]
    v.push(line!(via_callback));
    v
}
