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

const INV: &str = "\
machine Inv() {
  event Reserve { n: int }
  emits Reserved { n: int }
  initial A
  state A { on Reserve -> A  emit Reserved { n: event.n } }
}
";

fn sources(out: &roadbook_compiler::Compilation) -> Vec<(String, Option<String>)> {
    out.ir
        .as_ref()
        .unwrap()
        .sources
        .iter()
        .map(|s| (s.path.clone(), s.package.clone()))
        .collect()
}

#[test]
fn a_file_reached_by_a_relative_path_and_as_a_package_is_one_file() {
    let t = TempDir::new("same-file");
    t.write("inv/inv.road", &format!("{INV}type Qty {{ n: int }}\n"));
    let order = |first: &str, second: &str| {
        format!(
            "import {{ Inv }} from \"{first}\"\nimport {{ Qty }} from \"{second}\"\n\
             machine Order(i: ref Inv, q: Qty) {{\n  initial A\n  state A {{\n    \
             on i.Reserved -> A\n  }}\n}}\n"
        )
    };
    // It keeps the name it is first reached by.
    for (first, second, name) in [
        ("../inv/inv.road", "inv/inv.road", ("inv/inv.road", None)),
        ("inv/inv.road", "../inv/inv.road", ("inv.road", Some("inv"))),
    ] {
        t.write("app/order.road", &order(first, second));
        let mut l = FsLoader::new(&t.0).with_package("inv", "inv");
        let out = compile(&mut l, "app/order.road", &Options::default()).unwrap();
        assert!(out.diagnostics.is_empty(), "{}", out.render());
        assert_eq!(out.files.len(), 2);
        assert_eq!(out.files[1].0, "inv/inv.road");
        assert_eq!(
            sources(&out)[1],
            (name.0.to_string(), name.1.map(String::from))
        );
    }
}

#[test]
fn packages_that_import_each_other() {
    let t = TempDir::new("mutual");
    t.write(
        "a/a.road",
        "import { B } from \"b/b.road\"\ntype Item { s: string }\nmachine A(b: ref B) {\n  event Go\n  emits Hi\n  initial X\n  state X { on Go -> X  send b.Poke  emit Hi }\n}\n",
    );
    t.write(
        "b/b.road",
        "import { A } from \"a/a.road\"\nmachine B(a: ref A) {\n  event Poke\n  initial X\n  state X { on Poke -> X  on a.Hi -> X }\n}\n",
    );
    t.write(
        "app.road",
        "import { A } from \"a/a.road\"\nmachine App(a: ref A) {\n  initial X\n  final state X {}\n}\n",
    );
    let loader = || {
        FsLoader::new(&t.0)
            .with_package("a", "a")
            .with_package("b", "b")
    };
    // The whole project, as `roadbook check` does: each package's file is also a root.
    let roots: Vec<String> = ["a/a.road", "app.road", "b/b.road"]
        .map(String::from)
        .into();
    let out = roadbook_compiler::check(&mut loader(), &roots, &Options::default()).unwrap();
    assert!(!out.has_errors(), "{}", out.render());
    for root in &roots {
        let out = compile(&mut loader(), root, &Options::default()).unwrap();
        assert!(!out.has_errors(), "{root}: {}", out.render());
    }
    let out = compile(&mut loader(), "app.road", &Options::default()).unwrap();
    assert_eq!(
        sources(&out),
        [
            ("app.road".to_string(), None),
            ("a.road".to_string(), Some("a".to_string())),
            ("b.road".to_string(), Some("b".to_string()))
        ]
    );
    check_schema(&out);
}

