//! Formatter tests: expected layouts, comment preservation, idempotence, and the round trip
//! `parse(format(x)) == parse(x)` ignoring spans.

mod common;

use roadbook_syntax::{FileId, format, parse_file};

/// `(input, canonical output)`
const LAYOUTS: &[(&str, &str)] = &[
    // Spacing is normalised; trailing commas and redundant parentheses go.
    (
        "machine   M ( a :int , ) -> R!E{initial A state A{on Go->A set x=(1+2)*3,y=(a)}}",
        "machine M(a: int) -> R ! E {\n  initial A\n  state A { on Go -> A  set x = (1 + 2) * 3, y = a }\n}\n",
    ),
    // `not` gets parentheses around a comparison; precedence keeps the ones it needs.
    (
        "machine M() {\n  var a: bool = not a > b\n  var b: int = a - (b - c) - -(-d)\n  var c: bool = (not a) and not (b or c)\n}\n",
        "machine M() {\n  var a: bool = not (a > b)\n  var b: int = a - (b - c) - -(-d)\n  var c: bool = not a and not (b or c)\n}\n",
    ),
    // Blank lines are kept, at most one, never at the start or end of a block.
    (
        "\n\nmachine M() {\n\n\n  initial A\n\n\n\n  state A {}\n\n}\n\n\n",
        "machine M() {\n  initial A\n\n  state A {}\n}\n",
    ),
    // A state with one simple item is one line; anything else is expanded.
    (
        "machine M() {\n  state A {\n    on Go -> B\n  }\n  state B { entry do f() always -> A }\n  state C {\n  }\n}\n",
        "machine M() {\n  state A { on Go -> B }\n  state B {\n    entry do f()\n    always -> A\n  }\n  state C {}\n}\n",
    ),
    // `error E {}` is `error E`; a record value with no fields is the bare name.
    ("error E {}\n\nfn f() -> E\n", "error E\n\nfn f() -> E\n"),
    (
        "machine M() {\n  final state A { fail E {} }\n}\n",
        "machine M() {\n  final state A { fail E }\n}\n",
    ),
    // Every `set` comes before every `do`, which is the order they run in, and effects of
    // one kind share a clause.
    (
        "machine M() {\n  entry set a = 1 set b = 2 do f() do g() set c = 3\n}\n",
        "machine M() {\n  entry set a = 1, b = 2, c = 3  do f(), g()\n}\n",
    ),
    // `send` and `emit` come after every `set`, in the order written with the `do`s, each a
    // clause of its own; an empty body goes.
    (
        "machine M(p: ref  P) {\n  emits Progress{percent:int}\n  emits Done {}\n  event Go\n  state A {\n    invoke U( ) as   u { done -> B }\n    on u.Progress  emit Progress {percent: event.percent,} set n = 1\n    on Go do f() send p.Ping {} do g() do h() set x = 1 emit Done\n  }\n}\n",
        "machine M(p: ref P) {\n  emits Progress { percent: int }\n  emits Done\n  event Go\n  state A {\n    invoke U() as u {\n      done -> B\n    }\n    on u.Progress set n = 1  emit Progress { percent: event.percent }\n    on Go         set x = 1  do f()  send p.Ping  do g(), h()  emit Done\n  }\n}\n",
    ),
    // A record value as an `after` delay keeps its parentheses.
    (
        "machine M() {\n  after (Cfg { ms: 1 }) -> A\n  after (d) -> A\n}\n",
        "machine M() {\n  after (Cfg { ms: 1 }) -> A\n  after d               -> A\n}\n",
    ),
    // Alignment: transition arrows and effects across a block, nested branches included.
    (
        "machine M() {\n  state A {\n    on Go -> B set x = 1\n    on LongerName -> Somewhere\n    on Pick {\n      if x > 1 -> B do f()\n      else -> C\n    }\n    on After -> B\n\n    on Alone -> B set y = 2\n  }\n}\n",
        "machine M() {\n  state A {\n    on Go         -> B          set x = 1\n    on LongerName -> Somewhere\n    on Pick {\n      if x > 1    -> B          do f()\n      else        -> C\n    }\n    on After      -> B\n\n    on Alone -> B  set y = 2\n  }\n}\n",
    ),
    // Alignment: variable types and events.
    (
        "machine M() {\n  var a: int = 1\n  var longer: string\n  event Go\n  event Change { x: int }\n}\n",
        "machine M() {\n  var a:      int = 1\n  var longer: string\n  event Go\n  event Change { x: int }\n}\n",
    ),
    // Alignment: one-line declarations, data and callables apart, broken by a comment line.
    (
        "type A { x: int }\ntype Longer = int\n// comment\nerror E\nerror Longer { y: int }\ntask t() -> int ! E\ntask longer(a: int) ! E\n",
        "type A      { x: int }\ntype Longer = int\n// comment\nerror E\nerror Longer { y: int }\ntask t()            -> int ! E\ntask longer(a: int)        ! E\n",
    ),
    // Multi-line lists keep the author's line breaks; the last element has no comma.
    (
        "import {\n  A, B,\n  C,\n} from \"x\"\ntype T {\n  a: int, b: int,\n  c: int,\n}\nenum E {\n  X,\n  Y\n}\naction act(\n  a: int,\n  b: int\n)\n",
        "import {\n  A, B,\n  C\n} from \"x\"\ntype T {\n  a: int, b: int,\n  c: int\n}\nenum E {\n  X,\n  Y\n}\naction act(\n  a: int,\n  b: int\n)\n",
    ),
    // A list broken somewhere other than after the opener is joined.
    (
        "import { A,\n  B } from \"x\"\n",
        "import { A, B } from \"x\"\n",
    ),
];

