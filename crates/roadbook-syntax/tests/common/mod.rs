//! Inputs shared by the test files: one valid file per construct of the grammar.

#![allow(dead_code)]

/// `(name, source)`: each parses with no diagnostics.
pub const CASES: &[(&str, &str)] = &[
    (
        "imports",
        "import { A, b, } from \"./x.road\"\nimport {\n  A, B,\n  c\n} from \"y \\\"q\\\".road\"\n",
    ),
    (
        "records_and_aliases",
        "type A { x: int, y: string[]?, z: B? }\ntype Id = string\ntype Empty {}\ntype Lists = int[][]\n",
    ),
    ("enum", "enum Role { Customer, Admin, }\n"),
    (
        "errors",
        "error E\nerror F { reason: string, code: int }\nerror G {}\n",
    ),
    ("extern_type", "extern type Money\n"),
    (
        "functions_actions_tasks",
        "fn f(a: int, b: duration) -> bool\naction log(what: string)\naction ping()\ntask t(x: json) -> float ! E | F\ntask u()\ntask v() ! E\n",
    ),
    (
        "machine_signatures",
        "machine A() {}\nmachine B(x: int, y: User) -> Out ! E | F {}\nmachine C() ! E {}\n",
    ),
    (
        "vars_and_events",
        "machine M() {\n  var a: int = 1\n  var b: string?\n  var c: Order[]\n  event Go\n  event Change { value: int, note: string }\n  initial S\n  state S {}\n}\n",
    ),
    (
        "states",
        "machine M() {\n  initial A\n  state A {\n    initial Inner\n    state Inner { on Go -> Done }\n    final state Done {}\n  }\n  parallel state P {\n    state R1 {}\n    state R2 {}\n  }\n  final state Z {}\n}\n",
    ),
    (
        "entry_and_exit",
        "machine M() {\n  state A {\n    entry set a = 1, b = 2  do f(a), g()\n    exit do h()\n    entry do f()  set a = 2\n  }\n}\n",
    ),
    (
        "transitions",
        "machine M() {\n  state A {\n    on Go -> B\n    on * -> C\n    on Tick if n < 3 -> A.Inner  set n = n + 1\n    on Poke do log(\"poked\")\n    after 30s -> Late\n    after delay(n) -> Late\n    after (Cfg { ms: 5 }) -> Late\n    always if ready -> Go.Deep.Path\n    done -> Next\n  }\n}\n",
    ),
    (
        "branch_blocks",
        "machine M() {\n  state A {\n    on X {\n      if a -> A\n      if b -> B  set x = 1\n      else do f()\n    }\n    always {\n      if ok -> Done\n    }\n  }\n}\n",
    ),
    (
        "invoke",
        "machine M() {\n  state A {\n    invoke charge(order, user) {\n      done -> B  set r = result\n      fail CardDeclined -> C  set d = error\n      fail * -> D\n      fail Gateway {\n        if attempts < 3 -> Backoff\n        else -> GaveUp\n      }\n    }\n  }\n  state B {\n    invoke Checkout() {\n      done -> A\n    }\n  }\n}\n",
    ),
    (
        "messaging",
        "machine M(peer: ref Peer, peers: ref Peer[]?) {\n  emits Progress { percent: int }\n  emits Done\n  state A {\n    invoke Upload(file) as upload {\n      done -> B  emit Done\n    }\n    on Pause send upload.Pause\n    on upload.Progress set p = event.percent  emit Progress { percent: p, }\n    on peer.Reserved -> B  send peer.Ack { id: 1 }  do log()\n    entry send peer.Hello {}\n  }\n}\n",
    ),
    (
        "spawn",
        "machine M() {\n  /// The sessions.\n  children sessions: Session by string\n  children children: Job by int\n  on Connected spawn sessions[event.id] = Session(event.socket, 1)\n  on Gone stop sessions[event.id]  stop children[2]\n  on All send sessions.*.Notify { text: \"hi\" }\n  on One send sessions[event.id].Notify { text: event.text, }\n  on sessions.Typing do log(key)\n  on sessions.done -> A  set n = n - 1\n  on sessions.fail Lost do log(key)\n  on children.fail * do log(error)\n  state A {}\n}\n",
    ),
    (
        "final_states",
        "machine M() -> R ! E {\n  final state Ok { output R { value: 1 } }\n  final state Bad { fail E }\n  final state Worse { fail E { reason: \"x\" } }\n  final state Plain { output receipt }\n}\n",
    ),
    (
        "machine_level_items",
        "machine M() {\n  entry do start()\n  exit do stop()\n  on Cancel -> Cancelled\n  after 1h -> TimedOut\n  always if broken -> Cancelled\n  final state Cancelled {}\n  final state TimedOut {}\n}\n",
    ),
    (
        "expressions",
        "machine M() {\n  var a: bool = not x > 1 and y or z\n  var b: int = -(1 + 2) * 3 - (4 - 5)\n  var c: float = 1.5\n  var d: duration = 200ms\n  var e: string = \"tab\\tnew\\nline\"\n  var f: bool = true == false\n  var g: Role = Role.Admin\n  var h: int = event.sku.length\n  var i: R = R { a: f(1, \"x\"), b: S { c: error.issues } }\n  var j: int = -x.y\n  var k: bool = (not a) == b\n  var l: duration = 5m + 1h + 30s\n}\n",
    ),
    (
        "doc_comments",
        "/// A type.\ntype A { x: int }\n/// An enum.\nenum B { C }\n/// An error.\nerror D\n/// Extern.\nextern type E\n/// A fn.\n/// Two lines.\nfn f() -> int\n/// An action.\naction g()\n/// A task.\ntask h()\n/// A machine.\nmachine M() {\n  /// A variable.\n  var v: int\n  /// An event.\n  event Go\n  /// A state.\n  state S {\n    /// A final child.\n    final state T {}\n  }\n  /// A parallel state.\n  parallel state P {}\n}\n",
    ),
    (
        "comments",
        "// File comment.\n\nmachine M() { // trailing on header\n  // Before the state.\n  state A {\n    on Go -> B // trailing on transition\n    // At the end of the block.\n  }\n\n  //// Four slashes is a plain comment.\n  final state B {}\n}\n// End of file.\n",
    ),
];
