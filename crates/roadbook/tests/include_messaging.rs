//! Messaging through `roadbook::include!`: the upload example (a parent that sends `Pause`
//! and `Resume` to the child it invokes `as upload` and shows the `Progress` it emits), a
//! `ref` passed on to an invoked child, and a `ref` given to an action.

#![deny(warnings)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use roadbook::{BoxFuture, Config, MemorySink, Stopped, Timer, TraceRecord};

mod up {
    roadbook::include!("../../examples/upload/publish.road");
}

mod workers {
    roadbook::include!("../../spec/conformance/charts/workers.road");
}

mod pair {
    roadbook::include!("../../spec/conformance/charts/pair.road");
}

/// Virtual milliseconds on Tokio's paused clock.
struct VirtualTimer(tokio::time::Instant);

impl Timer for VirtualTimer {
    fn now(&self) -> u64 {
        self.0.elapsed().as_millis() as u64
    }

    fn start(&self, _run: &str, _timer: u64, delay: u64) -> BoxFuture<'static, ()> {
        Box::pin(tokio::time::sleep(Duration::from_millis(delay)))
    }
}

fn config(sink: &Arc<MemorySink>) -> Config {
    Config::new()
        .sink(sink.clone())
        .timer(Arc::new(VirtualTimer(tokio::time::Instant::now())))
}

/// Lets the runs go on until `done` holds.
async fn until(mut done: impl FnMut() -> bool) {
    for _ in 0..10_000 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("the runs never got there");
}

fn has(records: &[TraceRecord], run: &str, kind: &str, state: &str) -> bool {
    records.iter().any(|r| {
        let j = r.to_json();
        r.run == run && j["kind"] == kind && j["state"] == state
    })
}

// ---- upload ------------------------------------------------------------------------------

struct Storage {
    /// One permit per chunk the storage service may store.
    chunks: tokio::sync::Semaphore,
    progress: Mutex<Vec<i64>>,
}

impl up::upload::Handlers for Storage {
    async fn send_chunk(&self, file: &up::File, offset: i64) -> Result<i64, up::SendChunkError> {
        self.chunks.acquire().await.unwrap().forget();
        Ok((offset + 100).min(file.size))
    }

    async fn finish_upload(&self, file: &up::File) -> Result<up::Url, up::FinishUploadError> {
        Ok(format!("https://files.example/{}", file.name))
    }

    fn percent_of(&self, file: &up::File, offset: i64) -> i64 {
        offset * 100 / file.size
    }

    fn complete(&self, file: &up::File, offset: i64) -> bool {
        offset >= file.size
    }
}

impl up::publish::Handlers for Storage {
    async fn show_progress(&self, percent: i64) {
        self.progress.lock().unwrap().push(percent);
    }

    async fn publish_post(&self, title: &str, url: &up::Url) -> up::Post {
        up::Post {
            title: title.to_owned(),
            url: url.clone(),
        }
    }

    async fn upload(&self, file: &up::File) -> Result<up::Url, up::UploadError> {
        up::upload::run(self, file.clone()).await
    }
}

