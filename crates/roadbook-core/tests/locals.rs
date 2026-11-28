//! Data local to a state (`spec/execution.md#data-local-to-a-state`): binding on entry,
//! arguments, re-entry, internal transitions, history records, finishing, defects, saved
//! snapshots and redaction.
//!
//! The charts are written with machine variables and turned into state locals by [`localize`],
//! which edits the compiled IR the way the compiler lowers a state's parameters, variables and
//! arguments, so these tests read the IR the runtimes are given.

use roadbook_core::{Command, Defect, Input, Program, Saved, Status};
use serde_json::{Map, Value, json};

/// The locals of one state: which machine variables become its parameters and its variables.
struct Locals<'a> {
    state: &'a str,
    params: &'a [&'a str],
    vars: &'a [&'a str],
}

/// Compiles `src`, then moves machine variables into states as `locals` says, drops the
/// initial values of the variables in `uninit`, points every `ref` and `set` of a moved name
/// at its state, and turns each `set a = e` of a variable `a` in `args` into an argument `e`
/// of `args[a]`'s state on its transition, in the order written.
fn localize(src: &str, locals: &[Locals<'_>], uninit: &[&str], args: &[(&str, &str)]) -> Program {
    let c = roadbook_compiler::compile_source("t.road", src, &Default::default());
    assert!(!c.has_errors(), "{}", c.render());
    let mut ir = serde_json::to_value(c.ir.unwrap()).unwrap();
    for m in ir["machines"].as_object_mut().unwrap().values_mut() {
        let mut vars: Vec<Value> = m["vars"].as_array().unwrap().clone();
        let mut take = |name: &str| {
            let i = vars.iter().position(|v| v["name"] == name).unwrap();
            vars.remove(i)
        };
        let mut owner: Vec<(String, String)> = Vec::new();
        for l in locals {
            let mut params = Vec::new();
            for &p in l.params {
                let v = take(p);
                params.push(json!({"name": v["name"], "type": v["type"],
                                   "sensitive": v["sensitive"], "doc": ""}));
                owner.push((p.to_owned(), l.state.to_owned()));
            }
            let mut svars = Vec::new();
            for &n in l.vars {
                let mut v = take(n);
                if uninit.contains(&n) {
                    v["init"] = Value::Null;
                }
                svars.push(v);
                owner.push((n.to_owned(), l.state.to_owned()));
            }
            let st = find(&mut m["root"], l.state).expect("a state of the chart");
            st["params"] = Value::Array(params);
            st["vars"] = Value::Array(svars);
        }
        for (a, _) in args {
            take(a);
        }
        m["vars"] = Value::Array(vars);
        scope(m, &owner);
        arguments(&mut m["root"], args);
    }
    Program::new(serde_json::from_value(ir).unwrap()).unwrap()
}

fn find<'v>(s: &'v mut Value, id: &str) -> Option<&'v mut Value> {
    if s["id"] == id {
        return Some(s);
    }
    s["states"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find_map(|c| find(c, id))
}

