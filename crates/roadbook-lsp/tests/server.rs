//! The server driven over JSON-RPC messages: diagnostics, navigation, rename, hover, completion,
//! formatting, outline and quick fixes, on small charts and on copies of the examples.

mod common;

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use common::{Client, Project, apply, at, position, slice};
use roadbook_compiler::{MemoryLoader, Options};
use serde_json::{Value, json};

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

fn error_free(files: &[(&str, &str)]) {
    let mut loader = MemoryLoader::new();
    for (p, t) in files {
        loader.insert(p, *t);
    }
    let roots: Vec<String> = files.iter().map(|(p, _)| p.to_string()).collect();
    let out = roadbook_compiler::check(&mut loader, &roots, &Options::default()).unwrap();
    assert!(!out.has_errors(), "{}", out.render());
}

#[test]
fn diagnostics_are_published_and_cleared() {
    let bad = "machine M() {\n  initial A\n  state A { always -> Dne }\n  state A {}\n}\n";
    let p = Project::new("diagnostics", &[("m.road", bad)]);
    let uri = p.uri("m.road");
    let mut c = Client::start(json!({}));
    assert_eq!(c.init["capabilities"]["positionEncoding"], "utf-16");
    c.open(&uri, bad);
    let diags = c.diagnostics(&uri);
    let codes: Vec<&str> = diags.iter().map(|d| d["code"].as_str().unwrap()).collect();
    assert_eq!(codes, ["RB0201", "RB0108"], "{diags:#?}");
    assert_eq!(slice(bad, &diags[0]["range"]), "Dne");
    assert_eq!(diags[0]["severity"], 1);
    assert_eq!(diags[0]["source"], "roadbook");
    // The secondary label is related information.
    let related = &diags[1]["relatedInformation"][0];
    assert_eq!(related["message"], "first declared here");
    assert_eq!(related["location"]["uri"], uri.as_str());
    assert_eq!(slice(bad, &related["location"]["range"]), "A");

    let good = "machine M() {\n  initial A\n  state A { always -> B }\n  final state B {}\n}\n";
    c.change(&uri, 2, good);
    assert_eq!(c.diagnostics(&uri), Vec::<Value>::new());
    c.shutdown();
}

#[test]
fn a_failing_check_is_a_diagnostic_with_its_path() {
    let src = "machine M() {\n  event Go\n  initial A\n  state A { on Go -> B }\n  final state B {}\n  check never B\n}\n";
    let p = Project::new("checks", &[("m.road", src)]);
    let uri = p.uri("m.road");
    let mut c = Client::start(json!({}));
    c.open(&uri, src);
    let diags = c.diagnostics(&uri);
    assert_eq!(diags.len(), 1, "{diags:#?}");
    assert_eq!(diags[0]["code"], "RB0501");
    assert_eq!(diags[0]["severity"], 1);
    assert_eq!(slice(src, &diags[0]["range"]), "check never B");
    // The counterexample's transition, as related information.
    let related = &diags[0]["relatedInformation"][0];
    assert_eq!(related["message"], "1. Go -> B");
    assert_eq!(slice(src, &related["location"]["range"]), "on Go -> B");
    let fixed = src.replace("check never B", "check reachable B");
    c.change(&uri, 2, &fixed);
    assert_eq!(c.diagnostics(&uri), Vec::<Value>::new());
    c.shutdown();
}

#[test]
fn a_change_rechecks_the_files_that_import_it() {
    let types = "type Order { id: string }\n";
    let a = "import { Order } from \"./types.road\"\n\nmachine A(o: Order) -> Order {\n  initial Done\n  final state Done { output o }\n}\n";
    let p = Project::new("importers", &[("types.road", types), ("a.road", a)]);
    let (tu, au) = (p.uri("types.road"), p.uri("a.road"));
    let mut c = Client::start(json!({}));
    c.open(&tu, types);
    assert!(c.diagnostics(&tu).is_empty());
    assert!(c.diagnostics(&au).is_empty());
    // `a.road` is not open, but it imports the changed file.
    c.change(&tu, 2, "type Ordr { id: string }\n");
    let diags = c.diagnostics(&au);
    assert_eq!(diags.len(), 1, "{diags:#?}");
    assert_eq!(diags[0]["code"], "RB0112");
    // A quick fix is offered for the unopened file too, as an edit to it.
    let actions = c
        .request(
            "textDocument/codeAction",
            json!({ "textDocument": { "uri": au }, "range": diags[0]["range"], "context": { "diagnostics": diags } }),
        )
        .unwrap();
    assert_eq!(actions[0]["title"], "import `Ordr`");
    let fixed = apply(a, &actions[0]["edit"]["changes"][au.as_str()]);
    assert!(fixed.starts_with("import { Ordr }"), "{fixed}");
    c.change(&tu, 3, types);
    assert!(c.diagnostics(&au).is_empty());
    c.shutdown();
}

