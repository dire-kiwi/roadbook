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

/// A task's emitted events, through its `as` name or a collection of tasks, are the host's to
/// give, like a task's outcomes: a path through them assumes nothing, so a check that needs
/// one is proved. A child machine's emitted event is that machine's move, so the same check
/// through one holds only on that condition.
#[test]
fn task_emits_assume_nothing() {
    use roadbook_compiler::{MemoryLoader, Verdict, check};
    let src = "\
task ticks(every: duration) emits Tick
task upload(name: string) -> int emits Progress { percent: int }

machine Child() {
  emits Tick
  initial A
  state A { entry emit Tick }
}

machine Clock() {
  event Add
  children uploads: upload by string
  initial Waiting
  state Waiting {
    invoke ticks(1s) as clock
    on clock.Tick -> Ticked
  }
  state Ticked {
    on Add spawn uploads[\"a\"] = upload(\"a\")
    on uploads.Progress -> Sent
  }
  state Sent {
    on uploads.done -> Stored
  }
  final state Stored {}
  check reachable Ticked
  check reachable Sent
  check reachable Stored
}

machine Parent() {
  initial Waiting
  state Waiting {
    invoke Child() as child
    on child.Tick -> Ticked
  }
  final state Ticked {}
  check reachable Ticked
}
";
    let mut files = MemoryLoader::new().with("t.road", src);
    let out = check(&mut files, &["t.road".into()], &Options::default()).unwrap();
    let got: Vec<(&str, &str, Verdict)> = out
        .checks
        .iter()
        .map(|c| (c.machine.as_str(), c.check.as_str(), c.verdict))
        .collect();
    assert_eq!(
        got,
        [
            ("Clock", "reachable Ticked", Verdict::Proved),
            ("Clock", "reachable Sent", Verdict::Proved),
            ("Clock", "reachable Stored", Verdict::Proved),
            ("Parent", "reachable Ticked", Verdict::Conditional),
        ],
        "{}",
        out.render()
    );
}