/// Gives every `ref` and `set` of a moved name its state.
fn scope(v: &mut Value, owner: &[(String, String)]) {
    match v {
        Value::Object(o) => {
            let of = |n: &Value| owner.iter().find(|(x, _)| n == x).map(|(_, s)| s.clone());
            if o.get("kind") == Some(&json!("ref")) && o.contains_key("path") {
                if let Some(s) = of(&o["path"][0]) {
                    o.insert("state".into(), json!(s));
                }
            }
            if o.get("kind") == Some(&json!("set")) {
                if let Some(s) = of(&o["var"]) {
                    o.insert("state".into(), json!(s));
                }
            }
            for x in o.values_mut() {
                scope(x, owner);
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| scope(x, owner)),
        _ => {}
    }
}

fn arguments(s: &mut Value, args: &[(&str, &str)]) {
    for t in s["transitions"].as_array_mut().unwrap() {
        let mut list: Vec<(String, Vec<Value>)> = Vec::new();
        let effects = t["effects"].as_array_mut().unwrap();
        effects.retain(|e| {
            let Some((_, state)) = args
                .iter()
                .find(|(a, _)| e["kind"] == "set" && e["var"] == *a)
            else {
                return true;
            };
            match list.iter_mut().find(|(s, _)| s == state) {
                Some((_, values)) => values.push(e["value"].clone()),
                None => list.push(((*state).to_owned(), vec![e["value"].clone()])),
            }
            false
        });
        t["args"] = list
            .into_iter()
            .map(|(state, values)| json!({"state": state, "values": values}))
            .collect();
    }
    for c in s["states"].as_array_mut().unwrap() {
        arguments(c, args);
    }
}

fn no_fns(name: &str, _: Vec<Value>) -> Result<Value, Defect> {
    panic!("unexpected call of {name}")
}

fn event(tag: &str) -> Input {
    Input::Event {
        value: json!({ "_tag": tag }),
    }
}

fn records(commands: &[Command]) -> Vec<Value> {
    commands
        .iter()
        .filter_map(|c| match c {
            Command::Trace { record } => Some(serde_json::to_value(record).unwrap()),
            _ => None,
        })
        .collect()
}

fn locals_of(s: &roadbook_core::Snapshot) -> Value {
    serde_json::to_value(&s.locals).unwrap()
}

const STEPS: &str = r#"
machine M() -> int {
  var n: int = 0
  var tries: int = n + 1
  var opt: int?
  var later: int = 0
  var r: int = 0
  var total: int = 0
  var argB: int = 0
  var argF: int = 0
  event Go
  event Again
  event Read
  event Stop
  initial A
  state A { on Go -> B  set argB = 5 }
  state B {
    entry set total = tries
    on Again -> B  set total = n, argB = total + tries
    on Read -> C  set total = later
    on Stop -> F  set argF = tries
  }
  state C {}
  final state F { output r }
}
"#;

fn steps() -> Program {
    localize(
        STEPS,
        &[
            Locals {
                state: "M.B",
                params: &["n"],
                vars: &["tries", "opt", "later"],
            },
            Locals {
                state: "M.F",
                params: &["r"],
                vars: &[],
            },
        ],
        &["later"],
        &[("argB", "M.B"), ("argF", "M.F")],
    )
}

#[test]
fn entering_binds_parameters_then_variables_each_with_a_set_record() {
    let p = steps();
    let (s, _) = p.start("M", "r1", Map::new(), &mut no_fns).unwrap();
    assert_eq!(locals_of(&s), json!({}));
    let (s, cmds) = p.step(&s, event("Go"), &mut no_fns);
    assert_eq!(
        records(&cmds),
        [
            json!({"kind": "event", "event": {"kind": "event", "value": {"_tag": "Go"}}}),
            json!({"kind": "transition", "from": "M.A", "index": 0, "to": ["M.B"]}),
            json!({"kind": "exited", "state": "M.A"}),
            json!({"kind": "entered", "state": "M.B"}),
            json!({"kind": "set", "var": "M.B.n", "value": 5}),
            json!({"kind": "set", "var": "M.B.tries", "value": 6}),
            json!({"kind": "set", "var": "M.B.opt", "value": null}),
            json!({"kind": "set", "var": "total", "value": 6}),
        ]
    );
    // `later` has no initial value and no optional type: it has no member.
    assert_eq!(
        locals_of(&s),
        json!({"M.B": {"n": 5, "tries": 6, "opt": null}})
    );
}

#[test]
fn arguments_see_the_transitions_sets_and_the_locals_of_the_state_left() {
    let p = steps();
    let (s, _) = p.start("M", "r1", Map::new(), &mut no_fns).unwrap();
    let (s, _) = p.step(&s, event("Go"), &mut no_fns);
    // `-> B(total + tries)  set total = n`: `total` is 5 once the effect has run, and `tries`
    // is still B's 6 though B was exited. Re-entering B binds everything again.
    let (s, cmds) = p.step(&s, event("Again"), &mut no_fns);
    assert_eq!(
        records(&cmds)[1..],
        [
            json!({"kind": "transition", "from": "M.B", "index": 0, "to": ["M.B"]}),
            json!({"kind": "exited", "state": "M.B"}),
            json!({"kind": "set", "var": "total", "value": 5}),
            json!({"kind": "entered", "state": "M.B"}),
            json!({"kind": "set", "var": "M.B.n", "value": 11}),
            json!({"kind": "set", "var": "M.B.tries", "value": 12}),
            json!({"kind": "set", "var": "M.B.opt", "value": null}),
            json!({"kind": "set", "var": "total", "value": 12}),
        ]
    );
    assert_eq!(
        locals_of(&s),
        json!({"M.B": {"n": 11, "tries": 12, "opt": null}})
    );
}

#[test]
fn reading_a_local_with_no_value_is_unassigned_by_its_qualified_name() {
    let p = steps();
    let (s, _) = p.start("M", "r1", Map::new(), &mut no_fns).unwrap();
    let (s, _) = p.step(&s, event("Go"), &mut no_fns);
    let (failed, cmds) = p.step(&s, event("Read"), &mut no_fns);
    assert_eq!(failed.status, Status::Defect);
    assert_eq!(
        records(&cmds),
        [json!({"kind": "defect", "defect": {"kind": "unassigned", "var": "M.B.later"}})]
    );
    // Atomic: the locals are those of the snapshot given.
    assert_eq!(locals_of(&failed), locals_of(&s));
}

#[test]
fn a_final_states_parameter_is_the_output_and_finishing_clears_the_locals() {
    let p = steps();
    let (s, _) = p.start("M", "r1", Map::new(), &mut no_fns).unwrap();
    let (s, _) = p.step(&s, event("Go"), &mut no_fns);
    let (done, cmds) = p.step(&s, event("Stop"), &mut no_fns);
    assert_eq!(done.status, Status::Done);
    assert_eq!(
        done.result,
        Some(roadbook_core::RunResult::Output(json!(6)))
    );
    assert!(done.locals.is_empty());
    let r = records(&cmds);
    assert!(r.contains(&json!({"kind": "set", "var": "M.F.r", "value": 6})));
    // Cancelling clears them too.
    let (cancelled, _) = p.step(&s, Input::Cancel, &mut no_fns);
    assert_eq!(cancelled.status, Status::Cancelled);
    assert!(cancelled.locals.is_empty());
}

#[test]
fn a_defect_in_an_argument_makes_the_step_atomic() {
    let p = localize(
        "machine M(big: int) {\n  var n: int = 0\n  var argB: int = 0\n  event Go\n  initial A\n  state A { on Go -> B  set argB = big * big }\n  state B {}\n}\n",
        &[Locals {
            state: "M.B",
            params: &["n"],
            vars: &[],
        }],
        &[],
        &[("argB", "M.B")],
    );
    let input = json!({"big": 1_i64 << 40}).as_object().unwrap().clone();
    let (s, _) = p.start("M", "r1", input, &mut no_fns).unwrap();
    let (failed, cmds) = p.step(&s, event("Go"), &mut no_fns);
    assert_eq!(failed.status, Status::Defect);
    assert_eq!(
        records(&cmds),
        [json!({"kind": "defect", "defect": {"kind": "overflow", "expr": "big * big"}})]
    );
    assert_eq!(failed.active, ["M", "M.A"]);
}

const HISTORY: &str = r#"
machine H() {
  var k: int = 0
  var c: int = 0
  var d: int = 0
  var argP: int = 0
  event In
  event Inc
  event Reset
  event Out
  event Back
  event DeepBack
  initial O
  state O {
    on In -> P  set argP = 7
    on Back -> P.S  set argP = 1
    on DeepBack -> P.D  set argP = 2
  }
  state P {
    initial X
    history S
    deep history D
    on Out -> O
    on Reset -> .X
    state X {
      initial X1
      state X1 { on Inc set c = c + 1, d = d + 1 }
    }
  }
}
"#;

fn history() -> Program {
    localize(
        HISTORY,
        &[
            Locals {
                state: "H.P",
                params: &["k"],
                vars: &[],
            },
            Locals {
                state: "H.P.X",
                params: &[],
                vars: &["c"],
            },
            Locals {
                state: "H.P.X.X1",
                params: &[],
                vars: &["d"],
            },
        ],
        &[],
        &[("argP", "H.P")],
    )
}

fn run(p: &Program, events: &[&str]) -> roadbook_core::Snapshot {
    let mut s = p.start("H", "r1", Map::new(), &mut no_fns).unwrap().0;
    for e in events {
        s = p.step(&s, event(e), &mut no_fns).0;
    }
    s
}

#[test]
fn an_internal_transition_keeps_its_sources_locals() {
    let p = history();
    let s = run(&p, &["In", "Inc", "Inc"]);
    assert_eq!(
        locals_of(&s),
        json!({"H.P": {"k": 7}, "H.P.X": {"c": 2}, "H.P.X.X1": {"d": 2}})
    );
    // `-> .X` in P: P keeps `k`; X and X1 are entered again, afresh.
    let (s, cmds) = p.step(&s, event("Reset"), &mut no_fns);
    assert_eq!(
        locals_of(&s),
        json!({"H.P": {"k": 7}, "H.P.X": {"c": 0}, "H.P.X.X1": {"d": 0}})
    );
    assert!(!records(&cmds).contains(&json!({"kind": "set", "var": "H.P.k", "value": 7})));
}

#[test]
fn history_states_record_and_restore_locals() {
    let p = history();
    let s = run(&p, &["In", "Inc", "Reset", "Inc", "Out"]);
    assert_eq!(locals_of(&s), json!({}));
    assert_eq!(
        serde_json::to_value(&s.history_locals).unwrap(),
        json!({
            "H.P.S": {"H.P.X": {"c": 1}},
            "H.P.D": {"H.P.X": {"c": 1}, "H.P.X.X1": {"d": 1}},
        })
    );
    // Shallow: X gets its `c` back with a `set` record; X1 is entered by default, afresh. P
    // gets its argument.
    let (back, cmds) = p.step(&s, event("Back"), &mut no_fns);
    assert_eq!(
        records(&cmds)[3..],
        [
            json!({"kind": "entered", "state": "H.P"}),
            json!({"kind": "set", "var": "H.P.k", "value": 1}),
            json!({"kind": "entered", "state": "H.P.X"}),
            json!({"kind": "set", "var": "H.P.X.c", "value": 1}),
            json!({"kind": "entered", "state": "H.P.X.X1"}),
            json!({"kind": "set", "var": "H.P.X.X1.d", "value": 0}),
        ]
    );
    assert_eq!(
        locals_of(&back),
        json!({"H.P": {"k": 1}, "H.P.X": {"c": 1}, "H.P.X.X1": {"d": 0}})
    );
    // Deep: every state below P gets its locals back.
    let s = p.step(&back, event("Inc"), &mut no_fns).0;
    let s = p.step(&s, event("Out"), &mut no_fns).0;
    let deep = p.step(&s, event("DeepBack"), &mut no_fns).0;
    assert_eq!(
        locals_of(&deep),
        json!({"H.P": {"k": 2}, "H.P.X": {"c": 2}, "H.P.X.X1": {"d": 1}})
    );
}

#[test]
fn a_record_without_locals_removes_the_old_one() {
    // A chart where only a sibling of X has locals: leaving from it records them, leaving from
    // X records none, and the old member goes.
    let p = localize(
        "machine H() {\n  var c: int = 3\n  event Go\n  event Out\n  event Back\n  initial O\n  state O { on Back -> P.S  on Out -> P }\n  state P {\n    initial X\n    history S\n    on Out -> O\n    state X { on Go -> Y }\n    state Y {}\n  }\n}\n",
        &[Locals {
            state: "H.P.Y",
            params: &[],
            vars: &["c"],
        }],
        &[],
        &[],
    );
    let s = p.start("H", "r1", Map::new(), &mut no_fns).unwrap().0;
    let s = p.step(&s, event("Out"), &mut no_fns).0;
    let s = p.step(&s, event("Go"), &mut no_fns).0;
    let s = p.step(&s, event("Out"), &mut no_fns).0;
    assert_eq!(
        serde_json::to_value(&s.history_locals).unwrap(),
        json!({"H.P.S": {"H.P.Y": {"c": 3}}})
    );
    let s = p.step(&s, event("Back"), &mut no_fns).0;
    assert_eq!(s.active, ["H", "H.P", "H.P.Y"]);
    let s = p.step(&s, event("Out"), &mut no_fns).0;
    let s = p.step(&s, event("Out"), &mut no_fns).0;
    let s = p.step(&s, event("Out"), &mut no_fns).0;
    assert_eq!(s.active, ["H", "H.O"]);
    assert!(s.history_locals.is_empty());
}

fn saved(s: &roadbook_core::Snapshot) -> Value {
    Saved::new(s, |_, delay| delay).to_value()
}

#[test]
fn saved_snapshots_keep_locals_and_restore_them() {
    let p = history();
    let s = run(&p, &["In", "Inc", "Out", "Back"]);
    let v = saved(&s);
    assert_eq!(v["locals"], locals_of(&s));
    assert_eq!(
        v["historyLocals"],
        serde_json::to_value(&s.history_locals).unwrap()
    );
    let (checked, _) = p.check_saved(v.clone(), Some("H"), None).unwrap();
    let (restored, cmds) = p.restore(&checked, "r2", 0, None, &mut no_fns).unwrap();
    assert_eq!(restored.locals, s.locals);
    assert_eq!(restored.history_locals, s.history_locals);
    assert_eq!(records(&cmds)[0]["snapshot"]["locals"], v["locals"]);
    // The restored run goes on with them.
    let after = p.step(&restored, event("Inc"), &mut no_fns).0;
    assert_eq!(after.locals["H.P.X"]["c"], json!(2));

    // A machine with no locals writes neither member.
    let fresh = saved(&run(&p, &[]));
    assert!(fresh.get("locals").is_none());
    assert!(fresh.get("historyLocals").is_none());
}

#[test]
fn snapshots_whose_locals_do_not_fit_are_invalid() {
    let p = history();
    let s = run(&p, &["In", "Inc", "Out", "Back"]);
    let base = saved(&s);
    let reason = |f: &dyn Fn(&mut Value)| {
        let mut v = base.clone();
        f(&mut v);
        match p.check_saved(v, None, None) {
            Ok(_) => "ok",
            Err(e) => e.reason(),
        }
    };
    assert_eq!(reason(&|_| {}), "ok");
    // The schema: a member that is not a state id, a name that is not a name.
    assert_eq!(reason(&|v| v["locals"]["nope"] = json!({})), "invalid");
    assert_eq!(reason(&|v| v["locals"]["H.P"]["K"] = json!(1)), "invalid");
    assert_eq!(
        reason(&|v| v["historyLocals"]["H.P.S"] = json!({})),
        "invalid"
    );
    // The machine: a missing state, one that is not active or declares nothing, a missing
    // parameter, a name that is no local, a value of the wrong type.
    assert_eq!(
        reason(&|v| {
            v["locals"].as_object_mut().unwrap().remove("H.P.X");
        }),
        "invalid"
    );
    assert_eq!(reason(&|v| v["locals"]["H.O"] = json!({})), "invalid");
    assert_eq!(
        reason(&|v| v["locals"]["H.P.X"] = json!({"c": 1, "z": 2})),
        "invalid"
    );
    assert_eq!(reason(&|v| v["locals"]["H.P"] = json!({})), "invalid");
    assert_eq!(
        reason(&|v| v["locals"]["H.P"]["k"] = json!("one")),
        "invalid"
    );
    // A variable may have no value.
    assert_eq!(reason(&|v| v["locals"]["H.P.X"] = json!({})), "ok");
    // History locals: only for a history state with a record, exactly the states with locals
    // it re-enters.
    assert_eq!(
        reason(&|v| {
            v["history"].as_object_mut().unwrap().remove("H.P.S");
        }),
        "invalid"
    );
    assert_eq!(
        reason(&|v| v["historyLocals"]["H.P.S"]["H.P.X.X1"] = json!({"d": 0})),
        "invalid"
    );
    assert_eq!(
        reason(&|v| {
            v["historyLocals"]["H.P.D"]
                .as_object_mut()
                .unwrap()
                .remove("H.P.X.X1");
        }),
        "invalid"
    );
    // Without them a snapshot of a run in P lacks P's locals.
    assert_eq!(
        reason(&|v| {
            v.as_object_mut().unwrap().remove("locals");
        }),
        "invalid"
    );
}

#[test]
fn sensitive_locals_are_redacted_in_set_and_restored_records() {
    let p = localize(
        "machine M() {\n  sensitive var pin: int = 0\n  var shown: int = 0\n  var argB: int = 0\n  event Go\n  initial A\n  state A { on Go -> B  set argB = 1234 }\n  state B {}\n}\n",
        &[Locals {
            state: "M.B",
            params: &["pin"],
            vars: &["shown"],
        }],
        &[],
        &[("argB", "M.B")],
    );
    let (s, _) = p.start("M", "r1", Map::new(), &mut no_fns).unwrap();
    let (s, cmds) = p.step(&s, event("Go"), &mut no_fns);
    let r = records(&cmds);
    assert!(r.contains(&json!({"kind": "set", "var": "M.B.pin", "value": "<redacted>"})));
    assert!(r.contains(&json!({"kind": "set", "var": "M.B.shown", "value": 0})));
    // The snapshot keeps the real value; the `restored` record shows it redacted.
    assert_eq!(s.locals["M.B"]["pin"], json!(1234));
    let (checked, _) = p.check_saved(saved(&s), None, None).unwrap();
    let (_, cmds) = p.restore(&checked, "r2", 0, None, &mut no_fns).unwrap();
    assert_eq!(
        records(&cmds)[0]["snapshot"]["locals"],
        json!({"M.B": {"pin": "<redacted>", "shown": 0}})
    );
}