#[test]
fn a_quick_fix_applies_the_compilers_fix() {
    let text = "machine M() {\n  initial Waiting\n  state Waiting { always -> Dnoe }\n  final state Done {}\n}\n";
    let p = Project::new("fix", &[("m.road", text)]);
    let uri = p.uri("m.road");
    let mut c = Client::start(json!({}));
    c.open(&uri, text);
    let diags = c.diagnostics(&uri);
    let range = diags[0]["range"].clone();
    let actions = c
        .request(
            "textDocument/codeAction",
            json!({ "textDocument": { "uri": uri }, "range": range, "context": { "diagnostics": diags } }),
        )
        .unwrap();
    let action = &actions[0];
    assert_eq!(action["title"], "replace with `Done`");
    assert_eq!(action["kind"], "quickfix");
    assert_eq!(action["isPreferred"], true);
    assert_eq!(action["diagnostics"][0]["code"], "RB0201");
    let fixed = apply(text, &action["edit"]["changes"][uri.as_str()]);
    assert!(fixed.contains("always -> Done"));
    error_free(&[("m.road", &fixed)]);
    // Nothing to fix elsewhere.
    let none = c
        .request(
            "textDocument/codeAction",
            json!({ "textDocument": { "uri": uri }, "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 1 } }, "context": { "diagnostics": [] } }),
        )
        .unwrap();
    assert_eq!(none, json!([]));
    c.shutdown();
}

/// `(use, declaration)`: going to the definition from the use lands on the declaration.
const DEFINITIONS: &[(&str, &str)] = &[
    ("-> ^Item !", "type ^Item"),
    ("fetch(id: ^Id)", "type ^Id"),
    ("! ^Declined\n", "error ^Declined"),
    ("= ^Role.Admin", "enum ^Role"),
    ("Role.^Admin", "Customer, ^Admin"),
    ("on ^Pause", "event ^Pause"),
    ("emit ^Progress", "emits ^Progress"),
    ("^percent: n", "^percent: int"),
    ("percent: ^n", "Child(^n: int)"),
    ("initial ^Running", "state ^Running {\n    on Pause"),
    ("invoke ^fetch", "task ^fetch"),
    ("fetch(^id)", "Parent(^id: Id"),
    ("-> ^Running  set", "state ^Running {\n    initial"),
    ("set ^item = result", "var ^item"),
    ("fail ^Declined ->", "error ^Declined"),
    ("initial ^Waiting", "state ^Waiting"),
    ("invoke ^Child", "machine ^Child"),
    ("item.^qty", "^qty: int"),
    ("on ^child.Progress", "as ^child"),
    ("child.^Progress", "emits ^Progress"),
    ("event.^percent", "^percent: int"),
    ("-> ^Running.Waiting", "state ^Running {\n    initial"),
    ("Running.^Waiting", "state ^Waiting"),
    ("do ^log", "action ^log"),
    ("on ^Go", "event ^Go"),
    ("event.^count", "^count: int"),
    ("child.^Pause", "event ^Pause"),
    ("send ^peer", "^peer: ref"),
    ("ref ^Child", "machine ^Child"),
    ("output ^Item", "type ^Item"),
    ("^sku: item", "^sku: string"),
    ("^price(item)", "fn ^price"),
    ("^reason: \"no", "^reason: string"),
];

#[test]
fn definition_and_references_for_every_kind_of_name() {
    let p = Project::new("kinds", &[("c.road", CHART)]);
    let uri = p.uri("c.road");
    let mut c = Client::start(json!({}));
    c.open(&uri, CHART);
    assert!(c.diagnostics(&uri).iter().all(|d| d["severity"] == 2));
    for (from, to) in DEFINITIONS {
        let loc = c
            .request("textDocument/definition", at(&uri, CHART, from))
            .unwrap();
        assert_eq!(loc["uri"], uri.as_str(), "{from}");
        assert_eq!(loc["range"]["start"], position(CHART, to), "{from} -> {to}");
    }
    // References to the state `Parent.Running`: `-> Running` and `-> Running.Waiting`,
    // and the declaration if asked for.
    let refs = |c: &mut Client, decl: bool| {
        let mut params = at(&uri, CHART, "state ^Running {\n    initial");
        params["context"] = json!({ "includeDeclaration": decl });
        let found = c.request("textDocument/references", params).unwrap();
        found
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["range"]["start"]["line"].as_u64().unwrap() + 1)
            .collect::<Vec<_>>()
    };
    assert_eq!(refs(&mut c, false), [27, 36]);
    assert_eq!(refs(&mut c, true), [31, 27, 36]);
    // Nothing to find on a keyword.
    let none = c
        .request(
            "textDocument/definition",
            at(&uri, CHART, "^machine Parent"),
        )
        .unwrap();
    assert_eq!(none, Value::Null);
    c.shutdown();
}

/// Renames the name at `needle` and returns the new text, which must compile.
fn rename(c: &mut Client, uri: &str, text: &str, needle: &str, new: &str) -> String {
    let prepared = c
        .request("textDocument/prepareRename", at(uri, text, needle))
        .unwrap();
    assert!(prepared["placeholder"].is_string(), "{needle}: {prepared}");
    let mut params = at(uri, text, needle);
    params["newName"] = json!(new);
    let edit = c
        .request("textDocument/rename", params)
        .unwrap_or_else(|e| panic!("{needle} -> {new}: {e}"));
    let changes = edit["changes"].as_object().unwrap();
    assert_eq!(changes.len(), 1);
    let out = apply(text, &changes[uri]);
    error_free(&[("c.road", &out)]);
    out
}

fn rename_error(c: &mut Client, uri: &str, text: &str, needle: &str, new: &str) -> String {
    let mut params = at(uri, text, needle);
    params["newName"] = json!(new);
    c.request("textDocument/rename", params)
        .expect_err(&format!("{needle} -> {new}"))
}

#[test]
fn rename_each_kind_of_name() {
    let p = Project::new("rename", &[("c.road", CHART)]);
    let uri = p.uri("c.road");
    let mut c = Client::start(json!({}));
    c.open(&uri, CHART);
    c.diagnostics(&uri);
    let cases: &[(&str, &str, &[&str])] = &[
        (
            "state ^Running {\n    initial",
            "Active",
            &[
                "-> Active  set",
                "-> Active.Waiting",
                "state Active {\n    initial",
            ],
        ),
        (
            "Active.^Waiting",
            "Idle",
            &["initial Idle", "Active.Idle", "state Idle {}"],
        ),
        (
            "var ^item",
            "thing",
            &[
                "set thing = result",
                "Child(thing.qty)",
                "sku: thing.sku",
                "price(thing)",
            ],
        ),
        (
            "event ^Pause",
            "Stop",
            &["on Stop ->", "send child.Stop  send peer.Stop"],
        ),
        (
            "emits ^Progress",
            "Advanced",
            &["emits Advanced", "emit Advanced {", "on child.Advanced"],
        ),
        (
            "^percent: int",
            "pct",
            &[
                "emits Advanced { pct: int }",
                "{ pct: n }",
                "event.pct > 50",
            ],
        ),
        (
            "type ^Item",
            "Thing",
            &["-> Thing !", "output Thing {", "price(item: Thing)"],
        ),
        (
            "^sku: string",
            "code",
            &["code: string", "code: thing.code"],
        ),
        ("Customer, ^Admin", "Owner", &["Role.Owner"]),
        ("as ^child", "kid", &["on kid.Advanced", "send kid.Stop"]),
        ("Child(^n: int)", "count", &["{ pct: count }"]),
        (
            "machine ^Child",
            "Worker",
            &["ref Worker", "invoke Worker("],
        ),
        (
            "error ^Declined",
            "Refused",
            &["! Refused", "fail Refused ->", "fail Refused {"],
        ),
        ("task ^fetch", "load", &["invoke load(id)"]),
    ];
    let mut text = CHART.to_string();
    for (version, (needle, new, expect)) in cases.iter().enumerate() {
        text = rename(&mut c, &uri, &text, needle, new);
        for e in *expect {
            assert!(
                text.contains(e),
                "after renaming to {new}, no `{e}` in\n{text}"
            );
        }
        c.change(&uri, version as i32 + 2, &text);
    }

    // Invalid names, clashes, and names that would change what another name means.
    let why = rename_error(&mut c, &uri, &text, "state ^Active", "active");
    assert!(why.contains("upper-case"), "{why}");
    let why = rename_error(&mut c, &uri, &text, "var ^thing", "on");
    assert!(why.contains("reserved"), "{why}");
    let why = rename_error(&mut c, &uri, &text, "var ^thing", "my_thing");
    assert!(why.contains("`_`"), "{why}");
    let why = rename_error(&mut c, &uri, &text, "var ^thing", "role");
    assert!(
        why.contains("already the name of a variable `Parent.role`"),
        "{why}"
    );
    let why = rename_error(&mut c, &uri, &text, "state ^Fetching", "Failed");
    assert!(
        why.contains("already the name of a state `Parent.Failed`"),
        "{why}"
    );
    let why = rename_error(&mut c, &uri, &text, "type ^Thing", "Role");
    assert!(why.contains("would cause an error"), "{why}");
    let why = rename_error(&mut c, &uri, &text, "event ^Go", "Thing");
    assert!(
        why.contains("already the name of a record type `Thing`"),
        "{why}"
    );
    c.shutdown();
}

#[test]
fn states_named_in_checks_are_uses() {
    let text = "machine M() {\n  event Go\n  initial Idle\n  state Idle { on Go -> Applying }\n  state Applying {\n    initial Recording\n    state Recording { on Go -> Done }\n  }\n  final state Done {}\n  check reachable Applying.Recording\n  check Done only after Applying.Recording\n  check never Idle and Applying\n}\n";
    let p = Project::new("checks", &[("c.road", text)]);
    let uri = p.uri("c.road");
    let mut c = Client::start(json!({}));
    c.open(&uri, text);
    assert!(c.diagnostics(&uri).is_empty());
    // Go to definition from a check.
    let loc = c
        .request(
            "textDocument/definition",
            at(&uri, text, "check reachable Applying.^Recording"),
        )
        .unwrap();
    assert_eq!(loc["range"]["start"], position(text, "state ^Recording"));
    // References include the checks.
    let mut params = at(&uri, text, "state ^Recording");
    params["context"] = json!({ "includeDeclaration": false });
    let found = c.request("textDocument/references", params).unwrap();
    let lines: Vec<u64> = found
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["range"]["start"]["line"].as_u64().unwrap() + 1)
        .collect();
    assert_eq!(lines, [6, 10, 11]);
    // Renaming a state renames it in the checks, from its declaration or from a check.
    let text = rename(&mut c, &uri, text, "state ^Recording", "Logging");
    assert!(
        text.contains("check reachable Applying.Logging\n"),
        "{text}"
    );
    assert!(
        text.contains("check Done only after Applying.Logging\n"),
        "{text}"
    );
    c.change(&uri, 2, &text);
    let text = rename(
        &mut c,
        &uri,
        &text,
        "check never Idle and ^Applying",
        "Paying",
    );
    for e in [
        "-> Paying }",
        "state Paying {",
        "check reachable Paying.Logging",
        "after Paying.Logging",
        "and Paying\n",
    ] {
        assert!(text.contains(e), "no `{e}` in\n{text}");
    }
    c.shutdown();
}