#[tokio::test(start_paused = true)]
async fn a_parent_pauses_and_resumes_its_child_and_shows_its_progress() {
    let sink = Arc::new(MemorySink::new());
    let storage = Arc::new(Storage {
        chunks: tokio::sync::Semaphore::new(0),
        progress: Mutex::new(Vec::new()),
    });
    let draft = up::Draft {
        title: "Holiday".into(),
        file: up::File {
            name: "beach.jpg".into(),
            size: 300,
        },
    };
    let actor = up::publish::spawn_with(config(&sink), storage.clone(), draft);
    let parent = actor.id().to_owned();
    let child = format!("{parent}.1");

    storage.chunks.add_permits(1);
    until(|| *storage.progress.lock().unwrap() == [33]).await;

    actor.send(up::publish::Event::PauseClicked).await;
    until(|| has(&sink.records(), &child, "entered", "Upload.Paused")).await;
    // Paused: a permit now is not taken until the child is resumed.
    storage.chunks.add_permits(2);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(*storage.progress.lock().unwrap(), [33]);

    actor.send(up::publish::Event::ResumeClicked).await;
    let post = actor.result().await.unwrap();
    assert_eq!(
        post,
        up::Post {
            title: "Holiday".into(),
            url: "https://files.example/beach.jpg".into()
        }
    );
    assert_eq!(*storage.progress.lock().unwrap(), [33, 66, 100]);

    let records = sink.records();
    let sent: Vec<_> = records
        .iter()
        .filter(|r| r.run == parent && r.to_json()["kind"] == "sent")
        .map(|r| {
            (
                r.to_json()["to"].clone(),
                r.to_json()["value"]["_tag"].clone(),
            )
        })
        .collect();
    assert_eq!(
        sent,
        [
            (child.clone().into(), "Pause".into()),
            (child.clone().into(), "Resume".into())
        ]
    );
    let messages = records
        .iter()
        .filter(|r| r.run == child && r.to_json()["event"]["kind"] == "message")
        .count();
    assert_eq!(messages, 2);
    let progress = records
        .iter()
        .filter(|r| r.run == parent && r.to_json()["event"]["kind"] == "emitted")
        .count();
    assert_eq!(progress, 3);
}

// ---- a ref passed on to an invoked child ---------------------------------------------------

#[derive(Default)]
struct Office {
    log: Mutex<Vec<String>>,
}

impl workers::logbook::Handlers for Office {
    async fn log(&self, text: &str) {
        self.log.lock().unwrap().push(text.to_owned());
    }
}

impl workers::worker::Handlers for Office {}

impl workers::boss::Handlers for Office {
    async fn log(&self, text: &str) {
        self.log.lock().unwrap().push(format!("boss: {text}"));
    }

    async fn worker(&self, book: &workers::logbook::Ref) {
        workers::worker::run(self, book.clone()).await
    }
}

#[tokio::test(start_paused = true)]
async fn a_child_sends_to_the_peer_its_parent_passed_it() {
    let sink = Arc::new(MemorySink::new());
    let office = Arc::new(Office::default());
    let book = workers::logbook::spawn_with(config(&sink), office.clone());
    let boss = workers::boss::spawn_with(config(&sink), office.clone(), book.handle());
    boss.send(workers::boss::Event::Restart).await;
    boss.send(workers::boss::Event::Leave).await;
    until(|| office.log.lock().unwrap().len() == 2).await;
    boss.cancel().await;
    assert_eq!(boss.result().await, Err(Stopped::Cancelled));
    until(|| office.log.lock().unwrap().len() == 3).await;
    // What the cancelled workers emitted reached Boss too late: it never logged them.
    assert_eq!(
        *office.log.lock().unwrap(),
        ["worker stopped", "worker stopped", "boss stopped"]
    );
    book.handle().send(workers::logbook::Event::Close).await;
    assert_eq!(book.result().await, Ok(()));
}

// ---- a ref given to an action ------------------------------------------------------------

#[derive(Default)]
struct Notes(Mutex<Vec<String>>);

impl pair::receiver::Handlers for Notes {}

impl pair::sender::Handlers for Notes {
    async fn note(&self, peer: &pair::receiver::Ref) {
        self.0.lock().unwrap().push(peer.id().to_owned());
    }
}

#[tokio::test]
async fn an_action_gets_the_handle_its_ref_argument_names() {
    let notes = Arc::new(Notes::default());
    let receiver = pair::receiver::spawn(notes.clone());
    let id = receiver.id().to_owned();
    let sender = pair::sender::spawn(notes.clone(), receiver.handle());
    sender.handle().send(pair::sender::Event::Go).await;
    assert_eq!(sender.result().await, Ok(()));
    assert_eq!(receiver.result().await, Ok(()));
    assert_eq!(*notes.0.lock().unwrap(), [id]);
}
