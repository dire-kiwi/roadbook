//! `invoke … retry N [on E | …] [after delay]`, checked and written out as the explicit states
//! `docs/design/language.md` gives under "What `retry` compiles to": the invoking state becomes
//! compound, its invoke and outcomes move into a child `Attempt`, the wait into a child
//! `Backoff`, and a counter variable guards a branch put first in each retried error's block.
//! Nothing here is new to the IR, so the runtimes run it unchanged.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use roadbook_ir::{self as ir, Builtin, Literal, MAX_SAFE_INTEGER, Op};
use roadbook_syntax::ast::{self, ExprKind, Invoke, Pattern, Trigger};
use roadbook_syntax::{Diagnostic, Span};

use super::{Cx, Reads, Scope, StateInfo, TransitionReads, Var, VarInfo, invoke_head};
use crate::check::{UNANSWERED, ir_span};
use crate::codes;
use crate::names::suggest;
use crate::sensitive::Dest;
use crate::types::{DURATION, Ty};

const INT: Ty = Ty::Builtin(Builtin::Int);

/// An invoke with `retry`, as lowered so far: an ordinary invoke whose outcomes are the IR
/// transitions `outcomes` of its state.
pub(super) struct Plan<'p> {
    pub inv: &'p Invoke,
    pub retry: &'p ast::Retry,
    pub outcomes: Range<usize>,
    /// The errors the task or machine declares, or `None` if it is unknown.
    pub declared: Option<Vec<String>>,
}

/// A checked retry, ready to be written out.
pub(super) struct Checked<'p> {
    plan: Plan<'p>,
    /// The errors retried, in declaration order.
    retried: Vec<String>,
    /// The counter variable.
    var: String,
    /// The delay with `attempt` read as the counter, and what it reads.
    delay: Option<(ir::Expr, Reads)>,
}