#[test]
fn rename_refuses_to_capture_a_target() {
    // `-> Out` in `P.A` means the top-level `Out`; a sibling of `A` named `Out` would take it.
    let text = "machine M() {\n  initial P\n  state P {\n    initial A\n    state A { always -> Out }\n    state X {}\n  }\n  final state Out {}\n}\n";
    let p = Project::new("capture", &[("c.road", text)]);
    let uri = p.uri("c.road");
    let mut c = Client::start(json!({}));
    c.open(&uri, text);
    c.diagnostics(&uri);
    let why = rename_error(&mut c, &uri, text, "state ^X", "Out");
    assert!(why.contains("would change what names refer to"), "{why}");
    assert!(why.contains("`M.Out`"), "{why}");
    c.shutdown();
}

#[test]
fn the_shop_across_files() {
    let p = Project::copy("shop", "examples/shop");
    let files = ["types.road", "checkout.road", "place_order.road"];
    let texts: BTreeMap<&str, String> = files.iter().map(|f| (*f, p.read(f))).collect();
    let uri = |f: &str| p.uri(f);
    let mut c = Client::start(json!({}));
    let order = &texts["place_order.road"];
    c.open(&uri("place_order.road"), order);
    for f in files {
        assert!(c.diagnostics(&uri(f)).is_empty(), "{f}");
    }

    // A machine invoked in another file, and an imported type.
    let loc = c
        .request(
            "textDocument/definition",
            at(&uri("place_order.road"), order, "invoke ^Checkout"),
        )
        .unwrap();
    assert_eq!(loc["uri"], uri("checkout.road").as_str());
    assert_eq!(slice(&texts["checkout.road"], &loc["range"]), "Checkout");
    let loc = c
        .request(
            "textDocument/definition",
            at(&uri("place_order.road"), order, "import {\n  ^OrderDraft"),
        )
        .unwrap();
    assert_eq!(loc["uri"], uri("types.road").as_str());
    assert_eq!(
        loc["range"]["start"],
        position(&texts["types.road"], "type ^OrderDraft")
    );

    // Every use of `CardDeclined`, in all three files.
    let mut params = at(
        &uri("types.road"),
        &texts["types.road"],
        "error ^CardDeclined",
    );
    params["context"] = json!({ "includeDeclaration": false });
    let refs = c.request("textDocument/references", params).unwrap();
    let mut by_file: BTreeMap<String, usize> = BTreeMap::new();
    for r in refs.as_array().unwrap() {
        let file = r["uri"]
            .as_str()
            .unwrap()
            .rsplit('/')
            .next()
            .unwrap()
            .to_string();
        *by_file.entry(file).or_default() += 1;
    }
    assert_eq!(
        by_file,
        BTreeMap::from([
            ("checkout.road".to_string(), 4),
            ("place_order.road".to_string(), 2),
            ("types.road".to_string(), 1),
        ])
    );

    // Rename it everywhere; the result compiles.
    let mut params = at(
        &uri("checkout.road"),
        &texts["checkout.road"],
        "fail ^CardDeclined",
    );
    params["newName"] = json!("CardRefused");
    let edit = c.request("textDocument/rename", params).unwrap();
    let changes = edit["changes"].as_object().unwrap();
    assert_eq!(changes.len(), 3);
    let renamed: Vec<(&str, String)> = files
        .iter()
        .map(|f| (*f, apply(&texts[f], &changes[uri(f).as_str()])))
        .collect();
    for (f, t) in &renamed {
        assert!(!t.contains("CardDeclined"), "{f}");
    }
    let refs: Vec<(&str, &str)> = renamed.iter().map(|(f, t)| (*f, t.as_str())).collect();
    error_free(&refs);

    // Hover: the signature and the doc comment.
    let hover = c
        .request(
            "textDocument/hover",
            at(&uri("place_order.road"), order, "invoke ^Checkout"),
        )
        .unwrap();
    let value = hover["contents"]["value"].as_str().unwrap();
    assert!(
        value.starts_with("```roadbook\nmachine Checkout(order: Order, user: User) -> Receipt ! CardDeclined | GatewayUnavailable\n```"),
        "{value}"
    );
    assert!(value.contains("machine in `checkout.road`"), "{value}");
    assert!(value.contains("Charge the customer's card"), "{value}");
    assert_eq!(slice(order, &hover["range"]), "Checkout");
    let hover = c
        .request(
            "textDocument/hover",
            at(&uri("place_order.road"), order, "-> ^Pricing"),
        )
        .unwrap();
    let value = hover["contents"]["value"].as_str().unwrap();
    assert!(
        value.contains("state PlaceOrder.Processing.Pricing"),
        "{value}"
    );
    assert!(
        value.contains("state of `PlaceOrder.Processing`"),
        "{value}"
    );
    assert!(value.contains("Apply the user's prices"), "{value}");

    // The outline of a file.
    let symbols = c
        .request(
            "textDocument/documentSymbol",
            json!({ "textDocument": { "uri": uri("place_order.road") } }),
        )
        .unwrap();
    let machine = &symbols[0];
    assert_eq!(machine["name"], "PlaceOrder");
    let names: Vec<&str> = machine["children"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names[..8],
        [
            "token",
            "draft",
            "user",
            "order",
            "receipt",
            "issues",
            "Cancel",
            "Authenticating"
        ]
    );
    let processing = machine["children"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "Processing")
        .unwrap();
    assert_eq!(processing["children"][0]["name"], "Validating");
    c.shutdown();
}

