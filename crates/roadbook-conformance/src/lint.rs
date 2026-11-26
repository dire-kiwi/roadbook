//! Structural checks of a trace that need no runtime.
//!
//! These catch the kinds of mistake a hand-derived or a buggy trace makes:
//! an `exited` that does not match the innermost active state, an invoke
//! cancelled twice, a timer left running when the run ends, a child run
//! whose result never reaches its parent, a message received that was never
//! sent or received out of order. They do not decide which transition is
//! right; that is what the expected traces are for.

use std::collections::{BTreeMap, HashMap};

use roadbook_ir::{Ir, State};
use serde_json::Value;

use crate::{ROOT_RUN, Status, arrange, compare_run_ids};

/// A message or emitted event a run took, for checking against its sender.
struct Received {
    line: usize,
    run: String,
    kind: String,
    from: String,
    message: u64,
    value: Value,
}

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
    next_message: u64,
    /// Per message id: `sent` (with `to`) or `emitted` (with `null`), and
    /// the value.
    messages: HashMap<u64, (String, Value, Value)>,
    /// The `sent` record just written, which an `undelivered` may follow.
    last_sent: Option<(u64, Value, Value)>,
}

fn is_top_level(id: &str) -> bool {
    id.strip_prefix('r').is_some_and(|n| {
        !n.is_empty() && !n.starts_with('0') && n.bytes().all(|b| b.is_ascii_digit())
    })
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
/// problem. `status` is the root run's expected status, and the trace has no
/// other top-level run. With `ir`, state ids, transitions, invokes and
/// hashes are also checked against it.
pub fn lint(records: &[Value], status: Status, ir: Option<&Ir>) -> Vec<String> {
    lint_runs(records, &[(ROOT_RUN.to_owned(), status)], ir)
}

/// [`lint`] for a trace with several top-level runs: `statuses` lists each
/// one's id and expected status, the root run's first
/// ([`crate::Case::top_level_runs`]).
pub fn lint_runs(records: &[Value], statuses: &[(String, Status)], ir: Option<&Ir>) -> Vec<String> {
    let mut problems = Vec::new();
    let mut runs: HashMap<String, Run> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut received: Vec<Received> = Vec::new();

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
                (None, None) if statuses.iter().any(|(r, _)| *r == id) => {}
                (None, _) => bad(format!("run {id} is not a top-level run of the case")),
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
                    next_message: 1,
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
        let last_sent = run.last_sent.take();
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
                if ekind == "message" || ekind == "emitted" {
                    received.push(Received {
                        line,
                        run: id.clone(),
                        kind: ekind.clone(),
                        from: str_of(&e, "from").to_owned(),
                        message: u64_of(&e, "message"),
                        value: e.get("value").cloned().unwrap_or(Value::Null),
                    });
                }
                if ekind == "message" {
                    if kind == "stale" {
                        bad("a message is never stale".to_owned());
                    }
                } else if ekind == "emitted" {
                    match e.get("invoke").and_then(Value::as_u64) {
                        Some(n) => {
                            if str_of(&e, "from") != format!("{id}.{n}") {
                                bad(format!(
                                    "emitted event of invoke {n} is not from run {id}.{n}"
                                ));
                            }
                            let live = run.invokes.contains_key(&n);
                            if kind == "stale" && (live || n >= run.next_invoke) {
                                bad(format!(
                                    "stale emitted event of invoke {n}, which is running or never started"
                                ));
                            } else if kind != "stale" && !live {
                                bad(format!("emitted event of invoke {n}, which is not running"));
                            }
                        }
                        None if kind == "stale" => {
                            bad("an emitted event from a peer is never stale".to_owned());
                        }
                        None => {}
                    }
                } else if let Some(n) = e.get("invoke").and_then(Value::as_u64) {
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
            "sent" | "emitted" => {
                let n = u64_of(r, "message");
                if n != run.next_message {
                    bad(format!("message id {n}, expected {}", run.next_message));
                }
                run.next_message = n + 1;
                let to = r.get("to").cloned().unwrap_or(Value::Null);
                let value = r.get("value").cloned().unwrap_or(Value::Null);
                if kind == "sent" {
                    run.last_sent = Some((n, to.clone(), value.clone()));
                }
                run.messages.insert(n, (kind.to_owned(), to, value));
            }
            "undelivered" => {
                let n = u64_of(r, "message");
                let to = r.get("to").cloned().unwrap_or(Value::Null);
                let value = r.get("value").cloned().unwrap_or(Value::Null);
                if to.is_null() {
                    if n != run.next_message {
                        bad(format!("message id {n}, expected {}", run.next_message));
                    }
                    run.next_message = n + 1;
                } else if last_sent != Some((n, to, value)) {
                    bad(format!(
                        "undelivered message {n} does not follow its `sent` record"
                    ));
                }
            }
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
    for (top, status) in statuses {
        let end_kind = runs
            .get(top)
            .and_then(|r| r.end.as_ref())
            .map(|e| str_of(e, "kind").to_owned());
        if end_kind.as_deref() != status.final_record() {
            problems.push(format!(
                "run {top} ends with {end_kind:?}, but the case expects status {status:?}"
            ));
        }
        if !runs.contains_key(top) {
            problems.push(format!("top-level run {top} has no records"));
        }
        if end_kind.is_some() {
            let below = format!("{top}.");
            for id in &order {
                if id.starts_with(&below) && runs[id].end.is_none() {
                    problems.push(format!("run {id} never ended, though {top} did"));
                }
            }
        }
    }
    for id in &order {
        if runs[id].parent.is_none() && !is_top_level(id) {
            problems.push(format!("run {id} has no parent but is not named r<n>"));
        }
    }

    // Every message or emitted event taken was sent, to this run, once, and
    // the inputs from one sender are taken in the order it sent them.
    let mut last: HashMap<(String, String), u64> = HashMap::new();
    let mut seen: BTreeMap<(String, String, u64), usize> = BTreeMap::new();
    for m in &received {
        let line = m.line;
        let Some(sender) = runs.get(&m.from) else {
            problems.push(format!("line {line}: no run {} sent this", m.from));
            continue;
        };
        match sender.messages.get(&m.message) {
            Some((k, to, value)) => {
                let want = if m.kind == "message" {
                    "sent"
                } else {
                    "emitted"
                };
                if k != want {
                    problems.push(format!(
                        "line {line}: message {} of {} is `{k}`, not `{want}`",
                        m.message, m.from
                    ));
                } else if k == "sent" && to.as_str() != Some(m.run.as_str()) {
                    problems.push(format!(
                        "line {line}: message {} of {} was sent to {to}, not {}",
                        m.message, m.from, m.run
                    ));
                }
                if *value != m.value {
                    problems.push(format!(
                        "line {line}: message {} of {} has another value",
                        m.message, m.from
                    ));
                }
            }
            None => problems.push(format!(
                "line {line}: {} has no message {}",
                m.from, m.message
            )),
        }
        let pair = (m.from.clone(), m.run.clone());
        if last.get(&pair).is_some_and(|&prev| prev >= m.message) {
            problems.push(format!(
                "line {line}: message {} of {} is taken out of order",
                m.message, m.from
            ));
        }
        last.insert(pair, m.message);
        let n = seen
            .entry((m.from.clone(), m.run.clone(), m.message))
            .or_default();
        *n += 1;
        if *n > 1 {
            problems.push(format!(
                "line {line}: message {} of {} is taken twice",
                m.message, m.from
            ));
        }
    }
    let mut sorted = order.clone();
    sorted.sort_by(|a, b| compare_run_ids(a, b));
    if sorted != order {
        problems.push("runs are not in run order".to_owned());
    }
    problems
}
