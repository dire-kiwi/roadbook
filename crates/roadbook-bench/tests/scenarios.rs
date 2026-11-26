//! The two levels the benchmarks measure run the same runs: a whole run through the driver
//! writes, for its root run, exactly the records the core writes replaying its script. The
//! orders run, many runs messaging a peer, has no single script to compare with: it is
//! checked to go as intended.

use std::sync::Arc;

use roadbook::{MemorySink, Record};
use roadbook_bench::{AtOnce, Never, Script, config, large, orders, runtime, shop};
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

#[test]
fn orders() {
    let sink = Arc::new(MemorySink::new());
    runtime().block_on(orders::run(&config(Never, sink.clone())));
    let records = sink.records();
    assert!(!records.iter().any(|r| matches!(
        r.record,
        Record::Dropped { .. } | Record::Stale { .. } | Record::Undelivered { .. }
    )));
    let done = |state: &str| {
        records
            .iter()
            .filter(|r| matches!(&r.record, Record::Done { state: s, .. } if s == state))
            .count()
    };
    assert_eq!(done("Order.Paid"), orders::ORDERS);
    assert_eq!(done("Inventory.Closed"), 1);
}