#[test]
fn formatting_is_the_canonical_formatter() {
    let messy = "machine M()   {\ninitial A\n      final state A {   }\n}";
    let p = Project::new("format", &[("m.road", messy)]);
    let uri = p.uri("m.road");
    let mut c = Client::start(json!({}));
    c.open(&uri, messy);
    let edits = c
        .request("textDocument/formatting", json!({ "textDocument": { "uri": uri }, "options": { "tabSize": 2, "insertSpaces": true } }))
        .unwrap();
    let formatted = apply(messy, &edits);
    assert_eq!(formatted, roadbook_syntax::format(messy).unwrap());
    c.change(&uri, 2, &formatted);
    let edits = c
        .request("textDocument/formatting", json!({ "textDocument": { "uri": uri }, "options": { "tabSize": 2, "insertSpaces": true } }))
        .unwrap();
    assert_eq!(edits, json!([]));
    // A file with syntax errors is left alone.
    c.change(&uri, 3, "machine M( {");
    let edits = c
        .request("textDocument/formatting", json!({ "textDocument": { "uri": uri }, "options": { "tabSize": 2, "insertSpaces": true } }))
        .unwrap();
    assert_eq!(edits, Value::Null);
    c.shutdown();
}

const UNICODE: &str = "action say(what: string, n: int)\n\nmachine M() {\n  var n: int = 1\n  initial A\n  /// Ünïcödé 𝄞 docs\n  state A { entry do say(\"𝄞é𝄞\", n)  do say(\"ü\", nn) }\n}\n";

#[test]
fn positions_count_utf16_code_units() {
    let p = Project::new("utf16", &[("u.road", UNICODE)]);
    let uri = p.uri("u.road");
    let mut c = Client::start(json!({ "general": { "positionEncodings": ["utf-16"] } }));
    c.open(&uri, UNICODE);
    let diags = c.diagnostics(&uri);
    let unknown = diags.iter().find(|d| d["code"] == "RB0105").unwrap();
    assert_eq!(slice(UNICODE, &unknown["range"]), "nn");
    // `n` after a string with two astral characters: two more UTF-16 code units than
    // characters.
    let params = at(&uri, UNICODE, "\", ^n)");
    let line = UNICODE.lines().nth(6).unwrap();
    let chars = line[..line.find("\", n)").unwrap() + 3].chars().count();
    assert_eq!(params["position"]["character"], chars + 2);
    let loc = c.request("textDocument/definition", params).unwrap();
    assert_eq!(slice(UNICODE, &loc["range"]), "n");
    assert_eq!(loc["range"]["start"], position(UNICODE, "var ^n"));
    c.shutdown();
}

#[test]
fn positions_count_bytes_when_the_client_offers_utf8() {
    let p = Project::new("utf8", &[("u.road", UNICODE)]);
    let uri = p.uri("u.road");
    let mut c = Client::start(json!({ "general": { "positionEncodings": ["utf-8", "utf-16"] } }));
    assert_eq!(c.init["capabilities"]["positionEncoding"], "utf-8");
    c.open(&uri, UNICODE);
    let diags = c.diagnostics(&uri);
    let unknown = diags.iter().find(|d| d["code"] == "RB0105").unwrap();
    let line = UNICODE.lines().nth(6).unwrap();
    assert_eq!(
        unknown["range"]["start"]["character"],
        line.find("nn").unwrap()
    );
    let byte = line.find("\", n)").unwrap() + 3;
    let loc = c
        .request(
            "textDocument/definition",
            json!({ "textDocument": { "uri": uri }, "position": { "line": 6, "character": byte } }),
        )
        .unwrap();
    assert_eq!(loc["range"]["start"], json!({ "line": 3, "character": 6 }));
    c.shutdown();
}

#[test]
fn a_change_to_the_large_example_is_answered_quickly() {
    let p = Project::copy("large", "examples/large");
    let uri = p.uri("ticket.road");
    let text = p.read("ticket.road");
    let mut c = Client::start(json!({}));
    c.open(&uri, &text);
    assert!(c.diagnostics(&uri).is_empty());
    let started = Instant::now();
    let edited = text.replacen(
        "var reopens:          int = 0",
        "var reopens:          int = 1",
        1,
    );
    assert_ne!(edited, text);
    c.change(&uri, 2, &edited);
    assert!(c.diagnostics(&uri).is_empty());
    let loc = c
        .request(
            "textDocument/definition",
            at(&uri, &edited, "event ^Assign"),
        )
        .unwrap();
    assert_eq!(loc["uri"], uri.as_str());
    let took = started.elapsed();
    // Generous, for a debug build on a busy machine; a release build takes milliseconds.
    assert!(took < Duration::from_secs(3), "{took:?}");
    c.shutdown();
}

const BASE: &str = "type Base { id: string }\n";
const LIB: &str = "import { Base } from \"/sub/base.road\"\n\nfn helper(b: Base) -> string\n\nmachine Lib(b: Base) -> Base {\n  initial Done\n  final state Done { output b }\n}\n";
const MAIN: &str = "import { Lib } from \"pkg/lib.road\"\nimport { Base } from \"pkg/sub/base.road\"\n\nmachine Main(b: Base) -> Base {\n  initial Run\n  state Run {\n    invoke Lib(b) {\n      done -> Done\n    }\n  }\n  final state Done { output b }\n}\n";

fn with_package(name: &str) -> Project {
    Project::new(
        name,
        &[
            ("main.road", MAIN),
            ("node_modules/pkg/lib.road", LIB),
            ("node_modules/pkg/sub/base.road", BASE),
        ],
    )
}