impl<'p> Cx<'_, 'p> {
    /// Checks the retry of node `idx`'s invoke, type-checks its delay, and returns what to
    /// write out, or `None` if it cannot be (with the reason reported).
    pub(super) fn check_retry(
        &mut self,
        idx: usize,
        kind: ir::StateKind,
        plan: Plan<'p>,
    ) -> Option<Checked<'p>> {
        let r = plan.retry;
        // A call is named as it is written, through its `ref`: `inventory.reserve`.
        let callee = match &plan.inv.to {
            Some(to) => format!("{}.{}", to.name, plan.inv.callee.name),
            None => plan.inv.callee.name.clone(),
        };
        let mut ok = true;

        // The delay is checked whatever else is wrong, so that its errors are reported.
        let var = self.retry_var(idx, r.delay.as_ref());
        let delay = r.delay.as_ref().map(|d| {
            let owner = match self.local("attempt") {
                Some((n, l)) => Some((
                    self.node_id(n).to_string(),
                    if l.param { "a parameter" } else { "a variable" },
                )),
                None => self.vars.get("attempt").map(|v| {
                    (
                        self.machine.clone(),
                        if v.param { "a parameter" } else { "a variable" },
                    )
                }),
            };
            if let Some((owner, what)) = owner {
                let mut uses = Vec::new();
                var_uses(d, "attempt", &mut uses);
                for span in uses {
                    self.ck.diags.push(
                        Diagnostic::error(
                            codes::ATTEMPT_SHADOWED,
                            span,
                            format!("`attempt` is ambiguous: `{owner}` has {what} named `attempt`"),
                        )
                        .with_help("in a retry's delay `attempt` counts the failed attempts; rename the parameter or variable, or write the retry out as explicit states"),
                    );
                }
            }
            let mut renamed = d.clone();
            rename_var(&mut renamed, "attempt", &var);
            self.reads.clear();
            self.vars.insert(
                var.clone(),
                Var {
                    ty: INT,
                    param: false,
                    sensitive: false,
                },
            );
            self.counter = Some(var.clone());
            let e = self.expect_to(
                &renamed,
                &Scope::none(),
                &DURATION,
                "a retry delay",
                Dest::Shown,
            );
            self.counter = None;
            self.vars.remove(&var);
            (e, std::mem::take(&mut self.reads))
        });

        match kind {
            // Reported as an invoke in a final state; a history state has no invoke.
            ir::StateKind::Final | ir::StateKind::History | ir::StateKind::DeepHistory => {
                return None;
            }
            ir::StateKind::Parallel | ir::StateKind::Compound => {
                self.ck.diags.push(
                    Diagnostic::error(
                        codes::RETRY_WITH_CHILDREN,
                        r.span,
                        format!("`{}` has child states, so its invoke cannot `retry`", self.nodes[idx].id),
                    )
                    .with_help("`retry` gives the state the children `Attempt` and `Backoff`; move the invoke into a state of its own, or write the retry out as explicit states"),
                );
                ok = false;
            }
            // A choice has no invoke.
            ir::StateKind::Atomic | ir::StateKind::Choice => {}
        }
        // `as` on a call, or on a task that emits nothing, is reported as such (`RB0214`).
        let named = plan.inv.is_machine()
            || (!plan.inv.is_call() && self.task_emits(&plan.inv.callee.name) == Some(true));
        if let Some(a) = plan.inv.alias.as_ref().filter(|_| named) {
            self.ck.diags.push(
                Diagnostic::error(
                    codes::RETRY_WITH_ALIAS,
                    r.span,
                    format!("`invoke {callee} as {}` cannot `retry`", a.name),
                )
                .with_secondary(a.span, "named here")
                .with_help("each attempt starts it anew, so the name would address nothing while the retry waits; drop `as`, or write the retry out as explicit states"),
            );
            ok = false;
        }
        if r.count < 1 {
            self.ck.diags.push(
                Diagnostic::error(
                    codes::RETRY_NOTHING,
                    r.count_span,
                    "`retry 0` retries nothing",
                )
                .with_help("give the number of retries after the first attempt, or remove `retry`"),
            );
            ok = false;
        } else if r.count.unsigned_abs() > MAX_SAFE_INTEGER as u64 {
            self.ck.diags.push(
                Diagnostic::error(
                    codes::LITERAL_OUT_OF_RANGE,
                    r.count_span,
                    "this number of retries is too large",
                )
                .with_label(format!("the largest allowed is {MAX_SAFE_INTEGER}")),
            );
            ok = false;
        }

        // The errors after `on`, each declared once.
        let mut listed: Vec<String> = Vec::new();
        for n in &r.errors {
            match self.ck.decl_symbol(&n.name) {
                Some(e) if plan.declared.as_ref().is_some_and(|d| d.contains(&n.name)) => {
                    self.ck.index.refer(e, n.span);
                }
                _ => self.ck.refer_decl(self.file, n),
            }
            if listed.contains(&n.name) {
                self.ck.diags.push(
                    Diagnostic::error(
                        codes::RETRY_NOTHING,
                        n.span,
                        format!("`{}` is listed twice", n.name),
                    )
                    .with_help("list each retried error once"),
                );
                ok = false;
                continue;
            }
            listed.push(n.name.clone());
            if let Some(declared) = &plan.declared
                && !declared.contains(&n.name)
            {
                let mut d = Diagnostic::error(
                    codes::UNDECLARED_ERROR_OUTCOME,
                    n.span,
                    format!(
                        "`{callee}` does not declare the error `{}`, so it cannot be retried",
                        n.name
                    ),
                );
                let unlisted = declared
                    .iter()
                    .filter(|e| !r.errors.iter().any(|l| l.name == **e));
                if n.name == UNANSWERED && plan.inv.is_call() {
                    d = d.with_help("`Unanswered` is not retried: the callee is not running, has ended or had no transition for the call; handle it in the `fail Unanswered` outcome");
                } else if let Some(s) = suggest(&n.name, unlisted.map(String::as_str)) {
                    d = d.with_help(format!("did you mean `{s}`?")).with_fix(
                        format!("replace with `{s}`"),
                        n.span,
                        s,
                    );
                } else {
                    d = d.with_help("retry only errors the task, machine or call declares");
                }
                self.ck.diags.push(d);
                ok = false;
            }
        }
        let declared = plan.declared.clone()?;
        if declared.is_empty() && r.count >= 1 {
            self.ck.diags.push(
                Diagnostic::error(
                    codes::RETRY_NOTHING,
                    r.span,
                    format!("`{callee}` declares no errors, so there is nothing to retry"),
                )
                .with_help("only a declared error is retried; a host defect never is"),
            );
            return None;
        }
        if declared.is_empty() {
            return None;
        }
        let retried: Vec<String> = if r.errors.is_empty() {
            declared.clone()
        } else {
            declared
                .iter()
                .filter(|e| listed.contains(e))
                .cloned()
                .collect()
        };

        // A `fail *` that handles a retried error must handle only retried ones, since the
        // retry branch goes first in its block.
        let own: HashSet<&str> = plan
            .inv
            .outcomes
            .iter()
            .filter_map(|o| match &o.trigger {
                Trigger::Fail(Pattern::Name(n)) => Some(n.name.as_str()),
                _ => None,
            })
            .collect();
        if let Some(any) = plan
            .inv
            .outcomes
            .iter()
            .find(|o| matches!(o.trigger, Trigger::Fail(Pattern::Any { .. })))
        {
            // A call's `fail *` also handles `Unanswered`, which is never retried.
            let unanswered = UNANSWERED.to_string();
            let covered: Vec<&String> = declared
                .iter()
                .chain(plan.inv.is_call().then_some(&unanswered))
                .filter(|e| !own.contains(e.as_str()))
                .collect();
            let (yes, no): (Vec<&String>, Vec<&String>) =
                covered.iter().partition(|e| retried.contains(e));
            if !yes.is_empty() && !no.is_empty() {
                let list = |v: &[&String]| {
                    v.iter()
                        .map(|e| format!("`{e}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                self.ck.diags.push(
                    Diagnostic::error(
                        codes::RETRY_SHARED_OUTCOME,
                        r.span,
                        format!(
                            "{} {} retried, but `fail *` also handles {}, which {} not",
                            list(&yes),
                            if yes.len() == 1 { "is" } else { "are" },
                            list(&no),
                            if no.len() == 1 { "is" } else { "are" },
                        ),
                    )
                    .with_secondary(any.trigger_span, "handled together here")
                    .with_help(format!(
                        "give each retried error an outcome of its own: `fail {} -> …`",
                        yes[0]
                    )),
                );
                ok = false;
            }
        }
        if !ok {
            return None;
        }
        Some(Checked {
            plan,
            retried,
            var,
            delay,
        })
    }

    /// The counter's name: the state's path below the machine, joined, with a lower-case first
    /// letter, and `Attempt`; with a number from 2 if that is taken.
    fn retry_var(&self, idx: usize, delay: Option<&ast::Expr>) -> String {
        let id = &self.nodes[idx].id;
        let path: String = id.split('.').skip(1).collect();
        let mut chars = path.chars();
        let base = match chars.next() {
            Some(c) => format!("{}{}Attempt", c.to_ascii_lowercase(), chars.as_str()),
            None => "attempt".to_string(),
        };
        let mut in_delay = Vec::new();
        if let Some(d) = delay {
            all_vars(d, &mut in_delay);
        }
        let taken = |n: &str| {
            self.vars.contains_key(n)
                || self.locals.iter().flatten().any(|l| l.name == n)
                || self
                    .nodes
                    .iter()
                    .any(|x| x.aliases.iter().any(|a| a.name.name == n))
                || self.retry_vars.iter().any(|(v, _)| v.name == n)
                || in_delay.iter().any(|v| v == n)
        };
        if !taken(&base) {
            return base;
        }
        (2..)
            .map(|i| format!("{base}{i}"))
            .find(|n| !taken(n))
            .expect("some number is free")
    }

    /// Rewrites `st`, an atomic state with an invoke, as the compound state with `Attempt` and
    /// `Backoff` children, and declares its counter.
    pub(super) fn write_out_retry(&mut self, st: &mut ir::State, c: Checked<'p>) {
        let Checked {
            plan,
            retried,
            var,
            delay,
        } = c;
        let r = plan.retry;
        let span = r.span;
        let attempt_id = format!("{}.Attempt", st.id);
        let backoff_id = format!("{}.Backoff", st.id);
        let retry_target = if delay.is_some() {
            backoff_id.clone()
        } else {
            attempt_id.clone()
        };
        let Some(mut info) = self.info.states.remove(&st.id) else {
            return;
        };

        // Split the state's transitions into its own and its invoke's outcomes.
        let mut own = Vec::new();
        let mut outcomes = Vec::new();
        let reads = std::mem::take(&mut info.transition_reads);
        let triggers = std::mem::take(&mut info.triggers);
        for (i, ((t, rd), tr)) in std::mem::take(&mut st.transitions)
            .into_iter()
            .zip(reads)
            .zip(triggers)
            .enumerate()
        {
            if plan.outcomes.contains(&i) {
                outcomes.push((t, rd, tr));
            } else {
                own.push((t, rd, tr));
            }
        }

        // The errors that the outcomes name; `fail *` handles the others.
        let named: HashSet<String> = outcomes
            .iter()
            .filter_map(|(t, ..)| match &t.trigger {
                ir::Trigger::InvokeFail { error, .. } if error != "*" => Some(error.clone()),
                _ => None,
            })
            .collect();
        let any_retried = retried.iter().any(|e| !named.contains(e));

        // The retry branch, first in each retried error's block.
        let counter = || ir::Expr {
            kind: ir::ExprKind::Ref {
                path: vec![var.clone()],
                state: None,
            },
            ty: INT.to_ir(),
            text: var.clone(),
        };
        let int = |n: i64| ir::Expr {
            kind: ir::ExprKind::Lit(Literal::Int(n)),
            ty: INT.to_ir(),
            text: n.to_string(),
        };
        let binary = |op: Op, sym: &str, ty: Ty, a: ir::Expr, b: ir::Expr| ir::Expr {
            text: format!("{} {sym} {}", a.text, b.text),
            kind: ir::ExprKind::Op {
                op,
                args: vec![a, b],
            },
            ty: ty.to_ir(),
        };
        let read = vec![(var.clone(), span)];
        let mut attempt = Vec::new();
        let mut retry_branches = Vec::new();
        let mut seen_group = HashSet::new();
        for (t, rd, tr) in outcomes {
            let retries = match &t.trigger {
                ir::Trigger::InvokeFail { error, .. } if error == "*" => any_retried,
                ir::Trigger::InvokeFail { error, .. } => retried.contains(error),
                _ => false,
            };
            if retries && seen_group.insert(t.group) {
                retry_branches.push(attempt.len());
                attempt.push((
                    ir::Transition {
                        trigger: t.trigger.clone(),
                        group: t.group,
                        guard: Some(binary(
                            Op::Lt,
                            "<",
                            Ty::Builtin(Builtin::Bool),
                            counter(),
                            int(r.count),
                        )),
                        targets: vec![retry_target.clone()],
                        internal: false,
                        args: Vec::new(),
                        effects: vec![ir::Effect::Set {
                            var: var.clone(),
                            state: None,
                            value: binary(Op::Add, "+", INT, counter(), int(1)),
                        }],
                        span: ir_span(span),
                    },
                    TransitionReads {
                        delay: Vec::new(),
                        guard: read.clone(),
                        effects: vec![read.clone()],
                        args: Vec::new(),
                        span,
                    },
                    span,
                ));
            }
            attempt.push((t, rd, tr));
        }
        renumber(&mut own);
        renumber(&mut attempt);

        let unzip = |v: Vec<(ir::Transition, TransitionReads, Span)>| {
            let mut ts = Vec::new();
            let mut rs = Vec::new();
            let mut ss = Vec::new();
            for (t, r, s) in v {
                ts.push(t);
                rs.push(r);
                ss.push(s);
            }
            (ts, rs, ss)
        };
        let (attempt_ts, attempt_reads, attempt_triggers) = unzip(attempt);
        let (own_ts, own_reads, own_triggers) = unzip(own);

        let mut states = vec![ir::State {
            id: attempt_id.clone(),
            name: "Attempt".into(),
            kind: ir::StateKind::Atomic,
            doc: String::new(),
            span: ir_span(plan.inv.span),
            params: Vec::new(),
            vars: Vec::new(),
            initial: None,
            default: Vec::new(),
            states: Vec::new(),
            entry: Vec::new(),
            exit: Vec::new(),
            invokes: std::mem::take(&mut st.invokes),
            transitions: attempt_ts,
            output: None,
            fail: None,
        }];
        self.info.states.insert(
            attempt_id.clone(),
            StateInfo {
                name_span: invoke_head(plan.inv),
                invoke_spans: std::mem::take(&mut info.invoke_spans),
                triggers: attempt_triggers,
                initial_span: None,
                entry_reads: Vec::new(),
                exit_reads: Vec::new(),
                invoke_reads: std::mem::take(&mut info.invoke_reads),
                result_reads: Vec::new(),
                transition_reads: attempt_reads,
                retry_branches,
                written_as: Some(st.id.clone()),
                locals: Vec::new(),
            },
        );
        if let Some((delay, delay_reads)) = delay {
            states.push(ir::State {
                id: backoff_id.clone(),
                name: "Backoff".into(),
                kind: ir::StateKind::Atomic,
                doc: String::new(),
                span: ir_span(span),
                params: Vec::new(),
                vars: Vec::new(),
                initial: None,
                default: Vec::new(),
                states: Vec::new(),
                entry: Vec::new(),
                exit: Vec::new(),
                invokes: Vec::new(),
                transitions: vec![ir::Transition {
                    trigger: ir::Trigger::After { delay },
                    group: 0,
                    guard: None,
                    targets: vec![attempt_id.clone()],
                    internal: false,
                    args: Vec::new(),
                    effects: Vec::new(),
                    span: ir_span(span),
                }],
                output: None,
                fail: None,
            });
            self.info.states.insert(
                backoff_id,
                StateInfo {
                    name_span: span,
                    invoke_spans: Vec::new(),
                    triggers: vec![span],
                    initial_span: None,
                    entry_reads: Vec::new(),
                    exit_reads: Vec::new(),
                    invoke_reads: Vec::new(),
                    result_reads: Vec::new(),
                    transition_reads: vec![TransitionReads {
                        delay: delay_reads,
                        guard: Vec::new(),
                        effects: Vec::new(),
                        args: Vec::new(),
                        span,
                    }],
                    retry_branches: Vec::new(),
                    written_as: Some(st.id.clone()),
                    locals: Vec::new(),
                },
            );
        }

        st.kind = ir::StateKind::Compound;
        st.initial = Some(attempt_id);
        st.states = states;
        st.transitions = own_ts;
        st.entry.insert(
            0,
            ir::Effect::Set {
                var: var.clone(),
                state: None,
                value: int(0),
            },
        );
        info.entry_reads.insert(0, Vec::new());
        info.transition_reads = own_reads;
        info.triggers = own_triggers;
        self.info.states.insert(st.id.clone(), info);

        self.retry_vars.push((
            ir::Var {
                name: var.clone(),
                ty: INT.to_ir(),
                init: Some(int(0)),
                sensitive: false,
                doc: String::new(),
                span: ir_span(span),
            },
            VarInfo {
                name: var,
                span,
                init: true,
                optional: false,
                init_reads: Vec::new(),
            },
        ));
    }
}

