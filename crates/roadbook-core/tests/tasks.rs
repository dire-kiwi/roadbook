//! Tasks that emit (`spec/execution.md#tasks-that-emit`): rules the conformance cases do not
//! reach on their own.

use roadbook_core::{Command, Defect, Input, Program, Record, Saved, SrcKind, Status};
use serde_json::{Map, Value, json};

fn program(src: &str) -> Program {
    let c = roadbook_compiler::compile_source("t.road", src, &Default::default());
    assert!(!c.has_errors(), "{}", c.render());
    Program::new(c.ir.unwrap()).unwrap()
}

fn no_fns(name: &str, _: Vec<Value>) -> Result<Value, Defect> {
    panic!("unexpected call of {name}")
}

fn records(commands: &[Command]) -> Vec<&Record> {
    commands
        .iter()
        .filter_map(|c| match c {
            Command::Trace { record } => Some(record),
            _ => None,
        })
        .collect()
}

fn event(tag: &str) -> Input {
    Input::Event {
        value: json!({ "_tag": tag }),
    }
}

fn emitted(invoke: u64, value: Value) -> Input {
    Input::TaskEmitted { invoke, value }
}

const CHART: &str = "\
task upload(size: int) -> int emits Progress { percent: int, sensitive token: string }

machine Child() {
  emits Progress { percent: int, sensitive token: string }
  initial A
  state A {}
}

machine M() -> int {
  var percent: int = 0
  var keyed: string = \"\"
  event Go
  event Add { name: string }
  event Done

  children uploads: upload by string

  initial Up

  state Up {
    invoke upload(1) as up
    on up.Progress set percent = event.percent
    on uploads.Progress set keyed = key
    on Add spawn uploads[event.name] = upload(2)
    on Go -> Other
    on Done -> End

    initial Inner
    state Inner {
      invoke Child() as kid
      on kid.Progress set percent = 0 - event.percent
    }
  }

  state Other {
    on Done -> End
  }

  final state End { output percent }
}
";

#[test]
fn a_tasks_emit_is_taken_through_its_as_name_and_not_through_a_childs() {
    let p = program(CHART);
    let (s, cmds) = p.start("M", "r1", Map::new(), &mut no_fns).unwrap();
    // Invoke 1 is the task, 2 is the child machine.
    assert!(cmds.iter().any(|c| matches!(c,
        Command::InvokeStart { invoke: 1, src_kind: SrcKind::Task, alias: Some(a), .. } if a == "up")));
    let progress = json!({"_tag": "Progress", "percent": 40, "token": "t"});
    let (s, cmds) = p.step(&s, emitted(1, progress.clone()), &mut no_fns);
    assert_eq!(s.vars["percent"], json!(40));
    // The input is shown redacted, by the task's own declaration.
    assert_eq!(
        records(&cmds)[0],
        &Record::Event {
            event: emitted(
                1,
                json!({"_tag": "Progress", "percent": 40, "token": "<redacted>"})
            )
        }
    );
    // The same event as the child's `emitted` input goes through `kid`, not `up`.
    let from_child = Input::Emitted {
        from: "r1.2".into(),
        invoke: Some(2),
        message: 1,
        value: progress.clone(),
    };
    let (s, _) = p.step(&s, from_child, &mut no_fns);
    assert_eq!(s.vars["percent"], json!(-40));
    // A `task.emitted` naming the child machine's invoke matches neither address.
    let (s, cmds) = p.step(&s, emitted(2, progress.clone()), &mut no_fns);
    assert!(matches!(records(&cmds)[..], [Record::Dropped { .. }]));
    // Taking it consumed nothing.
    assert_eq!(s.invokes.len(), 2);
    // Leaving the state cancels the invoke: an emit queued before is stale, and redacted by
    // every task that declares one of its shape.
    let (s, _) = p.step(&s, event("Go"), &mut no_fns);
    let (s, cmds) = p.step(&s, emitted(1, progress), &mut no_fns);
    assert_eq!(
        records(&cmds),
        [&Record::Stale {
            event: emitted(
                1,
                json!({"_tag": "Progress", "percent": 40, "token": "<redacted>"})
            )
        }]
    );
    assert_eq!(s.status, Status::Running);
}

#[test]
fn a_spawned_tasks_emit_binds_key_and_its_spawn_start_says_task() {
    let p = program(CHART);
    let (s, _) = p.start("M", "r1", Map::new(), &mut no_fns).unwrap();
    let add = Input::Event {
        value: json!({"_tag": "Add", "name": "a"}),
    };
    let (s, cmds) = p.step(&s, add, &mut no_fns);
    let start = cmds
        .iter()
        .find(|c| matches!(c, Command::SpawnStart { .. }))
        .unwrap();
    assert_eq!(
        serde_json::to_value(start).unwrap(),
        json!({"kind": "spawn.start", "spawn": 3, "src": "upload", "srcKind": "task",
               "args": {"size": 2}, "saved": null})
    );
    let progress = json!({"_tag": "Progress", "percent": 5, "token": "t"});
    let (s, _) = p.step(&s, emitted(3, progress.clone()), &mut no_fns);
    assert_eq!(s.vars["keyed"], json!("a"));
    // An `emitted` input with the spawned task's id never names the collection of tasks.
    let from_run = Input::Emitted {
        from: "r1.3".into(),
        invoke: Some(3),
        message: 1,
        value: progress,
    };
    let (_, cmds) = p.step(&s, from_run, &mut no_fns);
    assert!(matches!(records(&cmds)[..], [Record::Dropped { .. }]));
    // A spawned task's undeclared error is an `undeclared` defect.
    let fail = Input::SpawnFail {
        spawn: 3,
        error: json!({"_tag": "Nope"}),
    };
    let (s, _) = p.step(&s, fail, &mut no_fns);
    assert_eq!(s.status, Status::Defect);
}

#[test]
fn a_machine_hears_a_tasks_emits_by_the_tasks_name() {
    let p = program(CHART);
    let heard = p.heard("M", "upload");
    assert!(heard.contains("Progress") && heard.len() == 1);
    assert_eq!(p.heard("M", "Child").len(), 1);
}

#[test]
fn a_spawned_task_in_a_saved_snapshot_restarts_with_its_args() {
    let p = program(CHART);
    let (s, _) = p.start("M", "r1", Map::new(), &mut no_fns).unwrap();
    let add = Input::Event {
        value: json!({"_tag": "Add", "name": "a"}),
    };
    let (s, _) = p.step(&s, add, &mut no_fns);
    let v = Saved::new(&s, |_, delay| delay).to_value();
    // Its saved entry has no `child`, and with one it would be invalid.
    assert_eq!(
        v["spawned"],
        json!([{"id": 3, "children": "uploads", "key": "a", "args": {"size": 2}}])
    );
    let (saved, _) = p.check_saved(v, None, None).unwrap();
    let (_, cmds) = p.restore(&saved, "r2", 0, None, &mut no_fns).unwrap();
    assert!(cmds.iter().any(|c| matches!(c,
        Command::SpawnStart { spawn: 3, src_kind: SrcKind::Task, saved: None, args, .. }
            if args["size"] == json!(2))));
}