#[test]
fn a_file_of_a_package_is_checked_as_the_package_s() {
    let p = with_package("package");
    let (main, lib) = (p.uri("main.road"), p.uri("node_modules/pkg/lib.road"));
    let mut c = Client::start(json!({}));
    c.open(&lib, LIB);
    // Its `/` import is from the package's root, and its unused function is not reported:
    // only errors are, in another package's files, as with `roadbook check`.
    assert_eq!(c.diagnostics(&lib), Vec::<Value>::new());
    assert_eq!(c.diagnostics(&main), Vec::<Value>::new());
    // Nothing is said about other files, such as a `sub/base.road` of the project.
    let others = c.published();
    assert!(
        others.iter().all(|u| [&lib, &main].contains(&u)),
        "{others:?}"
    );
    // An error in it is reported, by compiling the project's file that imports it.
    let broken = LIB.replace("output b", "output c");
    c.change(&lib, 2, &broken);
    let diags = c.diagnostics(&lib);
    assert_eq!(diags.len(), 1, "{diags:#?}");
    assert_eq!(diags[0]["severity"], 1);
    // Go to definition from the package's file leads into the package.
    let loc = c
        .request(
            "textDocument/definition",
            at(&lib, &broken, "(b: ^Base) -> Base {"),
        )
        .unwrap();
    assert_eq!(loc["uri"], p.uri("node_modules/pkg/sub/base.road"));
    c.change(&lib, 3, LIB);
    assert_eq!(c.diagnostics(&lib), Vec::<Value>::new());

    // A rename that would edit the package's files is refused.
    c.open(&main, MAIN);
    c.forget();
    let why = rename_error(&mut c, &main, MAIN, "Main(b: ^Base)", "Thing");
    assert!(why.contains("a file of another package"), "{why}");
    assert!(why.contains("node_modules/pkg/sub/base.road"), "{why}");
    let why = rename_error(&mut c, &lib, LIB, "fn ^helper", "assist");
    assert!(why.contains("a file of another package"), "{why}");
    // A name of the project's own is renamed as usual.
    let renamed = rename_edits(&mut c, &main, MAIN, "state ^Run", "Running");
    assert_eq!(renamed.as_object().unwrap().len(), 1);
    c.shutdown();
}

/// The `changes` of a rename's workspace edit.
fn rename_edits(c: &mut Client, uri: &str, text: &str, needle: &str, new: &str) -> Value {
    let mut params = at(uri, text, needle);
    params["newName"] = json!(new);
    let edit = c
        .request("textDocument/rename", params)
        .unwrap_or_else(|e| panic!("{needle} -> {new}: {e}"));
    edit["changes"].clone()
}

#[test]
fn as_names_of_states_side_by_side_may_be_the_same() {
    let text = "machine Child() {\n  event Stop\n  initial A\n  state A { on Stop -> B }\n  final state B {}\n}\n\nmachine M() {\n  event Go\n  initial P\n  state P {\n    invoke Child() as first {\n      done -> Q\n    }\n    on Go send first.Stop\n  }\n  state Q {\n    invoke Child() as second {\n      done -> R\n    }\n    on Go send second.Stop\n    initial Inner\n    state Inner {\n      invoke Child() as inner {\n        done -> Q\n      }\n    }\n  }\n  final state R {}\n}\n";
    let p = Project::new("as-names", &[("c.road", text)]);
    let uri = p.uri("c.road");
    let mut c = Client::start(json!({}));
    c.open(&uri, text);
    assert_eq!(c.diagnostics(&uri), Vec::<Value>::new());
    // `P` and `Q` are side by side: the compiler takes the same name in both.
    let out = rename(&mut c, &uri, text, "as ^second", "first");
    assert!(out.contains("send first.Stop\n    initial Inner"), "{out}");
    // `Q` contains `Q.Inner`: an `as` name cannot be one of an `as` around it.
    let why = rename_error(&mut c, &uri, text, "as ^inner", "second");
    assert!(why.contains("already the name of"), "{why}");
    c.shutdown();
}

const HUB: &str = r#"error Lost { reason: string }
action log(what: string)

machine Session(name: string) -> int ! Lost {
  event Notify { text: string }
  emits Typing { key: string }
  initial Open
  state Open {
    on Notify -> Left  emit Typing { key: event.text }
  }
  final state Left { output 1 }
}

machine Hub(greeting: string) {
  event Join { id: string }
  event Kick { id: string }
  event Shout
  event Close
  /// The open sessions.
  children sessions: Session by string
  var open: int = 0
  initial Serving
  state Serving {
    on Join            spawn sessions[event.id] = Session(greeting)  send sessions[event.id].Notify { text: "hi" }
    on Kick            stop sessions[event.id]
    on Shout           send sessions.*.Notify { text: greeting }
    on sessions.Typing do log(key)
    on sessions.done   set open = open - 1
    on sessions.fail * do log(error.reason)
    on Close           -> Closed
  }
  final state Closed {}
}
"#;

#[test]
fn children_collections() {
    let p = Project::new("children", &[("c.road", HUB)]);
    let uri = p.uri("c.road");
    let mut c = Client::start(json!({}));
    c.open(&uri, HUB);
    assert_eq!(c.diagnostics(&uri), Vec::<Value>::new());

    // Every use of the collection goes to its declaration.
    let uses = [
        "spawn ^sessions[",
        "send ^sessions[",
        "stop ^sessions[",
        "send ^sessions.*",
        "on ^sessions.Typing",
        "on ^sessions.done",
        "on ^sessions.fail",
    ];
    for from in uses {
        let loc = c
            .request("textDocument/definition", at(&uri, HUB, from))
            .unwrap();
        assert_eq!(
            loc["range"]["start"],
            position(HUB, "children ^sessions"),
            "{from}"
        );
    }
    let mut params = at(&uri, HUB, "children ^sessions");
    params["context"] = json!({ "includeDeclaration": false });
    let refs = c.request("textDocument/references", params).unwrap();
    assert_eq!(refs.as_array().unwrap().len(), uses.len(), "{refs}");

    // Hover: the declaration, what it belongs to and its doc comment.
    let hover = c
        .request("textDocument/hover", at(&uri, HUB, "stop ^sessions"))
        .unwrap();
    let value = hover["contents"]["value"].as_str().unwrap();
    assert!(
        value.starts_with("```roadbook\nchildren sessions: Session by string\n```"),
        "{value}"
    );
    assert!(value.contains("children collection of `Hub`"), "{value}");
    assert!(value.contains("The open sessions."), "{value}");

    // The outline lists it among the machine's members.
    let symbols = c
        .request(
            "textDocument/documentSymbol",
            json!({ "textDocument": { "uri": uri } }),
        )
        .unwrap();
    let hub = symbols
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "Hub")
        .unwrap();
    let names: Vec<&str> = hub["children"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "greeting", "Join", "Kick", "Shout", "Close", "sessions", "open", "Serving", "Closed"
        ]
    );
    let sessions = &hub["children"][5];
    assert_eq!(sessions["detail"], "Session by string");
    assert_eq!(slice(HUB, &sessions["selectionRange"]), "sessions");

    // Rename: a collection shares the machine's namespace with its parameters, variables
    // and `as` names, and cannot be called `key`.
    let why = rename_error(&mut c, &uri, HUB, "children ^sessions", "key");
    assert!(why.contains("reserved"), "{why}");
    let why = rename_error(&mut c, &uri, HUB, "children ^sessions", "Sessions");
    assert!(why.contains("lower-case"), "{why}");
    let why = rename_error(&mut c, &uri, HUB, "children ^sessions", "open");
    assert!(
        why.contains("already the name of a variable `Hub.open`"),
        "{why}"
    );
    let why = rename_error(&mut c, &uri, HUB, "children ^sessions", "greeting");
    assert!(
        why.contains("already the name of a parameter `Hub.greeting`"),
        "{why}"
    );
    let why = rename_error(&mut c, &uri, HUB, "var ^open", "sessions");
    assert!(
        why.contains("already the name of a children collection `Hub.sessions`"),
        "{why}"
    );
    let why = rename_error(&mut c, &uri, HUB, "var ^open", "key");
    assert!(why.contains("reserved"), "{why}");
    let out = rename(&mut c, &uri, HUB, "children ^sessions", "conns");
    assert_eq!(out.matches("conns").count(), 8, "{out}");
    for e in [
        "children conns: Session",
        "spawn conns[event.id]",
        "send conns[event.id].Notify",
        "stop conns[event.id]",
        "send conns.*.Notify",
        "on conns.Typing",
        "on conns.done",
        "on conns.fail *",
    ] {
        assert!(out.contains(e), "no `{e}` in\n{out}");
    }
    // `key` can still name a field.
    let out = rename(&mut c, &uri, HUB, "^key: string", "id");
    assert!(out.contains("emit Typing { id: event.text }"), "{out}");
    c.shutdown();
}

