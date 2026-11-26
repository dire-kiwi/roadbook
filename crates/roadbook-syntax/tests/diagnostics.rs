//! One failing snippet per syntax diagnostic, error recovery, and the rendered form.

use roadbook_syntax::ast::{DeclKind, Item};
use roadbook_syntax::diagnostic::SingleFile;
use roadbook_syntax::{FileId, parse_file};

fn codes(src: &str) -> Vec<String> {
    parse_file(FileId(0), src)
        .1
        .iter()
        .map(|d| d.code.to_string())
        .collect()
}

fn m(body: &str) -> String {
    format!("machine M() {{\n{body}\n}}\n")
}

fn s(body: &str) -> String {
    m(&format!("  state S {{\n{body}\n  }}"))
}

#[test]
fn each_syntax_error_has_its_code() {
    let cases: Vec<(String, &str)> = vec![
        ("type A { x: int } #".into(), "RB0001"),
        ("import { A } from \"x".into(), "RB0002"),
        ("import { A } from \"\\q\"".into(), "RB0003"),
        (m("  after 5min -> A"), "RB0004"),
        (m("  var x: int = 99999999999999999999"), "RB0005"),
        (
            m(&format!("  var x: float = 1{}.0", "0".repeat(400))),
            "RB0005",
        ),
        ("type Line_item { x: int }".into(), "RB0006"),
        ("type A { x int }".into(), "RB0010"),
        ("machine M(p: ref) {}".into(), "RB0010"),
        ("machine M(p: ref int) {}".into(), "RB0010"),
        (s("    on Go send upload"), "RB0010"),
        (s("    on Go send upload.pause"), "RB0012"),
        (s("    on Go emit progress"), "RB0012"),
        (
            s("    invoke U() as Up {\n      done -> S\n    }"),
            "RB0012",
        ),
        (m("  var send: int"), "RB0013"),
        (m("  var emits: int"), "RB0013"),
        (
            s("    invoke U() as result {\n      done -> S\n    }"),
            "RB0013",
        ),
        ("machine M() {\n  initial S\n".into(), "RB0011"),
        ("type A { x: int\ntype B = int".into(), "RB0011"),
        ("type order { x: int }".into(), "RB0012"),
        (m("  var on: int"), "RB0013"),
        (m("  var result: int"), "RB0013"),
        ("task t(result: int)".into(), "RB0013"),
        ("type Evt { type: string }".into(), "RB0013"),
        ("type Evt {\n  fn: int\n}".into(), "RB0013"),
        ("fn g(error: int) -> int".into(), "RB0013"),
        (m("  var error: string = \"\""), "RB0013"),
        (m("  event E { task: int }"), "RB0013"),
        (m("  done -> A"), "RB0014"),
        (m("  invoke t() { done -> A }"), "RB0014"),
        (m("  output 1"), "RB0014"),
        (m("  history H"), "RB0014"),
        (m("  deep history H"), "RB0014"),
        (s("    deep H"), "RB0010"),
        (s("    deep history h"), "RB0012"),
        (s("    var x: int"), "RB0015"),
        (s("    event Go"), "RB0015"),
        (s("    emits Go"), "RB0015"),
        ("state S {}".into(), "RB0016"),
        (s("    on Go\n    on Stop -> S"), "RB0017"),
        (s("    on Go if ready"), "RB0017"),
        (s("    after Cfg { ms: 5 } -> S"), "RB0018"),
        (s("    fail CardDeclined -> S"), "RB0019"),
        (s("    fail * -> S"), "RB0019"),
        (s("    invoke t() {\n      fail declined\n    }"), "RB0020"),
        (
            s("    invoke t() {\n      done -> S\n      on Go -> S\n    }"),
            "RB0020",
        ),
        (s("    on Go {\n      else -> S\n    }"), "RB0021"),
        (
            s("    on Go {\n      if a -> S\n      else -> S\n      if b -> S\n    }"),
            "RB0021",
        ),
        (s("    on Go {\n      -> S\n    }"), "RB0021"),
        (s("    on Go {}"), "RB0021"),
        (s("    always if a < b < c -> S"), "RB0022"),
        (s("    /// Documents nothing.\n    on Go -> S"), "RB0023"),
        ("type A { x: integer }".into(), "RB0024"),
        (
            m(&format!(
                "  var x: int = {}1{}",
                "(".repeat(70),
                ")".repeat(70)
            )),
            "RB0025",
        ),
        (m(&format!("  var x: int{}", "[]".repeat(70))), "RB0025"),
        (
            m(&format!("  var x: bool = {}", vec!["a"; 70].join(" and "))),
            "RB0025",
        ),
        (m(&format!("  var x: int = a{}", ".b".repeat(70))), "RB0025"),
        (
            m(&format!("{}{}", "state S {\n".repeat(70), "}\n".repeat(70))),
            "RB0025",
        ),
    ];
    for (src, code) in cases {
        let got = codes(&src);
        assert_eq!(got, [code], "{src}");
    }
}

