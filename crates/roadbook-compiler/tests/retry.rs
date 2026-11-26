//! `retry` is written out as explicit states: a chart with `retry` and the same chart with the
//! states written by hand compile to machines with the same hash.

use std::path::Path;

use roadbook_compiler::{Compilation, MemoryLoader, Options, compile};

const TYPES: &str = "error Busy
error Gone { reason: string }
error Other

fn backoff(attempt: int) -> duration
action log(what: string)
task fetch(id: string) -> string ! Busy | Gone
task ping() ! Busy
";

fn build(machine: &str) -> Compilation {
    let src = format!("{TYPES}\n{machine}");
    let mut loader = MemoryLoader::new().with("main.road", src);
    compile(&mut loader, "main.road", &Options::default()).unwrap()
}

fn hash(machine: &str) -> String {
    let out = build(machine);
    assert!(!out.has_errors(), "{}\n{}", machine, out.render());
    out.ir.unwrap().machines["M"].hash.clone()
}

fn same(sugar: &str, explicit: &str) {
    assert_eq!(hash(sugar), hash(explicit), "{sugar}");
}

#[test]
fn a_delay_and_one_retried_error() {
    same(
        "machine M(id: string) -> string ! Busy | Gone {
  var body: string
  initial Loading
  state Loading {
    entry do log(\"in\")
    invoke fetch(id) retry 2 on Busy after backoff(attempt) {
      done      -> Done  set body = result
      fail Busy -> Failed
      fail Gone -> Failed
    }
    on * do log(\"no events\")
    after 10s -> Failed
  }
  final state Done { output body }
  final state Failed { fail Busy }
}",
        "machine M(id: string) -> string ! Busy | Gone {
  var body: string
  var loadingAttempt: int = 0
  initial Loading
  state Loading {
    entry set loadingAttempt = 0  do log(\"in\")
    initial Attempt
    on * do log(\"no events\")
    after 10s -> Failed
    state Attempt {
      invoke fetch(id) {
        done -> Done  set body = result
        fail Busy {
          if loadingAttempt < 2 -> Backoff  set loadingAttempt = loadingAttempt + 1
          else -> Failed
        }
        fail Gone -> Failed
      }
    }
    state Backoff { after backoff(loadingAttempt) -> Attempt }
  }
  final state Done { output body }
  final state Failed { fail Busy }
}",
    );
}

#[test]
fn no_delay_every_error_fail_any_and_guarded_branches() {
    same(
        "machine M(id: string) -> string ! Busy | Gone {
  var body: string = \"\"
  initial Loading
  state Loading {
    invoke fetch(id) retry 1 {
      done -> Done  set body = result
      fail * {
        if body == \"\" -> Failed
      }
    }
  }
  final state Done { output body }
  final state Failed { fail Busy }
}",
        "machine M(id: string) -> string ! Busy | Gone {
  var body: string = \"\"
  var loadingAttempt: int = 0
  initial Loading
  state Loading {
    entry set loadingAttempt = 0
    initial Attempt
    state Attempt {
      invoke fetch(id) {
        done -> Done  set body = result
        fail * {
          if loadingAttempt < 1 -> Attempt  set loadingAttempt = loadingAttempt + 1
          if body == \"\" -> Failed
        }
      }
    }
  }
  final state Done { output body }
  final state Failed { fail Busy }
}",
    );
}

#[test]
fn nested_states_and_taken_names() {
    // The counter's name is the state's path, with a number when variables have taken it.
    same(
        "machine M() ! Busy {
  var outerInnerAttempt: int = 0
  var outerInnerAttempt2: int = 0
  initial Outer
  state Outer {
    initial Inner
    state Inner {
      invoke ping() retry 3 after backoff(attempt + outerInnerAttempt2) {
        done      -> Waiting
        fail Busy -> Waiting
      }
    }
  }
  state Waiting { on * -> Outer }
}",
        "machine M() ! Busy {
  var outerInnerAttempt: int = 0
  var outerInnerAttempt2: int = 0
  var outerInnerAttempt3: int = 0
  initial Outer
  state Outer {
    initial Inner
    state Inner {
      entry set outerInnerAttempt3 = 0
      initial Attempt
      state Attempt {
        invoke ping() {
          done -> Waiting
          fail Busy {
            if outerInnerAttempt3 < 3 -> Backoff  set outerInnerAttempt3 = outerInnerAttempt3 + 1
            else -> Waiting
          }
        }
      }
      state Backoff { after backoff(outerInnerAttempt3 + outerInnerAttempt2) -> Attempt }
    }
  }
  state Waiting { on * -> Outer }
}",
    );
}