#[test]
fn exports_with_a_main_entry_and_specific_patterns() {
    let t = TempDir::new("exports");
    t.write(
        "node_modules/pk/package.json",
        r#"{ "name": "pk", "exports": { ".": "./index.js", "./*.road": "./charts/*.road", "./legacy/*.road": "./old/*.road" } }"#,
    );
    t.write(
        "node_modules/pk/charts/y.road",
        "type InCharts { s: string }\n",
    );
    t.write(
        "node_modules/pk/charts/legacy/x.road",
        "type FromCharts { s: string }\n",
    );
    t.write("node_modules/pk/old/x.road", "type FromOld { s: string }\n");
    t.write(
        "app.road",
        "import { InCharts } from \"pk/y.road\"\nimport { FromOld } from \"pk/legacy/x.road\"\n\
         machine M(a: InCharts, b: FromOld) {\n  initial X\n  final state X {}\n}\n",
    );
    let mut l = FsLoader::new(&t.0).with_node_modules(true);
    let out = compile(&mut l, "app.road", &Options::default()).unwrap();
    assert!(out.diagnostics.is_empty(), "{}", out.render());
    let names: Vec<&str> = out.files.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(
        names,
        [
            "app.road",
            "node_modules/pk/charts/y.road",
            "node_modules/pk/old/x.road"
        ]
    );
}

const LEAF: &str = "type Out { s: string }\n";

fn importing(spec: &str) -> String {
    format!(
        "import {{ Out }} from \"{spec}\"\nmachine M(o: Out) {{\n  initial X\n  final state X {{}}\n}}\n"
    )
}

/// Compiles `entry` in the project `proj` of `t`, with the loader `setup` makes.
fn check_in(
    t: &TempDir,
    entry: &str,
    setup: impl FnOnce(FsLoader) -> FsLoader,
) -> roadbook_compiler::Compilation {
    let mut l = setup(FsLoader::new(t.0.join("proj")));
    compile(&mut l, entry, &Options::default()).unwrap()
}

/// An import names a file of the project or of a package, never one above either's root,
/// and nothing outside is read: the file is never shown, nor put in the IR's `sources`.
#[test]
fn imports_do_not_go_above_the_project_root() {
    let t = TempDir::new("above-root");
    t.write("outside.road", "type Out { secret: string }\n");
    for spec in [
        "/../outside.road",
        "../../outside.road",
        "./../a/../../outside.road",
        "/../../../../../../../../../../etc/os-release",
    ] {
        t.write("proj/charts/m.road", &importing(spec));
        let out = check_in(&t, "charts/m.road", |l| l);
        let rendered = out.render();
        assert!(out.ir.is_none(), "{spec}");
        assert_eq!(out.diagnostics.len(), 1, "{spec}\n{rendered}");
        assert_eq!(out.diagnostics[0].code.to_string(), "RB0111");
        assert!(
            rendered.contains("goes above the project root"),
            "{spec}\n{rendered}"
        );
        assert_eq!(out.files.len(), 1, "{spec}: read another file");
        assert!(!rendered.contains("secret: string"), "{rendered}");
    }

    // `../` inside the project is fine.
    t.write("proj/lib/out.road", LEAF);
    for spec in [
        "../lib/out.road",
        "/lib/out.road",
        "./../charts/../lib/out.road",
    ] {
        t.write("proj/charts/m.road", &importing(spec));
        let out = check_in(&t, "charts/m.road", |l| l);
        assert!(out.diagnostics.is_empty(), "{spec}\n{}", out.render());
        assert_eq!(out.ir.unwrap().sources[1].path, "lib/out.road");
    }

    // An entry named outside the root may import its neighbours, but nothing further up.
    t.write("side/m.road", &importing("./out.road"));
    t.write("side/out.road", LEAF);
    let out = check_in(&t, "../side/m.road", |l| l);
    assert!(out.diagnostics.is_empty(), "{}", out.render());
    t.write("side/m.road", &importing("../../outside.road"));
    let out = check_in(&t, "../side/m.road", |l| l);
    assert!(
        out.render().contains("further above the project root"),
        "{}",
        out.render()
    );
    assert_eq!(out.files.len(), 1);
}

