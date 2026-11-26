# The Roadbook IR, version 0.5

The compiler turns an entry `.road` file and everything it imports into one JSON document, the IR. The viewer, both runtimes, the code generators and the exporters read the IR and nothing else. This page says what every member means. The schema, [`ir.schema.json`](ir.schema.json) (JSON Schema draft 2020-12), checks the shape, and the Rust crate [`roadbook-ir`](../crates/roadbook-ir) reads and writes it. A complete example, for [`examples/shop/place_order.road`](../examples/shop/place_order.road), is [`crates/roadbook-ir/tests/data/place_order.ir.json`](../crates/roadbook-ir/tests/data/place_order.ir.json). One with messaging and a declaration from another package, for [`examples/orders/orders.road`](../examples/orders/orders.road), is [`crates/roadbook-ir/tests/data/orders.ir.json`](../crates/roadbook-ir/tests/data/orders.ir.json).

## Conventions

**Every member is always present.** Nothing is ever omitted. An absent value is `null`, a missing doc comment is `""`, and an empty list or map is `[]` or `{}`. This gives each input exactly one output, which matters because compiled IR is committed and hashed, and it means readers never need to tell a missing member from a null one. Readers reject members this page does not list.

**Order is source order.** Maps keyed by name (`types`, `functions`, `actions`, `tasks`, `machines`, a machine's `events` and `emits`) keep declaration order: the entry file's declarations first, then each imported file's in the order the files were first loaded (depth first: a file's imports, in the order written, are loaded before the next import of the file that imports it). Lists (fields, parameters, variables, states, transitions, effects, arguments) are in the order written, except that a list of effects puts every `set` before the other effects (see [effects](#effects)). A JSON parser that does not keep member order loses nothing a runtime needs, since order in the maps is for readability and diffs only.

**Names.** Upper-case names (`[A-Z][A-Za-z0-9]*`) are types, errors, events, states and machines. Lower-case names (`[a-z][A-Za-z0-9]*`) are variables, parameters, fields, functions, actions and tasks. Declaration names are unique among the files of a compilation (the entry file and everything it imports), so a name in `types`, `functions`, `actions`, `tasks` or `machines` identifies a declaration on its own.

**Docs.** `doc` is the text of the `///` lines directly before the item, one leading space removed from each line, trailing whitespace removed, lines joined with `\n`. `""` if there are none.

**Spans.** A span is `{"file", "line", "col", "endLine", "endCol"}`. `file` is an index into `sources`. Lines and columns are 1-based. Columns count Unicode scalar values (Rust `char`s, JavaScript code points), and a tab is one column. The end is exclusive: `endLine`/`endCol` is the position just after the last character. A span never includes the item's doc comment. Which text each span covers is given with each item below.

**Numbers.** Integers in the IR (literal values, span positions, `group`) are within ±(2^53 − 1), so every JSON parser reads them exactly and canonical JSON represents them exactly. An `int` literal outside that range is a compile error, and so is a `duration` literal of more than 2^53 − 1 milliseconds.

**File form.** The compiler writes the IR as JSON indented by two spaces, members in the order this page lists them, with a final newline (`Ir::to_json` in the Rust crate). Hashing and byte-for-byte comparison use [canonical JSON](#canonical-json) instead.

## Top level

```jsonc
{
  "irVersion": "0.5",
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
| `types` | Records, aliases, enums, errors and extern types. |
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
| `Name` | A key of `types`; inside a generic declaration, one of its `params`; or, inside a machine, one of that machine's own events, a key of its `events` or `emits` (see below). |
| `Name<A, B>` | An instance of the generic record or alias `Name`, a key of `types` with as many `params` as there are arguments, each a type reference. See [generic types](#generic-types). |
| `map<K, V>` | A map from keys of type `K` to values of type `V`. `K` is `string` once aliases are expanded: `string`, `UserId` after `type UserId = string`. |
| `Machine.Name` | Event `Name` of machine `Machine`: a key of `machines`, then of that machine's `events` or `emits`. |
| `ref Machine` | A handle to a running machine `Machine`, a key of `machines`. One space after `ref`. `ref Inventory[]` is a list of handles. |

There is no whitespace other than one space after `ref` and one after each `,` between type arguments, as the formatter writes them: `map<string, Order[]>?`, `Page<Pair<string, int>>[]`. The grammar, whose nesting a regular expression cannot check (the schema checks only the tokens):

```
type  = base { "[]" | "?" }
base  = builtin | Upper [ "<" type { ", " type } ">" ] | Upper "." Upper | "ref " Upper
      | "map<" type ", " type ">"
```

**Event types.** Events are not declared types: an event's type is named after the event. Inside machine `M`, a bare name that is one of `M`'s events or emitted events is that event: the type of `event` in `on Name`, and of the value of `emit Name { … }`. The event of another machine, or of a machine reached through a `ref` or a child, is written `Machine.Name`: the type of `event` in `on upload.Progress` is `Upload.Progress`, and the value of `send upload.Pause` has type `Upload.Pause`, also when the other machine is `M` itself. Names never collide: an event never has the name of a type, error or machine of the document, and a machine's `events` and `emits` never share a name, so a bare name is a key of `types` or of the current machine's `events` or `emits`, never two of them.

**Where a `ref` can be.** Machine parameters, variables, and the parameters of functions, actions and tasks may have a `ref` type, directly or through an alias, also inside a list, an optional or a map's values. Fields of records, errors and events, type arguments, and the results of functions, tasks and machines, never do, so a machine can only get a handle from its own signature or from `invoke … as`. A `ref` value is never a literal, never compared, never passed where `json` is expected, and a runtime passes it to host code as its own handle type. In snapshots and traces it is the id of the run it refers to ([values](execution.md#values)).

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
  "emits": {},
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
| `emits` | Events the machine emits (`emits Name { fields }`), keyed by name, in source order, with the shape of `events`. A name is in at most one of `events` and `emits`. |
| `root` | The machine body as a state. See below. |
| `hash` | The [machine hash](#the-machine-hash). |

A **variable** is `{"name", "type", "sensitive", "init", "doc", "span"}`. `sensitive` is `true` for `sensitive var`, as for a [field](#fields-and-parameters). `init` is the initial value expression, or `null` if there is none. With no initialiser, a variable of optional type starts as `null` and any other variable starts unassigned (see `spec/execution.md`). The span covers the declaration from `var` to the end of its type or initial value.

An **event** is `{"fields", "doc", "span"}`, keyed by its name, in `events` and in `emits` alike. The span covers the declaration from `event` or `emits` to its end.

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
| `kind` | `atomic` (no children), `compound` (children, one active at a time), `parallel` (children are regions, all active at once), `final`, or `history` and `deepHistory` for a [history state](#history-states). |
| `doc`, `span` | The span runs from the state's first keyword (`state`, `final` or `parallel`) to its closing brace; for a history state, from `history` or `deep` to the end of its name. |
| `initial` | Id of the child entered first. Set exactly when `kind` is `compound`, otherwise `null`. Never a history state. |
| `states` | Children, in source order, history states among them. Empty for `atomic`, `final`, `history` and `deepHistory`. |
| `entry`, `exit` | [Effects](#effects) run on entering and leaving the state: all `set` assignments, then all `do` actions, each in the order written. |
| `invoke` | The task or machine run while the state is active, or `null`. At most one per state. |
| `transitions` | The state's transitions, including its invoke's outcomes, in document order. |
| `output`, `fail` | In a final state at the top level of a machine: the machine's result or failure expression. At most one of them is set. `null` everywhere else. |

### History states

`history Name` and `deep history Name` are states of kind `history` and `deepHistory` in the `states` of the state they are written in, their parent, at the position they are written. Their parent is a `compound` or `parallel` state that is not the root, and has at least one child that is not a history state. They are pseudo-states: `initial` is `null`, `states`, `entry`, `exit` and `transitions` are `[]`, and `invoke`, `output` and `fail` are `null`. Only a transition's `target` names one. Whether a state is `atomic`, `compound` or `parallel`, which child is its `initial`, and which children are a parallel state's regions are all decided by its other children: wherever the IR or [`execution.md`](execution.md) speaks of a state's children or regions, it means those that are not history states. What entering one does is in [`execution.md`](execution.md#history-states).

## Invoke

```json
{ "kind": "task", "src": "chargeCard", "as": null, "args": [ /* expressions */ ], "span": { "…": "…" } }
```

| Member | Meaning |
| --- | --- |
| `kind` | `task` (`src` is a key of `tasks`) or `machine` (`src` is a key of `machines`). |
| `src` | The task's or machine's name. |
| `as` | The name given with `invoke Machine(…) as name`, or `null`. Always `null` for a task. The state and its descendants send to the child and receive what it emits through this name ([addresses](#addresses)). |
| `args` | Argument expressions, positional, in parameter order. |
| `span` | From `invoke` to the closing brace of its outcomes. |

The outcomes written inside `invoke { … }` are transitions of the state, with `invoke.done` and `invoke.fail` triggers.

**`retry` is not in the IR.** `invoke … retry N [on …] [after …]` is shorthand that the compiler writes out as a compound state with the children `Attempt` and `Backoff`, a variable counting failed attempts, and guarded `invoke.fail` branches, exactly as [the language page](../docs/design/language.md#what-retry-compiles-to) gives them. A reader sees ordinary states, variables and transitions, and cannot tell a retry from the same states written by hand except by their spans, which point at the `retry` clause. The two forms have the same machine hash.

## Addresses

An address names another running machine: the receiver of a `send`, or the sender of the events an `emitted` trigger takes.

```json
{ "kind": "child", "name": "upload", "machine": "Upload", "state": "Publish.Uploading" }
{ "kind": "ref", "name": "inventory", "machine": "Inventory", "state": null }
```

| Member | Meaning |
| --- | --- |
| `kind` | `child`: the machine invoked with `invoke Machine(…) as name`. `ref`: the machine held by a parameter or variable of type `ref Machine`. |
| `name` | The `as` name, or the parameter or variable. |
| `machine` | The machine at the other end, a key of `machines`: the invoked machine, or the `ref`'s. Its `events` are what can be sent to it, its `emits` what can be received from it. |
| `state` | For `child`, the id of the state whose invoke has the `as` name: the innermost state with that name that contains, or is, the state the item is written in. `null` for `ref`. |

A child is addressed only from its invoking state and that state's descendants, which is while it runs: the compiler rejects a `send` to it from the invoking state's own `entry`, `exit` or invoke outcomes, and from a transition that leaves the invoking state (that would reach it after it was stopped). A send the compiler cannot rule out, made while the state has no running invoke, is undelivered at run time ([execution](execution.md#exiting-and-entering-a-state)). A `ref` variable addresses whichever machine it holds when the item runs.

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
| `target` | Id of the target state, a history state among them, or `null` for an internal transition (the state is not left). |
| `effects` | [Effects](#effects) run when the transition is taken: all `set` assignments, then all `do` actions, each in the order written. |
| `span` | For a single-branch trigger, from the trigger keyword to the end of the branch. For a branch in a block, from its `if` or `else` to the end of the branch. |

**Transitions are identified by position**: the id of the state they belong to and their index in its `transitions`. The trace's `transition` records name a transition this way, with `from` and `index`.

### Triggers

| `kind` | Source | Other members |
| --- | --- | --- |
| `event` | `on Name` | `event`: the event's name, a key of the machine's `events`. |
| `emitted` | `on name.Name` | `from`: the [address](#addresses) of the sender. `event`: the event's name, a key of the sender's `emits`. Taken for an event the addressed machine emitted. `event` is in scope, of type `Machine.Name`. |
| `any` | `on *` | none. Matches any of the machine's own events (its `events`) for which the state has no `on`. Never an emitted event. |
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
| `send` | `send name.Name [{ fields }]` | `to`: the receiver's [address](#addresses). `event`: the event's name, a key of the receiver's `events`. `value`: the event, a `record` expression of type `Machine.Name` with every field of the event. |
| `emit` | `emit Name [{ fields }]` | `event`: the event's name, a key of the machine's own `emits`. `value`: the event, a `record` expression of type `Name` with every field of the event. |

`set a = 1, b = 2` is two `set` effects. Every list of effects (a transition's, an `entry`'s, an `exit`'s) holds all its `set` effects, in the order written, then all its other effects (`do`, `send` and `emit`), in the order written among themselves, because that is the order they run in: `do f(a)  set a = 1` and `set a = 1  do f(a)` give the same list, and the same hash, and so do `send x.E { v: a }  set a = 1` and `set a = 1  send x.E { v: a }`. A runtime runs the list in order. A `send` or `emit` value is evaluated when its effect runs, so it sees every `set` of the list.

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

**Types of the implicit names.** `result` in an `invoke.done` transition has the invoked task's or machine's `returns` type. `error` in an `invoke.fail` transition for error `E` has type `E`. `event` in an `on Name` transition has type `Name` (the event, see [type references](#type-references)), in an `emitted` transition type `Machine.Name`, and `event.field` has the field's type.

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
  "interfaces": { /* Name: strip({"events": …, "emits": …}), for every machine in R */ }
}
```

- **M** is the machine itself and every machine it invokes, directly or through other machines. A machine that invokes itself is included once. (Including the whole set, rather than each child's hash, is what makes the hash defined for recursive machines.)
- **F**, **A** and **K** are the functions called (`call`), the actions run (`do`) and the tasks invoked by the machines in M.
- **R** is every machine not in M that the machines in M or the declarations in F, A and K name without invoking it: by a `ref Machine` type, by the `machine` of an [address](#addresses) (in a `send` or an `emitted` trigger), or by an event type `Machine.Name`. Only its interface, its `events` and `emits`, is part of the view, so the hash of a machine that talks to a peer changes when what the peer accepts or emits changes, and not when how the peer works inside does. (A child addressed with `as` is invoked, so it is in M whole.)
- **T** is every type named by the machines in M, by the declarations in F, A and K and by the interfaces of R, together with every type named by a type in T. A type is named by any type reference (parameters, results, variables, event fields, alias targets, record and error fields, and every expression's `type`) whose base, type arguments or map key or value names it, at any depth, by each entry of an `errors` list, and by an `invoke.fail` trigger's `error` other than `"*"`. A name that refers to an event rather than to `types` (a bare event name of the machine being walked, or `Machine.Name`), a type parameter inside its declaration, and the machine of a `ref`, are not part of T.
- **strip** removes every member named `doc`, `span`, `text` or `sensitive` from every object, recursively, and the member `params` of a type declaration when it is `[]`. The only name-keyed maps inside these values are a machine's `events` and `emits`, whose keys are upper-case, so no user-chosen name is ever removed.

So comments, formatting, source positions, file names, packages, declaration order, `sensitive` marks and unused declarations do not change a machine's hash (a run compiled without a peer's marks therefore talks to it all the same, and shows what the peer marks: build the runs that talk to each other from the same charts), and any change to what the machine does, to a type it uses, to a machine it invokes, or to the interface of a machine it talks to does. A declaration copied from another package's compiled IR therefore keeps the hash it had there. In Rust, `roadbook_ir::machine_hash(&ir, name)` computes it, `roadbook_ir::semantic_view` returns the view, and `roadbook_ir::fill_hashes(&mut ir)` sets every machine's `hash`; the compiler calls the last after type checking. Snapshots and traces record the hash, so restoring a snapshot into a changed machine is refused and the viewer can tell when a trace came from another version of a chart.

## Versioning

`irVersion` is `"MAJOR.MINOR"`, two decimal numbers without leading zeros. This page describes `0.5`.

- While the major version is 0, every change to the IR is breaking and changes the minor version. A reader accepts only the exact version it was written for.
- From `1.0` on, a minor version only adds to the IR. A reader accepts its own major version with any minor version up to its own, and refuses a newer minor version rather than ignore members it does not know. A breaking change increases the major version.

A runtime checks the version before anything else and refuses an IR it does not understand, with a message naming both versions. In Rust, `roadbook_ir::check_version` does this and `Ir::from_json` calls it before reading the rest of the document. Any change to the IR's shape can change every machine hash, so a new IR version invalidates saved snapshots.

## Changes from 0.4

Version 0.5 adds maps and generic types ([maps and generic types](../docs/design/language.md#maps-and-generic-types)). Every `record` and `alias` has the new member `params`, so a 0.4 document is not a 0.5 document. The [machine hash](#the-machine-hash) strips `params` when it is `[]`, so a machine that uses no generic type has the hash it had in 0.4, and its saved snapshots still restore.

- [Type references](#type-references) may be `map<K, V>` and `Name<A, B>`, nested.
- `record` and `alias` declarations have [`params`](#generic-types); inside a generic declaration a bare name may be a parameter.
- The `type` of a `record` expression may be an instance, `Page<Order>`.
- In the [semantic view](#the-machine-hash), T takes the types named anywhere in a type reference, not only its base.

What a runtime does with the new values is in [`execution.md`](execution.md#values): a map is a JSON object, an instance takes the form of its declaration, and a trace redacts the sensitive fields inside either by their declared types.

## Changes from 0.3

Version 0.4 adds [history states](#history-states): the state kinds `history` and `deepHistory`. Nothing else changed, so a machine with no history state has the hash it had in 0.3, and its saved snapshots still restore.

## Changes from 0.2

Version 0.3 adds `sensitive` marks ([redaction](trace.md#redaction)). Every [field and parameter](#fields-and-parameters) and every variable has the new member `sensitive`, so a 0.2 document is not a 0.3 document. The [machine hash](#the-machine-hash) strips `sensitive` as it strips `doc`, so no hash changed: a snapshot saved with 0.2 restores into the same machine compiled to 0.3, and marking a field later does not invalidate snapshots, because a mark changes only what traces show.

## Changes from 0.1

Version 0.2 adds messaging between machines ([actors and messaging](../docs/design/actors.md), milestone 5) and imports from other packages. A 0.1 document is not a 0.2 document: every machine has the new member `emits`, every invoke `as`, every source `package`, so every hash changed.

- Sources have a [`package`](#sources); declarations from another package are those in its files.
- [Type references](#type-references) may be `ref Machine` and `Machine.Event`.
- Machines have `emits`; invokes have `as`.
- The trigger kind `emitted`, and the effect kinds `send` and `emit`, with [addresses](#addresses).
- A list of effects is every `set`, then the other effects in the order written (in 0.1, the `do`s).
- The [semantic view](#the-machine-hash) has `interfaces`.

What a runtime does with the new members (queues, delivery to the invoker and to every holder of a reference, sending to a stopped machine, the trace records) is in [`execution.md`](execution.md#messages-between-runs) and [`trace.md`](trace.md#messages). A runtime that does not implement messaging refuses a machine that uses any of it before running it: in Rust, `Machine::messaging` names the first construct that does.
