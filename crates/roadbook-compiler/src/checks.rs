//! The `check` lines of machines: assertions about a machine's states, which `roadbook check`
//! verifies by exploring the chart's state graph. They are resolved here, once a machine is
//! lowered, to the ids of its IR states, and kept beside the IR, not in it: the IR, and every
//! machine hash, is the same with and without them. The states of `in(…)` expressions are
//! resolved here too, by the same rule.
//!
//! A state path is resolved from the machine body, as a target written at machine level is:
//! the first segment among the top-level states, then each further segment among the
//! children of the one before. It may name any state of the IR (the children a `retry` writes
//! out included) but not a history state, which is never active, a choice state, which a run
//! never rests in, nor the machine.

use std::collections::HashMap;

use roadbook_ir as ir;
use roadbook_syntax::ast::{self, Body, Target};
use roadbook_syntax::{Diagnostic, Span};

use crate::check::{Checker, doc};
use crate::codes;
use crate::index::SymbolId;
use crate::names::suggest;

/// A resolved `check` line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    /// The machine the check is written in.
    pub machine: String,
    pub property: Property,
    /// The whole check line, from `check` to the end of the property; its doc comment is not
    /// included.
    pub span: Span,
    /// The doc comment, `""` if none.
    pub doc: String,
    /// The property as the formatter writes it after `check`, such as `never A and B.C`.
    pub text: String,
}

/// What a check asserts, with its states resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Property {
    /// `never A and B`: the states are never all active at once (with one state: it is
    /// never active).
    Never(Vec<StatePath>),
    /// `reachable A`: some reachable configuration has the state active.
    Reachable(StatePath),
    /// `A only after B`: `state` is entered only once `after` has been active.
    OnlyAfter { state: StatePath, after: StatePath },
    /// `can finish`: from every reachable configuration, a top-level final state can be
    /// reached.
    CanFinish,
}

/// A state named in a check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatePath {
    /// The IR state id, such as `PaymentWebhook.Applying.RecordingRefund`.
    pub id: String,
    /// The path as written.
    pub span: Span,
}

/// An `in(…)` as lowered, before its path is resolved.
#[derive(Clone, Debug)]
pub(crate) struct InRef {
    /// The path as written.
    pub target: Target,
    /// The innermost state certain to be active, with its ancestors, where the expression is
    /// evaluated: naming it or an ancestor is always true (`RB0917`).
    pub sure: Option<String>,
}

/// What names a state path: a `check`, or an `in(…)` expression.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Naming {
    Check,
    In,
}

impl Naming {
    fn code(self) -> roadbook_syntax::diagnostic::Code {
        match self {
            Naming::Check => codes::UNKNOWN_CHECK_STATE,
            Naming::In => codes::UNKNOWN_IN_STATE,
        }
    }

    /// The path's name in a sentence.
    fn path(self) -> &'static str {
        match self {
            Naming::Check => "a check's state path",
            Naming::In => "the path in `in(…)`",
        }
    }

    /// The subject of a sentence: `a check`, `` `in(…)` ``.
    fn what(self) -> &'static str {
        match self {
            Naming::Check => "a check",
            Naming::In => "`in(…)`",
        }
    }
}

/// Resolves the `in(…)` expressions `ins` of machine `machine` against its lowered `root`,
/// adding each state named to the symbol index. The IR already holds the id each names; an
/// unknown, history or choice state is `RB0128`, and one always active where it is evaluated
/// a warning, `RB0917`.
pub(crate) fn resolve_in(
    ck: &mut Checker<'_>,
    machine: &str,
    ins: &[InRef],
    root: &ir::State,
    symbols: &HashMap<String, SymbolId>,
) {
    for r in ins {
        let Some(found) = state_path(ck, machine, root, symbols, &r.target, Naming::In) else {
            continue;
        };
        let Some(sure) = &r.sure else {
            continue;
        };
        let always = *sure == found.id
            || sure
                .strip_prefix(found.id.as_str())
                .is_some_and(|rest| rest.starts_with('.'));
        if always {
            let written = r
                .target
                .path
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>()
                .join(".");
            let help = if *sure == found.id {
                format!("this is evaluated only while `{written}` is active")
            } else {
                let below = sure.split_once('.').map_or(sure.as_str(), |(_, b)| b);
                format!(
                    "this is evaluated only while `{below}` is active, and `{written}` is a state around it"
                )
            };
            ck.diags.push(
                Diagnostic::warning(
                    codes::IN_ALWAYS_TRUE,
                    r.target.span,
                    format!("`in({written})` is always true here"),
                )
                .with_label("active wherever this is evaluated")
                .with_help(format!("{help}; name another state, or drop the test")),
            );
        }
    }
}