/// Inputs with comments in awkward places; every comment must survive.
const COMMENTS: &[(&str, &str)] = &[
    (
        "// head\n\n/// doc\nmachine M() { // after brace\n  // own line\n  on Go -> A // trailing\n  /// state doc\n  state A {\n    // only a comment\n  }\n  // before close\n} // after close\n// tail\n",
        "// head\n\n/// doc\nmachine M() { // after brace\n  // own line\n  on Go -> A // trailing\n  /// state doc\n  state A {\n    // only a comment\n  }\n  // before close\n} // after close\n// tail\n",
    ),
    // A comment inside something printed on one line moves before it.
    (
        "machine M() {\n  on Go // why\n    -> A\n  var x: int = 1 + // one\n    2\n}\n",
        "machine M() {\n  // why\n  on Go -> A\n  // one\n  var x: int = 1 + 2\n}\n",
    ),
    // Comments in multi-line lists.
    (
        "import {\n  // types\n  A, B, // trailing\n  c\n  // last\n} from \"x\"\n",
        "import {\n  // types\n  A, B, // trailing\n  c\n  // last\n} from \"x\"\n",
    ),
    // A comment in a one-line state keeps it expanded.
    (
        "machine M() {\n  final state A {\n    output 1 // the answer\n  }\n}\n",
        "machine M() {\n  final state A {\n    output 1 // the answer\n  }\n}\n",
    ),
    // Two transitions on one line: the comment stays with the second.
    (
        "machine M() {\n  on A -> X on B -> Y // b\n}\n",
        "machine M() {\n  on A -> X\n  on B -> Y // b\n}\n",
    ),
    // A comment after the closer of a multi-line list stays at the end of its line.
    (
        "machine M(\n  n: int\n) // c\n{\n  initial A\n  final state A {}\n}\n",
        "machine M(\n  n: int\n) { // c\n  initial A\n  final state A {}\n}\n",
    ),
    (
        "import {\n  A, B\n} // c\nfrom \"./x.road\"\n",
        "import {\n  A, B\n} from \"./x.road\" // c\n",
    ),
    (
        "action a(\n  n: int\n) // c\n",
        "action a(\n  n: int\n) // c\n",
    ),
    // Comments after the row that starts with a list's closer belong to what follows: the
    // next declaration keeps its doc comment, and nothing moves into the list.
    (
        "action log(\n  what: string\n)\n\n/// Doc.\nmachine M() {}\n",
        "action log(\n  what: string\n)\n\n/// Doc.\nmachine M() {}\n",
    ),
    (
        "fn f(\n  a: int\n) -> int\n// c\n/// Doc.\naction g(\n  b: int\n) // d\n/// Doc.\ntask t()\n",
        "fn f(\n  a: int\n) -> int\n// c\n/// Doc.\naction g(\n  b: int\n) // d\n/// Doc.\ntask t()\n",
    ),
    (
        "machine M(\n  a: int\n) {}\n\n/// Doc.\nmachine N() {}\n",
        "machine M(\n  a: int\n) {}\n\n/// Doc.\nmachine N() {}\n",
    ),
    (
        "import {\n  A\n} from \"x\"\n\n/// Doc.\ntype T {\n  a: int\n}\n/// Doc.\nenum E {\n  X\n}\n// c\n/// Doc.\nmachine M() {}\n",
        "import {\n  A\n} from \"x\"\n\n/// Doc.\ntype T {\n  a: int\n}\n/// Doc.\nenum E {\n  X\n}\n// c\n/// Doc.\nmachine M() {}\n",
    ),
    (
        "machine M() {\n  event E {\n    a: int\n  }\n  /// Doc.\n  state A {}\n}\n",
        "machine M() {\n  event E {\n    a: int\n  }\n  /// Doc.\n  state A {}\n}\n",
    ),
    // Comments inside the closer's row, on the lines after the closer, go into the list.
    (
        "task t(\n  a: int\n) ! A\n// c\n| B\n",
        "task t(\n  a: int\n  // c\n) ! A | B\n",
    ),
    (
        "machine M(\n  a: int\n) -> int\n// c\n! E { // d\n  initial A\n  final state A { output 1 }\n}\n",
        "machine M(\n  a: int\n  // c\n) -> int ! E { // d\n  initial A\n  final state A { output 1 }\n}\n",
    ),
    ("// only a comment\n", "// only a comment\n"),
    ("", ""),
];