#[test]
fn several_errors_are_reported_and_the_rest_is_parsed() {
    let src = "\
type Ok1 { x: int }
type Bad { x: }
machine M() {
  var a: int = 1 +
  initial A
  state A {
    on Go ->
    on Stop -> B
    invoke t(a) {
      done -> B  set = 1
      fail E -> B
    }
  }
  final state B {}
}
type Ok2 = string
";
    let (file, diags) = parse_file(FileId(0), src);
    let got: Vec<_> = diags
        .iter()
        .map(|d| (d.code.to_string(), d.span.start.line))
        .collect();
    assert_eq!(
        got,
        [
            ("RB0010".to_string(), 2),
            ("RB0010".into(), 5),
            ("RB0010".into(), 8),
            ("RB0010".into(), 10)
        ]
    );

    let names: Vec<_> = file.decls().map(|d| d.name.name.as_str()).collect();
    assert_eq!(names, ["Ok1", "M", "Ok2"]);
    let (_, m) = file.machines().next().unwrap();
    let states: Vec<_> = m.body.states().map(|s| s.name.name.as_str()).collect();
    assert_eq!(states, ["A", "B"]);
    let a = m.body.states().next().unwrap();
    assert_eq!(a.body.transitions().count(), 1, "`on Stop -> B` survives");
    let outcomes = &a.body.invokes().next().unwrap().outcomes;
    assert_eq!(outcomes.len(), 1, "`fail E -> B` survives");
}

#[test]
fn an_unclosed_machine_keeps_its_items() {
    let src = "machine M() {\n  initial A\n  state A {}\n\ntype T = int\n";
    let (file, diags) = parse_file(FileId(0), src);
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].code.to_string(), "RB0011");
    assert_eq!(diags[0].labels[0].message, "unclosed `{`");
    assert_eq!(file.decls().count(), 2);
    let DeclKind::Machine(m) = &file.decls().next().unwrap().kind else {
        panic!()
    };
    assert!(matches!(
        m.body.items.as_slice(),
        [Item::Initial(_), Item::State(_)]
    ));
}

#[test]
fn misplaced_items_are_reported_and_dropped() {
    let src = m("  var a: int\n  state S {\n    var b: int\n  }\n  done -> S");
    let (file, diags) = parse_file(FileId(0), &src);
    assert_eq!(diags.len(), 2);
    let (_, machine) = file.machines().next().unwrap();
    assert_eq!(machine.body.items.len(), 2);
    assert!(machine.body.states().next().unwrap().body.items.is_empty());
}

#[test]
fn wrong_case_names_are_kept() {
    let (file, diags) = parse_file(
        FileId(0),
        "type order { x: int }\nmachine M() {\n  initial idle\n  state idle {}\n}\n",
    );
    assert_eq!(diags.len(), 3);
    assert!(
        diags
            .iter()
            .all(|d| d.code.to_string() == "RB0012" && d.fixes.len() == 1)
    );
    assert_eq!(diags[0].fixes[0].edits[0].text, "Order");
    assert_eq!(file.decls().count(), 2);
}

#[test]
fn rendered_syntax_error() {
    let src = s("    on Go if ready");
    let (_, diags) = parse_file(FileId(0), &src);
    let out = diags[0].render(&SingleFile {
        name: "m.road",
        source: &src,
    });
    let expected = "\
error[RB0017]: a transition must do something: expected `-> Target`, `set`, `do`, `send` or `emit`, found `}`
 --> m.road:4:3
  |
3 |     on Go if ready
  |     ----- this transition
4 |   }
  |   ^
  |
  = help: give it a target (`-> State`), assignments (`set x = …`), actions (`do f(…)`) or messages (`send child.Event`, `emit Event`)
";
    assert_eq!(out, expected);
}

#[test]
fn json_form() {
    let (_, diags) = parse_file(FileId(2), "type order { x: int }");
    let json = serde_json::to_value(&diags[0]).unwrap();
    assert_eq!(json["code"], "RB0012");
    assert_eq!(json["severity"], "error");
    assert_eq!(json["file"], 2);
    assert_eq!(
        json["span"],
        serde_json::json!({ "file": 2, "start": 5, "end": 10, "line": 1, "col": 6, "endLine": 1, "endCol": 11 })
    );
    assert_eq!(json["fixes"][0]["edits"][0]["text"], "Order");
}
