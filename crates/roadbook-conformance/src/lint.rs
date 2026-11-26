//! Structural checks of a trace that need no runtime.
//!
//! These catch the kinds of mistake a hand-derived or a buggy trace makes:
//! an `exited` that does not match the innermost active state, an invoke
//! cancelled twice, a timer left running when the run ends, a child run
//! whose result never reaches its parent. They do not decide which
//! transition is right; that is what the expected traces are for.

use std::collections::{BTreeMap, HashMap};

use roadbook_ir::{Ir, State};
use serde_json::Value;

use crate::{ROOT_RUN, Status, arrange, compare_run_ids};

#[derive(Default)]
struct Run {
    machine: String,
    parent: Option<String>,
    stack: Vec<String>,
    invokes: BTreeMap<u64, (String, String)>,
    started_invokes: HashMap<u64, (String, String)>,
    next_invoke: u64,
    timers: BTreeMap<u64, (String, u64)>,
    next_timer: u64,
    last_t: u64,
    end: Option<Value>,
    /// Per invoke id: the input kind and payload the run took for it, or
    /// `"cancelled"`.
    invoke_ends: HashMap<u64, (String, Value)>,
    expect: Option<&'static [&'static str]>,
}

fn str_of<'a>(r: &'a Value, k: &str) -> &'a str {
    r.get(k).and_then(Value::as_str).unwrap_or("")
}

fn u64_of(r: &Value, k: &str) -> u64 {
    r.get(k).and_then(Value::as_u64).unwrap_or(0)
}

fn find_state<'a>(s: &'a State, id: &str) -> Option<&'a State> {
    if s.id == id {
        return Some(s);
    }
    s.states.iter().find_map(|c| find_state(c, id))
}

