//! The fixture both cores run, `tests/data/redact-malformed.json`: redaction fails closed on
//! values no conformance script can give. `packages/core/test/redact.test.ts` compares the same
//! expected records, so the runtimes agree byte for byte on these too.

use roadbook_core::{Command, Defect, Input, Program};
use serde_json::Value;

fn no_fns(name: &str, _: Vec<Value>) -> Result<Value, Defect> {
    panic!("unexpected call of {name}")
}

#[test]
fn redaction_fails_closed_on_malformed_values() {
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let data = repo.join("crates/roadbook-core/tests/data");
    let read = |p: std::path::PathBuf| std::fs::read_to_string(p).unwrap();
    let fixture: Value = serde_json::from_str(&read(data.join("redact-malformed.json"))).unwrap();
    let program = Program::from_json(&read(repo.join(fixture["ir"].as_str().unwrap()))).unwrap();
    let mut lines = String::new();
    let mut keep = |commands: &[Command]| {
        for c in commands {
            if let Command::Trace { record } = c {
                lines.push_str(&roadbook_ir::jcs::to_canonical_string(&record.to_value()));
                lines.push('\n');
            }
        }
    };
    let input = fixture["input"].as_object().unwrap().clone();
    let machine = fixture["machine"].as_str().unwrap();
    let (mut snap, commands) = program.start(machine, "r1", input, &mut no_fns).unwrap();
    keep(&commands);
    for i in fixture["inputs"].as_array().unwrap() {
        let input: Input = serde_json::from_value(i.clone()).unwrap();
        let (next, commands) = program.step(&snap, input, &mut no_fns);
        keep(&commands);
        snap = next;
    }
    assert_eq!(lines, read(data.join("redact-malformed.jsonl")));
}
