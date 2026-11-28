//! `roadbook paths`, run as a user would.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{Value, json};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn roadbook(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_roadbook"))
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// A fresh directory with a `roadbook.toml` and the given files.
fn project(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("roadbook-paths-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("roadbook.toml"), "").unwrap();
    for (path, src) in files {
        let p = dir.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, src).unwrap();
    }
    dir
}

/// A project holding a copy of the shop's charts, so that its machines' names are unique.
fn shop(name: &str) -> PathBuf {
    let read = |f: &str| std::fs::read_to_string(repo().join("examples/shop").join(f)).unwrap();
    let (types, checkout, place) = (
        read("types.road"),
        read("checkout.road"),
        read("place_order.road"),
    );
    project(
        &format!("shop-{name}"),
        &[
            ("shop/types.road", &types),
            ("shop/checkout.road", &checkout),
            ("shop/place_order.road", &place),
        ],
    )
}

fn json_of(out: &Output) -> Value {
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    serde_json::from_slice(&out.stdout).unwrap()
}

/// Each path as `Owner#index>Owner#index…`, ids without the machine.
fn steps(v: &Value) -> Vec<String> {
    let machine = v["machine"].as_str().unwrap().to_string() + ".";
    v["paths"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            let s: Vec<String> = p["steps"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| {
                    let from = s["from"].as_str().unwrap();
                    format!(
                        "{}#{}",
                        from.strip_prefix(&machine).unwrap_or(from),
                        s["index"]
                    )
                })
                .collect();
            s.join(">")
        })
        .collect()
}

const CHECKOUT: &str = "Checkout: 6 paths
1. Charging --done--> Charged (final: output receipt)
2. Charging --fail CardDeclined--> Declined (final: fail declined)
3. Charging
     --fail GatewayUnavailable [attempts < 3]--> Backoff
     --after retryDelay(attempts)--> Charging
     --done--> Charged (final: output receipt)
4. Charging
     --fail GatewayUnavailable [attempts < 3]--> Backoff
     --after retryDelay(attempts)--> Charging
     --fail CardDeclined--> Declined (final: fail declined)
5. Charging
     --fail GatewayUnavailable [attempts < 3]--> Backoff
     --after retryDelay(attempts)--> Charging
     --fail GatewayUnavailable [else]--> GaveUp (final: fail GatewayUnavailable)
6. Charging --fail GatewayUnavailable [else]--> GaveUp (final: fail GatewayUnavailable)
";

#[test]
fn checkout_has_six_paths_each_loop_taken_once() {
    // From a file, in the repository.
    let out = roadbook(
        &repo(),
        &["paths", "Checkout", "examples/shop/checkout.road"],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), CHECKOUT);
    assert_eq!(text(&out.stderr), "");
    // Imported by the file given.
    let out = roadbook(
        &repo(),
        &["paths", "Checkout", "examples/shop/place_order.road"],
    );
    assert_eq!(text(&out.stdout), CHECKOUT);
    // Found in the project.
    let dir = shop("checkout");
    let out = roadbook(&dir, &["paths", "Checkout"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), CHECKOUT);
    assert_eq!(text(&out.stderr), "");
    let out = roadbook(&dir.join("shop"), &["paths", "Checkout"]);
    assert_eq!(text(&out.stdout), CHECKOUT);
}