/// Numbers the groups of `ts` from 0, in the order they first appear.
fn renumber(ts: &mut [(ir::Transition, TransitionReads, Span)]) {
    let mut map: HashMap<u32, u32> = HashMap::new();
    for (t, ..) in ts.iter_mut() {
        let next = map.len() as u32;
        t.group = *map.entry(t.group).or_insert(next);
    }
}

/// The spans where `e` reads the variable `name`.
fn var_uses(e: &ast::Expr, name: &str, out: &mut Vec<Span>) {
    visit(e, &mut |x| {
        if let ExprKind::Var(n) = &x.kind
            && n.name == name
        {
            out.push(n.span);
        }
    });
}

/// Every variable name `e` reads.
fn all_vars(e: &ast::Expr, out: &mut Vec<String>) {
    visit(e, &mut |x| {
        if let ExprKind::Var(n) = &x.kind {
            out.push(n.name.clone());
        }
    });
}

fn visit(e: &ast::Expr, f: &mut impl FnMut(&ast::Expr)) {
    f(e);
    match &e.kind {
        ExprKind::Field(b, _) | ExprKind::Unary(_, b) => visit(b, f),
        ExprKind::Binary(_, a, b) => {
            visit(a, f);
            visit(b, f);
        }
        ExprKind::Call(c) => c.args.iter().for_each(|a| visit(a, f)),
        ExprKind::Record { fields, .. } => fields.iter().for_each(|x| visit(&x.value, f)),
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Duration(_)
        | ExprKind::String(_)
        | ExprKind::Bool(_)
        | ExprKind::Var(_)
        | ExprKind::Enum(..) => {}
    }
}

/// Renames every read of the variable `from` in `e` to `to`.
fn rename_var(e: &mut ast::Expr, from: &str, to: &str) {
    match &mut e.kind {
        ExprKind::Var(n) if n.name == from => n.name = to.to_string(),
        ExprKind::Field(b, _) | ExprKind::Unary(_, b) => rename_var(b, from, to),
        ExprKind::Binary(_, a, b) => {
            rename_var(a, from, to);
            rename_var(b, from, to);
        }
        ExprKind::Call(c) => c.args.iter_mut().for_each(|a| rename_var(a, from, to)),
        ExprKind::Record { fields, .. } => fields
            .iter_mut()
            .for_each(|x| rename_var(&mut x.value, from, to)),
        _ => {}
    }
}
