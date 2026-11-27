//! The symbol index: each kind of name resolves to its declaration, across files too.

use roadbook_compiler::{
    Compilation, FileId, MemoryLoader, Options, SymbolId, SymbolKind, check, compile,
    compile_source,
};

const CHART: &str = r#"type Item { sku: string, qty: int }
type Id = string
enum Role { Customer, Admin }
error Declined { reason: string }
fn price(item: Item) -> int
action log(what: string)
task fetch(id: Id) -> Item ! Declined

machine Child(n: int) {
  event Pause
  emits Progress { percent: int }
  initial Running
  state Running {
    on Pause -> Done  emit Progress { percent: n }
  }
  final state Done {}
}

/// Fetch an item.
machine Parent(id: Id, peer: ref Child) -> Item ! Declined {
  var item: Item
  var role: Role = Role.Admin
  event Go { count: int }
  initial Fetching
  state Fetching {
    invoke fetch(id) {
      done          -> Running  set item = result
      fail Declined -> Failed
    }
  }
  state Running {
    initial Waiting
    invoke Child(item.qty) as child {
      done -> Finished
    }
    on child.Progress if event.percent > 50 -> Running.Waiting  do log("half")
    on Go if event.count > 1 send child.Pause  send peer.Pause
    state Waiting {}
  }
  final state Finished { output Item { sku: item.sku, qty: price(item) } }
  final state Failed   { fail Declined { reason: "no" } }
}
"#;

/// The symbol used or declared at the first occurrence of `needle`, where `^` marks the
/// name.
fn at(out: &Compilation, file: u32, needle: &str) -> SymbolId {
    let src = &out.files[file as usize].1;
    let (before, after) = needle.split_once('^').unwrap_or(("", needle));
    let offset = src
        .find(&format!("{before}{after}"))
        .unwrap_or_else(|| panic!("no `{needle}`"))
        + before.len();
    let (id, span) = out
        .index
        .at(FileId(file), offset as u32)
        .unwrap_or_else(|| panic!("nothing at `{needle}`"));
    assert!(after.starts_with(&src[span.range()]), "{needle}");
    assert_eq!(span.start.offset as usize, offset, "{needle}");
    id
}

fn name(out: &Compilation, id: SymbolId) -> String {
    out.index.qualified_name(id)
}

#[test]
fn every_kind_of_name_resolves() {
    let out = compile_source("c.road", CHART, &Options::default());
    assert!(!out.has_errors(), "{}", out.render());
    let cases: &[(&str, &str, SymbolKind)] = &[
        ("-> ^Item !", "Item", SymbolKind::Record),
        ("fetch(id: ^Id)", "Id", SymbolKind::Alias),
        ("! ^Declined\n", "Declined", SymbolKind::Error),
        ("= ^Role.Admin", "Role", SymbolKind::Enum),
        ("Role.^Admin", "Role.Admin", SymbolKind::Variant),
        ("on ^Pause", "Child.Pause", SymbolKind::Event),
        ("emit ^Progress", "Child.Progress", SymbolKind::EmittedEvent),
        ("^percent: n", "Child.Progress.percent", SymbolKind::Field),
        ("percent: ^n", "Child.n", SymbolKind::Parameter),
        ("initial ^Running", "Child.Running", SymbolKind::State),
        ("initial ^Fetching", "Parent.Fetching", SymbolKind::State),
        ("invoke ^fetch", "fetch", SymbolKind::Task),
        ("fetch(^id)", "Parent.id", SymbolKind::Parameter),
        ("Parent(^id: Id", "Parent.id", SymbolKind::Parameter),
        ("-> ^Running  set", "Parent.Running", SymbolKind::State),
        ("set ^item = result", "Parent.item", SymbolKind::Variable),
        ("fail ^Declined ->", "Declined", SymbolKind::Error),
        ("-> ^Failed", "Parent.Failed", SymbolKind::State),
        (
            "initial ^Waiting",
            "Parent.Running.Waiting",
            SymbolKind::State,
        ),
        ("invoke ^Child", "Child", SymbolKind::Machine),
        ("item.^qty", "Item.qty", SymbolKind::Field),
        (
            "on ^child.Progress",
            "Parent.Running.child",
            SymbolKind::InvokeName,
        ),
        (
            "child.^Progress",
            "Child.Progress",
            SymbolKind::EmittedEvent,
        ),
        (
            "event.^percent",
            "Child.Progress.percent",
            SymbolKind::Field,
        ),
        ("-> ^Running.Waiting", "Parent.Running", SymbolKind::State),
        (
            "Running.^Waiting",
            "Parent.Running.Waiting",
            SymbolKind::State,
        ),
        ("do ^log", "log", SymbolKind::Action),
        ("on ^Go", "Parent.Go", SymbolKind::Event),
        ("event.^count", "Parent.Go.count", SymbolKind::Field),
        (
            "send ^child.Pause",
            "Parent.Running.child",
            SymbolKind::InvokeName,
        ),
        ("child.^Pause", "Child.Pause", SymbolKind::Event),
        ("send ^peer", "Parent.peer", SymbolKind::Parameter),
        ("ref ^Child", "Child", SymbolKind::Machine),
        ("output ^Item", "Item", SymbolKind::Record),
        ("^sku: item", "Item.sku", SymbolKind::Field),
        ("item.^sku", "Item.sku", SymbolKind::Field),
        ("^price(item)", "price", SymbolKind::Function),
        ("^reason: \"no", "Declined.reason", SymbolKind::Field),
        ("fail ^Declined {", "Declined", SymbolKind::Error),
    ];
    for &(needle, want, kind) in cases {
        let id = at(&out, 0, needle);
        assert_eq!(name(&out, id), want, "`{needle}`");
        assert_eq!(out.index.symbol(id).kind, kind, "`{needle}`");
    }

    let parent = at(&out, 0, "Parent(");
    let s = out.index.symbol(parent);
    assert_eq!(
        s.signature,
        "machine Parent(id: Id, peer: ref Child) -> Item ! Declined"
    );
    assert_eq!(s.doc.as_deref(), Some("Fetch an item."));
    let running = at(&out, 0, "-> ^Running  set");
    assert_eq!(out.index.references_to(running).count(), 2);
    let role = at(&out, 0, "role");
    assert_eq!(
        out.index.symbol(role).signature,
        "var role: Role = Role.Admin"
    );
}