#[test]
fn checkout_json_names_transitions_as_traces_do() {
    let out = roadbook(
        &repo(),
        &["paths", "Checkout", "examples/shop/checkout.road", "--json"],
    );
    let v = json_of(&out);
    assert_eq!(v["machine"], "Checkout");
    assert_eq!(v["truncated"], false);
    assert_eq!(
        steps(&v),
        [
            "Charging#0",
            "Charging#1",
            "Charging#2>Backoff#0>Charging#0",
            "Charging#2>Backoff#0>Charging#1",
            "Charging#2>Backoff#0>Charging#3",
            "Charging#3",
        ]
    );
    assert_eq!(
        v["paths"][2],
        json!({
            "end": "final",
            "state": "Checkout.Charged",
            "steps": [
                { "from": "Checkout.Charging", "index": 2, "trigger": "fail GatewayUnavailable",
                  "guard": "attempts < 3", "to": ["Checkout.Backoff"] },
                { "from": "Checkout.Backoff", "index": 0, "trigger": "after retryDelay(attempts)",
                  "guard": null, "to": ["Checkout.Charging"] },
                { "from": "Checkout.Charging", "index": 0, "trigger": "done",
                  "guard": null, "to": ["Checkout.Charged"] },
            ]
        })
    );
    assert_eq!(v["paths"][5]["steps"][0]["guard"], "else");
    let ends: Vec<&str> = v["paths"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["state"].as_str().unwrap())
        .collect();
    assert_eq!(
        ends,
        [
            "Checkout.Charged",
            "Checkout.Declined",
            "Checkout.Charged",
            "Checkout.Declined",
            "Checkout.GaveUp",
            "Checkout.GaveUp"
        ]
    );
    // The members in the documented order.
    let raw = text(&out.stdout);
    assert!(raw.starts_with("{\n  \"machine\": \"Checkout\",\n  \"paths\": ["));
    assert!(raw.contains(
        "{\"from\": \"Checkout.Charging\", \"index\": 0, \"trigger\": \"done\", \"guard\": null, \"to\": [\"Checkout.Charged\"]}"
    ));
}

#[test]
fn place_order_reaches_every_final_and_the_compounds_transitions_apply_inside() {
    let out = roadbook(
        &shop("place-order-json"),
        &["paths", "PlaceOrder", "--json"],
    );
    let v = json_of(&out);
    let paths = v["paths"].as_array().unwrap();
    assert_eq!(paths.len(), 11, "{}", text(&out.stdout));
    assert!(paths.iter().all(|p| p["end"] == "final"));
    for end in [
        "Confirmed",
        "Unauthorized",
        "Invalid",
        "PaymentFailed",
        "Cancelled",
        "TimedOut",
    ] {
        let id = format!("PlaceOrder.{end}");
        assert!(paths.iter().any(|p| p["state"] == id.as_str()), "{end}");
    }
    // `Cancel` and the 30 second budget are Processing's (indexes 0 and 1), taken from each
    // state inside it but Confirming, whose `always` is taken at once: the step before
    // names where the path was by its target.
    for index in [0, 1] {
        let mut before: Vec<&str> = paths
            .iter()
            .filter_map(|p| {
                let s = p["steps"].as_array().unwrap();
                let last = s.last().unwrap();
                (last["from"] == "PlaceOrder.Processing" && last["index"] == index)
                    .then(|| s[s.len() - 2]["to"][0].as_str().unwrap())
            })
            .collect();
        before.sort();
        assert_eq!(
            before,
            [
                "PlaceOrder.Processing",
                "PlaceOrder.Processing.Paying",
                "PlaceOrder.Processing.Pricing"
            ]
        );
    }
    let t = roadbook(&shop("place-order"), &["paths", "PlaceOrder"]);
    let t = text(&t.stdout);
    assert!(t.starts_with(
        "PlaceOrder: 11 paths\n1. Authenticating\n     --done--> Processing.Validating\n"
    ));
    assert!(t.contains("     --Cancel--> Cancelled (final: output problem(499, \"cancelled\"))\n"));
    assert!(t.ends_with(
        "11. Authenticating\n      --fail Unauthenticated--> Unauthorized (final: output problem(401, \"unauthenticated\"))\n"
    ));
}

const LOOP: &str = "machine Loop() {
  event Retry
  event Give
  event Finish
  event Next

  initial Work

  state Work {
    initial Trying
    done -> Done

    state Trying {
      on Retry  -> Waiting
      on Finish -> Finished
      on Next   -> Ping
    }

    state Waiting {
      on Retry -> Trying
      on Give  -> Stuck
    }

    final state Finished {}
  }

  state Ping { on Next -> Pong }
  state Pong { on Next -> Ping }
  state Stuck {}
  final state Done {}
}
";