const HISTORY: &str = r#"machine M() {
  var history: int = 0
  event Go
  event Back
  initial Work
  state Work {
    initial A
    /// Back to where `Work` was.
    history Last
    deep history All
    state A { on Go -> B }
    state B { on Back -> Last }
    on Back -> Away
  }
  state Away {
    on Go -> Work.Last  set history = history + 1
    on Back -> Work.All
  }
}
"#;

#[test]
fn history_states() {
    error_free(&[("c.road", HISTORY)]);
    let p = Project::new("history", &[("c.road", HISTORY)]);
    let uri = p.uri("c.road");
    let mut c = Client::start(json!({}));
    c.open(&uri, HISTORY);
    assert_eq!(c.diagnostics(&uri), Vec::<Value>::new());

    // Targets lead to the history state, and back.
    for (from, to) in [
        ("-> ^Last }", "history ^Last"),
        ("Work.^Last", "history ^Last"),
        ("Work.^All", "deep history ^All"),
        ("set ^history", "var ^history"),
    ] {
        let loc = c
            .request("textDocument/definition", at(&uri, HISTORY, from))
            .unwrap();
        assert_eq!(
            loc["range"]["start"],
            position(HISTORY, to),
            "{from} -> {to}"
        );
    }
    let mut params = at(&uri, HISTORY, "history ^Last");
    params["context"] = json!({ "includeDeclaration": false });
    let refs = c.request("textDocument/references", params).unwrap();
    let lines: Vec<u64> = refs
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["range"]["start"]["line"].as_u64().unwrap() + 1)
        .collect();
    assert_eq!(lines, [12, 16]);

    // Hover shows which kind of history state it is, its parent and its doc comment.
    let hover = c
        .request("textDocument/hover", at(&uri, HISTORY, "Work.^Last"))
        .unwrap();
    let value = hover["contents"]["value"].as_str().unwrap();
    assert!(value.contains("history M.Work.Last"), "{value}");
    assert!(value.contains("state of `M.Work`"), "{value}");
    assert!(value.contains("Back to where `Work` was."), "{value}");
    let hover = c
        .request("textDocument/hover", at(&uri, HISTORY, "deep history ^All"))
        .unwrap();
    let value = hover["contents"]["value"].as_str().unwrap();
    assert!(value.contains("deep history M.Work.All"), "{value}");

    // The outline shows them among their parent's states.
    let symbols = c
        .request(
            "textDocument/documentSymbol",
            json!({ "textDocument": { "uri": uri } }),
        )
        .unwrap();
    let work = symbols[0]["children"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "Work")
        .unwrap();
    let children: Vec<(&str, &str)> = work["children"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["name"].as_str().unwrap(), s["detail"].as_str().unwrap()))
        .collect();
    assert_eq!(
        children,
        [
            ("Last", "history"),
            ("All", "deep history"),
            ("A", "state"),
            ("B", "state")
        ]
    );
    assert_eq!(
        slice(HISTORY, &work["children"][1]["range"]),
        "deep history All"
    );
    assert_eq!(
        slice(HISTORY, &work["children"][1]["selectionRange"]),
        "All"
    );

    // Rename, from the declaration and from a target; `history` the variable stays.
    let text = rename(&mut c, &uri, HISTORY, "history ^Last", "Before");
    for e in [
        "history Before\n",
        "on Back -> Before }",
        "-> Work.Before  set history = history + 1",
    ] {
        assert!(text.contains(e), "no `{e}` in\n{text}");
    }
    c.change(&uri, 2, &text);
    let text = rename(&mut c, &uri, &text, "Work.^All", "Everything");
    assert!(text.contains("deep history Everything\n"), "{text}");
    assert!(text.contains("-> Work.Everything\n"), "{text}");
    c.change(&uri, 3, &text);
    // A history state shares the names of its sibling states.
    let why = rename_error(&mut c, &uri, &text, "history ^Before", "A");
    assert!(
        why.contains("already the name of a state `M.Work.A`"),
        "{why}"
    );
    let why = rename_error(&mut c, &uri, &text, "state ^B ", "Everything");
    assert!(
        why.contains("already the name of a state `M.Work.Everything`"),
        "{why}"
    );
    c.shutdown();
}

#[test]
fn an_untitled_document_is_a_file_of_its_own() {
    let uri = "untitled:Untitled-1";
    let bad = "machine M() {\n  initial A\n  state A { always -> Dne }\n}\n";
    let mut c = Client::start(json!({}));
    c.open(uri, bad);
    let diags = c.diagnostics(uri);
    assert_eq!(diags.len(), 1, "{diags:#?}");
    assert_eq!(diags[0]["code"], "RB0201");
    let good = "machine M() {\n  initial A\n  state A { always -> B }\n  final state B {}\n}\n";
    c.change(uri, 2, good);
    assert_eq!(c.diagnostics(uri), Vec::<Value>::new());
    let loc = c
        .request("textDocument/definition", at(uri, good, "-> ^B"))
        .unwrap();
    assert_eq!(loc["uri"], uri);
    let out = rename(&mut c, uri, good, "state ^A", "Start");
    assert!(out.contains("initial Start"), "{out}");
    let ir = c
        .request("roadbook/ir", json!({ "textDocument": { "uri": uri } }))
        .unwrap();
    assert_eq!(ir["root"], Value::Null);
    assert_eq!(ir["entry"], "Untitled-1.road");
    assert!(ir["ir"].as_str().unwrap().contains("\"M\""), "{ir}");
    c.notify(
        "textDocument/didClose",
        json!({ "textDocument": { "uri": uri } }),
    );
    assert_eq!(c.diagnostics(uri), Vec::<Value>::new());
    c.shutdown();
}