/// Resolves the checks of machine `machine`'s `body` against its lowered `root`, adding the
/// resolved ones to `ck.checks` and each state named to the symbol index. `symbols` maps the
/// IR id of each state written in the chart to its symbol.
pub(crate) fn resolve(
    ck: &mut Checker<'_>,
    machine: &str,
    body: &Body,
    root: &ir::State,
    symbols: &HashMap<String, SymbolId>,
) {
    for c in body.checks() {
        let mut path = |t: &Target| state_path(ck, machine, root, symbols, t, Naming::Check);
        let property = match &c.property {
            ast::Property::Never(states) => {
                let resolved: Vec<_> = states.iter().map(&mut path).collect();
                resolved
                    .into_iter()
                    .collect::<Option<_>>()
                    .map(Property::Never)
            }
            ast::Property::Reachable(s) => path(s).map(Property::Reachable),
            ast::Property::OnlyAfter { state, after } => {
                let (state, after) = (path(state), path(after));
                state
                    .zip(after)
                    .map(|(state, after)| Property::OnlyAfter { state, after })
            }
            ast::Property::CanFinish => Some(Property::CanFinish),
        };
        if let Some(property) = property {
            ck.checks.push(Check {
                machine: machine.to_string(),
                property,
                span: c.span,
                doc: doc(&c.doc),
                text: roadbook_syntax::property_text(&c.property),
            });
        }
    }
}

/// The state `t` names, or `None` with the reason reported.
fn state_path(
    ck: &mut Checker<'_>,
    machine: &str,
    root: &ir::State,
    symbols: &HashMap<String, SymbolId>,
    t: &Target,
    naming: Naming,
) -> Option<StatePath> {
    let written = t
        .path
        .iter()
        .map(|p| p.name.as_str())
        .collect::<Vec<_>>()
        .join(".");
    let mut node = root;
    for (i, seg) in t.path.iter().enumerate() {
        let Some(child) = node.states.iter().find(|s| s.name == seg.name) else {
            ck.diags
                .push(unknown(machine, node, &written, t, i, naming));
            return None;
        };
        if let Some(&symbol) = symbols.get(&child.id) {
            ck.index.refer(symbol, seg.span);
        }
        if matches!(
            child.kind,
            ir::StateKind::History | ir::StateKind::DeepHistory
        ) {
            let parent = &t.path[..i];
            let mut d = Diagnostic::error(
                naming.code(),
                seg.span,
                format!(
                    "`{written}` is a history state, which {} cannot name",
                    naming.what()
                ),
            );
            d = if parent.is_empty() {
                d.with_help(
                    "a history state is never active: it only returns to where its parent was",
                )
            } else {
                let parent = parent
                    .iter()
                    .map(|p| p.name.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
                d.with_help(format!(
                    "a history state is never active, only a target that returns to where `{parent}` was; name `{parent}` or one of its children"
                ))
            };
            ck.diags.push(d);
            return None;
        }
        if child.kind == ir::StateKind::Choice {
            ck.diags.push(
                Diagnostic::error(
                    naming.code(),
                    seg.span,
                    format!(
                        "`{written}` is a choice state, which {} cannot name",
                        naming.what()
                    ),
                )
                .with_help("a run never rests in a choice: it leaves it in the same step; name the states its branches go to"),
            );
            return None;
        }
        node = child;
    }
    Some(StatePath {
        id: node.id.clone(),
        span: t.span,
    })
}

/// The path from the top-level states to the one state below them named `name`, but not
/// including it, if exactly one state below them has that name.
fn deeper(root: &ir::State, name: &str) -> Option<String> {
    fn walk<'s>(s: &'s ir::State, name: &str, depth: usize, out: &mut Vec<&'s ir::State>) {
        for c in &s.states {
            if depth > 0 && c.name == name {
                out.push(s);
            }
            walk(c, name, depth + 1, out);
        }
    }
    let mut found = Vec::new();
    walk(root, name, 0, &mut found);
    match found.as_slice() {
        [parent] => parent
            .id
            .split_once('.')
            .map(|(_, below)| below.to_string()),
        _ => None,
    }
}

