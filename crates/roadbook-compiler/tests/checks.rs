//! `check` lines are not part of the IR: a chart compiles to the same IR, with the same
//! machine hashes, with and without them.

use roadbook_compiler::{Options, compile_source};

const CHART: &str = "\
error Declined
task charge() ! Declined

/// A payment.
machine Pay() {
  event Go
  initial Idle
  check reachable Idle
  state Idle { on Go -> Paying }
  /// Never both.
  check never Both.Left and Both.Right.B
  state Paying {
    invoke charge() retry 2 after 1s {
      done -> Both
      fail Declined -> Done
    }
  }
  check reachable Paying.Backoff
  parallel state Both {
    state Left { on Go -> Done }
    state Right {
      initial B
      state B { on Go -> Done }
    }
  }
  final state Done {}
  check Done only after Idle
  check can finish
}

machine Other() {
  initial A
  final state A {}
  check reachable A
}
";

/// `src` with every check line, and its doc comment, blank: every other line stays where it
/// was, so every span is the same.
fn without_checks(src: &str) -> String {
    let lines: Vec<&str> = src.lines().collect();
    let mut out = lines.clone();
    for (i, l) in lines.iter().enumerate() {
        if l.trim_start().starts_with("check ") {
            out[i] = "";
            let mut j = i;
            while j > 0 && lines[j - 1].trim_start().starts_with("///") {
                j -= 1;
                out[j] = "";
            }
        }
    }
    out.join("\n") + "\n"
}

#[test]
fn checks_leave_the_ir_and_the_hashes_unchanged() {
    let plain = without_checks(CHART);
    assert!(!plain.contains("check"), "{plain}");
    let with = compile_source("pay.road", CHART, &Options::default());
    let without = compile_source("pay.road", &plain, &Options::default());
    assert!(with.diagnostics.is_empty(), "{}", with.render());
    assert!(without.diagnostics.is_empty(), "{}", without.render());
    let (with, without) = (with.ir.unwrap(), without.ir.unwrap());
    assert_eq!(with.machines.len(), 2);
    for (name, m) in &with.machines {
        assert!(!m.hash.is_empty());
        assert_eq!(m.hash, without.machines[name].hash, "{name}");
    }
    assert_eq!(with.to_json(), without.to_json());
}

#[test]
fn a_pass_chart_with_a_check_compiles_to_the_ir_without_it() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/pass/history.road");
    let src = std::fs::read_to_string(path).unwrap();
    assert!(src.contains("\n  check "), "the chart has a check");
    let with = compile_source("history.road", &src, &Options::default());
    let without = compile_source("history.road", &without_checks(&src), &Options::default());
    assert!(with.diagnostics.is_empty(), "{}", with.render());
    assert_eq!(with.ir.unwrap().to_json(), without.ir.unwrap().to_json());
}
