//! The generator on the shop example and on charts that use what the shop does not.

use std::path::Path;

use roadbook_compiler::{FsLoader, Loaded, Loader, MemoryLoader, Options, compile};
use roadbook_gen_ts::{GenError, PackageModule, generate};
use roadbook_ir::Ir;

fn ir(loader: &mut dyn Loader, entry: &str) -> Ir {
    let out = compile(loader, entry, &Options::default()).unwrap();
    assert!(!out.has_errors(), "{}", out.render());
    out.ir.unwrap()
}

fn gen_ok(ir: &Ir) -> String {
    let code = generate(ir, &Default::default()).unwrap();
    assert!(code.ends_with('\n') && !code.ends_with("\n\n"));
    assert_eq!(generate(ir, &Default::default()).unwrap(), code);
    code
}

/// The module with the inlined IR replaced by `<ir>`, after checking that it is the IR.
fn without_ir(ir: &Ir, code: &str) -> String {
    let json = ir.to_json();
    let literal = format!("const ir: unknown = {}", json.trim_end());
    if code.contains("const ir: unknown") {
        assert!(code.contains(&literal), "the IR is not inlined verbatim");
    }
    code.replace(&literal, "const ir: unknown = <ir>")
}

fn snapshot(name: &str, ir: &Ir) {
    let code = gen_ok(ir);
    insta::with_settings!({ prepend_module_to_snapshot => false, omit_expression => true }, {
        insta::assert_snapshot!(name, without_ir(ir, &code));
    });
}

#[test]
fn the_shop() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for file in ["types", "checkout", "place_order"] {
        let ir = ir(
            &mut FsLoader::new(&root),
            &format!("examples/shop/{file}.road"),
        );
        snapshot(&format!("shop_{file}"), &ir);
    }
}

const COMMON: &str = r#"
/// An amount in the host's own representation.
extern type Money
type Ids = string[]
type Tree { label: string, children: Tree[] }
error Boom { why: string }
error Quiet

task fetch(id: string) -> Tree ! Boom
task ping()
action log(msg: string, at: duration)
action unused(n: int)
fn label(t: Tree) -> string
fn delay(n: int) -> duration
fn ok(n: int) -> bool
fn price(m: Money) -> Money
fn wrap(s: string) -> string
fn start() -> int
fn ids() -> Ids
fn reason() -> string
fn blob() -> json
"#;

const MAIN: &str = r#"
import {
  Money, Ids, Tree, Boom, Quiet,
  fetch, ping, log, label, delay, ok, price, wrap, start, ids, reason, blob
} from "../lib/common.road"

type Basket { items: Ids, total: Money, best: Tree, extra: json }
type Wrapper { basket: Basket, maybe: string[]? , many: string?[] }
enum Mode { Fast, Slow }

machine Child() {
  /// Move on.
  event Go
  initial Done
  on Go -> Done
  final state Done {}
}

machine Parent(id: string, cost: Money) -> Basket ! Boom | Quiet {
  var n: int = start()
  var tree: Tree

  event Go { n: int }

  initial Fetching

  state Fetching {
    invoke fetch(wrap(id)) {
      done      -> Waiting  set tree = result
      fail Boom -> Failed
    }
  }

  state Waiting {
    entry do log(label(tree), delay(1))
    on Go {
      if ok(event.n) -> Pinging
    }
    after delay(n) -> Running
  }

  state Pinging {
    invoke ping() {
      done -> Running
    }
  }

  state Running {
    invoke Child() {
      done -> Finished
    }
  }

  final state Finished { output Basket { items: ids(), total: price(cost), best: tree, extra: blob() } }
  final state Failed   { fail Boom { why: reason() } }
}
"#;

fn project() -> MemoryLoader {
    MemoryLoader::new()
        .with("lib/common.road", COMMON)
        .with("app/main.road", MAIN)
}

#[test]
fn imports_recursion_extern_and_clashing_events() {
    let main = ir(&mut project(), "app/main.road");
    snapshot("main", &main);
    let common = ir(&mut project(), "lib/common.road");
    snapshot("common", &common);
}

fn error_of(files: &[(&str, &str)]) -> GenError {
    let mut loader = MemoryLoader::new();
    for (path, src) in files {
        loader.insert(path, *src);
    }
    let ir = ir(&mut loader, files[0].0);
    generate(&ir, &Default::default()).unwrap_err()
}

#[test]
fn rejects_what_it_cannot_express() {
    let e = error_of(&[("a.road", "type T { e: E, next: T? }\nerror E\n")]);
    assert!(
        matches!(&e, GenError::Recursive { types, .. } if types == &["T"]),
        "{e}"
    );
    let e = error_of(&[("a.road", "error R { next: R? }\n")]);
    assert!(matches!(e, GenError::Recursive { .. }), "{e}");
    let e = error_of(&[
        (
            "x.road",
            "import { Y } from \"./y.road\"\ntype X { y: Y? }\n",
        ),
        (
            "y.road",
            "import { X } from \"./x.road\"\ntype Y { x: X? }\n",
        ),
    ]);
    assert!(e.to_string().contains("spans several files"), "{e}");
    let e = error_of(&[(
        "c.road",
        "type MDef { a: int }\nmachine M() {\n  initial D\n  final state D {}\n}\n",
    )]);
    assert_eq!(
        e,
        GenError::Clash {
            name: "MDef".into()
        }
    );
    let e = error_of(&[("d.road", "type Schema { a: int }\n")]);
    assert_eq!(
        e,
        GenError::Clash {
            name: "Schema".into()
        }
    );
}

