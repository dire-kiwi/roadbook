//! The two levels the benchmarks measure run the same runs: a whole run through the driver
//! writes, for its root run, exactly the records the core writes replaying its script.

use std::sync::Arc;

use roadbook::{MemorySink, Record};
use roadbook_bench::{AtOnce, Never, Script, config, large, runtime, shop};
use roadbook_core::Command;

fn core_records(script: &Script) -> Vec<Record> {
    let mut out = Vec::new();
    script.replay(|_, commands| {
        out.extend(commands.iter().filter_map(|c| match c {
            Command::Trace { record } => Some(record.clone()),
            _ => None,
        }))
    });
    out
}

fn root_records(sink: &MemorySink) -> Vec<Record> {
    let records = sink.records();
    let root = records[0].run.clone();
    records
        .into_iter()
        .filter(|r| r.run == root)
        .map(|r| r.record)
        .collect()
}

#[test]
fn place_order() {
    let sink = Arc::new(MemorySink::new());
    runtime().block_on(shop::place_order(
        &config(Never, sink.clone()),
        &shop::App::new(0),
    ));
    assert_eq!(
        root_records(&sink),
        core_records(&shop::place_order_script())
    );
}

#[test]
fn checkout() {
    let sink = Arc::new(MemorySink::new());
    runtime().block_on(shop::checkout(
        &config(AtOnce, sink.clone()),
        &shop::App::new(3),
    ));
    assert_eq!(root_records(&sink), core_records(&shop::checkout_script()));
}

#[test]
fn ticket() {
    let sink = Arc::new(MemorySink::new());
    runtime().block_on(large::run(&config(Never, sink.clone())));
    let records = root_records(&sink);
    assert!(
        !records
            .iter()
            .any(|r| matches!(r, Record::Dropped { .. } | Record::Stale { .. }))
    );
    assert_eq!(records, core_records(&large::script()));
}
