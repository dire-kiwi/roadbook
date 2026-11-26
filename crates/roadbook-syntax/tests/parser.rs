//! Parser tests: a snapshot of the tree for every construct, and checks of details the
//! snapshots do not show (spans, doc comments, precedence).

mod common;

use roadbook_syntax::ast::*;
use roadbook_syntax::{FileId, parse_file};

fn parse(src: &str) -> File {
    let (file, diags) = parse_file(FileId(0), src);
    assert!(diags.is_empty(), "{diags:#?}");
    file
}

#[test]
fn every_construct() {
    for (name, src) in common::CASES {
        let file = parse(src);
        insta::assert_yaml_snapshot!(*name, file);
    }
}

fn machine(file: &File) -> &Machine {
    file.machines().next().expect("a machine").1
}

fn var_init<'a>(file: &'a File, name: &str) -> &'a Expr {
    machine(file)
        .body
        .vars()
        .find(|v| v.name.name == name)
        .and_then(|v| v.init.as_ref())
        .expect("var with an initializer")
}

#[test]
fn spans_have_lines_and_columns() {
    let file = parse("machine M() {\n  state Charging {\n    on Go -> B\n  }\n}\n");
    let state = machine(&file).body.states().next().unwrap();
    assert_eq!((state.span.start.line, state.span.start.col), (2, 3));
    assert_eq!((state.span.end.line, state.span.end.col), (4, 4));
    assert_eq!(
        (state.name.span.start.col, state.name.span.end.col),
        (9, 17)
    );
    let t = state.body.transitions().next().unwrap();
    assert_eq!((t.trigger_span.start.col, t.trigger_span.end.col), (5, 10));
    assert_eq!(
        &"machine M() {\n  state Charging {\n    on Go -> B\n  }\n}\n"[t.span.range()],
        "on Go -> B"
    );
}

#[test]
fn doc_comments_attach_to_the_next_declaration() {
    let file = parse(
        "/// One.\n///Two.\n// plain\n///   Three.\nmachine M() {\n  /// The state.\n  final state S {}\n}\n",
    );
    let (decl, m) = file.machines().next().unwrap();
    assert_eq!(decl.doc.as_ref().unwrap().text, "One.\nTwo.\n  Three.");
    assert_eq!(
        m.body.states().next().unwrap().doc.as_ref().unwrap().text,
        "The state."
    );
    assert_eq!(file.comments.len(), 5);
}

#[test]
fn not_binds_looser_than_comparison() {
    let file = parse("machine M() {\n  var a: bool = not x > 1 and y\n}\n");
    let ExprKind::Binary(BinaryOp::And, left, _) = &var_init(&file, "a").kind else {
        panic!("expected and")
    };
    let ExprKind::Unary(UnaryOp::Not, inner) = &left.kind else {
        panic!("expected not")
    };
    assert!(matches!(inner.kind, ExprKind::Binary(BinaryOp::Gt, ..)));
}

#[test]
fn arithmetic_precedence_and_associativity() {
    let file = parse("machine M() {\n  var a: int = 1 - 2 - 3 * 4\n}\n");
    // (1 - 2) - (3 * 4)
    let ExprKind::Binary(BinaryOp::Sub, l, r) = &var_init(&file, "a").kind else {
        panic!()
    };
    assert!(matches!(l.kind, ExprKind::Binary(BinaryOp::Sub, ..)));
    assert!(matches!(r.kind, ExprKind::Binary(BinaryOp::Mul, ..)));
}

#[test]
fn implicit_names_are_variables() {
    let file = parse("machine M() {\n  var a: int = event.x + error.y + result\n}\n");
    let mut names = Vec::new();
    var_init(&file, "a").walk(&mut |e| {
        if let ExprKind::Var(n) = &e.kind {
            names.push(n.name.clone());
        }
    });
    assert_eq!(names, ["event", "error", "result"]);
}

#[test]
fn literals() {
    let file = parse(
        "machine M() {\n  var a: duration = 90m\n  var b: string = \"a\\\"b\"\n  var c: int = 007\n}\n",
    );
    let ExprKind::Duration(d) = var_init(&file, "a").kind else {
        panic!()
    };
    assert_eq!(d.millis(), 90 * 60_000);
    assert_eq!(var_init(&file, "b").kind, ExprKind::String("a\"b".into()));
    assert_eq!(var_init(&file, "c").kind, ExprKind::Int(7));
}

#[test]
fn invoke_distinguishes_tasks_and_machines() {
    let file = parse(
        "machine M() {\n  state A {\n    invoke task1() { done -> A }\n  }\n  state B {\n    invoke Child() { done -> A }\n  }\n}\n",
    );
    let kinds: Vec<_> = machine(&file)
        .body
        .states()
        .map(|s| s.body.invokes().next().unwrap().is_machine())
        .collect();
    assert_eq!(kinds, [false, true]);
}

#[test]
fn from_and_builtin_type_names_are_not_reserved() {
    let file = parse("type Transfer { from: string, string: int, duration: duration }\n");
    assert_eq!(file.decls().count(), 1);
}