#[test]
fn imports_and_uses_in_other_files_point_at_the_declaration() {
    let types = "type Order { id: string }\nerror Gone\n";
    let a = "import { Order, Gone } from \"./types.road\"\n\nmachine A(o: Order) -> Order ! Gone {\n  initial Done\n  final state Done { output o }\n}\n";
    let b = "import { Order } from \"./types.road\"\nimport { A } from \"./a.road\"\n\nmachine B(o: Order) {\n  initial Run\n  state Run {\n    invoke A(o) {\n      done      -> Done\n      fail Gone -> Done\n    }\n  }\n  final state Done {}\n}\n";
    let mut files = MemoryLoader::new()
        .with("types.road", types)
        .with("a.road", a)
        .with("b.road", b);
    let roots = ["a.road".to_string(), "b.road".to_string()];
    let out = check(&mut files, &roots, &Options::default()).unwrap();
    let file = |p: &str| out.files.iter().position(|(n, _)| n == p).unwrap() as u32;
    let order = at(&out, file("types.road"), "Order");
    // Declared once, though two compilations read `types.road`.
    let decls = out
        .index
        .iter()
        .filter(|(_, s)| s.name == "Order" && s.kind == SymbolKind::Record)
        .count();
    assert_eq!(decls, 1);
    let uses: Vec<(String, String)> = out
        .index
        .references_to(order)
        .map(|s| {
            let (path, text) = &out.files[s.file.0 as usize];
            (path.clone(), text[s.range()].to_string())
        })
        .collect();
    assert_eq!(
        uses,
        [
            ("a.road".to_string(), "Order".to_string()),
            ("a.road".into(), "Order".into()),
            ("a.road".into(), "Order".into()),
            ("b.road".into(), "Order".into()),
            ("b.road".into(), "Order".into()),
        ]
    );
    assert_eq!(
        at(&out, file("b.road"), "A(o)"),
        at(&out, file("a.road"), "A(")
    );
    // `Gone` is not imported into `b.road`, but it is the error `A` declares.
    assert_eq!(
        at(&out, file("b.road"), "Gone"),
        at(&out, file("types.road"), "Gone")
    );
}

#[test]
fn a_compilation_with_errors_still_indexes_what_resolves() {
    let src = "machine M() {\n  var n: int\n  initial A\n  state A { on Go -> B  set n = 1 }\n}\n";
    let out = compile(
        &mut MemoryLoader::new().with("m.road", src),
        "m.road",
        &Options::default(),
    )
    .unwrap();
    assert!(out.has_errors());
    let n = at(&out, 0, "n = 1");
    assert_eq!(out.index.symbol(n).kind, SymbolKind::Variable);
    assert!(
        out.index
            .at(FileId(0), src.find("Go").unwrap() as u32)
            .is_none()
    );
}