/// A package's relative import cannot leave the package's directory, even when `exports`
/// maps its files somewhere other than their paths in the package say.
#[test]
fn imports_do_not_leave_a_package_through_exports() {
    let t = TempDir::new("exports-escape");
    t.write("proj/secret.road", "type Out { secret: string }\n");
    // `pk/a/b/c.road` is `c.road` in the package's directory, so `../../secret.road` is
    // `secret.road` of the package by its path but two directories above it on disk.
    t.write(
        "proj/node_modules/pk/package.json",
        r#"{ "name": "pk", "exports": { "./a/b/*": "./*" } }"#,
    );
    t.write(
        "proj/node_modules/pk/c.road",
        "import { Out } from \"../../secret.road\"\ntype C { o: Out }\n",
    );
    t.write(
        "proj/m.road",
        "import { C } from \"pk/a/b/c.road\"\nmachine M(c: C) {\n  initial X\n  final state X {}\n}\n",
    );
    let out = check_in(&t, "m.road", |l| l.with_node_modules(true));
    let rendered = out.render();
    assert!(rendered.contains("outside package `pk`"), "{rendered}");
    assert!(out.ir.is_none());
    assert_eq!(out.files.len(), 2, "{rendered}");
    assert!(!rendered.contains("secret: string"), "{rendered}");

    // Within the package's directory, a relative import is fine however `exports` maps it.
    t.write("proj/node_modules/pk/shared.road", LEAF);
    t.write(
        "proj/node_modules/pk/c.road",
        "import { Out } from \"./shared.road\"\ntype C { o: Out }\n",
    );
    let out = check_in(&t, "m.road", |l| l.with_node_modules(true));
    assert!(out.diagnostics.is_empty(), "{}", out.render());
}

