//! `examples/large`: `SupportTicket`, a long-lived machine with nested, parallel and
//! timer-heavy states, taking a stream of events. The run goes through intake and triage on
//! its own, then takes [`ROUNDS`] rounds of `AgentReplied` and `CustomerReplied` (each one
//! leaves and enters states and restarts timers), a note, a hold, an escalation, the parallel
//! `Resolving` state, and the survey, and ends `Resolved`.

use std::time::Duration;

use roadbook::Config;
use serde_json::{Value, json};

use crate::{Outcome, Scenario, Script, event, object};

roadbook::include!("../../examples/large/ticket.road");

/// Rounds of agent and customer replies in one run.
pub const ROUNDS: usize = 50;

/// Handlers that answer at once. The survey is answered by a stub, not a child run: its
/// machine waits a day for an answer, and these runs never fire a timer.
pub struct Desk;

impl support_ticket::Handlers for Desk {
    fn is_spam(&self, _: &Classification) -> bool {
        false
    }
    fn is_duplicate(&self, _: &Classification) -> bool {
        false
    }
    fn backoff(&self, attempt: i64) -> Duration {
        Duration::from_secs(attempt as u64)
    }
    fn triage_sla(&self, _: Priority) -> Duration {
        Duration::from_secs(3600)
    }
    fn resolution_sla(&self, _: Priority) -> Duration {
        Duration::from_secs(86400)
    }
    fn closure(&self, ticket: &Ticket, reason: CloseReason, reopens: i64) -> Closure {
        Closure {
            ticket_id: ticket.id.clone(),
            reason,
            reopens,
            rating: None,
        }
    }
    fn rated_closure(&self, ticket: &Ticket, reopens: i64, survey: &SurveyResult) -> Closure {
        Closure {
            ticket_id: ticket.id.clone(),
            reason: CloseReason::Resolved,
            reopens,
            rating: Some(survey.score),
        }
    }
    async fn classify(&self, ticket: &Ticket) -> Result<Classification, ClassifyError> {
        Ok(Classification {
            category: Category::Technical,
            priority: ticket.priority,
            spam_score: 0.1,
            duplicate_of: None,
        })
    }
    async fn route_ticket(&self, _: &Ticket, _: Category) -> Result<String, RouteTicketError> {
        Ok("agent_7".into())
    }
    async fn file_bug(&self, _: &Ticket) -> Result<String, FileBugError> {
        Ok("bug_1".into())
    }
    async fn verify_fix(&self, _: &Ticket) -> bool {
        true
    }
    async fn draft_resolution(&self, _: &Ticket) -> String {
        "Fixed in the latest release.".into()
    }
    async fn notify_customer(&self, _: &Ticket, _: &str) {}
    async fn notify_agent(&self, _: &str, _: &Ticket, _: &str) {}
    async fn page_on_call(&self, _: &Ticket, _: i64) {}
    async fn send_message(&self, _: &Ticket, _: &str) {}
    async fn add_note(&self, _: &Ticket, _: &str) {}
    async fn satisfaction_survey(&self, _: &Ticket) -> SurveyResult {
        SurveyResult {
            answered: true,
            score: 5,
        }
    }
}

pub fn ticket() -> Ticket {
    Ticket {
        id: "t_1".into(),
        customer_id: "c_1".into(),
        subject: "It does not work".into(),
        priority: Priority::High,
    }
}

/// The events up to `Resolve`, which can all be queued at once: none waits for an invoke.
pub fn working_events() -> Vec<support_ticket::Event> {
    use support_ticket::Event as E;
    let mut out = Vec::new();
    for i in 0..ROUNDS {
        out.push(E::AgentReplied {
            body: format!("reply {i}"),
        });
        out.push(E::CustomerReplied {
            body: format!("answer {i}"),
        });
    }
    out.push(E::InternalNote {
        text: "a note".into(),
    });
    out.push(E::Hold);
    out.push(E::Unhold);
    out.push(E::Escalate);
    out.push(E::Resolve);
    out
}

/// Whether the run is where it waits for an agent.
fn with_agent(s: &support_ticket::State) -> bool {
    format!("{s:?}").contains("WithAgent")
}

/// Whether the run waits for the customer to confirm the resolution.
fn awaiting_confirmation(s: &support_ticket::State) -> bool {
    format!("{s:?}").contains("AwaitingConfirmation")
}

/// One whole `SupportTicket` run as a spawned actor.
pub async fn run(config: &Config) -> Closure {
    let mut actor = support_ticket::spawn_with(config.clone(), std::sync::Arc::new(Desk), ticket());
    while !with_agent(&actor.state()) {
        actor.changed().await;
    }
    for e in working_events() {
        actor.send(e).await;
    }
    while !awaiting_confirmation(&actor.state()) {
        actor.changed().await;
    }
    actor.send(support_ticket::Event::CustomerConfirmed).await;
    actor.result().await.expect("the ticket is resolved")
}

fn functions(name: &str, args: &[Value]) -> Value {
    match name {
        "isSpam" | "isDuplicate" => json!(false),
        "backoff" => json!(1000 * args[0].as_i64().unwrap_or(0)),
        "triageSla" => json!(3_600_000),
        "resolutionSla" => json!(86_400_000),
        "closure" => {
            json!({ "ticketId": args[0]["id"], "reason": args[1], "reopens": args[2], "rating": null })
        }
        "ratedClosure" => {
            json!({ "ticketId": args[0]["id"], "reason": "Resolved", "reopens": args[1], "rating": args[2]["score"] })
        }
        _ => panic!("no function {name}"),
    }
}

/// The same run on the core alone.
pub fn script() -> Script {
    let mut outside: Vec<_> = working_events()
        .iter()
        .map(|e| event(serde_json::to_value(e).unwrap()))
        .collect();
    outside.push(event(json!({ "_tag": "CustomerConfirmed" })));
    Script::record(Scenario {
        chart: "examples/large/ticket.road",
        machine: "SupportTicket",
        input: object(json!({ "ticket": serde_json::to_value(ticket()).unwrap() })),
        outside,
        respond: |src, _, _| {
            Outcome::Done(match src {
                "classify" => json!({
                    "category": "Technical", "priority": "High", "spamScore": 0.1, "duplicateOf": null
                }),
                "routeTicket" => json!("agent_7"),
                "fileBug" => json!("bug_1"),
                "verifyFix" => json!(true),
                "draftResolution" => json!("Fixed in the latest release."),
                "SatisfactionSurvey" => json!({ "answered": true, "score": 5 }),
                _ => panic!("no task {src}"),
            })
        },
        functions,
        fire_timers: false,
    })
}
