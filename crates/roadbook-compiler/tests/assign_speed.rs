//! Definite assignment stays about linear in the size of the chart: `examples/large` and
//! generated charts of 1,000 states, each shape built to defeat a naive fixpoint, compile
//! well within a generous time bound that a quadratic blowup would exceed.

use std::path::Path;
use std::time::{Duration, Instant};

use roadbook_compiler::{FsLoader, Options, compile, compile_source};

const N: usize = 1000;

/// A generous bound for a debug build on a slow machine; the charts take a few milliseconds.
const BOUND: Duration = Duration::from_secs(5);

const DECLS: &str = "action log(n: int)\nfn f(n: int) -> bool\n";
const EVENTS: &str = "  event Go\n  event Back\n";

/// A chain of `N` states written in reverse order, each assigning one variable and reading
/// the first one and the one its predecessor assigned, with a way back to the start from
/// each: the facts must flow the whole length of the chain, against document order.
fn chain() -> String {
    let mut s = format!("{DECLS}machine Chain() {{\n{EVENTS}");
    for i in 0..N {
        s += &format!("  var v{i}: int\n");
    }
    s += "  initial S0\n";
    for i in (0..N).rev() {
        let read = if i == 0 {
            String::new()
        } else {
            format!("  entry do log(v0)  do log(v{})\n", i - 1)
        };
        let next = if i + 1 == N {
            "End".to_string()
        } else {
            format!("S{}", i + 1)
        };
        s += &format!(
            "  state S{i} {{\n{read}    on Go -> {next}  set v{i} = {i}\n    on Back -> S0\n  }}\n"
        );
    }
    s += "  final state End {}\n}\n";
    s
}

/// `N` states under one compound state, each reachable from each of its neighbours and from
/// a machine-level transition, so the compound state's children change often.
fn flat() -> String {
    let mut s = format!("{DECLS}machine Flat() {{\n{EVENTS}  var x: int\n  var y: int = 0\n");
    s += "  initial P\n  on Back -> P.S0  set x = 0\n  state P {\n    initial S0\n";
    s += "    entry set x = 1\n    on Go if f(x) -> End\n";
    for i in 0..N {
        let next = (i + 1) % N;
        let prev = (i + N - 1) % N;
        s += &format!(
            "    state S{i} {{\n      entry do log(x)\n      on Go {{\n        if f(y) -> S{next}  set y = {i}\n        else -> S{prev}\n      }}\n    }}\n"
        );
    }
    s += "  }\n  final state End {}\n}\n";
    s
}

/// 50 levels of nesting, twenty states each, with transitions across the levels.
fn deep() -> String {
    let levels = 50;
    let mut s = format!("{DECLS}machine Deep() {{\n{EVENTS}  var x: int\n  initial L0\n");
    let mut close = String::new();
    for l in 0..levels {
        s += &format!("  state L{l} {{\n");
        if l + 1 < levels {
            s += &format!("    initial L{}\n", l + 1);
        } else {
            s += "    initial A0\n";
        }
        for k in 0..N / levels - 1 {
            s += &format!("    state A{l}x{k} {{ entry set x = {k}  on Go -> L0  do log(x) }}\n");
        }
        close += "  }\n";
    }
    s += "    state A0 { entry do log(x)  on Back -> L0.L1 }\n";
    s += &close;
    s += "}\n";
    s
}

/// A parallel state of `N / 4` regions of three states each. Each region reads `x` before its
/// own transition assigns it: an error in each, since another region's assignment does not
/// count.
fn regions() -> String {
    let mut s = format!(
        "{DECLS}machine Regions() {{\n{EVENTS}  var x: int\n  initial P\n  parallel state P {{\n"
    );
    for r in 0..N / 4 {
        s += &format!(
            "    state R{r} {{\n      initial A\n      state A {{ on Go -> B  set x = {r}  on Back do log(x) }}\n      state B {{ on Go -> C  do log(x) }}\n      final state C {{}}\n    }}\n"
        );
    }
    s += "    done -> End  do log(x)\n  }\n  final state End {}\n}\n";
    s
}

#[test]
fn large_charts_compile_quickly() {
    for (name, src, reads) in [
        ("chain", chain(), 0),
        ("flat", flat(), 0),
        ("deep", deep(), 1),
        ("regions", regions(), N / 4),
    ] {
        let start = Instant::now();
        let out = compile_source("big.road", &src, &Options::default());
        let took = start.elapsed();
        let unassigned = out
            .diagnostics
            .iter()
            .filter(|d| d.code.to_string() == "RB0403")
            .count();
        let others: Vec<_> = out
            .diagnostics
            .iter()
            .filter(|d| d.code.to_string() != "RB0403" && d.is_error())
            .collect();
        assert!(others.is_empty(), "{name}: {}", out.render());
        assert_eq!(unassigned, reads, "{name}: {}", out.render());
        let without = Options {
            allow_unassigned: vec!["big.road".into()],
            ..Options::default()
        };
        let start = Instant::now();
        compile_source("big.road", &src, &without);
        let base = start.elapsed();
        eprintln!(
            "{name}: {} lines in {took:?}, {base:?} without definite assignment",
            src.lines().count()
        );
        assert!(took < BOUND, "{name} took {took:?}");
    }
}

#[test]
fn examples_large_compiles_quickly() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for f in ["ticket", "survey"] {
        let start = Instant::now();
        let mut loader = FsLoader::new(&root);
        let out = compile(
            &mut loader,
            &format!("examples/large/{f}.road"),
            &Options::default(),
        )
        .unwrap();
        let took = start.elapsed();
        assert!(!out.has_errors(), "{}", out.render());
        eprintln!("examples/large/{f}.road in {took:?}");
        assert!(took < BOUND, "{f} took {took:?}");
    }
}