#[test]
fn ir_compiles_the_editor_s_text_as_export_ir_would() {
    let types = "type Order { id: string }\n";
    let a = "import { Order } from \"./types.road\"\nimport { B } from \"./b.road\"\n\nmachine A(o: Order) -> Order {\n  initial Done\n  final state Done { output o }\n}\n";
    // `b.road` imports `a.road` back: a cycle, which is not an error.
    let b = "import { A } from \"./a.road\"\n\nmachine B() {\n  initial Done\n  final state Done {}\n}\n";
    let p = Project::new("ir", &[("types.road", types), ("a.road", a), ("b.road", b)]);
    let (au, tu) = (p.uri("a.road"), p.uri("types.road"));
    let mut c = Client::start(json!({}));
    let ir = |c: &mut Client| {
        c.request("roadbook/ir", json!({ "textDocument": { "uri": au } }))
            .unwrap()
    };
    // Not open: read from disk.
    let first = ir(&mut c);
    assert_eq!(first["diagnostics"], json!([]), "{first}");
    assert_eq!(first["entry"], "a.road");
    assert_eq!(first["root"], p.root.to_string_lossy().as_ref());
    let text = first["ir"].as_str().unwrap();
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["sources"][0]["path"], "a.road");
    // An unsaved change to an imported file counts.
    c.open(&tu, types);
    c.change(&tu, 2, "type Order { id: string, total: int }\n");
    let second = ir(&mut c);
    assert!(second["ir"].as_str().unwrap().contains("total"), "{second}");
    // An error: no IR, and the diagnostics as `roadbook check --json` gives them.
    c.change(&tu, 3, "type Ordr { id: string }\n");
    let third = ir(&mut c);
    assert_eq!(third["ir"], Value::Null);
    let d = &third["diagnostics"][0];
    assert_eq!(d["code"], "RB0112", "{third}");
    assert_eq!(d["file"], "a.road");
    assert_eq!(d["severity"], "error");
    assert!(d["line"].is_u64() && d["endCol"].is_u64(), "{d}");
    // A file of a package is not compiled as an entry of the project.
    let q = with_package("ir-package");
    let why = c
        .request(
            "roadbook/ir",
            json!({ "textDocument": { "uri": q.uri("node_modules/pkg/lib.road") } }),
        )
        .unwrap_err();
    assert!(why.contains("another package"), "{why}");
    let main = c
        .request(
            "roadbook/ir",
            json!({ "textDocument": { "uri": q.uri("main.road") } }),
        )
        .unwrap();
    assert!(main["ir"].is_string(), "{main}");
    c.shutdown();
}

#[test]
fn the_errors_of_a_retry_are_references() {
    let text = "error Busy\nerror Gone\nfn backoff(n: int) -> duration\ntask ping() ! Busy | Gone\n\nmachine M() {\n  var tries: int = 0\n\n  initial A\n\n  state A {\n    invoke ping() retry 2 on Busy after backoff(attempt) {\n      done      -> B  set tries = 1\n      fail Busy -> B\n      fail Gone -> B\n    }\n  }\n\n  final state B {}\n}\n";
    let p = Project::new("retry", &[("c.road", text)]);
    let uri = p.uri("c.road");
    let mut c = Client::start(json!({}));
    c.open(&uri, text);
    assert!(c.diagnostics(&uri).is_empty());
    let loc = c
        .request("textDocument/definition", at(&uri, text, "on ^Busy"))
        .unwrap();
    assert_eq!(loc["range"]["start"], position(text, "error ^Busy"));
    let out = rename(&mut c, &uri, text, "error ^Busy", "Throttled");
    assert!(out.contains("retry 2 on Throttled after"), "{out}");
    assert!(out.contains("fail Throttled -> B"), "{out}");
    // `attempt` in the delay counts the failed attempts: a variable named so would make it
    // ambiguous.
    let why = rename_error(&mut c, &uri, text, "var ^tries", "attempt");
    assert!(why.contains("`attempt` is ambiguous"), "{why}");
    c.shutdown();
}

const ENDED: &str = r#"/// Record how a peer ended.
action note(status: Ended)

machine Inventory() {
  event Close
  emits Low
  emits Out
  event Drain
  initial Open
  state Open {
    on Close -> Closed
    on Drain emit Low  emit Out
  }
  final state Closed {}
}

type Shelf = ref Inventory

machine Order(inventory: ref Inventory, spare: Shelf?, all: ref Inventory[], n: int) {
  var current: ref Inventory = inventory
  initial Waiting
  state Waiting {
    on inventory.ended {
      if event.status == Ended.Defect -> Alarm  do note(event.status)
      else                            -> Gone
    }
    on current.ended -> Gone
    on inventory.Low -> Gone
    on inventory.Out -> Gone
  }
  final state Alarm {}
  final state Gone {}
}
"#;

