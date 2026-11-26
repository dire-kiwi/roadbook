// Mechanism 2: forward every chart-owning direct dependency's directory to rustc, where the
// proc macro reads it with std::env::var. Generic: it does not name any dependency.
fn main() {
    let vars: Vec<(String, String)> = std::env::vars().collect();
    for (k, dir) in &vars {
        let Some(links) = k.strip_prefix("DEP_").and_then(|k| k.strip_suffix("_ROAD_DIR")) else { continue };
        let pkg_key = format!("DEP_{links}_ROAD_PACKAGE");
        let pkg = vars.iter().find(|(k, _)| *k == pkg_key).map(|(_, v)| v.replace('-', "_")).unwrap_or_default();
        println!("cargo::rustc-env=ROADBOOK_DIR_{pkg}={dir}");
    }
    println!("cargo::rerun-if-changed=build.rs");
}
