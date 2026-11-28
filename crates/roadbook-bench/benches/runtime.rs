//! Where a run's time goes, on the example charts.
//!
//! - `core`: the interpreter alone, `Program::start` and `Program::step` replaying a recorded
//!   run ([`Script`]). Throughput is in steps.
//! - `floor`: building each step's result again by cloning it, the snapshot and the commands
//!   with their trace records. Any core that keeps today's snapshot and record types must at
//!   least build these, so `core` minus `floor` bounds what generated code could save.
//! - `parts`: work around each step that is not interpretation: cloning the snapshot a step
//!   starts from (the core does it once per step), building the `Saved` the driver publishes
//!   after each step, and writing each record as a canonical JSON line.
//! - `run`: whole runs through the driver and the generated handlers, with no trace and with
//!   canonical JSON lines thrown away.
//!
//! `cargo bench -p roadbook-bench`

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use roadbook::{TraceRecord, TraceSink};
use roadbook_bench::{AtOnce, Never, Script, config, large, orders, runtime, shop, sinks};
use roadbook_core::{Command, Saved};

fn scripts() -> Vec<(&'static str, Script)> {
    vec![
        ("shop/place_order", shop::place_order_script()),
        ("shop/checkout", shop::checkout_script()),
        ("large/ticket", large::script()),
        ("orders/order", orders::order_script()),
        ("orders/inventory", orders::inventory_script()),
    ]
}

fn core(c: &mut Criterion) {
    let scripts = scripts();
    let mut g = c.benchmark_group("core");
    for (name, script) in &scripts {
        g.throughput(Throughput::Elements(script.inputs.len() as u64 + 1));
        g.bench_function(*name, |b| b.iter(|| script.replay(|_, _| {})));
    }
    g.finish();

    let mut g = c.benchmark_group("floor");
    for (name, script) in &scripts {
        let steps = script.steps();
        g.throughput(Throughput::Elements(steps.len() as u64));
        g.bench_function(*name, |b| {
            b.iter(|| {
                for (snap, commands) in &steps {
                    std::hint::black_box((snap.clone(), commands.clone()));
                }
            })
        });
    }
    g.finish();

    let mut g = c.benchmark_group("parts");
    for (name, script) in &scripts {
        let steps = script.steps();
        g.throughput(Throughput::Elements(steps.len() as u64));
        g.bench_function(BenchmarkId::new("snapshot-clone", name), |b| {
            b.iter(|| {
                for (snap, _) in &steps {
                    std::hint::black_box(snap.clone());
                }
            })
        });
        g.bench_function(BenchmarkId::new("publish", name), |b| {
            b.iter(|| {
                for (snap, _) in &steps {
                    std::hint::black_box(Saved::new(snap, |_, delay| delay));
                }
            })
        });
        let sink = roadbook::JsonLines::new(std::io::sink());
        g.bench_function(BenchmarkId::new("json-lines", name), |b| {
            b.iter(|| {
                for (_, commands) in &steps {
                    for c in commands {
                        if let Command::Trace { record } = c {
                            sink.record(&TraceRecord {
                                t: 0,
                                run: "r1".into(),
                                parent: None,
                                record: record.clone(),
                            });
                        }
                    }
                }
            })
        });
    }
    g.finish();
}

fn run(c: &mut Criterion) {
    let rt = runtime();
    let mut g = c.benchmark_group("run");
    for (sink_name, sink) in sinks() {
        let never = config(Never, sink.clone());
        let at_once = config(AtOnce, sink.clone());
        g.bench_function(BenchmarkId::new("shop/place_order", sink_name), |b| {
            b.iter(|| rt.block_on(shop::place_order(&never, &shop::App::new(0))))
        });
        g.bench_function(BenchmarkId::new("shop/checkout", sink_name), |b| {
            b.iter(|| rt.block_on(shop::checkout(&at_once, &shop::App::new(3))))
        });
        g.bench_function(BenchmarkId::new("large/ticket", sink_name), |b| {
            b.iter(|| rt.block_on(large::run(&never)))
        });
        g.bench_function(BenchmarkId::new("orders/orders", sink_name), |b| {
            b.iter(|| rt.block_on(orders::run(&never)))
        });
    }
    g.finish();
}

criterion_group!(benches, core, run);
criterion_main!(benches);
