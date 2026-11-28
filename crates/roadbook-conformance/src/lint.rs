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

/// A peer's end a run took or dropped: its line, the holder, the peer and the status.
struct EndTaken {
    line: usize,
    run: String,
    from: String,
    status: String,
}

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
    /// The active states, in the order they were entered.
    stack: Vec<String>,
    /// For a restored run, the invokes and timers of its snapshot, which
    /// `invoke.started` and `timer.started` records restart with their ids.
    restarts: Vec<(&'static str, u64)>,
    invokes: BTreeMap<u64, (String, String)>,
    started_invokes: HashMap<u64, (String, String)>,
    /// The spawned children running, by id: their collection and key.
    spawned: BTreeMap<u64, (String, Value)>,
    /// The ids that were spawned children, as opposed to invokes.
    spawn_ids: std::collections::HashSet<u64>,
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
    /// The calls the run has taken and not answered: (caller, invoke, call).
    calls: Vec<(String, u64, String)>,
    /// A call just dropped, which a `replied` with `Unanswered` must follow.
    dropped_call: Option<(String, u64, String)>,
    /// For a suspended run, the ids of the invokes and spawned children still
    /// running: child runs suspended with it.
    suspended_with: Vec<u64>,
}

/// A call one run took, or an answer it gave, for checking against the
/// caller's records.
struct CallRecord {
    line: usize,
    callee: String,
    caller: String,
    invoke: u64,
    call: String,
    /// For a `replied` record: the input the caller is to take.
    answer: Option<(String, Value)>,
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

fn parent_of(id: &str) -> Option<&str> {
    id.rsplit_once('.').map(|(p, _)| p)
}

fn is_below(id: &str, ancestor: &str) -> bool {
    id.strip_prefix(ancestor)
        .is_some_and(|rest| rest.starts_with('.'))
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
    let mut calls_taken: Vec<CallRecord> = Vec::new();
    let mut ends: Vec<EndTaken> = Vec::new();

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
            if kind != "started" && kind != "restored" {
                bad(format!(
                    "run {id} does not begin with `started` or `restored`"
                ));
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
            let mut run = Run {
                machine,
                parent,
                next_invoke: 1,
                next_timer: 1,
                next_message: 1,
                ..Run::default()
            };
            if kind == "restored" {
                let snap = &r["snapshot"];
                // A child run restored from a snapshot nested in its
                // parent's: the migration, if any, is its top-level run's.
                if r.get("parent").is_some() && !r["migrated"].is_null() {
                    bad(format!("restored child run {id} has `migrated`"));
                }
                if snap["machine"] != r["machine"] || snap["ir"] != r["ir"] {
                    bad("`restored` has another machine or hash than its snapshot".to_owned());
                }
                if snap["status"] != "running" {
                    bad("restored a snapshot that is not running".to_owned());
                }
                let list = |k: &str| {
                    snap.get(k)
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default()
                };
                run.stack = list("active")
                    .iter()
                    .filter_map(|s| s.as_str().map(str::to_owned))
                    .collect();
                if run.stack.first() != Some(&run.machine) {
                    bad("the snapshot's active states do not begin with the root".to_owned());
                }
                for inv in list("invokes") {
                    let n = u64_of(&inv, "id");
                    run.invokes.insert(
                        n,
                        (
                            str_of(&inv, "state").to_owned(),
                            str_of(&inv, "src").to_owned(),
                        ),
                    );
                    run.restarts.push(("invoke", n));
                }
                for ch in list("spawned") {
                    let n = u64_of(&ch, "id");
                    run.spawned
                        .insert(n, (str_of(&ch, "children").to_owned(), ch["key"].clone()));
                    run.restarts.push(("spawn", n));
                }
                for tm in list("timers") {
                    let n = u64_of(&tm, "id");
                    run.timers
                        .insert(n, (str_of(&tm, "state").to_owned(), u64_of(&tm, "index")));
                    run.restarts.push(("timer", n));
                }
                run.next_invoke = u64_of(snap, "nextInvoke");
                run.next_timer = u64_of(snap, "nextTimer");
                run.next_message = u64_of(snap, "nextMessage");
            }
            runs.insert(id.clone(), run);
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
        let dropped_call = run.dropped_call.take();
        if let Some((caller, n, _)) = &dropped_call {
            if kind != "replied" {
                bad(format!(
                    "the dropped call of {caller}'s invoke {n} is not answered at once"
                ));
            }
        }
        let machine = ir.and_then(|ir| ir.machines.get(&run.machine));
        let state_in_ir = |s: &str| machine.is_none_or(|m| find_state(&m.root, s).is_some());

        match kind {
            "started" | "restored" => bad(format!("run {id} starts twice")),
            "entered" => {
                let s = str_of(r, "state");
                // With the IR, the active states are a tree: the parent of a
                // state entered is active, and a compound parent has no other
                // active child. Without it, they are a chain.
                let ok = match (run.stack.last(), machine) {
                    (None, _) => s == run.machine,
                    (Some(_), Some(m)) => match parent_of(s) {
                        Some(p) if run.stack.iter().any(|a| a == p) => {
                            let compound = find_state(&m.root, p)
                                .is_some_and(|ps| ps.kind == roadbook_ir::StateKind::Compound);
                            !run.stack.iter().any(|a| a == s)
                                && !(compound && run.stack.iter().any(|a| parent_of(a) == Some(p)))
                        }
                        _ => false,
                    },
                    (Some(top), None) => s
                        .strip_prefix(top.as_str())
                        .and_then(|rest| rest.strip_prefix('.'))
                        .is_some_and(|name| !name.is_empty() && !name.contains('.')),
                };
                if !ok {
                    bad(format!(
                        "entered {s}, whose parent is not active or already has its active child"
                    ));
                }
                if !state_in_ir(s) {
                    bad(format!("state {s} is not in the IR"));
                }
                run.stack.push(s.to_owned());
            }
            "exited" => {
                let s = str_of(r, "state");
                if !run.stack.iter().any(|a| a == s) || run.stack.iter().any(|a| is_below(a, s)) {
                    bad(format!("exited {s}, which is not an active leaf"));
                }
                if run.invokes.values().any(|(st, _)| st == s)
                    || run.timers.values().any(|(st, _)| st == s)
                {
                    bad(format!("{s} exited with its work still running"));
                }
                run.stack.retain(|a| a != s);
            }
            "event" | "dropped" | "stale" => {
                let e = r.get("event").cloned().unwrap_or(Value::Null);
                let ekind = str_of(&e, "kind").to_owned();
                if kind == "event" {
                    run.expect = Some(if ekind == "invoke.defect" || ekind == "spawn.defect" {
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
                if ekind == "ended" {
                    if kind == "stale" {
                        bad("a peer's end is never stale".to_owned());
                    }
                    ends.push(EndTaken {
                        line,
                        run: id.clone(),
                        from: str_of(&e, "from").to_owned(),
                        status: str_of(&e, "status").to_owned(),
                    });
                }
                if ekind == "call" {
                    let caller = str_of(&e, "from").to_owned();
                    let n = u64_of(&e, "invoke");
                    let call = str_of(&e, "call").to_owned();
                    if let Some(m) = machine {
                        if !m.calls.contains_key(&call) {
                            bad(format!("{} has no call {call}", run.machine));
                        }
                    }
                    calls_taken.push(CallRecord {
                        line,
                        callee: id.clone(),
                        caller: caller.clone(),
                        invoke: n,
                        call: call.clone(),
                        answer: None,
                    });
                    match kind {
                        "stale" => bad("a call is never stale".to_owned()),
                        "dropped" => run.dropped_call = Some((caller, n, call)),
                        _ => run.calls.push((caller, n, call)),
                    }
                } else if ekind == "message" {
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
                            let live = run.invokes.contains_key(&n) || run.spawned.contains_key(&n);
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
                } else if let Some(n) = e.get("spawn").and_then(Value::as_u64) {
                    let live = run.spawned.contains_key(&n);
                    if kind == "stale" {
                        if live || n >= run.next_invoke {
                            bad(format!(
                                "stale outcome of spawned child {n}, which is running or never started"
                            ));
                        }
                    } else if !live {
                        bad(format!(
                            "outcome of spawned child {n}, which is not running"
                        ));
                    } else {
                        run.spawned.remove(&n);
                        let payload = ["value", "error", "defect"]
                            .iter()
                            .find_map(|k| e.get(*k).cloned())
                            .unwrap_or(Value::Null);
                        run.invoke_ends.insert(n, (ekind.clone(), payload));
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
                        Some(tr) if serde_json::json!(tr.targets) != r["to"] => bad(format!(
                            "transition {from}[{index}] does not go to {}",
                            r["to"]
                        )),
                        Some(_) => {}
                    }
                }
            }
            "set" | "action" => {}
            "replied" => {
                let to = str_of(r, "to").to_owned();
                let n = u64_of(r, "invoke");
                let call = str_of(r, "call").to_owned();
                let key = (to.clone(), n, call.clone());
                let outcome = str_of(r, "outcome");
                let value = r.get("value").cloned().unwrap_or(Value::Null);
                if dropped_call.as_ref() == Some(&key) {
                    if outcome != "fail" || value != serde_json::json!({"_tag": "Unanswered"}) {
                        bad("a dropped call is answered other than `Unanswered`".to_owned());
                    }
                } else if let Some(i) = run.calls.iter().position(|c| *c == key) {
                    run.calls.remove(i);
                } else {
                    bad(format!(
                        "reply to {to}'s invoke {n}, which is not a pending {call} call"
                    ));
                }
                let answer = match outcome {
                    "done" => ("invoke.done".to_owned(), value),
                    "fail" => ("invoke.fail".to_owned(), value),
                    o => {
                        bad(format!("unknown outcome `{o}`"));
                        ("".to_owned(), value)
                    }
                };
                calls_taken.push(CallRecord {
                    line,
                    callee: id.clone(),
                    caller: to,
                    invoke: n,
                    call,
                    answer: Some(answer),
                });
            }
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
                if let Some(i) = run.restarts.iter().position(|x| *x == ("invoke", n)) {
                    run.restarts.remove(i);
                    if run.invokes.remove(&n) != Some((s.clone(), src.clone())) {
                        bad(format!("restarted invoke {n} is not the snapshot's"));
                    }
                } else {
                    if n != run.next_invoke {
                        bad(format!("invoke id {n}, expected {}", run.next_invoke));
                    }
                    run.next_invoke = n + 1;
                }
                if !run.stack.contains(&s) {
                    bad(format!("invoke started in {s}, which is not active"));
                }
                if let Some(st) = machine.and_then(|m| find_state(&m.root, &s)) {
                    let want = st.invoke.as_ref().map(|inv| match &inv.to {
                        Some(to) => format!("{}.{}", to.name, inv.src),
                        None => inv.src.clone(),
                    });
                    if want.as_deref() != Some(src.as_str()) {
                        bad(format!("{s} does not invoke {src}"));
                    }
                }
                run.invokes.insert(n, (s.clone(), src.clone()));
                run.started_invokes.insert(n, (s, src));
            }
            "spawn.started" => {
                let n = u64_of(r, "spawn");
                let children = str_of(r, "children").to_owned();
                let src = str_of(r, "src").to_owned();
                let key = r["key"].clone();
                if let Some(i) = run.restarts.iter().position(|x| *x == ("spawn", n)) {
                    run.restarts.remove(i);
                    if run.spawned.get(&n) != Some(&(children.clone(), key.clone())) {
                        bad(format!("restarted spawned child {n} is not the snapshot's"));
                    }
                } else {
                    if n != run.next_invoke {
                        bad(format!("spawn id {n}, expected {}", run.next_invoke));
                    }
                    run.next_invoke = n + 1;
                }
                if run
                    .spawned
                    .iter()
                    .any(|(m, (c, k))| *m != n && *c == children && *k == key)
                {
                    bad(format!(
                        "spawned child {n} has the key of another running child"
                    ));
                }
                if let Some(m) = machine {
                    match m.children.get(&children) {
                        Some(c) if c.src == src => {}
                        _ => bad(format!(
                            "{} has no collection {children} of {src}",
                            run.machine
                        )),
                    }
                }
                run.spawned.insert(n, (children, key));
                run.spawn_ids.insert(n);
                run.started_invokes.insert(n, (String::new(), src));
            }
            "spawn.stopped" => {
                let n = u64_of(r, "spawn");
                let want = (str_of(r, "children").to_owned(), r["key"].clone());
                match run.spawned.remove(&n) {
                    Some(have) if have == want => {
                        run.invoke_ends
                            .insert(n, ("cancelled".to_owned(), Value::Null));
                    }
                    _ => bad(format!(
                        "stopped spawned child {n}, which is not running as written"
                    )),
                }
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
                if let Some(i) = run.restarts.iter().position(|x| *x == ("timer", n)) {
                    run.restarts.remove(i);
                    if run.timers.remove(&n) != Some((s.clone(), u64_of(r, "index"))) {
                        bad(format!("restarted timer {n} is not the snapshot's"));
                    }
                } else {
                    if n != run.next_timer {
                        bad(format!("timer id {n}, expected {}", run.next_timer));
                    }
                    run.next_timer = n + 1;
                }
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
            "suspended" => {
                // What is still running must be child runs that are suspended
                // with it (checked below, once their records are read).
                run.suspended_with = run
                    .invokes
                    .keys()
                    .chain(run.spawned.keys())
                    .copied()
                    .collect();
                if !run.calls.is_empty() {
                    bad(format!("run {id} is suspended with a call pending"));
                }
                run.end = Some(r.clone());
            }
            "done" | "failed" | "cancelled" | "defect" => {
                if kind != "defect" {
                    if !run.stack.is_empty() {
                        bad(format!("run {id} ended with states still active"));
                    }
                    if !run.invokes.is_empty() || !run.timers.is_empty() || !run.spawned.is_empty()
                    {
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

    // A suspended run's tree is suspended with it: each invoke or spawned
    // child still running is a child run that is suspended too, and a
    // suspended child run's parent is suspended.
    let suspended = |id: &str| {
        runs.get(id)
            .and_then(|r| r.end.as_ref())
            .is_some_and(|e| str_of(e, "kind") == "suspended")
    };
    for id in &order {
        let run = &runs[id];
        for n in &run.suspended_with {
            if !suspended(&format!("{id}.{n}")) {
                problems.push(format!(
                    "run {id} is suspended with invoke or spawned child {n} running, which is not a suspended child run"
                ));
            }
        }
        if let Some(parent) = &run.parent
            && suspended(id)
            && !suspended(parent)
        {
            problems.push(format!(
                "child run {id} is suspended, but its parent {parent} is not"
            ));
        }
    }

    // How each run ended, and whether its parent saw it.
    for id in &order {
        let run = &runs[id];
        let Some(parent) = &run.parent else { continue };
        // Suspended with its parent: no outcome.
        if suspended(id) {
            continue;
        }
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
        // A child still running when its parent ended with a defect is
        // cancelled by `failWithDefect`, which writes no record in the
        // parent: it ends with its cancel step, `cancelled` or a defect of
        // its own, which the parent ignores.
        let cancelled_by_defect = seen.is_none()
            && parent_end == Some("defect")
            && runs
                .get(parent)
                .is_some_and(|p| p.invokes.contains_key(&n) || p.spawned.contains_key(&n))
            && run
                .end
                .as_ref()
                .is_some_and(|e| matches!(str_of(e, "kind"), "cancelled" | "defect"));
        if cancelled_by_defect {
            continue;
        }
        let spawned = runs.get(parent).is_some_and(|p| p.spawn_ids.contains(&n));
        let outcome = |k: &str| {
            if spawned {
                format!("spawn.{k}")
            } else {
                format!("invoke.{k}")
            }
        };
        let expected = run.end.as_ref().map(|e| match str_of(e, "kind") {
            "done" => (outcome("done"), e["output"].clone()),
            "failed" => (outcome("fail"), e["error"].clone()),
            "defect" => (outcome("defect"), e["defect"].clone()),
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
        let ended = end_kind.as_deref().filter(|k| *k != "suspended");
        if ended != status.final_record() {
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
    // Every peer's end taken names a top-level run whose group ends with the
    // record its status names; it is the holder's last input from that peer,
    // and the holder takes it once.
    let mut told: Vec<(&str, &str)> = Vec::new();
    for end in &ends {
        let line = end.line;
        let want = match end.status.as_str() {
            "Done" => "done",
            "Failed" => "failed",
            "Cancelled" => "cancelled",
            "Defect" => "defect",
            "Suspended" => "suspended",
            other => {
                problems.push(format!("line {line}: `{other}` is not a variant of Ended"));
                continue;
            }
        };
        if !is_top_level(&end.from) {
            problems.push(format!(
                "line {line}: {} is not a top-level run, so it has no holders",
                end.from
            ));
        }
        let got = runs
            .get(&end.from)
            .and_then(|r| r.end.as_ref())
            .map(|e| str_of(e, "kind"));
        if got != Some(want) {
            problems.push(format!(
                "line {line}: the end of {} is {:?}, but {} was told {}",
                end.from, got, end.run, end.status
            ));
        }
        if told.contains(&(end.run.as_str(), end.from.as_str())) {
            problems.push(format!(
                "line {line}: {} is told the end of {} twice",
                end.run, end.from
            ));
        }
        told.push((&end.run, &end.from));
        if received
            .iter()
            .any(|m| m.run == end.run && m.from == end.from && m.line > line)
        {
            problems.push(format!(
                "line {line}: {} takes an input from {} after its end",
                end.run, end.from
            ));
        }
    }
    // Every call taken was made by its caller's invoke of a call, and every
    // answer a caller took as a call's outcome was given by a `replied`
    // record, unless it is `Unanswered`, which a driver gives without one.
    for c in &calls_taken {
        let line = c.line;
        let made = runs
            .get(&c.caller)
            .and_then(|r| r.started_invokes.get(&c.invoke));
        match made {
            Some((_, src)) if src.rsplit_once('.').is_some_and(|(_, k)| k == c.call) => {}
            _ => problems.push(format!(
                "line {line}: {} made no call {} with invoke {}",
                c.caller, c.call, c.invoke
            )),
        }
        if let Some(answer) = &c.answer {
            let caller_end = runs
                .get(&c.caller)
                .and_then(|r| r.invoke_ends.get(&c.invoke));
            if let Some(have) = caller_end {
                if have.0 != "cancelled" && have != answer {
                    problems.push(format!(
                        "line {line}: {} took {have:?} for its call, not the reply {answer:?}",
                        c.caller
                    ));
                }
            }
        }
        let _ = &c.callee;
    }
    for (id, run) in &runs {
        for (n, (k, v)) in &run.invoke_ends {
            let Some((_, src)) = run.started_invokes.get(n) else {
                continue;
            };
            let is_call = src.contains('.');
            if !is_call || k == "cancelled" {
                continue;
            }
            let replied = calls_taken.iter().any(|c| {
                c.caller == *id
                    && c.invoke == *n
                    && c.answer.as_ref() == Some(&(k.clone(), v.clone()))
            });
            let unanswered = k == "invoke.fail" && *v == serde_json::json!({"_tag": "Unanswered"});
            if !replied && !unanswered {
                problems.push(format!(
                    "run {id} took {k} for its call invoke {n}, which no reply gave"
                ));
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
