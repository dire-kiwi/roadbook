# The Roadbook IR, version 0.1

The compiler turns an entry `.road` file and everything it imports into one JSON document, the IR. The viewer, both runtimes, the code generators and the exporters read the IR and nothing else. This page says what every member means. The schema, [`ir.schema.json`](ir.schema.json) (JSON Schema draft 2020-12), checks the shape, and the Rust crate [`roadbook-ir`](../crates/roadbook-ir) reads and writes it. A complete example, for [`examples/shop/place_order.road`](../examples/shop/place_order.road), is [`crates/roadbook-ir/tests/data/place_order.ir.json`](../crates/roadbook-ir/tests/data/place_order.ir.json).

## Conventions

**Every member is always present.** Nothing is ever omitted. An absent value is `null`, a missing doc comment is `""`, and an empty list or map is `[]` or `{}`. This gives each input exactly one output, which matters because compiled IR is committed and hashed, and it means readers never need to tell a missing member from a null one. Readers reject members this page does not list.

**Order is source order.** Maps keyed by name (`types`, `functions`, `actions`, `tasks`, `machines`, a machine's `events`) keep declaration order: the entry file's declarations first, then each imported file's in the order the files were first loaded (depth first: a file's imports, in the order written, are loaded before the next import of the file that imports it). Lists (fields, parameters, variables, states, transitions, effects, arguments) are in the order written, except that a list of effects puts every `set` before every `do` (see [effects](#effects)). A JSON parser that does not keep member order loses nothing a runtime needs, since order in the maps is for readability and diffs only.

**Names.** Upper-case names (`[A-Z][A-Za-z0-9]*`) are types, errors, events, states and machines. Lower-case names (`[a-z][A-Za-z0-9]*`) are variables, parameters, fields, functions, actions and tasks. Declaration names are unique among the files of a compilation (the entry file and everything it imports), so a name in `types`, `functions`, `actions`, `tasks` or `machines` identifies a declaration on its own.

**Docs.** `doc` is the text of the `///` lines directly before the item, one leading space removed from each line, trailing whitespace removed, lines joined with `\n`. `""` if there are none.

**Spans.** A span is `{"file", "line", "col", "endLine", "endCol"}`. `file` is an index into `sources`. Lines and columns are 1-based. Columns count Unicode scalar values (Rust `char`s, JavaScript code points), and a tab is one column. The end is exclusive: `endLine`/`endCol` is the position just after the last character. A span never includes the item's doc comment. Which text each span covers is given with each item below.

**Numbers.** Integers in the IR (literal values, span positions, `group`) are within ±(2^53 − 1), so every JSON parser reads them exactly and canonical JSON represents them exactly. An `int` literal outside that range is a compile error, and so is a `duration` literal of more than 2^53 − 1 milliseconds.

**File form.** The compiler writes the IR as JSON indented by two spaces, members in the order this page lists them, with a final newline (`Ir::to_json` in the Rust crate). Hashing and byte-for-byte comparison use [canonical JSON](#canonical-json) instead.

## Top level

```jsonc
{
  "irVersion": "0.1",
  "sources":   [{ "path": "examples/shop/place_order.road" }, { "path": "examples/shop/types.road" }],
  "types":     { /* Name: type declaration */ },
  "functions": { /* name: function declaration */ },
  "actions":   { /* name: action declaration */ },
  "tasks":     { /* name: task declaration */ },
  "machines":  { /* Name: machine */ }
}
```

| Member | Meaning |
| --- | --- |
| `irVersion` | `"MAJOR.MINOR"`. See [versioning](#versioning). |
| `sources` | Every file the IR was compiled from, the entry file first, then in load order (see [order](#conventions)). `path` is relative to the project root, with `/` separators. The project root is the nearest directory holding a `roadbook.toml`, searched upward from the entry file, or else the directory the compiler runs in. |
| `types` | Records, aliases, enums, errors and extern types. |
| `functions`, `actions`, `tasks` | The host declarations. |
| `machines` | Every machine declared in the entry file and its imports. |

## Type references

Wherever the IR names a type it uses a string in canonical source form: a built-in (`string`, `int`, `float`, `bool`, `duration`, `json`) or a declared upper-case name, followed by any number of `[]` (list of) and `?` (optional) suffixes, applied left to right, with no spaces. `LineItem[]` is a list of `LineItem`, `string?` an optional string, `string[]?` an optional list of strings, and `string?[]` a list of optional strings.

Pattern: `^(string|int|float|bool|duration|json|[A-Z][A-Za-z0-9]*)(\[\]|\?)*$`.

A declared name refers to a key of `types`, with one exception: inside an `on Name` transition the bare value `event` has type `Name`, which refers to the machine's `events`. The two never collide: a machine's event never has the name of a type, error or machine of the document, so a name is a key of `types` or of the machine's `events`, never both.

## Declarations

### Fields and parameters

Record fields, error fields, event fields and parameters have one shape:

```json
{ "name": "sku", "type": "string", "doc": "" }
```

### Types

Each entry of `types` has a `kind`, the kind's members, then `doc` and `span`. The span covers the whole declaration, from its first keyword to its closing brace or the end of its type.

| `kind` | Source | Members |
| --- | --- | --- |
| `record` | `type Name { fields }` | `fields`: fields, in order. |
| `alias` | `type Name = T` | `type`: the aliased type reference. |
| `enum` | `enum Name { A, B }` | `variants`: variant names, in order. At least one. |
| `error` | `error Name { fields }`, or `error Name` | `fields`, `[]` for a bare error. |
| `extern` | `extern type Name` | none. |

```json
"CardDeclined": {
  "kind": "error",
  "fields": [{ "name": "reason", "type": "string", "doc": "" }],
  "doc": "",
  "span": { "file": 1, "line": 15, "col": 1, "endLine": 15, "endCol": 44 }
}
```

### Functions, actions and tasks

| Declaration | Members, in order |
| --- | --- |
| `fn name(params) -> T` | `params`, `returns` (a type reference), `doc`, `span` |
| `action name(params)` | `params`, `doc`, `span` |
| `task name(params) [-> T] [! E \| …]` | `params`, `returns` (a type reference, or `null` if the task returns nothing), `errors` (error names in source order, `[]` if none), `doc`, `span` |

Spans cover the whole declaration.

## Machines

```jsonc
"Checkout": {
  "doc": "Charge the customer's card, retrying while the payment gateway is down.",
  "span": { "file": 2, "line": 4, "col": 1, "endLine": 32, "endCol": 2 },
  "params": [ /* fields */ ],
  "returns": "Receipt",
  "errors": ["CardDeclined", "GatewayUnavailable"],
  "vars": [ /* variables */ ],
  "events": {},
  "root": { /* state */ },
  "hash": "sha256:…"
}
```

| Member | Meaning |
| --- | --- |
| `doc`, `span` | The span runs from `machine` to the closing brace. |
| `params` | Parameters, in order. They are read-only variables. |
| `returns` | Result type, or `null` if the machine returns nothing. |
| `errors` | Declared errors, in source order. |
| `vars` | Variables, in source order. |
| `events` | Events the machine accepts, keyed by name, in source order. |
| `root` | The machine body as a state. See below. |
| `hash` | The [machine hash](#the-machine-hash). |

A **variable** is `{"name", "type", "init", "doc", "span"}`. `init` is the initial value expression, or `null` if there is none. With no initialiser, a variable of optional type starts as `null` and any other variable starts unassigned (see `spec/execution.md`). The span covers the declaration from `var` to the end of its type or initial value.

An **event** is `{"fields", "doc", "span"}`, keyed by its name. The span covers the declaration from `event` to its end.

The **root** is a state with `kind` `compound` whose `id` and `name` are the machine's name. Its `doc` and `span` are the machine's. It holds the machine's top-level states, its `initial`, and the `entry`, `exit`, `on`, `after` and `always` items written directly in the machine body.

## States

```jsonc
{
  "id": "Checkout.Charging",
  "name": "Charging",
  "kind": "atomic",
  "doc": "One attempt against the payment gateway.",
  "span": { "file": 2, "line": 12, "col": 3, "endLine": 21, "endCol": 4 },
  "initial": null,
  "states": [],
  "entry": [],
  "exit": [],
  "invoke": { /* invoke */ },
  "transitions": [ /* transitions */ ],
  "output": null,
  "fail": null
}
```

| Member | Meaning |
| --- | --- |
| `id` | Absolute dotted path from the machine: `PlaceOrder.Processing.Paying`. Unique within the machine. Traces, targets and `initial` use it. |
| `name` | The last segment of `id`. |
| `kind` | `atomic` (no children), `compound` (children, one active at a time), `parallel` (children are regions, all active at once) or `final`. |
| `doc`, `span` | The span runs from the state's first keyword (`state`, `final` or `parallel`) to its closing brace. |
| `initial` | Id of the child entered first. Set exactly when `kind` is `compound`, otherwise `null`. |
| `states` | Children, in source order. Empty for `atomic` and `final`. |
| `entry`, `exit` | [Effects](#effects) run on entering and leaving the state: all `set` assignments, then all `do` actions, each in the order written. |
| `invoke` | The task or machine run while the state is active, or `null`. At most one per state. |
| `transitions` | The state's transitions, including its invoke's outcomes, in document order. |
| `output`, `fail` | In a final state at the top level of a machine: the machine's result or failure expression. At most one of them is set. `null` everywhere else. |

## Invoke

```json
{ "kind": "task", "src": "chargeCard", "args": [ /* expressions */ ], "span": { "…": "…" } }
```

| Member | Meaning |
| --- | --- |
| `kind` | `task` (`src` is a key of `tasks`) or `machine` (`src` is a key of `machines`). |
| `src` | The task's or machine's name. |
| `args` | Argument expressions, positional, in parameter order. |
| `span` | From `invoke` to the closing brace of its outcomes. |

The outcomes written inside `invoke { … }` are transitions of the state, with `invoke.done` and `invoke.fail` triggers.

## Transitions

```jsonc
{
  "trigger": { "kind": "invoke.fail", "error": "GatewayUnavailable" },
  "group": 2,
  "guard": { "kind": "op", "op": "<", "args": [ /* attempts, 3 */ ], "type": "bool", "text": "attempts < 3" },
  "target": "Checkout.Backoff",
  "effects": [],
  "span": { "file": 2, "line": 17, "col": 9, "endLine": 17, "endCol": 41 }
}
```

| Member | Meaning |
| --- | --- |
| `trigger` | What enables it. See below. |
| `group` | The index, from 0, of the trigger block the transition was written in, among the blocks of its state. A single-line trigger is a block with one branch. The branches of one block are consecutive and share `group`; guards within a group are tried in order. |
| `guard` | Boolean expression, or `null` for an unguarded branch (including `else`). |
| `target` | Id of the target state, or `null` for an internal transition (the state is not left). |
| `effects` | [Effects](#effects) run when the transition is taken: all `set` assignments, then all `do` actions, each in the order written. |
| `span` | For a single-branch trigger, from the trigger keyword to the end of the branch. For a branch in a block, from its `if` or `else` to the end of the branch. |

**Transitions are identified by position**: the id of the state they belong to and their index in its `transitions`. The trace's `transition` records name a transition this way, with `from` and `index`.

### Triggers

| `kind` | Source | Other members |
| --- | --- | --- |
| `event` | `on Name` | `event`: the event's name, a key of the machine's `events`. |
| `any` | `on *` | none. Matches any event for which the state has no `on`. |
| `after` | `after delay` | `delay`: a `duration` expression, evaluated when the state's work starts, after the step that entered it has settled ([execution](execution.md#starting-work)). |
| `always` | `always` | none. |
| `done` | `done` in a state | none. The compound state reached a final child, or every region of the parallel state did. |
| `invoke.done` | `done` inside `invoke { }` | none. The invoked task or machine succeeded. `result` is in scope. |
| `invoke.fail` | `fail E` inside `invoke { }` | `error`: the error's name, or `"*"` for every declared error not listed. `error` is in scope. |

## Effects

| `kind` | Source | Other members |
| --- | --- | --- |
| `set` | `set var = value` | `var`: a machine variable (not a parameter). `value`: an expression. |
| `do` | `do action(args)` | `action`: a key of `actions`. `args`: expressions, positional, in parameter order. |

`set a = 1, b = 2` is two `set` effects. Every list of effects (a transition's, an `entry`'s, an `exit`'s) holds all its `set` effects, in the order written, then all its `do` effects, in the order written, because that is the order they run in: `do f(a)  set a = 1` and `set a = 1  do f(a)` give the same list, and the same hash. A runtime runs the list in order.

## Expressions

Every expression node is an object with `kind`, the kind's own members, then two members every node has:

- `type`: the node's type reference, filled in by the type checker.
- `text`: the node's canonical source form, as the formatter writes it, so tools can show any subexpression without a pretty-printer. Parentheses appear in `text` only where precedence needs them.

| `kind` | Source | Members |
| --- | --- | --- |
| `lit` | `3`, `1.5`, `"hi"`, `true`, `30s` | `value`, see below. `type` is the literal's built-in. |
| `ref` | `attempts`, `error.issues`, `event.sku` | `path`: a variable, parameter, or one of the implicit names `event`, `result`, `error`, followed by the names of the fields read from it. Never empty. |
| `field` | `confirmation(order, receipt).status` | `of`: the expression whose field is read, when it is not a variable. `name`: the field. |
| `call` | `retryDelay(attempts)` | `fn`: a key of `functions`. `args`: expressions, positional, in parameter order. |
| `op` | `a + b`, `not ok`, `-n` | `op`: one of `==` `!=` `<` `<=` `>` `>=` `+` `-` `*` `and` `or` (two `args`, left then right) or `not` `neg` (one `args`). |
| `record` | `CardDeclined { reason: "expired" }`, `GatewayUnavailable` | `fields`: `[{"name", "value"}]`, in the order written. `type` is the record or error type being built. A field-less error written bare is a record with no fields. |
| `enum` | `Role.Admin` | `variant`. `type` is the enum. |

**Literal values.** Read a literal by its `type`, never by the form of its JSON number, since JavaScript and canonical JSON do not keep the difference between `2` and `2.0`.

| `type` | `value` |
| --- | --- |
| `int` | A JSON integer within ±(2^53 − 1). |
| `float` | A finite JSON number. |
| `string` | A JSON string: the literal's value, escapes resolved. |
| `bool` | `true` or `false`. |
| `duration` | A non-negative JSON integer of milliseconds: `30s` is `30000`, `5m` is `300000`. |

**Types of the implicit names.** `result` in an `invoke.done` transition has the invoked task's or machine's `returns` type. `error` in an `invoke.fail` transition for error `E` has type `E`. `event` in an `on Name` transition has type `Name` (the event, see [type references](#type-references)), and `event.field` has the field's type.

```json
{
  "kind": "op",
  "op": "+",
  "args": [
    { "kind": "ref", "path": ["attempts"], "type": "int", "text": "attempts" },
    { "kind": "lit", "value": 1, "type": "int", "text": "1" }
  ],
  "type": "int",
  "text": "attempts + 1"
}
```

## Canonical JSON

Canonical JSON is the JSON Canonicalization Scheme of [RFC 8785](https://www.rfc-editor.org/rfc/rfc8785): no whitespace, object members sorted by name compared as arrays of UTF-16 code units, strings escaped as ECMAScript's `JSON.stringify` escapes them, and every number written as ECMAScript writes the IEEE 754 double nearest to it (`2.0` is `2`, `-0` is `0`, `1e21` is `1e+21`). In JavaScript it is `JSON.stringify` with object keys sorted by the default string sort. In Rust it is `roadbook_ir::jcs::to_canonical_string`.

It is used for the machine hash and for trace records in conformance tests, where the Rust and TypeScript runtimes must produce identical bytes.

## The machine hash

A machine's `hash` is `"sha256:"` followed by the 64 lowercase hexadecimal digits of the SHA-256 digest of the UTF-8 bytes of the canonical JSON of the machine's **semantic view**:

```jsonc
{
  "machine":   "Checkout",                  // the machine's name
  "machines":  { /* Name: strip(machine without "hash"), for every machine in M */ },
  "types":     { /* Name: strip(declaration), for every type in T */ },
  "functions": { /* name: strip(declaration), for every function in F */ },
  "actions":   { /* name: strip(declaration), for every action in A */ },
  "tasks":     { /* name: strip(declaration), for every task in K */ }
}
```

- **M** is the machine itself and every machine it invokes, directly or through other machines. A machine that invokes itself is included once. (Including the whole set, rather than each child's hash, is what makes the hash defined for recursive machines.)
- **F**, **A** and **K** are the functions called (`call`), the actions run (`do`) and the tasks invoked by the machines in M.
- **T** is every type named by the machines in M and by the declarations in F, A and K, together with every type named by a type in T. A type is named by the base of any type reference (parameters, results, variables, event fields, alias targets, record and error fields, and every expression's `type`), by each entry of an `errors` list, and by an `invoke.fail` trigger's `error` other than `"*"`. A name that refers to one of the machine's events rather than to `types` (the type of a bare `event`) is not part of T.
- **strip** removes every member named `doc`, `span` or `text` from every object, recursively. The only name-keyed map inside these values is a machine's `events`, whose keys are upper-case, so no user-chosen name is ever removed.

So comments, formatting, source positions, file names, declaration order and unused declarations do not change a machine's hash, and any change to what the machine does, to a type it uses, or to a machine it invokes does. In Rust, `roadbook_ir::machine_hash(&ir, name)` computes it, `roadbook_ir::semantic_view` returns the view, and `roadbook_ir::fill_hashes(&mut ir)` sets every machine's `hash`; the compiler calls the last after type checking. Snapshots and traces record the hash, so restoring a snapshot into a changed machine is refused and the viewer can tell when a trace came from another version of a chart.

## Versioning

`irVersion` is `"MAJOR.MINOR"`, two decimal numbers without leading zeros. This page describes `0.1`.

- While the major version is 0, every change to the IR is breaking and changes the minor version. A reader accepts only the exact version it was written for.
- From `1.0` on, a minor version only adds to the IR. A reader accepts its own major version with any minor version up to its own, and refuses a newer minor version rather than ignore members it does not know. A breaking change increases the major version.

A runtime checks the version before anything else and refuses an IR it does not understand, with a message naming both versions. In Rust, `roadbook_ir::check_version` does this and `Ir::from_json` calls it before reading the rest of the document. Any change to the IR's shape can change every machine hash, so a new IR version invalidates saved snapshots.

## Planned additions

Messaging between machines ([actors and messaging](../docs/design/actors.md), milestone 5) will need, in a later IR version:

- the type reference `ref Name`, a handle to a running machine `Name` (with the same `[]` and `?` suffixes);
- machine-level `emits`, keyed by event name with the shape of `events`;
- an optional name on `invoke` (`invoke Upload(file) as upload`);
- triggers on events from a child or peer (`on upload.Progress`), as an `event` trigger with the source's name;
- `send` and `emit` effects, each with an event name and a record value.