/// The error for segment `i` of `t`, which is not a child of `node`.
fn unknown(
    machine: &str,
    node: &ir::State,
    written: &str,
    t: &Target,
    i: usize,
    naming: Naming,
) -> Diagnostic {
    let seg = &t.path[i];
    let children: Vec<&str> = node
        .states
        .iter()
        .filter(|s| {
            !matches!(
                s.kind,
                ir::StateKind::History | ir::StateKind::DeepHistory | ir::StateKind::Choice
            )
        })
        .map(|s| s.name.as_str())
        .collect();
    if i == 0 {
        let what = naming.what();
        let d = Diagnostic::error(
            naming.code(),
            seg.span,
            match naming {
                Naming::Check => format!("unknown state `{}` in a check of `{machine}`", seg.name),
                Naming::In => format!("unknown state `{}` in `in(…)` in `{machine}`", seg.name),
            },
        );
        if seg.name == machine {
            // `check reachable M.A`: the path starts at the machine's top-level states.
            return match t.path.len() {
                1 => d.with_help(match naming {
                    Naming::Check => "a check names one of the machine's states; the machine itself is always active until it finishes, which `check can finish` is about",
                    Naming::In => "`in(…)` names one of the machine's states; the machine itself is always active while it runs",
                }),
                _ => {
                    let fixed = written[seg.name.len() + 1..].to_string();
                    d.with_help(format!(
                        "{what} names a state from the machine's top-level states, without the machine's name: `{fixed}`"
                    ))
                    .with_fix(format!("write `{fixed}`"), t.span, fixed)
                }
            };
        }
        return match suggest(&seg.name, children.iter().copied()) {
            Some(s) => d.with_help(format!("did you mean `{s}`?")).with_fix(
                format!("replace with `{s}`"),
                seg.span,
                s,
            ),
            None if children.is_empty() => d.with_help(format!("`{machine}` has no states")),
            None => match deeper(node, &seg.name) {
                // `in(Volume.Muted)` for `Main.Volume.Muted`: the path is not relative.
                Some(prefix) => {
                    let fixed = format!("{prefix}.{written}");
                    d.with_help(format!(
                        "{} starts at the machine's top-level states, and `{}` is inside `{prefix}`: `{fixed}`",
                        naming.path(),
                        seg.name
                    ))
                    .with_fix(format!("write `{fixed}`"), t.span, fixed)
                }
                None => d.with_help(format!(
                    "{} starts at the machine's top-level states: {}",
                    naming.path(),
                    children.join(", ")
                )),
            },
        };
    }
    let parent = &node.id;
    let d = Diagnostic::error(
        naming.code(),
        seg.span,
        format!(
            "unknown state `{written}` in {}: `{parent}` has no child `{}`",
            naming.what(),
            seg.name
        ),
    );
    match suggest(&seg.name, children.iter().copied()) {
        Some(s) => d.with_help(format!("did you mean `{s}`?")).with_fix(
            format!("replace with `{s}`"),
            seg.span,
            s,
        ),
        None if children.is_empty() => d.with_help(format!("`{parent}` has no child states")),
        None => d.with_help(format!("its children are {}", children.join(", "))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::MemoryLoader;
    use crate::project::Project;

    /// The checks the checker collects from `src`, and the codes of its diagnostics.
    fn checks(src: &str) -> (Vec<Check>, Vec<String>) {
        let mut loader = MemoryLoader::new().with("m.road", src);
        let project = Project::load(&mut loader, &["m.road".to_string()]).unwrap();
        let mut ck = Checker::new(&project);
        ck.lower(&project);
        let codes = ck.diags.iter().map(|d| d.code.to_string()).collect();
        (std::mem::take(&mut ck.checks), codes)
    }

    const CHART: &str = "\
error Declined
task charge() ! Declined
machine Pay() {
  event Go
  initial Idle
  state Idle { on Go -> Paying }
  state Paying {
    invoke charge() retry 2 after 1s {
      done -> Both
      fail Declined -> Done
    }
  }
  parallel state Both {
    state Left {
      initial A
      history H
      state A { on Go -> Done }
    }
    state Right {}
  }
  final state Done {}
  /// Never both.
  check never Both.Left.A   and Both.Right
  check reachable Paying.Backoff
  check Done only after Idle
  check can finish
}
";

    #[test]
    fn checks_are_resolved_to_state_ids() {
        let (found, codes) = checks(CHART);
        assert!(codes.is_empty(), "{codes:?}");
        let ids = |p: &Property| -> Vec<String> {
            match p {
                Property::Never(s) => s.iter().map(|s| s.id.clone()).collect(),
                Property::Reachable(s) => vec![s.id.clone()],
                Property::OnlyAfter { state, after } => vec![state.id.clone(), after.id.clone()],
                Property::CanFinish => vec![],
            }
        };
        let got: Vec<(String, Vec<String>, String, String)> = found
            .iter()
            .map(|c| {
                (
                    c.machine.clone(),
                    ids(&c.property),
                    c.text.clone(),
                    CHART[c.span.range()].to_string(),
                )
            })
            .collect();
        let row = |ids: &[&str], text: &str, line: &str| {
            (
                "Pay".to_string(),
                ids.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                text.to_string(),
                line.to_string(),
            )
        };
        assert_eq!(
            got,
            [
                row(
                    &["Pay.Both.Left.A", "Pay.Both.Right"],
                    "never Both.Left.A and Both.Right",
                    "check never Both.Left.A   and Both.Right"
                ),
                row(
                    &["Pay.Paying.Backoff"],
                    "reachable Paying.Backoff",
                    "check reachable Paying.Backoff"
                ),
                row(
                    &["Pay.Done", "Pay.Idle"],
                    "Done only after Idle",
                    "check Done only after Idle"
                ),
                row(&[], "can finish", "check can finish"),
            ]
        );
        assert_eq!(found[0].doc, "Never both.");
        assert_eq!(found[1].doc, "");
        let Property::Never(states) = &found[0].property else {
            panic!()
        };
        assert_eq!(&CHART[states[1].span.range()], "Both.Right");
    }

    #[test]
    fn unknown_paths_and_history_states_are_errors() {
        let bad = CHART
            .replace("check can finish", "check never Both.Left.H and Idel\n  check reachable Pay.Idle\n  check reachable Done.X")
            .replace("check reachable Paying.Backoff", "check reachable Paying.Attempt");
        let (found, codes) = checks(&bad);
        assert_eq!(codes, ["RB0123", "RB0123", "RB0123", "RB0123"]);
        // The good ones are kept, the others left out.
        let texts: Vec<&str> = found.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "never Both.Left.A and Both.Right",
                "reachable Paying.Attempt",
                "Done only after Idle"
            ]
        );
    }

    #[test]
    fn every_machine_of_the_compilation() {
        let src = "machine A() {\n  initial X\n  final state X {}\n  check reachable X\n}\nmachine B() {\n  initial Y\n  final state Y {}\n  check can finish\n}\n";
        let (found, codes) = checks(src);
        assert!(codes.is_empty(), "{codes:?}");
        let got: Vec<(&str, &str)> = found
            .iter()
            .map(|c| (c.machine.as_str(), c.text.as_str()))
            .collect();
        assert_eq!(got, [("A", "reachable X"), ("B", "can finish")]);
    }
}