#[test]
fn rejects_error_fields_an_error_class_cannot_have() {
    for (field, ty) in [
        ("name", "bool"),
        ("message", "int"),
        ("stack", "string?"),
        ("name", "R"),
        ("toString", "string"),
        ("toJSON", "int"),
        ("pipe", "int"),
    ] {
        let src = format!("type R {{ x: int }}\n\nerror Weird {{ why: string, {field}: {ty} }}\n");
        let e = error_of(&[("w.road", &src)]);
        let GenError::ErrorField {
            file, line, error, ..
        } = &e
        else {
            panic!("{field}: {ty}: {e}");
        };
        assert_eq!(
            (file.as_str(), *line, error.as_str()),
            ("w.road", 3, "Weird")
        );
        let text = e.to_string();
        assert!(
            text.starts_with(&format!(
                "w.road:3: the error type `Weird` cannot have the field `{field}: {ty}` in TypeScript: "
            )),
            "{text}"
        );
    }
    // A string (or an enum, or an alias of either) is what `Error` has; other names are free.
    let ir = ir(
        &mut MemoryLoader::new().with(
            "ok.road",
            "enum Kind { A, B }\ntype Text = string\n\
             error Fine { name: Text, message: Kind, stack: string, cause: int, constructor: bool }\n",
        ),
        "ok.road",
    );
    gen_ok(&ir);
}

#[test]
fn a_file_with_nothing_to_export() {
    let ir = ir(
        &mut MemoryLoader::new().with("t.road", "task t()\n"),
        "t.road",
    );
    assert_eq!(
        gen_ok(&ir),
        "// Generated by roadbook gen from t.road. Do not edit.\n"
    );
}

const STOCK: &str = r#"
type Item { sku: string, qty: int }

machine Stock() {
  event Reserve { item: Item }
  emits Reserved { sku: string }
  initial Ready
  state Ready {
    on Reserve emit Reserved { sku: event.item.sku }
  }
}
"#;

const SHOP: &str = r#"
import { Item, Stock } from "stock/stock.road"

type Stocks = ref Stock[]

action note(stock: ref Stock)

machine Child(stock: ref Stock) {
  event Pause
  emits Reserved { sku: string }
  initial Waiting
  state Waiting {
    entry do note(stock)
    on stock.Reserved emit Reserved { sku: event.sku }
  }
}

machine Shop(stock: ref Stock, spare: ref Stock?, all: Stocks, item: Item) {
  var current: ref Stock = stock
  initial Asking
  state Asking {
    entry send current.Reserve { item: item }
    invoke Child(stock) as child {
      done -> Over
    }
    on child.Reserved send child.Pause
  }
  final state Over {}
}
"#;

#[test]
fn messaging_and_other_packages() {
    let loader = MemoryLoader::new()
        .with("shop/shop.road", SHOP)
        .with_package("stock", "stock.road", Loaded::Source(STOCK.into()));
    let shop = ir(&mut loader.clone(), "shop/shop.road");
    // A package file is imported as `Options::modules` says: by a relative path to where it
    // is, or by a specifier; otherwise by `<package>/<path>.js`.
    let at = |m: PackageModule| {
        roadbook_gen_ts::Options::default()
            .with_modules([(("stock".to_owned(), "stock.road".to_owned()), m)].into())
    };
    let mapped = at(PackageModule::Path("vendor/stock/charts/stock.road".into()));
    let code = generate(&shop, &mapped).unwrap();
    insta::with_settings!({ prepend_module_to_snapshot => false, omit_expression => true }, {
        insta::assert_snapshot!("messaging", without_ir(&shop, &code));
    });
    for (m, specifier) in [
        // Outside the project root, as an absolute `[packages]` directory can be.
        (
            PackageModule::Path("../../elsewhere/stock/stock.road".into()),
            "../../../elsewhere/stock/stock.road.js",
        ),
        (
            PackageModule::Specifier("stock/dist/stock.road.js".into()),
            "stock/dist/stock.road.js",
        ),
    ] {
        let code = generate(&shop, &at(m)).unwrap();
        assert!(
            code.contains(&format!("type StockDef }} from \"{specifier}\"")),
            "{code}"
        );
    }
    let unmapped = gen_ok(&shop);
    assert!(
        unmapped.contains("import { Item, type StockDef } from \"stock/stock.road.js\""),
        "{unmapped}"
    );
    let stock = ir(
        &mut MemoryLoader::new().with("stock.road", STOCK),
        "stock.road",
    );
    let code = gen_ok(&stock);
    assert!(code.contains("readonly emits: Reserved"), "{code}");
}

