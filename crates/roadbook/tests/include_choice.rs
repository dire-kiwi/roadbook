//! A machine that starts in a choice through `roadbook::include!`: `State` has no variant for
//! the choice, the actor starts out in the state its `else` leads to, and the first step
//! publishes the state the choice really chose.

#![deny(warnings)]

mod choice {
    roadbook::include!("tests/charts/choice.road");
}

use choice::pick::{Event, State};

struct Host;

impl choice::pick::Handlers for Host {
    fn big(&self, n: i64) -> bool {
        n > 10
    }
}

#[tokio::test]
async fn a_choice_is_never_the_state_a_run_is_in() {
    let mut run = choice::pick::spawn(std::sync::Arc::new(Host), 20);
    // Before the first step: where `else` goes.
    assert_eq!(run.state(), State::Small);
    assert_eq!(run.changed().await, State::Large);
    run.handle().send(Event::Again).await;
    assert_eq!(run.state(), State::Large);

    let run = choice::pick::spawn(std::sync::Arc::new(Host), 1);
    assert_eq!(run.state(), State::Small);
    run.handle().send(Event::Again).await;
    assert_eq!(run.state(), State::Small);
}