#[test]
fn loops_are_taken_once_stuck_states_end_and_nested_finals_take_the_parents_done() {
    let dir = project("loop", &[("loop.road", LOOP)]);
    let v = json_of(&roadbook(&dir, &["paths", "Loop", "--json"]));
    // Trying#2 (Next) leads only round Ping and Pong: those paths end where they would loop.
    assert_eq!(
        steps(&v),
        [
            "Work.Trying#0>Work.Waiting#0>Work.Trying#1>Work#0",
            "Work.Trying#0>Work.Waiting#0>Work.Trying#2>Ping#0>Pong#0",
            "Work.Trying#0>Work.Waiting#1",
            "Work.Trying#1>Work#0",
            "Work.Trying#2>Ping#0>Pong#0",
        ]
    );
    let ends: Vec<(&str, &str)> = v["paths"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p["end"].as_str().unwrap(), p["state"].as_str().unwrap()))
        .collect();
    assert_eq!(
        ends,
        [
            ("final", "Loop.Done"),
            ("loop", "Loop.Ping"),
            ("stuck", "Loop.Stuck"),
            ("final", "Loop.Done"),
            ("loop", "Loop.Ping"),
        ]
    );
    assert_eq!(v["paths"][3]["steps"][0]["to"], serde_json::json!(["Loop.Work.Finished"]));
    assert_eq!(v["paths"][3]["steps"][1]["trigger"], "done");
    let out = roadbook(&dir, &["paths", "Loop", "loop.road"]);
    assert_eq!(
        text(&out.stdout),
        "Loop: 5 paths
1. Work.Trying
     --Retry--> Work.Waiting
     --Retry--> Work.Trying
     --Finish--> Work.Finished
     --done--> Done (final)
2. Work.Trying
     --Retry--> Work.Waiting
     --Retry--> Work.Trying
     --Next--> Ping
     --Next--> Pong
     --Next--> Ping (loops)
3. Work.Trying --Retry--> Work.Waiting --Give--> Stuck (stuck)
4. Work.Trying --Finish--> Work.Finished --done--> Done (final)
5. Work.Trying --Next--> Ping --Next--> Pong --Next--> Ping (loops)
"
    );
}

/// A call's outcomes are steps like a task's, `Unanswered` among them, and a callee's `on c`
/// is a step labelled `c`, like an event; an `on c` with no target (a reply at once) is none.
#[test]
fn calls_are_steps_of_the_caller_and_of_the_callee() {
    let chart = "spec/conformance/charts/calls.road";
    let out = roadbook(&repo(), &["paths", "Order", chart]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout),
        "Order: 4 paths
1. Reserving --done--> Reserved (final: output reservation)
2. Reserving --fail OutOfStock--> Failed (final: fail problem)
3. Reserving --fail Unanswered--> Failed (final: fail problem)
4. Reserving --after 5s--> Failed (final: fail problem)
"
    );
    let out = roadbook(&repo(), &["paths", "Counter", chart]);
    assert_eq!(
        text(&out.stdout),
        "Counter: 2 paths\n1. Asking --done--> Got (final: output n)\n2. Asking --fail *--> Gave (final: output 0)\n"
    );
    let v = json_of(&roadbook(&repo(), &["paths", "Inventory", chart, "--json"]));
    assert_eq!(
        v["paths"][4],
        json!({
            "end": "final",
            "state": "Inventory.Closed",
            "steps": [
                { "from": "Inventory.Ready", "index": 0, "trigger": "reserve",
                  "guard": null, "to": ["Inventory.Reserving"] },
                { "from": "Inventory.Reserving", "index": 4, "trigger": "Close",
                  "guard": null, "to": ["Inventory.Closed"] },
            ]
        })
    );
    // Neither `on count` (the machine's, internal) nor Reserving's `on reserve` is a step.
    let triggers: HashSet<&str> = v["paths"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|p| p["steps"].as_array().unwrap())
        .map(|s| s["trigger"].as_str().unwrap())
        .collect();
    assert_eq!(
        triggers,
        HashSet::from([
            "reserve",
            "done",
            "fail OutOfStock",
            "Pause",
            "Resume",
            "Close"
        ])
    );
}

/// A peer's end, `on x.ended`, is a step labelled `x.ended` with its guards, as an emitted event
/// would be; through a variable it names the variable.
#[test]
fn a_peer_s_end_is_a_step() {
    let chart = "spec/conformance/charts/ended.road";
    let out = roadbook(&repo(), &["paths", "Order", chart]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout),
        "Order: 4 paths
1. Waiting --inventory.ended [event.status == Ended.Done]--> Closed (final)
2. Waiting --inventory.ended [event.status == Ended.Defect]--> Alarm (final)
3. Waiting --inventory.ended [else]--> Gone (final)
4. Waiting --after 5s--> TimedOut (final)
"
    );
    let v = json_of(&roadbook(&repo(), &["paths", "Switcher", chart, "--json"]));
    assert_eq!(
        v["paths"],
        json!([{
            "end": "final",
            "state": "Switcher.Gone",
            "steps": [{ "from": "Switcher.Watching", "index": 1, "trigger": "current.ended",
                        "guard": null, "to": ["Switcher.Gone"] }]
        }])
    );
}

