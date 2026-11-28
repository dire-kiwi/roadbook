# The Roadbook IR, version 0.12

The compiler turns an entry `.road` file and everything it imports into one JSON document, the IR. The viewer, both runtimes, the code generators and the exporters read the IR and nothing else. This page says what every member means. The schema, [`ir.schema.json`](ir.schema.json) (JSON Schema draft 2020-12), checks the shape, and the Rust crate [`roadbook-ir`](../crates/roadbook-ir) reads and writes it. A complete example, for [`examples/shop/place_order.road`](../examples/shop/place_order.road), is [`crates/roadbook-ir/tests/data/place_order.ir.json`](../crates/roadbook-ir/tests/data/place_order.ir.json). One with a call to a peer, a message to it and a declaration from another package, for [`examples/orders/orders.road`](../examples/orders/orders.road), is [`crates/roadbook-ir/tests/data/orders.ir.json`](../crates/roadbook-ir/tests/data/orders.ir.json).

## Conventions

**Every member is always present.** Nothing is ever omitted. An absent value is `null`, a missing doc comment is `""`, and an empty list or map is `[]` or `{}`. This gives each input exactly one output, which matters because compiled IR is committed and hashed, and it means readers never need to tell a missing member from a null one. Readers reject members this page does not list.

**Order is source order.** Maps keyed by name (`types`, `functions`, `actions`, `tasks`, `machines`, a machine's `events`, `internalEvents`, `emits` and `children`, a task's `emits`) keep declaration order: the entry file's declarations first, then each imported file's in the order the files were first loaded (depth first: a file's imports, in the order written, are loaded before the next import of the file that imports it). Lists (fields, parameters, variables, states, transitions, effects, arguments) are in the order written, except that a list of effects puts every `set` before the other effects (see [effects](#effects)). A JSON parser that does not keep member order loses nothing a runtime needs, since order in the maps is for readability and diffs only.

**Names.** Upper-case names (`[A-Z][A-Za-z0-9]*`) are types, errors, events, states and machines. Lower-case names (`[a-z][A-Za-z0-9]*`) are variables, parameters, fields, functions, actions and tasks. Declaration names are unique among the files of a compilation (the entry file and everything it imports), so a name in `types`, `functions`, `actions`, `tasks` or `machines` identifies a declaration on its own.

**Docs.** `doc` is the text of the `///` lines directly before the item, one leading space removed from each line, trailing whitespace removed, lines joined with `\n`. `""` if there are none.

**Spans.** A span is `{"file", "line", "col", "endLine", "endCol"}`. `file` is an index into `sources`. Lines and columns are 1-based. Columns count Unicode scalar values (Rust `char`s, JavaScript code points), and a tab is one column. The end is exclusive: `endLine`/`endCol` is the position just after the last character. A span never includes the item's doc comment. Which text each span covers is given with each item below.

**Numbers.** Integers in the IR (literal values, span positions, `group`) are within ±(2^53 − 1), so every JSON parser reads them exactly and canonical JSON represents them exactly. An `int` literal outside that range is a compile error, and so is a `duration` literal of more than 2^53 − 1 milliseconds.

**File form.** The compiler writes the IR as JSON indented by two spaces, members in the order this page lists them, with a final newline (`Ir::to_json` in the Rust crate). Hashing and byte-for-byte comparison use [canonical JSON](#canonical-json) instead.

## Top level

```jsonc
{
  "irVersion": "0.11",
  "sources":   [{ "path": "examples/shop/place_order.road", "package": null },
                { "path": "examples/shop/types.road", "package": null }],
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
| `sources` | Every file the IR was compiled from, the entry file first, then in load order (see [order](#conventions)). See [sources](#sources). |
| `types` | Records, aliases, enums, errors and extern types, and the built-in enum [`Ended`](#type-references) when the document names it. |
| `functions`, `actions`, `tasks` | The host declarations. |
| `machines` | Every machine declared in the entry file and its imports. |

## Sources

A source is `{"path", "package"}`.

| Member | Meaning |
| --- | --- |
| `path` | With `/` separators. For a file of the project, relative to the project root: the nearest directory holding a `roadbook.toml`, searched upward from the entry file, or else the directory the compiler runs in. For a file of another package, the path in the package as imports write it, after the package's name (which a `package.json` `exports` map may place elsewhere on disk). |
| `package` | `null` for a file of the project. For a file of another package, the package's name as imports write it: `inventory`, `@shop/inventory`. |

A file of another package is named by its package and its path in the package, never by where the package happens to be installed, so the IR is the same wherever the package was found (a `node_modules` directory, a directory mapped in `roadbook.toml`, a Cargo dependency). `import { Inventory } from "inventory/inventory.road"` loads the source `{"path": "inventory.road", "package": "inventory"}`.

**Declarations from another package.** A declaration comes from another package exactly when its span's `file` is a source whose `package` is not `null`. Nothing else marks it: it appears in `types`, `functions`, `actions`, `tasks` or `machines` like any other, complete, so the document stays self-contained and the runtimes and the viewer need nothing but the IR. A code generator uses the mark to refer to the owning package's generated types instead of generating its own. Declaration names are unique among all the files of the document, the other packages' files included.

## Type references

Wherever the IR names a type it uses a string in canonical source form: a base followed by any number of `[]` (list of) and `?` (optional) suffixes, applied left to right. `LineItem[]` is a list of `LineItem`, `string?` an optional string, `string[]?` an optional list of strings, and `string?[]` a list of optional strings. The base is one of:

| Base | Meaning |
| --- | --- |
| `string`, `int`, `float`, `bool`, `duration`, `json` | A built-in. |
| `Name` | A key of `types` (the built-in `Ended` among them); inside a generic declaration, one of its `params`; or, inside a machine, one of that machine's own events, a key of its `events`, `internalEvents` or `emits` (see below). |
| `Name<A, B>` | An instance of the generic record or alias `Name`, a key of `types` with as many `params` as there are arguments, each a type reference. See [generic types](#generic-types). |
| `map<K, V>` | A map from keys of type `K` to values of type `V`. `K` is `string` once aliases are expanded: `string`, `UserId` after `type UserId = string`. |
| `Machine.Name` | Event `Name` of machine `Machine`: a key of `machines`, then of that machine's `events` or `emits`. |
| `task.Name` | Event `Name` that task `task` emits: a key of `tasks`, then of that task's `emits`. `upload.Progress` is the type of `event` in `on up.Progress` for `invoke upload(…) as up`, and in `on uploads.Progress` for a collection of `upload`. |
| `ref Machine` | A handle to a running machine `Machine`, a key of `machines`. One space after `ref`. `ref Inventory[]` is a list of handles. |
| `call name` | Inside a machine, a pending call of that machine's call `name`, a key of its `calls`. One space after `call`. Only a variable's type, and the type of the implicit `call`: never with a suffix. |

There is no whitespace other than one space after `ref` and after `call`, and one after each `,` between type arguments, as the formatter writes them: `map<string, Order[]>?`, `Page<Pair<string, int>>[]`. The grammar, whose nesting a regular expression cannot check (the schema checks only the tokens):

```
type  = base { "[]" | "?" } | "call " lower
base  = builtin | Upper [ "<" type { ", " type } ">" ] | Upper "." Upper | lower "." Upper | "ref " Upper
      | "map<" type ", " type ">"
```

**Event types.** Events are not declared types: an event's type is named after the event. Inside machine `M`, a bare name that is one of `M`'s events or emitted events is that event: the type of `event` in `on Name`, and of the value of `emit Name { … }`. The event of another machine, or of a machine reached through a `ref` or a child, is written `Machine.Name`: the type of `event` in `on upload.Progress` is `Upload.Progress`, and the value of `send upload.Pause` has type `Upload.Pause`, also when the other machine is `M` itself. An event a task emits is written `task.Name`, after the task: `upload.Progress`. It is never the type of a value a chart builds, since no chart sends to a task or emits for one. Names never collide: an event never has the name of a type, error or machine of the document, and a machine's `events`, `internalEvents` and `emits` never share a name, so a bare name is a key of `types` or of the current machine's `events`, `internalEvents` or `emits`, never two of them. An internal event is never named `Machine.Name`: no other machine can send it or receive it. A task's emitted events are named only with the task's name before them, and a task's name is lower-case and a machine's upper-case, so `upload.Progress` and `Upload.Progress` are different events.

**Where a `ref` can be.** Machine parameters, variables, and the parameters of functions, actions and tasks may have a `ref` type, directly or through an alias, also inside a list, an optional or a map's values. Fields of records, errors and events, the parameters of calls, type arguments, and the results of functions, tasks and machines, never do, so a machine can only get a handle from its own signature or from `invoke … as`. A `ref` value is never a literal, never compared, never passed where `json` is expected, and a runtime passes it to host code as its own handle type. In snapshots and traces it is the id of the run it refers to ([values](execution.md#values)).

**Where a pending call can be.** A `call name` is the type of a machine variable of the machine that declares `name`, and of the implicit `call` in its `on name` transitions; nowhere else, not even inside an alias, an optional, a list, a map or a type argument. It is never a literal, never compared, never passed where `json` is expected, and never leaves the run: no parameter, field, result or event has the type ([calls](execution.md#calls-between-runs)).

**`Unanswered`.** The error name `Unanswered` is built in: the error every call can fail with besides its declared ones, `{"_tag": "Unanswered"}`, with no fields. It is not a key of `types`, and the only places the IR names it are an `invoke.fail` trigger of a call invoke, and the `type` of `error` in such a transition.

**`Ended`.** The enum name `Ended` is built in too: how a peer's run ended, the type of `event.status` in an [`ended`](#triggers) transition. Unlike `Unanswered` it is an ordinary entry of `types` whenever any type reference of the document names it (an expression's `type`, a variable's, a parameter's of a function, action or task, anywhere in a type), and is absent otherwise, so a reader resolves it as it resolves any enum:

```json
"Ended": {
  "kind": "enum",
  "variants": ["Done", "Failed", "Cancelled", "Defect", "Suspended"],
  "doc": "How a peer's run ended: the type of `event.status` in `on name.ended`. Built in.",
  "span": { "file": 0, "line": 1, "col": 1, "endLine": 1, "endCol": 1 }
}
```

It comes after every declared type. It is declared in no file, so its `doc` is always the text above and its span is empty, at the start of the entry file (file 0): a reader that takes a declaration's file to be where it is defined (a code generator deciding which module to import it from) finds it in the entry file's own module. No declaration, event or emitted event can be named `Ended`, so the name never means two things. A declaration copied from another package's compiled IR that names it keeps naming it, and the compiler writes `Ended` into the document it builds, never the package's copy. The machine hash sees it like any type (below): through [strip](#the-machine-hash), as `{"kind": "enum", "variants": [...]}`.

## Declarations

### Fields and parameters

Record fields, error fields, event fields and parameters have one shape:

```json
{ "name": "sku", "type": "string", "sensitive": false, "doc": "" }
```

`sensitive` is `true` for a field or parameter written `sensitive name: T`: trace records show its value as `"<redacted>"` ([redaction](trace.md#redaction)). On a function's parameter it changes nothing, since function calls are not traced. It is not part of the [machine hash](#the-machine-hash).

### Types

Each entry of `types` has a `kind`, the kind's members, then `doc` and `span`. The span covers the whole declaration, from its first keyword to its closing brace or the end of its type.

| `kind` | Source | Members |
| --- | --- | --- |
| `record` | `type Name<params> { fields }` | `params`: the type parameters, in order, `[]` unless generic. `fields`: fields, in order. |
| `alias` | `type Name<params> = T` | `params`, as for a record. `type`: the aliased type reference. |
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

### Generic types

A record or alias with `params` is generic. Inside it, a bare type name that is one of its `params` is that parameter; no parameter has the name of a key of `types` or `machines`.

```json
"Page": { "kind": "record", "params": ["T"],
          "fields": [{ "name": "items", "type": "T[]", "doc": "" }, { "name": "next", "type": "string?", "doc": "" }], … },
"Dict": { "kind": "alias", "params": ["T"], "type": "map<string, T>", … }
```

Every use of a generic type is an instance with one argument per parameter, `Page<Order>`, never the bare name. The instance's fields, or its aliased type, are the declaration's with each parameter replaced by its argument: `Page<Order>` has `items: Order[]`, and `Dict<Order>` is `map<string, Order>`, an alias being the type it names. A reader resolves a type reference by expanding aliases and substituting arguments as it goes (in Rust, `Ir::expand_top` and `Ir::fields_of`). The IR is not monomorphised: `types` holds each declaration once, so code generators can generate generic code, and there is no entry per instance.

The compiler guarantees what a reader needs to stay finite, and a runtime refuses an IR in which an alias refers to itself (in Rust, `Ir::alias_cycle`): aliases do not refer to themselves (through anything, type arguments included), and a generic type refers to itself, directly or through other types, only with its own parameters in order, so a type has finitely many instances. A type argument holds no `ref` and is never an event type. Records, errors and events of a generic type have no `_tag` of their own: only `record` declarations and aliases are generic.

### Functions, actions and tasks

| Declaration | Members, in order |
| --- | --- |
| `fn name(params) -> T` | `params`, `returns` (a type reference), `doc`, `span` |
| `action name(params)` | `params`, `doc`, `span` |
| `task name(params) [-> T] [! E \| …] [emits N { fields } …]` | `params`, `returns` (a type reference, or `null` if the task returns nothing), `errors` (error names in source order, `[]` if none), `emits` (the events it emits, keyed by name in source order, each `{"fields", "doc", "span"}` as in a machine's `emits`; `{}` if none), `doc`, `span` |

Spans cover the whole declaration, its `emits` clauses included. The span of an entry of a task's `emits` covers the clause from `emits` to its end. A task's emitted event never has the name of a type, error or machine, and its names are unique within the task; two tasks, or a task and a machine, may emit events of the same name. What a runtime does with a task's `emits` is in [`execution.md`](execution.md#tasks-that-emit).

## Machines

```jsonc
"Checkout": {
  "doc": "Charge the customer's card, retrying while the payment gateway is down.",
  "span": { "file": 2, "line": 4, "col": 1, "endLine": 32, "endCol": 2 },
  "strict": false,
  "params": [ /* fields */ ],
  "returns": "Receipt",
  "errors": ["CardDeclined", "GatewayUnavailable"],
  "vars": [ /* variables */ ],
  "events": {},
  "internalEvents": {},
  "emits": {},
  "calls": {},
  "children": {},
  "root": { /* state */ },
  "hash": "sha256:…"
}
```

| Member | Meaning |
| --- | --- |
| `doc`, `span` | The span runs from `machine` (or `strict`) to the closing brace. |
| `strict` | `true` for a `strict machine`: an external event, a message, a call or a raised event that enables no transition is an `unhandled` defect instead of being dropped ([execution](execution.md#taking-an-input)). |
| `params` | Parameters, in order. They are read-only variables. |
| `returns` | Result type, or `null` if the machine returns nothing. |
| `errors` | Declared errors, in source order. |
| `vars` | Variables, in source order. |
| `events` | Events the machine accepts, keyed by name, in source order. |
| `internalEvents` | Events only the machine itself raises (`internal event Name { fields }`), keyed by name, in source order, with the shape of `events`. Host code and other runs cannot send them, and they are not part of the machine's interface. |
| `emits` | Events the machine emits (`emits Name { fields }`), keyed by name, in source order, with the shape of `events`. A name is in at most one of `events`, `internalEvents` and `emits`. |
| `calls` | Calls the machine answers (`call name(params) -> T ! E`), keyed by name, in source order. See below. |
| `children` | The machine's collections of spawned children (`children name: Machine by Key`) and spawned tasks (`children name: task by Key`), keyed by name, in source order. See below. |
| `root` | The machine body as a state. See below. |
| `hash` | The [machine hash](#the-machine-hash). |

A **variable** is `{"name", "type", "sensitive", "init", "doc", "span"}`. `sensitive` is `true` for `sensitive var`, as for a [field](#fields-and-parameters). `init` is the initial value expression, or `null` if there is none. With no initialiser, a variable of optional type starts as `null` and any other variable starts unassigned (see `spec/execution.md`). The span covers the declaration from `var` to the end of its type or initial value.

An **event** is `{"fields", "doc", "span"}`, keyed by its name, in `events`, `internalEvents` and `emits` alike. The span covers the declaration from `event`, `internal` or `emits` to its end.

A **call** is `{"params", "returns", "errors", "doc", "span"}`, keyed by its lower-case name, with the members of a task: `params` the fields of the arguments, `returns` a type reference or `null`, `errors` error names in source order (never `Unanswered`). The span covers the declaration from `call` to its end. A caller holding a `ref` to the machine makes a call with an invoke of kind `call`; the machine takes it with an `on name` transition (trigger kind `call`) and answers it with a `reply` effect.

A **children collection** is `{"kind", "src", "key", "doc", "span"}`, keyed by its name. `kind` is `machine` or `task`, as for an [invoke](#invoke): `src` is the machine its children run, a key of `machines`, or the task they run, a key of `tasks`. `key` is the type of its keys, a type reference that is `string`, `int` or an enum, or an alias of one of them (never optional or a list). The span covers the declaration from `children` to the end of its key type. A collection's name shares the machine's namespace with its parameters, variables, `as` names and the names of its delayed raises (`raise … as name`). What a runtime does with one is in [`execution.md`](execution.md#spawned-children).

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
  "invokes": [ /* invokes */ ],
  "transitions": [ /* transitions */ ],
  "output": null,
  "fail": null
}
```

| Member | Meaning |
| --- | --- |
| `id` | Absolute dotted path from the machine: `PlaceOrder.Processing.Paying`. Unique within the machine. Traces, targets and `initial` use it. |
| `name` | The last segment of `id`. |
| `kind` | `atomic` (no children), `compound` (children, one active at a time), `parallel` (children are regions, all active at once), `final`, or `history` and `deepHistory` for a [history state](#history-states). |
| `doc`, `span` | The span runs from the state's first keyword (`state`, `final` or `parallel`) to its closing brace; for a history state, from `history` or `deep` to the end of its name. |
| `initial` | Id of the child entered first. Set exactly when `kind` is `compound`, otherwise `null`. Never a history state. |
| `states` | Children, in source order, history states among them. Empty for `atomic`, `final`, `history` and `deepHistory`. |
| `entry`, `exit` | [Effects](#effects) run on entering and leaving the state: all `set` assignments, then all `do` actions, each in the order written. |
| `invokes` | The [invokes](#invoke), the tasks and machines run while the state is active, in the order written. `[]` for a state with none. When there is more than one, each has an `as`, and no two have the same. |
| `transitions` | The state's transitions, including its invokes' outcomes, in document order. |
| `output`, `fail` | In a final state at the top level of a machine: the machine's result or failure expression. At most one of them is set. `null` everywhere else. |

### History states

`history Name` and `deep history Name` are states of kind `history` and `deepHistory` in the `states` of the state they are written in, their parent, at the position they are written. Their parent is a `compound` or `parallel` state that is not the root, and has at least one child that is not a history state. They are pseudo-states: `initial` is `null`, `states`, `entry`, `exit`, `invokes` and `transitions` are `[]`, and `output` and `fail` are `null`. Only a transition's `targets` name one. Whether a state is `atomic`, `compound` or `parallel`, which child is its `initial`, and which children are a parallel state's regions are all decided by its other children: wherever the IR or [`execution.md`](execution.md) speaks of a state's children or regions, it means those that are not history states. What entering one does is in [`execution.md`](execution.md#history-states).

## Invoke

```json
{ "kind": "task", "src": "chargeCard", "to": null, "as": null, "args": [ /* expressions */ ], "span": { "…": "…" } }
{ "kind": "task", "src": "upload", "to": null, "as": "up", "args": [ /* expressions */ ], "span": { "…": "…" } }
{ "kind": "call", "src": "reserve", "to": { "kind": "ref", "name": "inventory", "machine": "Inventory", "state": null }, "as": null, "args": [ /* … */ ], "span": { "…": "…" } }
```

| Member | Meaning |
| --- | --- |
| `kind` | `task` (`src` is a key of `tasks`), `machine` (`src` is a key of `machines`) or `call` (`src` is a key of the `calls` of `to`'s machine). |
| `src` | The task's or machine's name, or the call's. |
| `to` | For a call, the [address](#addresses) of the callee, always of kind `ref`: `invoke inventory.reserve(…)` calls the run that `inventory` holds. `null` for a task or a machine. |
| `as` | The name given with `invoke Machine(…) as name` or `invoke task(…) as name`, or `null`. In a state with one invoke, set for a task only when the task declares `emits`, and always `null` for a call; in a state with several invokes, always set. The state and its descendants send to the child and receive what the child or the task emits through this name ([addresses](#addresses)), and the outcomes' triggers name their invoke by it. |
| `args` | Argument expressions, positional, in parameter order. |
| `span` | From `invoke` to the closing brace of its outcomes, or, for an invoke written with no outcomes, to the end of its arguments, its `as` name or its `retry` clause, whichever comes last. |

The outcomes written inside `invoke { … }` are transitions of the state, with `invoke.done` and `invoke.fail` triggers whose `as` is the invoke's. An invoke written with no block has none. A call's `invoke.fail` triggers may name `Unanswered` besides the call's errors, and its `"*"` covers `Unanswered` too.

**`retry` is not in the IR.** `invoke … retry N [on …] [after …]` is shorthand, allowed only on the one invoke of a state, that the compiler writes out as a compound state with the children `Attempt` and `Backoff`, a variable counting failed attempts, and guarded `invoke.fail` branches, exactly as [the language page](../docs/design/language.md#what-retry-compiles-to) gives them. A reader sees ordinary states, variables and transitions, and cannot tell a retry from the same states written by hand except by their spans, which point at the `retry` clause. The two forms have the same machine hash.

## Addresses

An address names another running machine, or a running task that emits: the receiver of a `send`, the sender of the events an `emitted` trigger takes, the peer whose end an `ended` trigger takes, or the callee of a call invoke.

```json
{ "kind": "child", "name": "upload", "machine": "Upload", "state": "Publish.Uploading" }
{ "kind": "ref", "name": "inventory", "machine": "Inventory", "state": null }
{ "kind": "task", "name": "up", "task": "upload", "state": "Publish.Uploading" }
{ "kind": "children", "name": "uploads", "task": "upload", "state": null }
```

| Member | Meaning |
| --- | --- |
| `kind` | `child`: the machine invoked with `invoke Machine(…) as name`. `task`: the task invoked with `invoke task(…) as name`. `ref`: the machine held by a parameter or variable of type `ref Machine` (for an `ended` trigger also `ref Machine?` or `ref Machine[]`). `children`: the spawned children of the collection `name`, machines or tasks; which of them is given by the item (a `send`'s `key`, or any of them for an `emitted` trigger). |
| `name` | The `as` name, the parameter or variable, or the collection. |
| `machine` | For `child`, `ref`, and `children` of a collection whose `kind` is `machine`: the machine at the other end, a key of `machines`: the invoked machine, the `ref`'s, or the collection's. Its `events` are what can be sent to it, its `emits` what can be received from it. |
| `task` | For `task`, and `children` of a collection whose `kind` is `task`: the task at the other end, a key of `tasks`, the invoked task or the collection's. Its `emits` are what can be received from it; nothing can be sent to it. |
| `state` | For `child` and `task`, the id of the state one of whose invokes has the `as` name, which picks that invoke: the innermost state with that name that contains, or is, the state the item is written in. `null` for `ref` and `children`. |

An address names its other end by `machine` or by `task`, never both, and has no member for the other: `{"kind", "name", "machine", "state"}` or `{"kind", "name", "task", "state"}`. An address that names a task (kind `task`, or `children` of a collection of tasks) appears only in `emitted` triggers, since a task takes no events, has no calls, and ends with its invoke's outcome.

A child or a task is addressed only from its invoking state and that state's descendants, which is while it runs: the compiler rejects a `send` to it from the invoking state's own `entry`, `exit` or the invoke's own outcomes, and from a transition that leaves the invoking state (that would reach it after it was stopped). A send the compiler cannot rule out, made while the invoke it names is not running, is undelivered at run time ([execution](execution.md#exiting-and-entering-a-state)). A `ref` variable addresses whichever machine it holds when the item runs. The address of an `ended` trigger is always of kind `ref`, and its `machine` is the machine of the run the parameter or variable holds, or of the runs a list of them holds.

## Transitions

```jsonc
{
  "trigger": { "kind": "invoke.fail", "as": null, "error": "GatewayUnavailable" },
  "group": 2,
  "guard": { "kind": "op", "op": "<", "args": [ /* attempts, 3 */ ], "type": "bool", "text": "attempts < 3" },
  "targets": ["Checkout.Backoff"],
  "internal": false,
  "effects": [],
  "span": { "file": 2, "line": 17, "col": 9, "endLine": 17, "endCol": 41 }
}
```

| Member | Meaning |
| --- | --- |
| `trigger` | What enables it. See below. |
| `group` | The index, from 0, of the trigger block the transition was written in, among the blocks of its state. A single-line trigger is a block with one branch. The branches of one block are consecutive and share `group`; guards within a group are tried in order. |
| `guard` | Boolean expression, or `null` for an unguarded branch (including `else`). |
| `targets` | Ids of the target states, in the order written; a history state may be among them. `[]` for a transition with no target, which leaves and enters no state. A transition with no target and no effects is an `ignore`: it takes its event and does nothing. Only an `on` transition (`event`, `emitted`, `ended` or `any`) is ever one. With more than one, no target is another's ancestor or descendant, and any two have a parallel state as their nearest common ancestor (a history state counting as a child of its parent, or as the parallel state itself when its parent is one), so they are in different regions of it. |
| `internal` | `true` for an internal transition (`-> .X`), whose domain is its own state, which it does not leave: every target is a proper descendant of that state, which is not the root. `false` for every other, a transition with no target among them ([a microstep](execution.md#a-microstep)). |
| `effects` | [Effects](#effects) run when the transition is taken: all `set` assignments, then all `do` actions, each in the order written. |
| `span` | For a single-branch trigger, from the trigger keyword to the end of the branch. For a branch in a block, from its `if` or `else` to the end of the branch. |

**Transitions are identified by position**: the id of the state they belong to and their index in its `transitions`. The trace's `transition` records name a transition this way, with `from` and `index`.

### Triggers

| `kind` | Source | Other members |
| --- | --- | --- |
| `event` | `on Name` | `event`: the event's name, a key of the machine's `events` or `internalEvents`. Taken for an external event, a message, or a raised event. |
| `emitted` | `on name.Name` | `from`: the [address](#addresses) of the sender. `event`: the event's name, a key of the sender's `emits`, the machine's or the task's. Taken for an event the addressed machine emitted, or that the addressed task emitted while it ran. `event` is in scope, of type `Machine.Name`, or `task.Name` for an address that names a task, and for an address of kind `children`, `key` too. |
| `any` | `on *` | none. Matches any of the machine's own events (its `events` and `internalEvents`) for which the state has no `on`. Never an emitted event. |
| `after` | `after delay` | `delay`: a `duration` expression, evaluated when the state's work starts, after the step that entered it has settled ([execution](execution.md#starting-work)). |
| `always` | `always` | none. |
| `done` | `done` in a state | none. The compound state reached a final child, or every region of the parallel state did. |
| `invoke.done` | `done` inside `invoke { }` | `as`: the invoke's `as`, which names one of the state's `invokes`, or `null` for a state's one invoke when it has none. That invoke succeeded. `result` is in scope. |
| `invoke.fail` | `fail E` inside `invoke { }` | `as`, as for `invoke.done`. `error`: the error's name, or `"*"` for every declared error of that invoke that its outcomes do not list (for a call, `Unanswered` among them). `error` is in scope. |
| `call` | `on name` | `call`: a key of the machine's own `calls`. Taken for a call of it from another run. `event` is in scope, a record of the call's parameters read through its fields (`event.items`), and so is `call`, of type `call name`. |
| `spawn.done` | `on name.done` | `children`: the collection. A spawned child of it, a machine or a task, ended `done`. `result` and `key` are in scope. |
| `spawn.fail` | `on name.fail E` | `children`: the collection. `error`: the error's name, a declared error of the collection's machine or task, or `"*"` for every declared error the state has no `on name.fail` for. A spawned child of it ended `failed`. `error` and `key` are in scope. |
| `ended` | `on name.ended` | `from`: the [address](#addresses) of the peer, always of kind `ref`. Taken once, when the run it names closes, however it closes ([a peer's end](execution.md#messages-between-runs)). `event` is in scope, read only through its field `status`, of type `Ended`. |

## Effects

| `kind` | Source | Other members |
| --- | --- | --- |
| `set` | `set var = value` | `var`: a machine variable (not a parameter). `value`: an expression. |
| `do` | `do action(args)` | `action`: a key of `actions`. `args`: expressions, positional, in parameter order. |
| `send` | `send name.Name [{ fields }]`, `send name[key].Name [{ fields }]`, `send name.*.Name [{ fields }]` | `to`: the receiver's [address](#addresses). `key`: for an address of kind `children`, the key expression (`send name[key].Name`), or `null` for every running child of the collection (`send name.*.Name`); `null` for the other kinds. `event`: the event's name, a key of the receiver's `events`. `value`: the event, a `record` expression of type `Machine.Name` with every field of the event. |
| `emit` | `emit Name [{ fields }]` | `event`: the event's name, a key of the machine's own `emits`. `value`: the event, a `record` expression of type `Name` with every field of the event. |
| `reply` | `reply name done [value]`, `reply name fail error` | `call`: a `ref` expression of type `call c`, the implicit `call` or a variable. `outcome`: `"done"` or `"fail"`. `value`: for `done`, an expression of `c`'s `returns` type, or `null` when `c` returns nothing; for `fail`, an expression of one of `c`'s `errors`. |
| `spawn` | `spawn name[key] = Machine(args)`, `spawn name[key] = task(args)` | `children`: the collection. `src`: its machine or task, the one the source names; which of the two it is, is the collection's `kind`. `key`: the key expression, of the collection's key type. `args`: expressions, positional, in parameter order. |
| `stop` | `stop name[key]` | `children`: the collection. `key`: the key expression. |
| `raise` | `raise Name [{ fields }]`, `raise Name [{ fields }] after delay [as name]` | `event`: the event's name, a key of the machine's own `events` or `internalEvents`. `value`: the event, a `record` expression of type `Name` with every field of the event. `delay`: a `duration` expression, or `null` for a raise on the internal queue. `name`: the delayed raise's lower-case name, or `null` (always `null` when `delay` is). |
| `cancel` | `cancel name` | `name`: the name of a delayed raise of the machine, the `name` of one of its `raise` effects. |

`set a = 1, b = 2` is two `set` effects. Every list of effects (a transition's, an `entry`'s, an `exit`'s) holds all its `set` effects, in the order written, then all its other effects (`do`, `send`, `emit`, `reply`, `spawn`, `stop`, `raise` and `cancel`), in the order written among themselves, because that is the order they run in: `do f(a)  set a = 1` and `set a = 1  do f(a)` give the same list, and the same hash, and so do `send x.E { v: a }  set a = 1` and `set a = 1  send x.E { v: a }`. A runtime runs the list in order. A `send`, `emit` or `raise` value is evaluated when its effect runs, so it sees every `set` of the list.

## Expressions

Every expression node is an object with `kind`, the kind's own members, then two members every node has:

- `type`: the node's type reference, filled in by the type checker.
- `text`: the node's canonical source form, as the formatter writes it, so tools can show any subexpression without a pretty-printer. Parentheses appear in `text` only where precedence needs them.

| `kind` | Source | Members |
| --- | --- | --- |
| `lit` | `3`, `1.5`, `"hi"`, `true`, `30s` | `value`, see below. `type` is the literal's built-in. |
| `ref` | `attempts`, `error.issues`, `event.sku` | `path`: a variable, parameter, or one of the implicit names `event`, `result`, `error`, `call`, `key`, followed by the names of the fields read from it. Never empty. |
| `field` | `confirmation(order, receipt).status` | `of`: the expression whose field is read, when it is not a variable. `name`: the field. |
| `call` | `retryDelay(attempts)` | `fn`: a key of `functions`. `args`: expressions, positional, in parameter order. |
| `op` | `a + b`, `not ok`, `-n` | `op`: one of `==` `!=` `<` `<=` `>` `>=` `+` `-` `*` `and` `or` (two `args`, left then right) or `not` `neg` (one `args`). |
| `record` | `CardDeclined { reason: "expired" }`, `GatewayUnavailable`, `Page { items: orders }` | `fields`: `[{"name", "value"}]`, in the order written. `type` is the record or error type being built: its name, or for a generic record its instance with the arguments the compiler worked out (`Page<Order>`); or, for the value of a `send` or `emit`, the [event type](#type-references). A field-less error or event written bare is a record with no fields. Its `text` is the value as written after `send name.` or `emit`: `Reserve { orderId: id }`. |
| `enum` | `Role.Admin` | `variant`. `type` is the enum. |

**Literal values.** Read a literal by its `type`, never by the form of its JSON number, since JavaScript and canonical JSON do not keep the difference between `2` and `2.0`.

| `type` | `value` |
| --- | --- |
| `int` | A JSON integer within ±(2^53 − 1). |
| `float` | A finite JSON number. |
| `string` | A JSON string: the literal's value, escapes resolved. |
| `bool` | `true` or `false`. |
| `duration` | A non-negative JSON integer of milliseconds: `30s` is `30000`, `5m` is `300000`. |

**Types of the implicit names.** `result` in an `invoke.done` transition has the invoked task's or machine's `returns` type, and in a `spawn.done` transition the collection's machine's or task's. `error` in an `invoke.fail` or `spawn.fail` transition for error `E` has type `E`. `key`, in a `spawn.done`, `spawn.fail` or `emitted` transition through a `children` address, has the collection's key type. `event` in an `on Name` transition has type `Name` (the event, see [type references](#type-references)), in an `emitted` transition type `Machine.Name`, or `task.Name` through an address that names a task, and `event.field` has the field's type. In an `ended` transition `event` has no type a chart can write: it is read only as `event.status`, a `ref` expression with `path` `["event", "status"]` and type `Ended`. In a `call` transition `event` is read only through its fields, and `call` has type `call name`. For a call invoke, `result` has the call's `returns` type, and `error` for `Unanswered` the type `Unanswered`.

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
  "tasks":     { /* name: strip(declaration), for every task in K */ },
  "interfaces": { /* Name: strip({"events": …, "emits": …, "calls": …}), for every machine in R */ }
}
```

- **M** is the machine itself and every machine it invokes or spawns (the `src` of its `children` collections whose `kind` is `machine`), directly or through other machines. A machine that invokes itself is included once. (Including the whole set, rather than each child's hash, is what makes the hash defined for recursive machines.)
- **F**, **A** and **K** are the functions called (`call`), the actions run (`do`) and the tasks invoked or spawned (the `src` of a `children` collection whose `kind` is `task`) by the machines in M. A collection of tasks adds its task to K and nothing to M, and an address of kind `task` adds nothing of its own: its task is in K through the invoke that gave it its name.
- **R** is every machine not in M that the machines in M or the declarations in F, A and K name without invoking it: by a `ref Machine` type, by the `machine` of an [address](#addresses) (in a `send`, an `emitted` or `ended` trigger or a call invoke; an address that names a task names no machine), or by an event type `Machine.Name`. Only its interface, its `events`, `emits` and `calls`, is part of the view, so the hash of a machine that talks to a peer changes when what the peer accepts, emits or answers changes, and not when how the peer works inside does. An `ended` trigger adds nothing to it: how a peer can end is the same for every machine, and the peer's output and errors are not part of what its holders get. (A child addressed with `as` is invoked, and a collection's machine is spawned, so they are in M whole.)
- **T** is every type named by the machines in M, by the declarations in F, A and K and by the interfaces of R, together with every type named by a type in T. A type is named by any type reference (parameters, results, variables, event fields, alias targets, record and error fields, and every expression's `type`) whose base, type arguments or map key or value names it, at any depth, by each entry of an `errors` list, and by an `invoke.fail` trigger's `error` other than `"*"` and `Unanswered`. A name that refers to an event rather than to `types` (a bare event name of the machine being walked, `Machine.Name`, or `task.Name`), a type parameter inside its declaration, and the machine of a `ref`, are not part of T.
- **strip** removes every member named `doc`, `span`, `text` or `sensitive` from every object, recursively, except the keys of a machine's `calls` and `children` maps, which are kept whatever they are, and the member `params` of a type declaration when it is `[]`. The other name-keyed maps inside these values are a machine's `events` and `emits` and a task's `emits`, whose keys are upper-case, so no user-chosen name is ever removed: a call named `text` keeps its entry (its value is stripped), and `children text: C by string` keeps its collection. A machine's `children` is also left out when it is empty, and a `send` effect's `key` when it is `null`, so a machine that uses no `sensitive` mark, keyed spawn or generic type has the hash it had in 0.5. A transition's `internal` is left out when it is `false`, and its `targets`, when it has at most one, is written as the `target` of 0.8 (the one id, or `null` for `[]`), so a machine with neither internal transitions nor several targets has the hash it had in 0.8. A task's `emits` is left out when it is `{}`. A collection whose `kind` is `machine` is written in its 0.9 form, `{"machine": src, "key": …}`, with no `kind` and no `src`, and so is a `spawn` effect of such a collection, with `machine` in place of `src`; a collection of tasks, its `spawn` effects and the addresses that name a task are written as they are. So a machine that uses no task that emits and no collection of tasks has the hash it had in 0.9. A machine's `internalEvents` is left out when it is `{}`, so a machine that uses no internal event has the hash it had in 0.10 (the `raise` and `cancel` effects appear only where a chart writes them). A state whose `invokes` has at most one entry is written in its 0.11 form: `invoke`, the one invoke or `null`, in place of `invokes`, and its transitions' `invoke.done` and `invoke.fail` triggers without `as`. So a machine that uses neither internal events nor a state of several invokes has the hash it had in 0.10. `strict` and `calls` are always part of the view, `calls` even when it is empty. An interface in R never has `internalEvents`: no other machine can send them.

So comments, formatting, source positions, file names, packages, declaration order, `sensitive` marks and unused declarations do not change a machine's hash (a run compiled without a peer's marks therefore talks to it all the same, and shows what the peer marks: build the runs that talk to each other from the same charts), and any change to what the machine does, to a type it uses, to a machine it invokes, or to the interface of a machine it talks to does. A declaration copied from another package's compiled IR therefore keeps the hash it had there. In Rust, `roadbook_ir::machine_hash(&ir, name)` computes it, `roadbook_ir::semantic_view` returns the view, and `roadbook_ir::fill_hashes(&mut ir)` sets every machine's `hash`; the compiler calls the last after type checking. Snapshots and traces record the hash, so restoring a snapshot into a changed machine is refused and the viewer can tell when a trace came from another version of a chart.

## Versioning

`irVersion` is `"MAJOR.MINOR"`, two decimal numbers without leading zeros. This page describes `0.12`.

- While the major version is 0, every change to the IR is breaking and changes the minor version. A reader accepts only the exact version it was written for.
- From `1.0` on, a minor version only adds to the IR. A reader accepts its own major version with any minor version up to its own, and refuses a newer minor version rather than ignore members it does not know. A breaking change increases the major version.

A runtime checks the version before anything else and refuses an IR it does not understand, with a message naming both versions. In Rust, `roadbook_ir::check_version` does this and `Ir::from_json` calls it before reading the rest of the document. Any change to the IR's shape can change every machine hash, so a new IR version invalidates saved snapshots.

## Changes in 0.12

Version 0.12 lets a state have several invokes, each named with `as` ([several invokes](../docs/design/language.md#several-invokes)). Every state has `invokes` in place of `invoke`, and every `invoke.done` and `invoke.fail` trigger has `as`, so a 0.11 document is not a 0.12 document. The [machine hash](#the-machine-hash) writes a state with at most one invoke in its 0.11 form, so a machine that has no state with several invokes has the hash it had in 0.11, and its saved snapshots still restore.

- [States](#states) have `invokes`, a list, in place of `invoke`. When it has several entries, each has an `as`, unique in the state, and it may then name a call or a task that declares no `emits`.
- The [triggers](#triggers) `invoke.done` and `invoke.fail` have `as`, which names the invoke they belong to.
- A snapshot's `invokes` entries, and the trace's `invoke.started` record, have `as` when their state has several invokes ([snapshot](execution.md#snapshot), [records](trace.md#records)).

What a runtime does with several invokes (starting them in order, cancelling them all on exit, offering each outcome only to its own invoke's transitions) is in [`execution.md`](execution.md#starting-work). Both runtimes implement it.

## Changes in 0.11

Version 0.11 adds events a machine raises itself, delayed raises and internal events ([events a machine raises](../docs/design/language.md#events-a-machine-raises)). Every machine has the new member `internalEvents`, so a 0.10 document is not a 0.11 document. The [machine hash](#the-machine-hash) leaves out an `internalEvents` that is `{}`, so a machine that uses none of the new features has the hash it had in 0.10, and its saved snapshots still restore.

- [Machines](#machines) have `internalEvents`, with the shape of `events`. An `event` trigger and `on *` take them as well as `events`.
- The [effects](#effects) `raise`, immediate or delayed, and `cancel`.
- Type references to a bare event name may name an internal event.
- The trace has three new records, `raised`, `raise.scheduled` and `raise.cancelled`, and the `event`, `dropped` and `unhandled` input `raised` ([inputs in records](trace.md#inputs-in-records)). A delayed raise's expiry is an `after` input.
- Snapshots have the member `scheduled`, and a saved snapshot writes it, with each entry's `due`, only when it is not empty, so `snapshotVersion` stays `4` ([the saved snapshot](execution.md#the-saved-snapshot)).

What a runtime does with them (the internal queue, the order it is taken in, delayed raises as timers of the run, strict machines, and how a delayed raise is saved and restored) is in [`execution.md`](execution.md#raised-events). Both runtimes implement them.

## Changes in 0.10

Version 0.10 adds tasks that emit, stream tasks and spawned tasks ([tasks that emit](../docs/design/language.md#tasks-that-emit), [collections of tasks](../docs/design/language.md#many-children)). Every task declaration has the new member `emits`, and a `children` collection and a `spawn` effect name what they run by `src` (with the collection's `kind`) in place of `machine`, so a 0.9 document is not a 0.10 document. The [machine hash](#the-machine-hash) leaves out a task's `emits` that is `{}` and writes a collection of machines and its `spawn` effects in their 0.9 form, so a machine that uses none of the new features has the hash it had in 0.9, and its saved snapshots still restore.

- [Task declarations](#functions-actions-and-tasks) have `emits`, with the shape of a machine's.
- An [invoke](#invoke) of a task that declares `emits` may have `as`. An invoke may have no outcomes.
- The [address](#addresses) kind `task`. An address names its other end by `machine` or by `task`; one that names a task appears only in `emitted` triggers.
- [Children collections](#machines) have `kind` and `src` in place of `machine`, and the [`spawn` effect](#effects) `src` in place of `machine`.
- [Type references](#type-references) may be `task.Name`, an event a task emits.
- In the [semantic view](#the-machine-hash), K takes the tasks that are spawned as well as those invoked.
- The trace's `event`, `dropped` and `stale` records may hold the new input `task.emitted` ([inputs in records](trace.md#inputs-in-records)), and a `spawn.started` record's `src` may be a task. There is no new record kind.

What a runtime does with them (the `task.emitted` input, the emitter a driver gives a task, who receives what a task emits, the `srcKind` of the `spawn.start` command, spawned tasks, and why a running stream task keeps a run from being idle) is in [`execution.md`](execution.md#tasks-that-emit) and [`trace.md`](trace.md#inputs-in-records). A runtime that does not implement tasks that emit refuses a machine that uses them; both runtimes implement them.

## Changes in 0.9

Version 0.9 adds internal transitions, `-> .X` ([transitions](../docs/design/language.md#transitions)), and transitions with several targets, `-> P.A.X, P.B.Y`. Every transition has the new member `internal`, and `target` is replaced by `targets`, so a 0.8 document is not a 0.9 document. The [machine hash](#the-machine-hash) leaves out an `internal` that is `false` and writes `targets` of at most one id as 0.8's `target`, so a machine that uses neither feature has the hash it had in 0.8, and its saved snapshots still restore.

- Transitions have [`targets`](#transitions), a list, in place of `target`, and `internal`.
- The trace's `transition` record has `to` as a list, the same as `targets` ([records](trace.md#records)).

What a runtime does with them (the domain of an internal transition, the regions an internal transition of a parallel state exits, the entry set of several targets, and a completion an internal transition takes back) is in [`execution.md`](execution.md#a-microstep).

## Changes in 0.8

Version 0.8 adds a peer's end, `on name.ended` ([when a peer ends](../docs/design/language.md#when-a-peer-ends)). Nothing else changes shape, so a 0.7 document differs from the same chart's 0.8 document only in `irVersion`, and the [machine hash](#the-machine-hash), which does not see `irVersion`, is the same: a machine that has no `ended` trigger and names no `Ended` has the hash it had in 0.7, and its saved snapshots still restore. A reader of 0.8 still refuses a 0.7 document, as it refuses any other version.

- The trigger kind [`ended`](#triggers), whose `from` is an [address](#addresses) of kind `ref`; for it, that address may name a parameter or variable of type `ref M?` or `ref M[]`.
- The built-in enum [`Ended`](#type-references), `Done`, `Failed`, `Cancelled`, `Defect` and `Suspended`, an entry of `types` whenever the document names it, with an empty span at the start of the entry file.
- `ref` paths `["event", "status"]` in `ended` transitions.
- In the [semantic view](#the-machine-hash), an `ended` trigger's address names its machine in R, which the `ref` it goes through already does, so `interfaces` do not change.

What a runtime does with them (the `ended` input, who is sent it and when, its order after a peer's last messages and `Unanswered` answers, a peer that had ended before its holder started) is in [`execution.md`](execution.md#messages-between-runs) and [`trace.md`](trace.md#inputs-in-records). An `ended` trigger is messaging: a runtime that does not implement messaging refuses a machine that has one (in Rust, `Machine::messaging` names it), and a conformance case with one requires `messaging` and `ended`.

## Changes in 0.7

Version 0.7 adds four features: `sensitive` marks, history states, keyed `spawn`, and maps and generic types. Every field, parameter and variable has the new member `sensitive`, every machine `children`, every `send` effect `key`, and every `record` and `alias` `params`, so a 0.5 document is not a 0.7 document. The [machine hash](#the-machine-hash) strips `sensitive` as it strips `doc`, and leaves out an empty `children`, an empty `params` and a `null` `key`, so a machine that uses none of the four features has the hash it had in 0.5, and its saved snapshots still restore.

Versions 0.3 and 0.6 were never published. Unpublished builds used 0.3 to 0.6 for these four features (0.3 for `sensitive` marks, 0.4 for history states, 0.5 for keyed spawn and 0.6 for maps and generic types), while the published 0.4 and 0.5 are calls and strict machines. A reader of 0.7 refuses the documents of those builds as it refuses any other version.

Redaction ([redaction](trace.md#redaction)):

- Every [field and parameter](#fields-and-parameters) and every variable has `sensitive`. The hash does not see it, so marking a field later does not invalidate snapshots: a mark changes only what traces show.

History states:

- The state kinds [`history` and `deepHistory`](#history-states). A transition's target may be one.

Keyed spawn ([spawned children](execution.md#spawned-children)):

- Machines have [`children`](#machines), the keyed collections of spawned children.
- The address kind `children`, and `key` on `send` effects (`send name[key].E`, `send name.*.E`).
- The effect kinds `spawn` and `stop`, and the trigger kinds `spawn.done` and `spawn.fail`.
- The implicit name `key` in `ref` paths.
- A collection's machine is in the hash's M.

Maps and generic types ([maps and generic types](../docs/design/language.md#maps-and-generic-types)):

- [Type references](#type-references) may be `map<K, V>` and `Name<A, B>`, nested.
- `record` and `alias` declarations have [`params`](#generic-types); inside a generic declaration a bare name may be a parameter.
- The `type` of a `record` expression may be an instance, `Page<Order>`.
- In the [semantic view](#the-machine-hash), T takes the types named anywhere in a type reference, not only its base.

What a runtime does with them is in [`execution.md`](execution.md): [history states](execution.md#history-states), [spawned children](execution.md#spawned-children), and [values](execution.md#values) (a map is a JSON object, an instance takes the form of its declaration, and a trace redacts the sensitive fields inside either by their declared types). Saved snapshots are version 3, which adds `history` and `spawned` ([saving and restoring](execution.md#saving-and-restoring)).

## Changes in 0.5

Version 0.5 adds [strict machines](../docs/design/language.md#strict-machines). Every machine has the new member `strict`, so every hash changed, and a snapshot saved under 0.4 is refused as `ir` unless the host gives a migration ([restoring](execution.md#restoring-a-snapshot)). There is no version 0.3: strict machines were built under that number while calls were built under 0.4, and calls landed first.

- Machines have `strict`.
- A transition may have no target and no effects: `on E ignore`, which 0.4 could not express.

## Changes in 0.4

Version 0.4 adds calls between machines ([asking a peer](../docs/design/actors.md#asking-a-peer)). Every machine has the new member `calls` and every invoke `to`, so every hash changed.

- Machines have [`calls`](#machines); invokes have `to`, and the invoke kind `call`.
- The trigger kind `call`, and the effect kind `reply`.
- [Type references](#type-references) may be `call name`; the built-in error `Unanswered`.
- The [semantic view](#the-machine-hash)'s `interfaces` have `calls`.

What a runtime does with them (the `call` input, the `reply` command, `Unanswered`, pending calls in snapshots, the `replied` record) is in [`execution.md`](execution.md#calls-between-runs) and [`trace.md`](trace.md#messages).

## Changes in 0.2

Version 0.2 adds messaging between machines ([actors and messaging](../docs/design/actors.md), milestone 5) and imports from other packages. A 0.1 document is not a 0.2 document: every machine has the new member `emits`, every invoke `as`, every source `package`, so every hash changed.

- Sources have a [`package`](#sources); declarations from another package are those in its files.
- [Type references](#type-references) may be `ref Machine` and `Machine.Event`.
- Machines have `emits`; invokes have `as`.
- The trigger kind `emitted`, and the effect kinds `send` and `emit`, with [addresses](#addresses).
- A list of effects is every `set`, then the other effects in the order written (in 0.1, the `do`s).
- The [semantic view](#the-machine-hash) has `interfaces`.

What a runtime does with the new members (queues, delivery to the invoker and to the holders of a reference that have an `on` for the event, sending to a stopped machine, the trace records) is in [`execution.md`](execution.md#messages-between-runs) and [`trace.md`](trace.md#messages). A runtime that does not implement messaging refuses a machine that uses any of it before running it: in Rust, `Machine::messaging` names the first construct that does.