/// The labels of a completion answer.
fn labels(answer: &Value) -> Vec<String> {
    answer
        .as_array()
        .unwrap_or_else(|| panic!("{answer}"))
        .iter()
        .map(|i| i["label"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn a_peer_s_end() {
    error_free(&[("c.road", ENDED)]);
    let p = Project::new("ended", &[("c.road", ENDED)]);
    let uri = p.uri("c.road");
    let mut c = Client::start(json!({}));
    c.open(&uri, ENDED);
    assert_eq!(c.diagnostics(&uri), Vec::<Value>::new());

    // The name before the dot goes to the parameter or variable holding the run.
    for (from, to) in [
        ("on ^inventory.ended", "Order(^inventory"),
        ("on ^current.ended", "var ^current"),
    ] {
        let loc = c
            .request("textDocument/definition", at(&uri, ENDED, from))
            .unwrap();
        assert_eq!(loc["range"]["start"], position(ENDED, to), "{from}");
    }
    let mut params = at(&uri, ENDED, "Order(^inventory");
    params["context"] = json!({ "includeDeclaration": false });
    let refs = c.request("textDocument/references", params).unwrap();
    // `on inventory.ended`, `.Low`, `.Out` and `current`'s initial value.
    assert_eq!(refs.as_array().unwrap().len(), 4, "{refs}");

    // Hover on the built-in names, which have no declaration.
    let hover = |c: &mut Client, needle: &str| {
        let h = c
            .request("textDocument/hover", at(&uri, ENDED, needle))
            .unwrap();
        let value = h["contents"]["value"].as_str().unwrap().to_string();
        (value, slice(ENDED, &h["range"]).to_string())
    };
    let (value, word) = hover(&mut c, "== ^Ended.Defect");
    assert_eq!(word, "Ended");
    assert!(
        value.starts_with(
            "```roadbook\nenum Ended { Done, Failed, Cancelled, Defect, Suspended }\n```"
        ),
        "{value}"
    );
    assert!(value.contains("built-in enum"), "{value}");
    let (value, word) = hover(&mut c, "status: ^Ended");
    assert_eq!(word, "Ended", "{value}");
    let (value, word) = hover(&mut c, "Ended.^Defect");
    assert_eq!(word, "Defect");
    assert!(
        value.starts_with("```roadbook\nEnded.Defect\n```"),
        "{value}"
    );
    assert!(value.contains("a defect"), "{value}");
    let (value, word) = hover(&mut c, "if event.^status");
    assert_eq!(word, "status");
    assert!(
        value.starts_with("```roadbook\nevent.status: Ended\n```"),
        "{value}"
    );
    assert!(
        value.contains("how the run `inventory` holds ended"),
        "{value}"
    );
    let (value, word) = hover(&mut c, "note(^event.status)");
    assert_eq!(word, "event");
    assert!(value.contains("event: { status: Ended }"), "{value}");
    let (value, word) = hover(&mut c, "on inventory.^ended");
    assert_eq!(word, "ended");
    assert!(
        value.starts_with("```roadbook\non inventory.ended\n```"),
        "{value}"
    );
    // Only `event.status` in `on name.ended` is the peer's status.
    let h = c
        .request("textDocument/hover", at(&uri, ENDED, "note(^status"))
        .unwrap();
    assert_eq!(h, Value::Null);

    // Completion, while the line is still being written.
    let complete = |c: &mut Client, text: &str, needle: &str| {
        c.change(&uri, 2, text);
        let _ = c.diagnostics(&uri);
        c.request("textDocument/completion", at(&uri, text, needle))
            .unwrap()
    };
    let typing = ENDED.replace("    on current.ended -> Gone\n", "    on current.\n");
    let got = complete(&mut c, &typing, "on current.^\n");
    assert_eq!(labels(&got), ["ended", "Low", "Out"]);
    assert_eq!(got[0]["kind"], 14, "a keyword: {got}");
    // An optional `ref` through an alias, and a list of them.
    let typing = ENDED.replace("    on current.ended -> Gone\n", "    on spare.en\n");
    assert_eq!(
        labels(&complete(&mut c, &typing, "on spare.en^\n")),
        ["ended", "Low", "Out"]
    );
    let typing = ENDED.replace("    on current.ended -> Gone\n", "    on all.\n");
    assert_eq!(
        labels(&complete(&mut c, &typing, "on all.^\n")),
        ["ended", "Low", "Out"]
    );
    // Not for a name that holds no run, nor outside `on`.
    let typing = ENDED.replace("    on current.ended -> Gone\n", "    on n.\n");
    assert_eq!(
        labels(&complete(&mut c, &typing, "on n.^\n")),
        Vec::<String>::new()
    );
    let typing = ENDED.replace("-> Gone\n    }", "-> Gone  do note(inventory.)\n    }");
    assert_eq!(
        labels(&complete(&mut c, &typing, "note(inventory.^)")),
        Vec::<String>::new()
    );
    // `Ended.` lists its variants.
    let typing = ENDED.replace("== Ended.Defect", "== Ended.");
    assert_eq!(
        labels(&complete(&mut c, &typing, "== Ended.^")),
        ["Done", "Failed", "Cancelled", "Defect", "Suspended"]
    );
    // `event.` in `on name.ended` has `status`, and nothing in another transition.
    let typing = ENDED.replace("do note(event.status)", "do note(event.)");
    let got = complete(&mut c, &typing, "note(event.^)");
    assert_eq!(labels(&got), ["status"]);
    assert_eq!(got[0]["detail"], "Ended");
    let typing = ENDED.replace(
        "on inventory.Low -> Gone",
        "on inventory.Low -> Gone  do note(event.)",
    );
    assert_eq!(
        labels(&complete(&mut c, &typing, "note(event.^)")),
        Vec::<String>::new()
    );
    c.shutdown();
}

const INTERNAL: &str = "machine M() {
  event Home
  event Sleep
  initial Session
  state Session {
    on Home -> .Browsing
    initial Browsing
    state Browsing { on Sleep -> Player }
    parallel state Player {
      on Sleep -> .Playback.Paused, .Volume.Muted
      state Playback {
        initial Playing
        state Playing {}
        state Paused {}
      }
      state Volume {
        initial Normal
        state Normal {}
        state Muted {}
      }
    }
  }
}
";

#[test]
fn internal_targets_and_several_targets() {
    error_free(&[("c.road", INTERNAL)]);
    let p = Project::new("internal", &[("c.road", INTERNAL)]);
    let uri = p.uri("c.road");
    let mut c = Client::start(json!({}));
    c.open(&uri, INTERNAL);
    assert_eq!(c.diagnostics(&uri), Vec::<Value>::new());

    // A target with a dot leads to the child it names, each of several targets to its own.
    for (from, to) in [
        ("-> .^Browsing", "state ^Browsing"),
        (".^Playback.Paused", "state ^Playback"),
        (".Playback.^Paused", "state ^Paused"),
        (".^Volume.Muted", "state ^Volume"),
        (".Volume.^Muted", "state ^Muted"),
    ] {
        let loc = c
            .request("textDocument/definition", at(&uri, INTERNAL, from))
            .unwrap();
        assert_eq!(
            loc["range"]["start"],
            position(INTERNAL, to),
            "{from} -> {to}"
        );
    }

    // Renaming a state renames it where a dotted target names it.
    let out = rename(&mut c, &uri, INTERNAL, "state ^Browsing", "Shopping");
    assert!(out.contains("on Home -> .Shopping"), "{out}");
    let out = rename(&mut c, &uri, INTERNAL, "state ^Muted", "Quiet");
    assert!(out.contains("-> .Playback.Paused, .Volume.Quiet"), "{out}");

    // Mixed dots: one quick fix adds the dot everywhere, one removes it everywhere.
    let mixed = INTERNAL.replace(", .Volume.Muted", ", Player.Volume.Muted");
    let uri2 = p.uri("c.road");
    c.change(&uri2, 2, &mixed);
    let diags = c.diagnostics(&uri2);
    assert_eq!(diags[0]["code"], "RB0221", "{diags:?}");
    let actions = c
        .request(
            "textDocument/codeAction",
            json!({ "textDocument": { "uri": uri2 }, "range": diags[0]["range"], "context": { "diagnostics": diags } }),
        )
        .unwrap();
    let titles: Vec<&str> = actions
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["title"].as_str().unwrap())
        .collect();
    assert_eq!(
        titles,
        [
            "write every target with the dot",
            "write every target without the dot"
        ]
    );
    let fixed = apply(&mixed, &actions[0]["edit"]["changes"][uri2.as_str()]);
    assert_eq!(fixed, INTERNAL);
    let fixed = apply(&mixed, &actions[1]["edit"]["changes"][uri2.as_str()]);
    assert!(
        fixed.contains("on Sleep -> Player.Playback.Paused, Player.Volume.Muted"),
        "{fixed}"
    );
    c.shutdown();
}