#[test]
fn outcome_targets_are_resolved_where_they_are_written() {
    // `-> Backoff` in the outcome is the machine's `Backoff`, not the generated child, which
    // a chart written by hand could not reach from inside `A`.
    let out = build(
        "machine M() ! Busy {
  initial A
  state A {
    invoke ping() retry 1 after 1s {
      done      -> Backoff
      fail Busy -> Backoff
    }
  }
  state Backoff { on * -> A }
}",
    );
    assert!(!out.has_errors(), "{}", out.render());
    let ir = out.ir.unwrap();
    let attempt = &ir.machines["M"].root.states[0].states[0];
    let targets: Vec<_> = attempt
        .transitions
        .iter()
        .map(|t| t.target.as_deref())
        .collect();
    assert_eq!(
        targets,
        [Some("M.Backoff"), Some("M.A.Backoff"), Some("M.Backoff")]
    );
}

#[test]
fn the_generated_states_and_variable_cannot_be_named() {
    let out = build(
        "machine M() ! Busy {
  initial A
  state A {
    invoke ping() retry 1 after backoff(aAttempt) {
      done      -> A.Attempt
      fail Busy -> B  set aAttempt = 1
    }
  }
  final state B { fail Busy }
}",
    );
    let codes: Vec<String> = out.diagnostics.iter().map(|d| d.code.to_string()).collect();
    assert_eq!(codes, ["RB0105", "RB0201", "RB0105"], "{}", out.render());
}

#[test]
fn the_generated_parts_point_at_the_retry_clause() {
    let src = "machine M() ! Busy {
  initial A
  state A {
    invoke ping() retry 1 after 1s {
      done      -> B
      fail Busy -> B
    }
  }
  final state B { fail Busy }
}";
    let out = build(src);
    assert!(!out.has_errors(), "{}", out.render());
    let ir = out.ir.unwrap();
    let m = &ir.machines["M"];
    let line = TYPES.lines().count() as u32 + 5;
    let v = &m.vars[0];
    assert_eq!(v.name, "aAttempt");
    assert_eq!((v.span.line, v.span.col, v.span.end_col), (line, 19, 35));
    let a = &m.root.states[0];
    assert_eq!(a.states[0].span.line, line);
    assert_eq!(a.states[0].span.col, 5);
    assert_eq!(a.states[1].span, v.span);
    assert_eq!(a.states[0].transitions[1].span, v.span);
}

#[test]
fn the_language_page_example() {
    // The page's `retry` example and what it says it compiles to, each in place of
    // `Charging` and `Backoff` in the shop's `Checkout`.
    let page = include_str!("../../../docs/design/language.md");
    let block = |start: &str| {
        let i = page.find(&format!("```\n{start}")).unwrap() + 4;
        let body = &page[i..];
        body[..body.find("```").unwrap()].to_string()
    };
    let indent = |s: &str| -> String {
        s.lines()
            .map(|l| {
                if l.is_empty() {
                    "\n".into()
                } else {
                    format!("  {l}\n")
                }
            })
            .collect()
    };
    let sugar = block("state Charging {\n  invoke chargeCard(order, user) retry");
    let explicit = block("var chargingAttempt");
    let shop = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/shop");
    let checkout = std::fs::read_to_string(shop.join("checkout.road")).unwrap();
    let types = std::fs::read_to_string(shop.join("types.road")).unwrap();
    let start = checkout.find("  /// One attempt").unwrap();
    let end = checkout.find("  final state Charged").unwrap();
    let with = |states: &str| {
        let text = format!(
            "{}{}\n{}",
            &checkout[..start],
            indent(states),
            &checkout[end..]
        );
        let mut loader = MemoryLoader::new()
            .with("examples/shop/checkout.road", text)
            .with("examples/shop/types.road", types.clone());
        let out = compile(
            &mut loader,
            "examples/shop/checkout.road",
            &Options::default(),
        )
        .unwrap();
        assert!(!out.has_errors(), "{}", out.render());
        out.ir.unwrap().machines["Checkout"].hash.clone()
    };
    // `attempts` is left unused by both.
    assert_eq!(with(&sugar), with(&explicit));
}