/// A symbolic link inside the project or a package is followed only if it leads to a file
/// inside it; a package directory that is itself a link (as package managers make) is fine.
#[cfg(unix)]
#[test]
fn symbolic_links_do_not_lead_out_of_the_project_or_a_package() {
    use std::os::unix::fs::symlink;
    let t = TempDir::new("links");
    t.write("outside.road", "type Out { secret: string }\n");
    t.write("proj/lib/out.road", LEAF);
    std::fs::create_dir_all(t.0.join("proj/charts")).unwrap();
    symlink(t.0.join("outside.road"), t.0.join("proj/charts/evil.road")).unwrap();
    symlink(
        t.0.join("proj/lib/out.road"),
        t.0.join("proj/charts/near.road"),
    )
    .unwrap();
    std::fs::create_dir_all(t.0.join("elsewhere")).unwrap();
    symlink(t.0.join("elsewhere"), t.0.join("proj/linked")).unwrap();
    t.write("elsewhere/out.road", LEAF);

    for spec in ["./evil.road", "../linked/out.road"] {
        t.write("proj/charts/m.road", &importing(spec));
        let out = check_in(&t, "charts/m.road", |l| l);
        let rendered = out.render();
        assert!(
            rendered.contains("a symbolic link leads from it to outside the project"),
            "{spec}\n{rendered}"
        );
        assert_eq!(out.files.len(), 1, "{spec}");
        assert!(!rendered.contains("secret: string"), "{rendered}");
    }
    // An entry that is such a link is not read either.
    let err = compile(
        &mut FsLoader::new(t.0.join("proj")),
        "charts/evil.road",
        &Options::default(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("symbolic link"), "{err}");

    // A link to a file inside the project is followed.
    t.write("proj/charts/m.road", &importing("./near.road"));
    let out = check_in(&t, "charts/m.road", |l| l);
    assert!(out.diagnostics.is_empty(), "{}", out.render());

    // A package whose directory is a link, with a link inside it leading out of it.
    t.write("pkgs/inv/out.road", LEAF);
    symlink(t.0.join("outside.road"), t.0.join("pkgs/inv/evil.road")).unwrap();
    std::fs::create_dir_all(t.0.join("proj/node_modules")).unwrap();
    symlink(t.0.join("pkgs/inv"), t.0.join("proj/node_modules/inv")).unwrap();
    t.write("proj/charts/m.road", &importing("inv/out.road"));
    let out = check_in(&t, "charts/m.road", |l| l.with_node_modules(true));
    assert!(out.diagnostics.is_empty(), "{}", out.render());
    t.write("proj/charts/m.road", &importing("inv/evil.road"));
    let out = check_in(&t, "charts/m.road", |l| l.with_node_modules(true));
    assert!(
        out.render().contains("outside package `inv`"),
        "{}",
        out.render()
    );
    assert_eq!(out.files.len(), 1);
}

const VAULT: &str = "\
type Secret { label: string, sensitive value: string }
type Key = Secret

task fetch(sensitive id: string) -> Key

machine Locker(sensitive pin: int) {
  event Open { sensitive code: string }
  emits Opened { sensitive code: string }

  initial Shut
  state Shut {
    on Open -> Shut  emit Opened { code: event.code }
  }
}
";

const SAFE: &str = "\
import { Key, fetch, Locker } from \"vault/vault.road\"

machine Safe(sensitive pin: int, id: string) {
  var label: string = \"\"
  var leaked: string = \"\"
  var kee: Key
  var blob: json = \"\"

  initial Fetching
  state Fetching {
    invoke fetch(id) {
      done -> Open  set kee = result, label = result.label, leaked = result.value, blob = result
    }
  }
  state Open {
    invoke Locker(pin) as locker { done -> Shut }
    on locker.Opened set leaked = event.code
  }
  final state Shut {}
}
";

/// The `sensitive` marks of another package's declarations reach the IR, and the checks,
/// whether the package is read from its sources or from its compiled IR.
#[test]
fn sensitive_marks_cross_packages() {
    let mut dep = MemoryLoader::new().with("vault.road", VAULT);
    let compiled = compile(&mut dep, "vault.road", &Options::default())
        .unwrap()
        .ir
        .unwrap();
    for loaded in [
        Loaded::Source(VAULT.into()),
        Loaded::Compiled(Box::new(compiled)),
    ] {
        let mut l =
            MemoryLoader::new()
                .with("safe.road", SAFE)
                .with_package("vault", "vault.road", loaded);
        let out = compile(&mut l, "safe.road", &Options::default()).unwrap();
        let found: Vec<(String, u32, u32)> = out
            .diagnostics
            .iter()
            .map(|d| (d.code.to_string(), d.span.start.line, d.span.start.col))
            .collect();
        assert_eq!(
            found,
            [
                ("RB0317".to_string(), 12, 70),
                ("RB0318".to_string(), 12, 91),
                ("RB0317".to_string(), 17, 35),
            ],
            "{}",
            out.render()
        );
        let ok = SAFE
            .replace(
                "leaked = result.value, blob = result",
                "blob = result.label",
            )
            .replace(
                "on locker.Opened set leaked = event.code",
                "on locker.Opened set label = \"opened\"",
            );
        let mut l = MemoryLoader::new().with("safe.road", ok).with_package(
            "vault",
            "vault.road",
            Loaded::Source(VAULT.into()),
        );
        let out = compile(&mut l, "safe.road", &Options::default()).unwrap();
        assert!(out.diagnostics.is_empty(), "{}", out.render());
        let ir = out.ir.unwrap();
        let roadbook_ir::TypeDef::Record { fields } = &ir.types["Secret"].def else {
            panic!("Secret is a record");
        };
        assert_eq!(
            fields.iter().map(|f| f.sensitive).collect::<Vec<_>>(),
            [false, true]
        );
        assert!(ir.tasks["fetch"].params[0].sensitive);
        assert!(ir.machines["Locker"].params[0].sensitive);
        assert!(ir.machines["Locker"].emits["Opened"].fields[0].sensitive);
        assert!(ir.machines["Safe"].params[0].sensitive);
        check_schema_ir(&ir);
    }
}

fn check_schema_ir(ir: &Ir) {
    let value: serde_json::Value = serde_json::from_str(&ir.to_json()).unwrap();
    let validator = common::validator();
    let errors: Vec<String> = validator
        .iter_errors(&value)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{errors:#?}");
}