/// The tree, with every list of effects in the order they run: `set`s, then `do`s.
fn structure(src: &str) -> serde_json::Value {
    let (file, diags) = parse_file(FileId(0), src);
    assert!(diags.is_empty(), "{src}\n{diags:#?}");
    let mut v = serde_json::to_value(&file).unwrap();
    sets_first(&mut v);
    v
}

fn sets_first(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::Object(m) => {
            for (k, x) in m.iter_mut() {
                if matches!(k.as_str(), "effects" | "entry" | "exit")
                    && let Some(list) = x.as_array_mut()
                {
                    list.sort_by_key(|e| e.get("set").is_none());
                }
                sets_first(x);
            }
        }
        serde_json::Value::Array(a) => a.iter_mut().for_each(sets_first),
        _ => {}
    }
}

fn check_stable(src: &str) {
    let once = format(src).unwrap_or_else(|d| panic!("{src}\n{d:#?}"));
    assert_eq!(format(&once).unwrap(), once, "not idempotent:\n{src}");
    assert_eq!(
        structure(&once),
        structure(src),
        "the tree changed:\n{src}\n---\n{once}"
    );
}

#[test]
fn layouts() {
    for (input, expected) in LAYOUTS {
        assert_eq!(format(input).unwrap(), *expected, "{input}");
    }
}

#[test]
fn comments_are_preserved() {
    for (input, expected) in COMMENTS {
        let out = format(input).unwrap();
        assert_eq!(out, *expected, "{input}");
        let texts = |s: &str| {
            parse_file(FileId(0), s)
                .0
                .comments
                .into_iter()
                .map(|c| c.text)
                .collect::<Vec<_>>()
        };
        assert_eq!(texts(&out), texts(input));
    }
}

#[test]
fn idempotent_and_round_trips() {
    let inputs = common::CASES
        .iter()
        .map(|c| c.1)
        .chain(LAYOUTS.iter().map(|c| c.0))
        .chain(COMMENTS.iter().map(|c| c.0));
    for src in inputs {
        check_stable(src);
    }
}