/// Charts that import each other: what a module takes from a module that may import it back
/// is read lazily, since that module may not have run yet when it runs.
#[test]
fn import_cycles_are_read_lazily() {
    let loader = MemoryLoader::new()
        .with(
            "a.road",
            "import { Bad, Flag, Id } from \"./b.road\"\nimport { Far } from \"./far.road\"\n\n\
             type T { n: int, flag: Flag?, far: Far }\ntype Ids = Id[]\n\n\
             machine A(t: T) -> T ! Bad {\n  event Poke { ids: Ids, id: Id }\n  initial X\n  \
             final state X { output t }\n}\n",
        )
        .with(
            "b.road",
            "import { T } from \"./a.road\"\n\nerror Bad { t: T }\nenum Flag { On, Off }\n\
             type Id = string\n\nmachine B() {\n  initial X\n  final state X {}\n}\n",
        )
        .with("far.road", "type Far { s: string }\nerror Unrelated\n");
    let a = gen_ok(&ir(&mut loader.clone(), "a.road"));
    for line in [
        "import { Bad, Flag, Id } from \"./b.road.js\"\n",
        "import { Far, Unrelated } from \"./far.road.js\"\n",
        "export const T = Schema.Struct({\n  n: Schema.Int,\n  \
         flag: Schema.NullOr(Schema.suspend(() => Flag)),\n  far: Far\n})\n",
        "export const Ids = Schema.Array(Schema.suspend(() => Id))\n",
        "export const Poke = Schema.TaggedStruct(\"Poke\", { ids: Ids, id: Schema.suspend(() => Id) })\n",
        "const errors = { get Bad() { return Bad }, Unrelated }\n",
    ] {
        assert!(a.contains(line), "no {line:?} in\n{a}");
    }
    let b = gen_ok(&ir(&mut loader.clone(), "b.road"));
    for line in [
        "import { T } from \"./a.road.js\"\n",
        "export class Bad extends Schema.TaggedError<Bad>()(\"Bad\", { t: Schema.suspend(() => T) }) {}\n",
        "const errors = { Bad, Unrelated }\n",
    ] {
        assert!(b.contains(line), "no {line:?} in\n{b}");
    }
    // `far.road` imports neither, so nothing is read lazily from it.
    assert!(
        b.contains("import { Unrelated } from \"./far.road.js\"\n"),
        "{b}"
    );
}

/// A module's errors map holds the error classes of the files its declarations refer to,
/// directly or not, not those of a file it imports and does not use: so it imports nothing
/// from that file, and two charts that import each other without using what they import do
/// not make their modules import each other.
#[test]
fn errors_come_from_the_files_used() {
    let loader = MemoryLoader::new()
        .with(
            "a.road",
            "import { Gone } from \"./b.road\"\n\nmachine A() {\n  initial X\n  final state X {}\n}\n",
        )
        .with("b.road", "import { A } from \"./a.road\"\n\nerror Gone\n");
    let a = gen_ok(&ir(&mut loader.clone(), "a.road"));
    assert!(
        !a.contains("\"./b.road.js\"") && a.contains("const errors = {}\n"),
        "{a}"
    );
}

/// A name that exists only as a type, a machine's definition or an alias of a type with a
/// `ref` in it, is imported with `type`: on its own as `import type`, so the module is not
/// loaded for it.
#[test]
fn types_are_imported_as_types() {
    let loader = MemoryLoader::new()
        .with(
            "w.road",
            "machine Worker() {\n  initial X\n  final state X {}\n}\n\n\
             type WorkerRef = ref Worker\ntype Job { id: string }\n",
        )
        .with(
            "only.road",
            "import { WorkerRef } from \"./w.road\"\n\n\
             machine Boss(w: WorkerRef) {\n  initial X\n  final state X {}\n}\n",
        )
        .with(
            "mixed.road",
            "import { Job, Worker, WorkerRef } from \"./w.road\"\n\n\
             machine Boss(w: WorkerRef, v: ref Worker, job: Job) {\n  initial X\n  \
             final state X {}\n}\n",
        );
    let only = gen_ok(&ir(&mut loader.clone(), "only.road"));
    assert!(
        only.contains("import type { WorkerRef } from \"./w.road.js\"\n"),
        "{only}"
    );
    let mixed = gen_ok(&ir(&mut loader.clone(), "mixed.road"));
    assert!(
        mixed.contains("import { Job, type WorkerDef, type WorkerRef } from \"./w.road.js\"\n"),
        "{mixed}"
    );
}

/// History states are never active, so the union of state ids leaves them out.
#[test]
fn history_states_are_not_states_of_the_union() {
    let ir = ir(
        &mut MemoryLoader::new().with(
            "h.road",
            "machine H() {\n  event Go\n  event Back\n  initial P\n  state P {\n    initial A\n    \
             history Last\n    deep history Deep\n    state A { on Go -> Q }\n  }\n  \
             state Q { on Back -> P.Last  on Go -> P.Deep }\n}\n",
        ),
        "h.road",
    );
    let code = gen_ok(&ir);
    assert!(
        code.contains("readonly state: \"H\" | \"H.P\" | \"H.P.A\" | \"H.Q\"\n"),
        "{code}"
    );
}
