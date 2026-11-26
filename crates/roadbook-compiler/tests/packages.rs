//! Imports from other packages: resolution, an import satisfied by an already compiled IR
//! (`Loaded::Compiled`), and packages found on disk by `FsLoader`.

mod common;

use std::path::PathBuf;

use roadbook_compiler::{FsLoader, Loaded, MemoryLoader, Options, compile};
use roadbook_ir::Ir;

const TYPES: &str = "/// A line.\ntype Item { sku: string, qty: int }\n";

const INVENTORY: &str = "\
import { Item } from \"./types.road\"

/// The stock.
machine Inventory() {
  event Reserve { orderId: string, items: Item[] }
  emits Reserved { orderId: string }

  initial Open
  state Open {
    on Reserve emit Reserved { orderId: event.orderId }
  }
}
";

const ORDERS: &str = "\
import { Inventory } from \"inventory/charts/inventory.road\"
import { Item } from \"inventory/charts/types.road\"

machine Order(id: string, items: Item[], inventory: ref Inventory) {
  initial Reserving
  state Reserving {
    entry send inventory.Reserve { orderId: id, items: items }
    on inventory.Reserved if event.orderId == id -> Done
  }
  final state Done {}
}
";

fn inventory_ir() -> Ir {
    let mut l = MemoryLoader::new()
        .with("charts/inventory.road", INVENTORY)
        .with("charts/types.road", TYPES);
    let out = compile(&mut l, "charts/inventory.road", &Options::default()).unwrap();
    assert!(
        out.diagnostics.iter().all(|d| !d.is_error()),
        "{}",
        out.render()
    );
    out.ir.unwrap()
}

fn check_schema(out: &roadbook_compiler::Compilation) {
    let errors = common::schema_errors(&common::validator(), out);
    assert!(errors.is_empty(), "{errors:#?}");
}

#[test]
fn a_package_file_from_source() {
    let mut l = MemoryLoader::new()
        .with("orders.road", ORDERS)
        .with_package(
            "inventory",
            "charts/inventory.road",
            Loaded::Source(INVENTORY.into()),
        )
        .with_package(
            "inventory",
            "charts/types.road",
            Loaded::Source(TYPES.into()),
        );
    let out = compile(&mut l, "orders.road", &Options::default()).unwrap();
    assert!(out.diagnostics.is_empty(), "{}", out.render());
    let names: Vec<&str> = out.files.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(
        names,
        [
            "orders.road",
            "inventory/charts/inventory.road",
            "inventory/charts/types.road"
        ]
    );
    let ir = out.ir.as_ref().unwrap();
    let sources: Vec<(&str, Option<&str>)> = ir
        .sources
        .iter()
        .map(|s| (s.path.as_str(), s.package.as_deref()))
        .collect();
    assert_eq!(
        sources,
        [
            ("orders.road", None),
            ("charts/inventory.road", Some("inventory")),
            ("charts/types.road", Some("inventory"))
        ]
    );
    assert!(ir.machines.contains_key("Inventory"));
    check_schema(&out);
}

#[test]
fn a_package_file_from_a_compiled_ir() {
    let dep = inventory_ir();
    let mut from_source = MemoryLoader::new()
        .with("orders.road", ORDERS)
        .with_package(
            "inventory",
            "charts/inventory.road",
            Loaded::Source(INVENTORY.into()),
        )
        .with_package(
            "inventory",
            "charts/types.road",
            Loaded::Source(TYPES.into()),
        );
    let expected = compile(&mut from_source, "orders.road", &Options::default())
        .unwrap()
        .ir
        .unwrap();

    let mut l = MemoryLoader::new()
        .with("orders.road", ORDERS)
        .with_package(
            "inventory",
            "charts/inventory.road",
            Loaded::Compiled(Box::new(dep.clone())),
        );
    let out = compile(&mut l, "orders.road", &Options::default()).unwrap();
    assert!(out.diagnostics.is_empty(), "{}", out.render());
    check_schema(&out);
    // The same document as from the package's sources: the same files, declarations, spans
    // and hashes.
    assert_eq!(out.ir.as_ref().unwrap(), &expected);
    // The dependency's machine is copied as it was, its spans renumbered.
    let merged = &out.ir.as_ref().unwrap().machines["Inventory"];
    assert_eq!(merged.hash, dep.machines["Inventory"].hash);
    assert_eq!(merged.span.file, 1);
    assert_eq!(out.ir.as_ref().unwrap().types["Item"].span.file, 2);
    assert_eq!(out.ir.as_ref().unwrap().types["Item"].doc, "A line.");
}

#[test]
fn a_compiled_ir_names_are_checked_like_parsed_ones() {
    let bad = ORDERS
        .replace("inventory.Reserve {", "inventory.Reserv {")
        .replace("import { Inventory }", "import { Inventory, Nope }");
    let mut l = MemoryLoader::new().with("orders.road", bad).with_package(
        "inventory",
        "charts/inventory.road",
        Loaded::Compiled(Box::new(inventory_ir())),
    );
    let out = compile(&mut l, "orders.road", &Options::default()).unwrap();
    let codes: Vec<String> = out.diagnostics.iter().map(|d| d.code.to_string()).collect();
    assert_eq!(codes, ["RB0112", "RB0116"], "{}", out.render());
    // Diagnostics about the dependency's declarations name its file, which has no text.
    assert!(
        out.render().contains("`Inventory` accepts `Reserve`"),
        "{}",
        out.render()
    );
}