/// Comments between the closer of a multi-line list and the rest of its line move into the
/// list, ahead of one at the end of the closer's line, and stay there.
#[test]
fn comments_after_a_list_closer_are_stable() {
    for src in [
        "machine M(\n  n: int\n) // c\n// d\n{ // e\n  initial A\n  final state A {}\n}\n",
        "import {\n  A, B\n} // c\n// d\nfrom \"./x.road\" // e\n",
        "fn f(\n  n: int\n) // c\n-> int\n",
        "task t(\n  a: int\n) ! A\n// c\n| B\n",
        "task t(\n  a: int\n) ! A // c\n// d\n| B // e\n// f\n",
        "action log(\n  what: string\n)\n\n/// Doc.\nmachine M() {}\n",
    ] {
        let once = format(src).unwrap();
        assert_eq!(format(&once).unwrap(), once, "not idempotent:\n{src}");
        let comments = |s: &str| {
            let mut c: Vec<String> = parse_file(FileId(0), s)
                .0
                .comments
                .into_iter()
                .map(|c| c.text)
                .collect();
            c.sort();
            c
        };
        assert_eq!(comments(&once), comments(src), "{src}");
    }
}

#[test]
fn refuses_files_with_syntax_errors() {
    let err = format("machine M() {\n  on Go\n}\n").unwrap_err();
    assert_eq!(err[0].code.to_string(), "RB0017");
}

/// The same tokens with different whitespace: a cheap generator of layouts the formatter must
/// normalise without changing the tree.
fn respaced(src: &str, seed: u64) -> String {
    let lexed = roadbook_syntax::lexer::lex(FileId(0), src);
    let mut state = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    let mut out = String::new();
    for t in &lexed.tokens {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        out.push_str(["", " ", "  ", "\n", "\n\n", " \n    "][(state >> 33) as usize % 6]);
        if out.is_empty() || out.ends_with(|c: char| !c.is_whitespace()) {
            out.push(' ');
        }
        out.push_str(&src[t.span.range()]);
    }
    out
}

#[test]
fn round_trips_under_any_whitespace() {
    for (name, src) in common::CASES.iter().filter(|c| !c.1.contains("//")) {
        let expected = structure(src);
        for seed in 0..25 {
            let variant = respaced(src, seed);
            assert_eq!(structure(&variant), expected, "{name} {seed}:\n{variant}");
            check_stable(&variant);
        }
    }
}

/// A comment inserted before any token, on the same line, on a line of its own, or between
/// blank lines: the output is stable and keeps every comment and doc comment.
#[test]
fn stable_with_a_comment_anywhere() {
    let inputs = common::CASES
        .iter()
        .map(|c| c.1)
        .chain(LAYOUTS.iter().map(|c| c.0))
        .chain(COMMENTS.iter().map(|c| c.0));
    for src in inputs {
        let tokens = roadbook_syntax::lexer::lex(FileId(0), src).tokens;
        for t in &tokens {
            for comment in [" // z\n", "\n// z\n", "\n\n// z\n\n"] {
                let at = t.span.range().start;
                let variant = format!("{}{comment}{}", &src[..at], &src[at..]);
                let Ok(once) = format(&variant) else {
                    continue;
                };
                assert_eq!(format(&once).unwrap(), once, "not idempotent:\n{variant}");
                assert_eq!(
                    unordered_comments(structure(&once)),
                    unordered_comments(structure(&variant)),
                    "the tree or the comments changed:\n{variant}\n---\n{once}"
                );
            }
        }
    }
}

/// The tree with its comments sorted: a comment after a list's closer may move ahead of one
/// on the closer's line.
fn unordered_comments(mut v: serde_json::Value) -> serde_json::Value {
    if let Some(c) = v.get_mut("comments").and_then(|c| c.as_array_mut()) {
        c.sort_by_key(|x| x.to_string());
    }
    v
}
