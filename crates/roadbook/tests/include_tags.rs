//! Tags through `roadbook::include!`: a generated `Tag` enum of every tag the machine
//! declares, and an actor's `tags` and `has_tag`, the tags of its active states.

#![deny(warnings)]

mod tags {
    roadbook::include!("tests/charts/tags.road");
}

use tags::spinner::{Event, State, Tag};

struct Host;

impl tags::spinner::Handlers for Host {}

#[tokio::test]
async fn an_actor_has_the_tags_of_its_active_states() {
    assert_eq!(Tag::Busy.as_str(), "busy");
    let mut run = tags::spinner::spawn(std::sync::Arc::new(Host));
    // Before the first step: the states down to where it starts.
    assert_eq!(run.tags(), [Tag::App, Tag::Idle]);
    run.handle().send(Event::Load).await;
    assert_eq!(run.changed().await, State::Loading);
    assert_eq!(run.tags(), [Tag::App, Tag::Busy, Tag::Network]);
    assert!(run.has_tag(Tag::Busy));
    assert!(!run.has_tag(Tag::Idle));
    run.handle().send(Event::Stop).await;
    assert_eq!(run.changed().await, State::Done);
    // A run that has ended has no active states.
    assert_eq!(run.tags(), []);
    assert!(!run.has_tag(Tag::App));
}
