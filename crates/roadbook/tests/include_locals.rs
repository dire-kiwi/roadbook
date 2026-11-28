//! State parameters and state variables through `roadbook::include!`: the functions only a
//! target's arguments and a state variable's initial value call are `Handlers` methods, and a
//! run passes the values along.

#![deny(warnings)]

use std::sync::Mutex;

mod locals {
    roadbook::include!("tests/charts/locals.road");
}

use locals::*;

#[derive(Default)]
struct Host {
    logged: Mutex<Vec<String>>,
}

impl count::Handlers for Host {
    async fn log(&self, text: &str) {
        self.logged.lock().unwrap().push(text.to_owned());
    }

    fn mint(&self, code: &str) -> Tok {
        Tok {
            id: format!("t-{code}"),
        }
    }

    fn start(&self, tok: &Tok) -> i64 {
        tok.id.len() as i64 - 2
    }
}

#[tokio::test]
async fn arguments_and_initial_values_reach_the_host() {
    let host = std::sync::Arc::new(Host::default());
    let run = count::spawn(host.clone());
    run.handle()
        .send(count::Event::Go { code: "a".into() })
        .await;
    for _ in 0..3 {
        run.handle().send(count::Event::Tick).await;
    }
    assert_eq!(run.result().await, Ok("t-a".to_string()));
    assert_eq!(*host.logged.lock().unwrap(), ["t-a"]);
}
