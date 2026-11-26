// Mechanism 2: publish where this package's sources (and so its charts) are.
// Dependents' build scripts see DEP_ROADBOOK_INVENTORY_ROAD_DIR and ..._ROAD_PACKAGE.
fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let pkg = std::env::var("CARGO_PKG_NAME").unwrap();
    println!("cargo::metadata=road_dir={dir}");
    println!("cargo::metadata=road_package={pkg}");
    println!("cargo::rerun-if-changed=build.rs");
}