/// Checks `records` (a whole trace, as compared) and returns one message per
/// problem. `status` is the root run's expected status. With `ir`, state
/// ids, transitions, invokes and hashes are also checked against it.
pub fn lint(records: &[Value], status: Status, ir: Option<&Ir>) -> Vec<String> {
    let mut problems = Vec::new();
    let mut runs: HashMap<String, Run> = HashMap::new();
    let mut order: Vec<String> = Vec::new();

    if arrange(records.to_vec()) != records {
        problems.push("records are not grouped by run in run order".to_owned());
    }

    for (i, r) in records.iter().enumerate() {
        let line = i + 1;
        let mut bad = |m: String| problems.push(format!("line {line}: {m}"));
        let id = str_of(r, "run").to_owned();
        let kind = str_of(r, "kind");
        let t = u64_of(r, "t");

        if !runs.contains_key(&id) {
            if kind != "started" {
                bad(format!("run {id} does not begin with `started`"));
            }
            let machine = str_of(r, "machine").to_owned();
            let parent = r.get("parent").and_then(Value::as_str).map(str::to_owned);
            match (&parent, id.rsplit_once('.')) {
                (None, None) if order.is_empty() && id == ROOT_RUN => {}
                (None, _) => bad(format!("run {id} is not the root run {ROOT_RUN}")),
                (Some(p), Some((prefix, n))) if p == prefix => {
                    let n: u64 = n.parse().unwrap_or(0);
                    match runs.get(p).and_then(|pr| pr.started_invokes.get(&n)) {
                        Some((_, src)) if *src == machine => {}
                        _ => bad(format!(
                            "run {id}: run {p} started no invoke {n} of {machine}"
                        )),
                    }
                }
                (Some(p), _) => bad(format!("run {id} cannot have parent {p}")),
            }
            if let Some(ir) = ir {
                match ir.machines.get(&machine) {
                    None => bad(format!("machine {machine} is not in the IR")),
                    Some(m) if m.hash != str_of(r, "ir") => {
                        bad(format!("`ir` of {machine} is not its hash {}", m.hash))
                    }
                    Some(_) => {}
                }
            }
            runs.insert(
                id.clone(),
                Run {
                    machine,
                    parent,
                    next_invoke: 1,
                    next_timer: 1,
                    ..Run::default()
                },
            );
            order.push(id.clone());
            continue;
        }

        let run = runs.get_mut(&id).expect("inserted above");
        if run.end.is_some() {
            bad(format!("run {id} has a record after it ended"));
        }
        if t < run.last_t {
            bad(format!("time goes backwards in run {id}"));
        }
        run.last_t = t;
        if let Some(expect) = run.expect.take() {
            if !expect.contains(&kind) {
                bad(format!(
                    "expected one of {expect:?} after `event`, found `{kind}`"
                ));
            }
        }
        let machine = ir.and_then(|ir| ir.machines.get(&run.machine));
        let state_in_ir = |s: &str| machine.is_none_or(|m| find_state(&m.root, s).is_some());

        match kind {
            "started" => bad(format!("run {id} starts twice")),
            "entered" => {
                let s = str_of(r, "state");
                let ok = match run.stack.last() {
                    None => s == run.machine,
                    Some(top) => s
                        .strip_prefix(top.as_str())
                        .and_then(|rest| rest.strip_prefix('.'))
                        .is_some_and(|name| !name.is_empty() && !name.contains('.')),
                };
                if !ok {
                    bad(format!(
                        "entered {s} is not a child of the innermost active state"
                    ));
                }
                if !state_in_ir(s) {
                    bad(format!("state {s} is not in the IR"));
                }
                run.stack.push(s.to_owned());
            }
            "exited" => {
                let s = str_of(r, "state");
                if run.stack.last().map(String::as_str) != Some(s) {
                    bad(format!("exited {s} is not the innermost active state"));
                }
                if run.invokes.values().any(|(st, _)| st == s)
                    || run.timers.values().any(|(st, _)| st == s)
                {
                    bad(format!("{s} exited with its work still running"));
                }
                run.stack.pop();
            }
            "event" | "dropped" | "stale" => {
                let e = r.get("event").cloned().unwrap_or(Value::Null);
                let ekind = str_of(&e, "kind").to_owned();
                if kind == "event" {
                    run.expect = Some(if ekind == "invoke.defect" {
                        &["defect"]
                    } else {
                        &["transition"]
                    });
                }
                if let Some(n) = e.get("invoke").and_then(Value::as_u64) {
                    let live = run.invokes.contains_key(&n);
                    if kind == "stale" {
                        if live || n >= run.next_invoke {
                            bad(format!(
                                "stale outcome of invoke {n}, which is running or never started"
                            ));
                        }
                    } else if !live {
                        bad(format!("outcome of invoke {n}, which is not running"));
                    } else {
                        run.invokes.remove(&n);
                        let payload = ["value", "error", "defect"]
                            .iter()
                            .find_map(|k| e.get(*k).cloned())
                            .unwrap_or(Value::Null);
                        run.invoke_ends.insert(n, (ekind.clone(), payload));
                    }
                } else if let Some(n) = e.get("timer").and_then(Value::as_u64) {
                    let live = run.timers.contains_key(&n);
                    if kind == "stale" {
                        if live || n >= run.next_timer {
                            bad(format!(
                                "stale expiry of timer {n}, which is running or never started"
                            ));
                        }
                    } else if !live {
                        bad(format!("expiry of timer {n}, which is not running"));
                    } else {
                        run.timers.remove(&n);
                    }
                } else if kind == "stale" {
                    bad("only outcomes and expiries can be stale".to_owned());
                }
            }
            "transition" => {
                let from = str_of(r, "from");
                if !run.stack.iter().any(|s| s == from) {
                    bad(format!("transition from {from}, which is not active"));
                }
                if let Some(st) = machine.and_then(|m| find_state(&m.root, from)) {
                    let index = u64_of(r, "index") as usize;
                    match st.transitions.get(index) {
                        None => bad(format!("{from} has no transition {index}")),
                        Some(tr) if tr.target.as_deref() != r["to"].as_str() => bad(format!(
                            "transition {from}[{index}] does not go to {}",
                            r["to"]
                        )),
                        Some(_) => {}
                    }
                }
            }
            "set" | "action" => {}
            "invoke.started" => {
                let n = u64_of(r, "invoke");
                let s = str_of(r, "state").to_owned();
                let src = str_of(r, "src").to_owned();
                if n != run.next_invoke {
                    bad(format!("invoke id {n}, expected {}", run.next_invoke));
                }
                run.next_invoke = n + 1;
                if !run.stack.contains(&s) {
                    bad(format!("invoke started in {s}, which is not active"));
                }
                if let Some(st) = machine.and_then(|m| find_state(&m.root, &s)) {
                    if st.invoke.as_ref().map(|inv| inv.src.as_str()) != Some(src.as_str()) {
                        bad(format!("{s} does not invoke {src}"));
                    }
                }
                run.invokes.insert(n, (s.clone(), src.clone()));
                run.started_invokes.insert(n, (s, src));
            }
            "invoke.cancelled" => {
                let n = u64_of(r, "invoke");
                let want = (str_of(r, "state").to_owned(), str_of(r, "src").to_owned());
                match run.invokes.remove(&n) {
                    Some(have) if have == want => {
                        run.invoke_ends
                            .insert(n, ("cancelled".to_owned(), Value::Null));
                    }
                    _ => bad(format!(
                        "cancelled invoke {n}, which is not running as written"
                    )),
                }
            }
            "timer.started" => {
                let n = u64_of(r, "timer");
                let s = str_of(r, "state").to_owned();
                if n != run.next_timer {
                    bad(format!("timer id {n}, expected {}", run.next_timer));
                }
                run.next_timer = n + 1;
                if !run.stack.contains(&s) {
                    bad(format!("timer started in {s}, which is not active"));
                }
                run.timers.insert(n, (s, u64_of(r, "index")));
            }
            "timer.cancelled" => {
                let n = u64_of(r, "timer");
                let want = (str_of(r, "state").to_owned(), u64_of(r, "index"));
                if run.timers.remove(&n) != Some(want) {
                    bad(format!(
                        "cancelled timer {n}, which is not running as written"
                    ));
                }
            }
            "done" | "failed" | "cancelled" | "defect" => {
                if kind != "defect" {
                    if !run.stack.is_empty() {
                        bad(format!("run {id} ended with states still active"));
                    }
                    if !run.invokes.is_empty() || !run.timers.is_empty() {
                        bad(format!("run {id} ended with work still running"));
                    }
                }
                if kind == "done" || kind == "failed" {
                    let s = str_of(r, "state");
                    let top_level = s
                        .strip_prefix(run.machine.as_str())
                        .and_then(|rest| rest.strip_prefix('.'))
                        .is_some_and(|n| !n.contains('.'));
                    if !top_level {
                        bad(format!("{s} is not a top-level state of {}", run.machine));
                    }
                }
                run.end = Some(r.clone());
            }
            other => bad(format!("unknown record kind `{other}`")),
        }
    }

    // How each run ended, and whether its parent saw it.
    for id in &order {
        let run = &runs[id];
        let end_kind = run.end.as_ref().map(|e| str_of(e, "kind").to_owned());
        if id == ROOT_RUN && end_kind.as_deref() != status.final_record() {
            problems.push(format!(
                "run {id} ends with {end_kind:?}, but the case expects status {status:?}"
            ));
        }
        let Some(parent) = &run.parent else { continue };
        let n: u64 = id
            .rsplit_once('.')
            .and_then(|(_, n)| n.parse().ok())
            .unwrap_or(0);
        let seen = runs.get(parent).and_then(|p| p.invoke_ends.get(&n));
        let parent_end = runs
            .get(parent)
            .and_then(|p| p.end.as_ref())
            .map(|e| str_of(e, "kind"));
        // A defect in the cancel step of a child its parent was cancelling
        // becomes the parent's own defect (unless the parent had already
        // ended with one).
        let defect_in_cancel = run
            .end
            .as_ref()
            .is_some_and(|e| str_of(e, "kind") == "defect")
            && seen.is_some_and(|(k, _)| k == "cancelled")
            && parent_end == Some("defect");
        if defect_in_cancel {
            continue;
        }
        let expected = run.end.as_ref().map(|e| match str_of(e, "kind") {
            "done" => ("invoke.done".to_owned(), e["output"].clone()),
            "failed" => ("invoke.fail".to_owned(), e["error"].clone()),
            "defect" => ("invoke.defect".to_owned(), e["defect"].clone()),
            _ => ("cancelled".to_owned(), Value::Null),
        });
        match (expected, seen) {
            (Some(want), Some(have)) if want == *have => {}
            (None, None) => {}
            (want, have) => problems.push(format!(
                "run {id} ended as {want:?}, but its parent {parent} saw {have:?}"
            )),
        }
    }
    if runs.get(ROOT_RUN).is_some_and(|r| r.end.is_some()) {
        for id in &order {
            if runs[id].end.is_none() {
                problems.push(format!("run {id} never ended, though the root run did"));
            }
        }
    }
    let mut sorted = order.clone();
    sorted.sort_by(|a, b| compare_run_ids(a, b));
    if sorted != order {
        problems.push("runs are not in run order".to_owned());
    }
    problems
}