#[test]
fn signatures_show_sensitive_marks() {
    let src = "type Login { user: string, sensitive password: string }\n\
               machine M(sensitive pin: int) {\n  sensitive var token: string = \"\"\n  initial A\n  final state A {}\n}\n";
    let out = compile_source("s.road", src, &Options::default());
    let sig = |needle: &str| out.index.symbol(at(&out, 0, needle)).signature.clone();
    assert_eq!(
        sig("Login {"),
        "type Login { user: string, sensitive password: string }"
    );
    assert_eq!(sig("password"), "sensitive password: string");
    assert_eq!(sig("M("), "machine M(sensitive pin: int)");
    assert_eq!(sig("token"), "sensitive var token: string = \"\"");
}

/// Type parameters, names in type arguments and maps, and fields read through an instance of
/// a generic type resolve to their declarations.
#[test]
fn generic_names_resolve() {
    let src = "type UserId = string\ntype Page<T> { items: T[], next: string? }\n\
               type Dict<V> = map<UserId, V>\nfn ints() -> int[]\n\n\
               machine M(p: Page<Dict<int>>) -> Page<int> {\n  initial Done\n  \
               final state Done { output Page { items: ints(), next: p.next } }\n}\n";
    let out = compile_source("g.road", src, &Options::default());
    assert!(!out.has_errors(), "{}", out.render());
    for (needle, want, kind) in [
        ("Page<^T>", "Page.T", SymbolKind::TypeParameter),
        ("items: ^T[]", "Page.T", SymbolKind::TypeParameter),
        ("Dict<^V>", "Dict.V", SymbolKind::TypeParameter),
        ("map<^UserId", "UserId", SymbolKind::Alias),
        ("UserId, ^V>", "Dict.V", SymbolKind::TypeParameter),
        ("p: ^Page<Dict", "Page", SymbolKind::Record),
        ("Page<^Dict<int>>", "Dict", SymbolKind::Alias),
        ("output ^Page", "Page", SymbolKind::Record),
        ("^items: ints", "Page.items", SymbolKind::Field),
        ("p.^next", "Page.next", SymbolKind::Field),
    ] {
        let id = at(&out, 0, needle);
        assert_eq!(name(&out, id), want, "`{needle}`");
        assert_eq!(out.index.symbol(id).kind, kind, "`{needle}`");
    }
    let page = at(&out, 0, "type ^Page");
    assert_eq!(
        out.index.symbol(page).signature,
        "type Page<T> { items: T[], next: string? }"
    );
    let t = at(&out, 0, "Page<^T>");
    assert_eq!(out.index.references_to(t).count(), 1);
}

#[test]
fn calls_resolve_to_their_declaration() {
    let src = r#"error Busy

machine Peer() {
  /// Ask how many.
  call count(of: string) -> int ! Busy
  var pending: call count
  var count: int = 0
  initial A
  state A {
    on count -> B  set pending = call
  }
  state B {
    always -> A  reply pending done event2
  }
}

machine User(peer: ref Peer) {
  initial A
  state A {
    invoke peer.count("x") {
      done            -> B
      fail Busy       -> B
      fail Unanswered -> B
    }
  }
  final state B {}
}
"#
    .replace("event2", "count");
    let out = compile_source("c.road", &src, &Options::default());
    assert!(!out.has_errors(), "{}", out.render());
    let cases: &[(&str, &str, SymbolKind)] = &[
        ("call ^count(of", "Peer.count", SymbolKind::Call),
        ("count(^of: string", "Peer.count.of", SymbolKind::Parameter),
        ("var pending: call ^count", "Peer.count", SymbolKind::Call),
        ("on ^count", "Peer.count", SymbolKind::Call),
        ("var ^count: int", "Peer.count", SymbolKind::Variable),
        ("done ^count", "Peer.count", SymbolKind::Variable),
        ("reply ^pending", "Peer.pending", SymbolKind::Variable),
        ("invoke ^peer.count", "User.peer", SymbolKind::Parameter),
        ("peer.^count(\"x\")", "Peer.count", SymbolKind::Call),
    ];
    for &(needle, want, kind) in cases {
        let id = at(&out, 0, needle);
        assert_eq!(name(&out, id), want, "`{needle}`");
        assert_eq!(out.index.symbol(id).kind, kind, "`{needle}`");
    }
    let call = at(&out, 0, "call ^count(of");
    let s = out.index.symbol(call);
    assert_eq!(s.signature, "call count(of: string) -> int ! Busy");
    assert_eq!(s.doc.as_deref(), Some("Ask how many."));
    assert_eq!(out.index.references_to(call).count(), 3);
}