#[test]
fn a_file_loaded_twice_keeps_one_copy() {
    // The order imports the types file from source before the compiled IR that also holds
    // it: the IR's copy is left out.
    let orders = ORDERS
        .replace("import { Inventory } from \"inventory/charts/inventory.road\"\n", "")
        .replace("import { Item } from \"inventory/charts/types.road\"\n", "import { Item } from \"inventory/charts/types.road\"\nimport { Inventory } from \"inventory/charts/inventory.road\"\n");
    let mut l = MemoryLoader::new()
        .with("orders.road", orders)
        .with_package(
            "inventory",
            "charts/types.road",
            Loaded::Source(TYPES.into()),
        )
        .with_package(
            "inventory",
            "charts/inventory.road",
            Loaded::Compiled(Box::new(inventory_ir())),
        );
    let out = compile(&mut l, "orders.road", &Options::default()).unwrap();
    assert!(out.diagnostics.is_empty(), "{}", out.render());
    let ir = out.ir.unwrap();
    assert_eq!(ir.sources.len(), 3);
    assert_eq!(ir.types["Item"].span.file, 1);
}

#[test]
fn a_compiled_ir_of_another_version_is_refused() {
    let mut dep = inventory_ir();
    dep.ir_version = "0.1".into();
    let mut l = MemoryLoader::new()
        .with("orders.road", ORDERS)
        .with_package(
            "inventory",
            "charts/inventory.road",
            Loaded::Compiled(Box::new(dep)),
        );
    let out = compile(&mut l, "orders.road", &Options::default()).unwrap();
    let d = &out.diagnostics[0];
    assert_eq!(d.code.to_string(), "RB0111");
    assert!(d.message.contains("IR version \"0.1\""), "{}", d.message);
}

#[test]
fn import_paths() {
    let chart = |import: &str| {
        format!(
            "import {{ A }} from \"{import}\"\nmachine M() {{\n  initial S\n  state S {{}}\n}}\n"
        )
    };
    for (spec, message) in [
        (
            "types.road",
            "for a file of this project write `./types.road`",
        ),
        ("inventory/", "names package `inventory` but no file in it"),
        ("pkg/x.road", "cannot import `pkg/x.road`: no such file"),
    ] {
        let mut l = MemoryLoader::new().with("m.road", chart(spec));
        let out = compile(&mut l, "m.road", &Options::default()).unwrap();
        assert_eq!(out.diagnostics[0].code.to_string(), "RB0111");
        assert!(
            out.diagnostics[0].message.contains(message),
            "{}",
            out.render()
        );
    }
    // A package's relative import stays in the package.
    let mut l = MemoryLoader::new()
        .with("m.road", chart("p/a.road"))
        .with_package(
            "p",
            "a.road",
            Loaded::Source("import { B } from \"../b.road\"\ntype A { x: int }\n".into()),
        );
    let out = compile(&mut l, "m.road", &Options::default()).unwrap();
    assert!(
        out.render().contains("goes above the root of package `p`"),
        "{}",
        out.render()
    );
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> TempDir {
        let dir = std::env::temp_dir().join(format!("roadbook-pkg-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    fn write(&self, path: &str, text: &str) {
        let p = self.0.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn packages_on_disk() {
    let t = TempDir::new("disk");
    t.write("app/orders.road", ORDERS);
    // Found in `node_modules` above the importing file, through `exports`.
    t.write("node_modules/inventory/package.json", r#"{"name": "inventory", "exports": {"./charts/*": "./dist/*", "./package.json": "./package.json"}}"#);
    t.write("node_modules/inventory/dist/inventory.road", INVENTORY);
    t.write("node_modules/inventory/dist/types.road", TYPES);
    let mut l = FsLoader::new(&t.0).with_node_modules(true);
    let out = compile(&mut l, "app/orders.road", &Options::default()).unwrap();
    assert!(out.diagnostics.is_empty(), "{}", out.render());
    let names: Vec<&str> = out.files.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(
        names,
        [
            "app/orders.road",
            "node_modules/inventory/dist/inventory.road",
            "node_modules/inventory/dist/types.road"
        ]
    );
    let ir = out.ir.unwrap();
    assert_eq!(ir.sources[1].path, "charts/inventory.road");
    assert_eq!(ir.sources[2].path, "charts/types.road");

    // Without the `node_modules` lookup, the package is unknown.
    let mut l = FsLoader::new(&t.0);
    let out = compile(&mut l, "app/orders.road", &Options::default()).unwrap();
    assert!(out.render().contains("`[packages]`"), "{}", out.render());

    // An explicit directory wins over `node_modules`.
    t.write("vendor/inventory/charts/inventory.road", INVENTORY);
    t.write("vendor/inventory/charts/types.road", TYPES);
    let mut l = FsLoader::new(&t.0)
        .with_node_modules(true)
        .with_package("inventory", "vendor/inventory");
    let out = compile(&mut l, "app/orders.road", &Options::default()).unwrap();
    assert!(out.diagnostics.is_empty(), "{}", out.render());
    assert_eq!(out.files[1].0, "vendor/inventory/charts/inventory.road");

    // A file the `exports` map does not list is not exported.
    t.write(
        "node_modules/inventory/package.json",
        r#"{"name": "inventory", "exports": {"./*.js": "./dist/*.js"}}"#,
    );
    let mut l = FsLoader::new(&t.0).with_node_modules(true);
    let out = compile(&mut l, "app/orders.road", &Options::default()).unwrap();
    assert!(
        out.render()
            .contains("does not export `./charts/inventory.road`"),
        "{}",
        out.render()
    );
}