#[test]
fn a_machine_with_no_final_or_stuck_state_lists_its_loops() {
    let chart = "machine AB() {\n  event Go\n  event Back\n  initial A\n  state A { on Go -> B }\n  state B {\n    on Go   -> A\n    on Back -> A\n  }\n}\n";
    let dir = project("ab", &[("ab.road", chart)]);
    let out = roadbook(&dir, &["paths", "AB"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout),
        "AB: 2 paths\n1. A --Go--> B --Go--> A (loops)\n2. A --Go--> B --Back--> A (loops)\n"
    );
    let v = json_of(&roadbook(&dir, &["paths", "AB", "--json"]));
    assert_eq!(v["paths"][0]["end"], "loop");
    assert_eq!(v["paths"][0]["state"], "AB.A");
}

#[test]
fn a_stuck_start_is_one_path_with_no_steps() {
    let chart = "machine S() {\n  initial A\n  state A {}\n}\n";
    let dir = project("stuck-start", &[("s.road", chart)]);
    let out = roadbook(&dir, &["paths", "S"]);
    assert_eq!(text(&out.stdout), "S: 1 path\n1. A (stuck)\n");
}

#[test]
fn a_machine_in_a_project_local_package_is_one_declaration() {
    // The package's file is a file of the project too: compiled as itself, and imported by
    // another chart through the package, it is still one declaration.
    let dir = project(
        "package",
        &[
            ("pkgs/inv/inv.road", LOOP),
            (
                "app.road",
                "import { Loop } from \"inv/inv.road\"\n\nmachine App() {\n  initial A\n  state A {\n    invoke Loop() {\n      done -> B\n    }\n  }\n  final state B {}\n}\n",
            ),
        ],
    );
    std::fs::write(
        dir.join("roadbook.toml"),
        "[packages]\ninv = \"pkgs/inv\"\n",
    )
    .unwrap();
    let out = roadbook(&dir, &["paths", "Loop"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(text(&out.stdout).starts_with("Loop: 5 paths\n"));
}

#[test]
fn a_parallel_state_is_one_position() {
    let chart = "machine Par() {
  event Stop

  initial Both

  parallel state Both {
    on Stop -> Halted
    done    -> Finished

    state Left {
      initial L1
      state L1 { on Stop -> L2 }
      final state L2 {}
    }

    state Right {
      initial R1
      final state R1 {}
    }
  }

  final state Halted {}
  final state Finished {}
}
";
    let dir = project("parallel", &[("par.road", chart)]);
    let out = roadbook(&dir, &["paths", "Par"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout),
        "Par: 2 paths\n1. Both --Stop--> Halted (final)\n2. Both --done--> Finished (final)\n"
    );
}

#[test]
fn a_transition_in_a_region_that_leaves_the_parallel_state_is_a_step_from_it() {
    // Both is left only from inside Left; Right's Go stays inside it, so is not a step.
    let chart = "machine Par() {
  event Go
  event Stop

  initial Both

  parallel state Both {
    state Left {
      initial L1
      state L1 { on Stop -> Out }
    }

    state Right {
      initial R1
      state R1 { on Go -> R2 }
      state R2 { on Go -> R1 }
    }
  }

  final state Out {}
}
";
    let dir = project("parallel-region", &[("par.road", chart)]);
    let out = roadbook(&dir, &["paths", "Par"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(text(&out.stderr), "");
    assert_eq!(
        text(&out.stdout),
        "Par: 1 path\n1. Both --Stop--> Out (final)\n"
    );
    let v = json_of(&roadbook(&dir, &["paths", "Par", "--json"]));
    assert_eq!(v["paths"][0]["steps"][0]["from"], "Par.Both.Left.L1");
}

#[test]
fn a_history_state_returns_to_where_its_parent_was_on_the_path() {
    // The conformance chart's `Bounce` leaves `P` from `B` while settling and goes back to it
    // through `P.H`; `Region` goes back into a parallel state through a region's history state.
    let chart = "spec/conformance/charts/history.road";
    let out = roadbook(&repo(), &["paths", "Bounce", chart]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout),
        "Bounce: 1 path\n1. P.A --Go--> P.B --always [n == 0]--> Q --always--> P.B (history P.H) (loops)\n"
    );
    let v = json_of(&roadbook(&repo(), &["paths", "Bounce", chart, "--json"]));
    // A step's `to` is the history state, as a trace's `transition` record names it.
    assert_eq!(
        v["paths"][0]["steps"][2],
        json!({ "from": "Bounce.Q", "index": 0, "trigger": "always", "guard": null, "to": ["Bounce.P.H"] })
    );
    // `Steps`: paused in `S1`, the deep history state restores `S1`.
    let out = roadbook(&repo(), &["paths", "Steps", chart]);
    assert!(
        text(&out.stdout)
            .contains("--Pause--> Paused\n     --Resume--> Flow.S1 (deep history Flow.H)\n"),
        "{}",
        text(&out.stdout)
    );
    let out = roadbook(
        &repo(),
        &[
            "paths",
            "Region",
            "spec/conformance/charts/history-regions.road",
        ],
    );
    assert_eq!(
        text(&out.stdout),
        "Region: 1 path\n1. P --Out--> Away --Back--> P (deep history P.A.H) (loops)\n"
    );
}

#[test]
fn max_stops_the_listing_and_says_so() {
    let chart = "machine Many() {
  event A
  event B
  event C
  event D

  initial S

  state S {
    on A -> X
    on B -> X
    on C -> X
    on D -> X
  }

  final state X {}
}
";
    let dir = project("max", &[("many.road", chart)]);
    let out = roadbook(&dir, &["paths", "Many", "--max", "3"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout),
        "Many: 3 paths\n1. S --A--> X (final)\n2. S --B--> X (final)\n3. S --C--> X (final)\n"
    );
    assert_eq!(
        text(&out.stderr),
        "stopped after 3 paths; the machine has more: use --max to raise it\n"
    );
    let v = json_of(&roadbook(&dir, &["paths", "Many", "--max", "3", "--json"]));
    assert_eq!(v["paths"].as_array().unwrap().len(), 3);
    assert_eq!(v["truncated"], true);
    // Exactly as many as there are: not truncated.
    let out = roadbook(&dir, &["paths", "Many", "--max", "4"]);
    assert_eq!(text(&out.stderr), "");
    assert!(text(&out.stdout).starts_with("Many: 4 paths\n"));
    let out = roadbook(&dir, &["paths", "Many", "--max", "0"]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn an_unknown_or_ambiguous_machine_is_a_usage_error() {
    let out = roadbook(&repo(), &["paths", "Nope", "examples/shop/checkout.road"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        text(&out.stderr),
        "error: no machine `Nope` in `examples/shop/checkout.road` (it has: Checkout)\n"
    );
    let out = roadbook(&shop("unknown"), &["paths", "Nope"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        text(&out.stderr),
        "error: no machine `Nope` in the project: none of its 3 `.road` files declares it\n"
    );
    // The repository has several machines named `Checkout`.
    let out = roadbook(&repo(), &["paths", "Checkout"]);
    assert_eq!(out.status.code(), Some(2));
    let err = text(&out.stderr);
    assert!(
        err.contains("examples/shop/checkout.road:4, spec/conformance/charts/shop/checkout.road:4"),
        "{err}"
    );
    assert!(err.contains("roadbook paths Checkout <file>"), "{err}");
    let out = roadbook(&repo(), &["paths", "Checkout", "no/such.road"]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn compile_errors_exit_1() {
    let bad = "machine Bad() {\n  initial Nowhere\n}\n";
    let dir = project("errors", &[("bad.road", bad), ("loop.road", LOOP)]);
    for args in [&["paths", "Loop"][..], &["paths", "Bad", "bad.road"]] {
        let out = roadbook(&dir, args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        assert!(
            text(&out.stderr).contains("bad.road"),
            "{}",
            text(&out.stderr)
        );
        assert_eq!(text(&out.stdout), "");
    }
}
