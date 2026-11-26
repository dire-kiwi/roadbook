# The Roadbook language

A `.road` file declares data shapes, the host functions a machine needs, and the machines themselves. It contains no implementation code. The compiler turns it into the [IR](semantics.md#the-ir), which everything else consumes.

The examples on this page come from [`examples/shop`](../../examples/shop), which `roadbook viz` draws.

## A machine, top to bottom

```
/// Charge the customer's card, retrying while the payment gateway is down.
machine Checkout(order: Order, user: User) -> Receipt ! CardDeclined | GatewayUnavailable {
  var attempts: int = 0
  var receipt:  Receipt
  var declined: CardDeclined

  initial Charging

  /// One attempt against the payment gateway.
  state Charging {
    invoke chargeCard(order, user) {
      done                    -> Charged   set receipt = result
      fail CardDeclined       -> Declined  set declined = error
      fail GatewayUnavailable {
        if attempts < 3       -> Backoff
        else                  -> GaveUp
      }
    }
  }

  /// Wait before the next attempt. The delay grows with each attempt.
  state Backoff {
    entry set attempts = attempts + 1
    after retryDelay(attempts) -> Charging
  }

  final state Charged  { output receipt }
  final state Declined { fail declined }
  final state GaveUp   { fail GatewayUnavailable }
}
```

Reading it:

- **The signature** says what the machine takes, what it returns and how it can fail: `Checkout(order, user) -> Receipt ! CardDeclined | GatewayUnavailable`. A task has the same kind of signature, so a machine can be used anywhere a task can. A machine is a task whose implementation is a chart.
- **`var`** declares the machine's data (xstate calls this context). Parameters are read-only variables. A `var` without a default starts unassigned, and the compiler proves it is assigned before anything reads it (see [definite assignment](#definite-assignment)).
- **`state`** blocks hold what happens while the machine is in that state. States nest. `final state` ends the machine with `output` or `fail`.
- **`invoke`** runs a task (or another machine) for as long as the state is active. Leaving the state cancels it. Each outcome is a transition: `done`, or `fail` with one of the task's declared errors. The compiler rejects an `invoke` that leaves a declared error unhandled.
- **Transitions** read `trigger [if guard] -> Target [set ...] [do ...]`. Several guarded branches for one trigger go in a block and are tried in order.
- **`///` comments** are documentation. They are kept in the IR and shown in the visualization.

## Declarations

Everything a machine refers to is declared, with types. Declarations live at the top level of any file and are shared with `import`, also with other packages (see [projects](#projects)).

| Declaration | Meaning | Implemented by |
| --- | --- | --- |
| `type User { id: string, role: Role }` | A record. | Generated |
| `type UserId = string` | An alias. | Generated |
| `enum Role { Customer, Admin }` | A closed set of names. | Generated |
| `error CardDeclined { reason: string }` | A tagged error. Also usable as a type. | Generated |
| `extern type Money` | A type that exists only in host code. Opaque to the compiler. | Host, bound per language in `roadbook.toml` |
| `fn retryDelay(attempt: int) -> duration` | A pure, synchronous function. Used in guards and expressions. | Host |
| `action audit(what: string, order: Order)` | A side effect that cannot fail. Awaited, in order. | Host |
| `task chargeCard(order: Order) -> Receipt ! CardDeclined` | Effectful, fallible, cancellable work. Only run by `invoke`. | Host |
| `machine Checkout(...) -> Receipt ! ... { }` | A statechart with a task's signature. | The chart |

Built-in types are `string`, `int` (64-bit signed), `float`, `bool`, `duration` and `json` (any JSON value). `T?` is optional and `T[]` is a list. `ref Machine` is a handle to a running machine (see [messages between machines](#messages-between-machines)).

An alias of an error type is an ordinary alias: it can be used wherever a type goes. Error lists (`! CardDeclined | Gone`) and `fail E` outcomes name the errors themselves, not aliases of them, since an error is matched by its name.

Names starting with an upper-case letter are types, errors, events, states and machines. Names starting with a lower-case letter are variables, fields, functions, actions and tasks. The grammar enforces this, which keeps `invoke chargeCard(...)` (a task) and `invoke Checkout(...)` (a machine) visibly different.

Host functions receive exactly the arguments the chart passes them. They never see the machine's variables or the current event. This is the main difference from xstate, where every guard and action receives the whole context: here the chart is the complete wiring diagram of both control and data, and the host code is a set of small typed functions that know nothing about the machine that calls them.

## Inside a machine

| Item | Meaning |
| --- | --- |
| `var name: T [= expr]` | A machine variable. |
| `event Name [{ fields }]` | An event the machine accepts from outside. |
| `emits Name [{ fields }]` | An event the machine emits, to whoever invoked it and every machine holding a reference to it. |
| `initial Name` | The child state entered first. Required, once, in a state that has children, except a `parallel state`, which has none: all its children are entered. |
| `state Name { ... }` | A state. Atomic if it has no children, compound if it does. |
| `parallel state Name { ... }` | All children are active at once (regions). A region may be any kind of state. |
| `final state Name { ... }` | Ends the enclosing compound state, or the machine at the top level. |

## Inside a state

| Item | Meaning |
| --- | --- |
| `entry set ... ` / `entry do ...` | Run when the state is entered. |
| `exit set ...` / `exit do ...` | Run when the state is left. |
| `on Event ...` | Transition on an external event. `on *` matches any of the machine's own events this state has no `on` for. |
| `on name.Event ...` | Transition on an event emitted by the machine `name` addresses: an invoked child (`invoke … as name`) or a `ref` parameter or variable. |
| `after 30s ...` | Transition after the state has been active for a duration. The delay is an expression. |
| `always ...` | Transition taken as soon as its guard holds (no event). |
| `done ...` | Transition taken when a compound state reaches a final child, or every region of a parallel state does. |
| `invoke name(args) [as name] { done ...  fail Err ... }` | Run a task or machine while the state is active. At most one per state. `fail *` matches the declared errors not listed. `as` names an invoked machine, so that the state can send to it and receive what it emits. |
| `output expr` / `fail expr` | Final states at the top level only: the machine's result. |

A transition has up to four parts, all optional except that it must do something:

```
on Submit if isValid(order) -> Paying  set attempts = 0  do audit("submitted", order)
   ^trigger ^guard             ^target ^assignments      ^actions
```

With no `-> Target` the transition is internal: the state is not left and re-entered. The effects are `set` assignments, `do` actions, and the messages `send name.Event { … }` and `emit Event { … }` (see [messages between machines](#messages-between-machines)). `set` assignments run first: every `set`, in the order written, then the `do`, `send` and `emit` effects, in the order written among themselves, wherever they are written, so an action or a message sees the values its transition's `set`s assign. The same goes for `entry` and `exit`. The formatter writes every `set` clause before the others, so the source shows the order they run in.

Inside a transition, three names are in scope besides the machine's variables: `event` (in `on`, also `on name.Event`), `result` (in an invoke's `done`) and `error` (in an invoke's `fail`).

A target is resolved like a name in a block-structured language: first among the siblings of the state the transition is written in, then among its parent's siblings, and so on outward. A dotted path such as `Processing.Paying` then descends into a nested state, which is also how a state targets one of its own children. A transition written at machine level resolves among the top-level states. An unknown target is a compile error, and so is a duplicate state name among siblings.

`entry`, `exit`, `on`, `after` and `always` may also be written directly in the machine body, where they apply to the whole machine.

### Which transition is taken

The precise rules are in the [semantics](semantics.md#execution-model) and [`spec/execution.md`](../../spec/execution.md); these are the ones a chart most often depends on.

- **Innermost first.** An event is offered to the active states from the innermost outward. The first state that has an `on` for it tries that `on`'s branches in order, and the first whose guard holds is taken. If none holds, or the state has no `on` for the event, its parent is tried, and so on up to the machine body. So when a state and its ancestor both have `on Resume`, the state's wins whenever its guard holds. A state's `on *` is tried only if that state has no `on` for the event at all. An invoke's outcome is offered only to the state that invokes, and an `after` only to the state it is written in.
- **A transition with a target is external.** It leaves its source state and enters its target, even when the target is the source itself or one of its children: `-> Self` written in `Self`, or `-> Parent.Child` written in `Parent`, exits `Self` (or `Parent`) and enters it again, running `exit` and `entry` and restarting its `invoke` and its `after` timers. The machine body is the one exception: a transition written there does not leave the machine, so the machine's own `entry`, `exit` and timers are left alone.
- **A transition with no target is internal.** Nothing is left or entered, and nothing is restarted: only its effects run. An `after` timer starts when its state is entered and is used up when it fires, so an `after` with no target fires once per entry into its state.
- **The order of a step.** States are exited innermost first, then the transition's effects run, then states are entered outermost first, each running its `entry`. Only then are `always` guards evaluated, and `done` transitions taken, until none applies: an `always` sees what the `entry` effects assigned. Invokes and timers start last, for the states still active.
- **Events go only to the machine they are sent to.** Host code sends to the machine it runs. An invoked child receives an event only when its parent sends it one with `send name.Event`, so a child that waits on `on E` with no such `send` waits until it times out or its parent leaves the invoking state.
- **A failure that is not a declared error** (a host defect, such as a thrown exception or a panic) is not flow: it fails the whole run and what it started, and propagates to whoever ran it. See [the semantics](semantics.md#work-is-tied-to-states).

## Expressions

Expressions are deliberately small. There are no loops, lambdas or string operations, and every expression terminates.

| Form | Example |
| --- | --- |
| Literals | `3`, `1.5`, `"timeout"`, `true`, `30s`, `200ms`, `5m`, `1h` |
| Variables and fields | `attempts`, `error.issues`, `event.sku` |
| Function calls | `retryDelay(attempts)` |
| Record and error values | `CardDeclined { reason: "expired" }`, `GatewayUnavailable` |
| Enum values | `Role.Admin` |
| Comparison | `==`, `!=`, `<`, `<=`, `>`, `>=` |
| Integer and float arithmetic | `+`, `-`, `*` |
| Logic | `and`, `or`, `not` |

`not` binds more loosely than comparison, so `not a > b` means `not (a > b)`, and the formatter writes the parentheses. Integer overflow is a runtime error in both runtimes, not a wrap. Anything more involved belongs in a `fn`. The operators exist so that the two most common pieces of flow logic, counters and thresholds, are readable in the chart (`if attempts < 3`) instead of hidden behind a name (`if canRetry`).

### Types

Every expression has a type, and the compiler checks it wherever a value goes: arguments of functions, actions, tasks and machines (as many as the declaration has parameters, in order), `set` (only to the machine's `var`s, never its parameters), guards (`bool`), `after` delays (`duration`), `var` initial values, the fields of record and error values (every field, and no others), `output` (the machine's return type) and `fail` (one of the machine's declared errors).

- There are no implicit conversions. `int` and `float` are distinct and do not mix: `ratio * 2` is an error when `ratio` is a `float`; write `ratio * 2.0`.
- `+`, `-`, `*` and unary `-` take two `int`s or two `float`s and give the same type. `<`, `<=`, `>` and `>=` compare two `int`s, two `float`s or two `duration`s. `==` and `!=` compare two values of the same scalar type (`string`, `int`, `float`, `bool`, `duration`) or the same enum; records, lists, optionals and `json` cannot be compared (see open question 5 on the [roadmap](roadmap.md#open-questions)). `and`, `or` and `not` take `bool`s.
- A `T` can be used where a `T?` is expected, but not the reverse, and a field cannot be read from a `T?`: an optional value goes to a `fn` that handles the absent case. A `T[]` can be used where a `U[]` is expected if a `T` can be used as a `U`.
- Any value can be used where `json` is expected.
- An alias is the type it names: `type UserId = string` makes `UserId` and `string` interchangeable. An alias cannot refer to itself, even through `[]` or `?`. A type is optional at most once, aliases expanded: `string??` is an error, and so is `S?` after `type S = string?`.
- An `int` or `duration` literal must be within ±(2^53 − 1) (milliseconds, for a duration), the integers the IR's JSON carries exactly.

The implicit names have these types:

- `event`, in `on E`, has type `E`, and `event.field` the type of the event's field; in `on name.E`, the event `E` that `name`'s machine emits. In `on *` it matches events of different types and cannot be used.
- `result`, in an invoke's `done`, has the return type of the task or machine. If it returns nothing, `result` cannot be used.
- `error`, in an invoke's `fail E`, has type `E`. In `fail *` it has a type only if exactly one declared error is left for `*` to cover.

Outside those transitions the three names are not in scope.

A `ref Machine` is a handle, not data. It can be a parameter of a machine, function, action or task and a `var`, and be passed as an argument to an invoked machine and to host functions, actions and tasks. It cannot be a field of a record, error or event, or what a function, task or machine returns (`RB0316`): a machine gets a handle only from its own signature or from `invoke … as`, so the charts' signatures show who can talk to whom. It has no JSON form, so it cannot be passed where `json` is expected, and `==` and `!=` are not defined on it. `ref A` and `ref B` are different types for different machines; a `ref A` can be used where a `ref A?` is expected, as for any `T`.

There are no generics, no built-in functions and no list literals, so anything a chart asks of a list goes through a host `fn`. A `json[]` parameter accepts a list of any element type, so one function serves every list: `fn isEmpty(items: json[]) -> bool`, used as `always if isEmpty(problems) -> Accept`. Optionals work the same way: there is no `none` literal and no narrowing, so a chart cannot test for presence and then use the value. When absence changes the flow, make it a declared error of the task that looks the value up (`task findToken(id: string) -> Token ! TokenNotFound`), and build a record whose optional field may be absent in a `fn`.

## Messages between machines

Machines talk to each other with typed events, as [actors and messaging](actors.md) describes. A machine's `event` declarations are what it accepts, its `emits` declarations what it sends out.

```
machine Upload(file: File) -> Url ! UploadFailed {
  event Pause
  event Resume
  emits Progress { percent: int }
  // … `emit Progress { percent: percentOf(file, offset) }` where it happens
}

state Uploading {
  invoke Upload(draft.file) as upload {
    done              -> Publishing  set url = result
    fail UploadFailed -> Failed      set failure = error
  }
  on PauseClicked    send upload.Pause
  on upload.Progress set percent = event.percent
}
```

- **`send name.Event { fields }`** queues the event for another machine and carries on. `name` is an `invoke … as name` in scope, or a parameter or variable of type `ref Machine`. The event must be one the machine declares with `event`, with every field and the right types, as a record value. A field-less event is written bare: `send upload.Pause`.
- **`emit Event { fields }`** emits one of the machine's own `emits`, to whoever invoked it and to every machine holding a reference to it. The machine does not receive its own emitted events.
- **`on name.Event`** is taken for an event that `name`'s machine emits. `name` is resolved as for `send`, and `Event` must be one that machine declares with `emits`.
- **An `as` name is in scope in the invoking state and the states inside it**, which is while the child runs: elsewhere it is an error (`RB0115`). A `send` to the child must also find it running (`RB0213`): not in the invoking state's own `entry` (the child starts once the step has settled), `exit` (it has been stopped) or invoke outcomes (it has ended), and not in a transition that leaves the invoking state, since leaving stops the child before the transition's effects run. A transition with no target, or one written in a state inside the invoking state whose target is also inside it, can send. Two states may use the same `as` name when neither contains the other.
- **Names.** An `as` name shares the machine's namespace with its parameters and variables, and cannot be the name of an `as` around it. It is not a value: it is used only in `send name.Event` and `on name.Event`. A machine's `event` and `emits` names share one namespace, and like events, emitted events cannot have the name of a type, error or machine.
- **Two namespaces of triggers.** The events a machine accepts are its own: `on Cancel`. The events it receives from others are named through their sender: `on upload.Cancel` is a different trigger from `on Cancel`, and from `on other.Cancel`. `on *` matches only the machine's own events, never an emitted one.
- **A `ref` variable** addresses whichever machine it holds when the `send` runs or the event arrives.

Keyed `spawn` (`children … by …`) is not part of the language yet.

## Projects

A project is a directory tree of `.road` files. Its root is the nearest directory holding a `roadbook.toml`, searched upward from the first path given to a command (or the current directory), else the current directory. Files are named by their path relative to the root, with `/` separators, in diagnostics and in the IR's `sources`.

Every command works file by file. `roadbook check` compiles each file it is given as an entry, together with everything it imports, and reports each diagnostic once even when several entries import the file it is about. A directory stands for every `.road` file under it, and no path for the whole project. `target`, `node_modules` and `.git` directories are never searched, and links to directories are not followed. `roadbook export ir <file>` compiles one entry.

An import path that starts with `./`, `../` or `/` names a file of the same project, or of the same package for a file in a package: relative to the importing file, or to the root for `/`. Any other path names a file in another package: `"inventory/inventory.road"` is the file `inventory.road` of the package `inventory`, and `"@shop/inventory/charts/inventory.road"` the file `charts/inventory.road` of the scoped package `@shop/inventory`. A package's charts must be shipped in it. The command line finds a package, in this order:

1. in the `[packages]` table of `roadbook.toml`, which maps a package's name to its directory;
2. as Node does, in `node_modules/<package>` of the importing file's directory and of each directory above it. If the package's `package.json` has an `exports` map whose keys are subpaths, such as `"./*.road": "./charts/*.road"`, an entry for the file that is a string, or an object with a string `default`, with at most one `*`, is honoured, and a file the map does not list is not exported. Any other `exports` is ignored and the path is taken inside the package's directory.

The Rust macro does not look for files: it receives the owning crate's compiled IR (see [actors and messaging](actors.md#across-crates-and-packages)), and the compiler takes the IR's declarations as it would a parsed file's. In diagnostics a package's file is named by where it is, relative to the project root; in the IR's `sources` by its package and its path in the package. Only errors are reported in another package's charts, never warnings. Declaration names are unique among all the files compiled together, other packages' included.

`roadbook.toml` may be empty. Its keys so far:

| Key | Meaning |
| --- | --- |
| `exclude = ["glob", …]` | Files and directories that a directory search skips, as globs over project paths: `*` matches within one path segment, `**` across segments and `?` one character. A directory that matches is skipped whole. A file named on the command line is checked even if it matches. |
| `gen-exclude = ["glob", …]` | More files and directories, as for `exclude`, that only `roadbook gen` skips: charts that need no generated TypeScript module, such as fixtures read as IR. `roadbook check` and the other commands still see them, and a file named on the command line is generated even if it matches. |
| `[packages]` | A table of `name = "directory"`: the directory of each package charts import from, relative to the root. A scoped name is quoted: `"@shop/inventory" = "packages/inventory"`. It comes after the keys above. |

```toml
exclude = ["crates/roadbook-compiler/tests/fail", "spike"]
gen-exclude = ["spec/conformance/charts"]

[packages]
inventory = "examples/inventory"
```

Other keys, such as the host types bound to `extern type`s, come with the code generators.

## What the compiler checks

Errors (the build fails):

- Unknown or duplicate names: states, events, variables, functions, actions, tasks, types. A name used as something it is not, such as an action in `invoke`, is an error too.
- Type errors in arguments, assignments, guards, `output` and `fail` (see [types](#types)).
- A compound state without `initial`, or an `initial` that is not a child.
- An `invoke` that does not handle every declared error of the task or machine, or that names an error the task or machine does not declare.
- The same trigger written twice in one state. Several branches go in one block, where their order is explicit and only the last may be unguarded, so a branch can never be shadowed by an earlier one.
- A read of a variable that might not be assigned yet. <a id="definite-assignment"></a> This is [milestone 6](roadmap.md#milestones); until then reading an unassigned variable is a runtime error.
- A machine that returns `T` with a top-level final state that has no `output` or `fail`, and `output` or `fail` anywhere but a final state at the top level.
- A transition or `invoke` in a final state. A final state ends its parent as soon as it is entered, so they could never run; it holds only `entry`, `exit`, `output` and `fail`.
- A `done` transition in a state with no child states: it waits for a final child, so it could never be taken. (An invoke's `done` goes inside `invoke { }`.)
- An unguarded cycle of `always` transitions: states each with an `always` whose first branch has no `if` (an `else` counts as guarded, since it runs only when the guards before it fail), each leading to the next, where entering a state also enters its `initial` chain (and every region of a parallel state), and a transition to a state's own ancestor leaves and re-enters it. An unguarded `always` with no target is the shortest such cycle. So is an unguarded `always` written in the machine body, whatever its target, unless that target is a top-level final state: the machine itself is never left, so its `always` would be taken again after every step.
- `send`, `emit` and `on name.Event` that do not match a declaration: an unknown or out-of-scope `name`, or one that is not a `ref` (a `ref` that is optional or a list cannot be sent to); an event the receiver does not declare with `event`, or the sender with `emits`; a missing, extra or wrongly typed field. A `send` to an invoked child where it is not running. An `as` on a task, or one that clashes with a variable, parameter or enclosing `as`. A `ref` to something that is not a machine, or in a field or a result.
- A machine that invokes itself, directly or through other machines, without an `after` or event in between (unbounded recursion). Precisely: machine `M` *starts* machine `N` if a state of `M` that invokes `N` can be entered from `M`'s initial states by taking only `always`, `done` and invoke outcome transitions, whatever their guards. Only `on` and `after` transitions count as waiting; an invoke's outcome does not, since a task can finish at once. A cycle of *starts* is an error.

Warnings:

- States that cannot be reached from the initial state, following every transition whatever its guard. Only the outermost unreachable state is reported.
- Non-final states with no way out: an atomic state that is reachable, where neither it nor any state containing it has a transition with a target (a `done` does not count: it needs a final child).
- Events, functions, actions or tasks that are declared but never used: an event no `on` of its machine names, or a function, action or task that nothing in the compilation calls, runs or invokes (a use in a `send` or `emit` counts). Only declarations in the files being checked are reported, and a use from any file checked at the same time counts, so checking a file of shared declarations on its own reports everything only other files use; check them together (`roadbook check` with no paths checks the whole project).
- A state with an `invoke` and no timeout anywhere above it: neither the state nor any state containing it has an `after` (off by default; `roadbook check --warn invoke-timeout`).
- An `emits E` that no `emit E` of its machine sends, and an `on name.E` for an `E` its sender declares but never emits, which can never be taken.
- An `emits E` that no `on name.E` in the checked files receives. As for unused declarations, a receiver in any file checked at the same time counts; host code holding a handle may still receive it.

The checks run in order: names and structure, then types, then the analyses (the cycle and recursion checks and the warnings). A file with syntax errors is checked no further: its declarations are left out, and the names other files import from it are not reported as unknown. The files around it are still checked. The analyses run for each entry file once its compilation (the file and everything it imports) has no other errors, so one mistake does not produce a cascade of diagnostics, and an error in one chart does not turn off the analyses of another.

Diagnostics carry a code, a source span and, where possible, a fix. `roadbook check --json` prints them in a machine-readable form so an agent can act on them without parsing prose.

```
error[RB0204]: `invoke chargeCard` does not handle `GatewayUnavailable`
  --> examples/shop/checkout.road:13:5
   |
13 |     invoke chargeCard(order, user) {
   |     ^^^^^^^^^^^^^^^^^ declared as `-> Receipt ! CardDeclined | GatewayUnavailable`
   |
   = help: add `fail GatewayUnavailable -> <State>` or `fail * -> <State>`
```

### Diagnostics as JSON

`roadbook check --json` prints one JSON object on standard output, whatever the outcome. Its shape is stable: members come in this order and are always present, `null` or `[]` when empty.

```jsonc
{
  "diagnostics": [
    {
      "code": "RB0204",
      "severity": "error",                       // or "warning"
      "message": "`invoke chargeCard` does not handle `GatewayUnavailable`",
      "file": "examples/shop/checkout.road",     // relative to the project root
      "line": 13, "col": 5, "endLine": 13, "endCol": 22,
      "label": "declared as `-> Receipt ! CardDeclined | GatewayUnavailable`",  // next to the primary span, or null
      "labels": [ { "file": "…", "line": 1, "col": 1, "endLine": 1, "endCol": 9, "message": "…" } ],
      "help": "add `fail GatewayUnavailable -> <State>` or `fail * -> <State>`",  // or null
      "fixes": [
        { "message": "replace with `Done`",
          "edits": [ { "file": "…", "line": 5, "col": 14, "endLine": 5, "endCol": 17, "text": "Done" } ] }
      ]
    }
  ],
  "errors": 1,
  "warnings": 0,
  "fatal": null                                  // or the message of a usage or I/O error
}
```

Ranges are as in the IR: lines and columns are 1-based, columns count Unicode scalar values, and the end is exclusive. A fix is a set of edits that together resolve the diagnostic: replace each range with its `text` (an empty range inserts). The command exits with 0 when there are no errors (warnings allowed), 1 when there are errors and 2 on a usage or I/O error, such as a file that does not exist. In that last case the object has no diagnostics and `fatal` holds the error's message, which is also printed on standard error.

### Definite assignment

A `var` with no default and a non-optional type starts unassigned. The compiler runs a forward dataflow analysis over the chart: a variable is assigned in a state if every transition that can enter that state either assigns it or leaves from a state where it was already assigned. Reading a variable in a state where it is not assigned is an error.

In `PlaceOrder`, `order` is assigned on the only way into `Pricing`, so `invoke priceOrder(order, user)` is accepted. Change `Validating`'s `done` branch to drop `set order = result` and the compiler points at the read in `Pricing`.

This is what lets host functions take plain non-optional arguments. In xstate the same machine would declare `order: Order | undefined` and every reader would assert it is present.

## Formatting

`roadbook fmt` is canonical: one accepted layout, no options. Agents and humans produce the same text, so diffs show only real changes.

The layout, as implemented in `roadbook-syntax`. The example files and the charts on these pages are written in it.

- **Lines.** Two-space indentation, one item per line, and no line wrapping. A file with syntax errors is not formatted.
- **Spacing.** One space around binary operators, `->`, `=`, `!` and `|`, after `:` and `,`, and inside the braces of a one-line list or record value; none inside parentheses. In a transition the target and each `set`, `do`, `send` or `emit` clause are separated by two spaces: `-> Paying  set attempts = 0  do audit(order)`. Every `set` comes first, then the other clauses in the order written; consecutive clauses of one kind are merged for `set` and `do`: `do f()  set a = 1  set b = 2` is written `set a = 1, b = 2  do f()`. Each `send` and `emit` is a clause of its own, and an empty body is dropped: `send upload.Pause {}` is written `send upload.Pause`.
- **Blank lines** between items are kept, at most one. There are none at the start or end of a block or file.
- **Lists.** No trailing commas. A list in a declaration (import names, fields, enum variants, parameters) with a line break right after its opening bracket stays multi-line: its elements are indented one level, keep the author's line breaks between them, and the closing bracket goes on its own line. Every other list is printed on one line.
- **One-line states.** A state whose body is empty, or is a single item other than a state, an `invoke` or a block of branches, and contains no comment, is written on one line: `final state Charged { output receipt }`. Every other state is written as a block.
- **Expressions** have only the parentheses precedence needs, plus `not (…)` around a comparison or arithmetic operand, and parentheses around a record value used as an `after` delay. Numbers are written as parsed (`007` becomes `7`, `1.50` becomes `1.5`). `error E {}` is written `error E`, and a record value with no fields `E {}` is written `E`.
- **Comments** are kept as written, trailing spaces removed. A comment on its own line stays on its own line, indented like the code that follows it. A comment after code stays at the end of that line, including one after the closing bracket of a multi-line list, which ends the line that bracket starts (`) { // c`, `} from "./x.road" // c`). A comment inside something printed on one line moves to its own line before it.
- **Alignment.** An alignment group is a run of consecutive lines of one kind with no blank line or comment line between them. The kinds and what they align:
  - one-line `type`, `enum`, `error` and `extern type` declarations: what follows the name (`{` or `=`);
  - one-line `fn`, `action` and `task` declarations: `->` and `!`;
  - `var` lines: the type;
  - `event` and `emits` lines, together: the field list;
  - one-line states: the `{`;
  - transitions and invoke outcomes, together with the header, branches and closing `}` of a block of branches among them: `->` and the first `set` or `do`, at the same column for nested branches as for the lines around them.

  Each aligned column starts one space after the widest text of the column before it, measured from the start of the line, over every line of the group (closing `}` lines excepted), including lines that end before that column. In transitions the effects column starts two spaces after the widest target. Lines are not padded at the end.

## Diagnostic codes

Codes are `RB` and four digits, grouped by range: `RB00xx` syntax, `RB01xx` names, `RB02xx` structure, `RB03xx` types, `RB04xx` analyses and `RB09xx` warnings. The syntax codes:

| Code | Meaning | Example |
| --- | --- | --- |
| `RB0001` | A character that cannot start a token. | `on Go # comment` |
| `RB0002` | A string with no closing quote on its line. | `"timeout` |
| `RB0003` | An escape other than `\"`, `\\`, `\n` and `\t`. | `"a\qb"` |
| `RB0004` | A number followed by letters that are not a duration unit. | `after 5min`, `after 1.5s` |
| `RB0005` | An integer that does not fit in an `int`, a duration whose number does not fit in 64 bits, or a float too large to be finite. | `99999999999999999999`, a `float` of 400 digits |
| `RB0006` | A name containing `_`. | `type Line_item` |
| `RB0010` | An unexpected token. | `type A { x int }` |
| `RB0011` | A `{` or `(` that is never closed. | `state A {` at the end of the file |
| `RB0012` | A name of the wrong case. | `type order`, `state idle` |
| `RB0013` | A keyword, or `result`, used as a name. | `var on: int`, `var result: int`, `type Evt { type: string }` |
| `RB0014` | `done`, `invoke`, `output` or `fail` directly in a machine body. | `machine M() { done -> A }` |
| `RB0015` | `var` or `event` inside a state. | `state A { var x: int }` |
| `RB0016` | A machine item at the top level of a file. | `state A {}` outside a machine |
| `RB0017` | A transition with no target, `set` or `do`. | `on Go` |
| `RB0018` | A record value as an `after` delay without parentheses. | `after Cfg { ms: 5 } -> A` |
| `RB0019` | An invoke outcome outside `invoke { }`. | `fail CardDeclined -> Declined` in a state |
| `RB0020` | Something other than an outcome inside `invoke { }`. | `fail declined`, `on Go -> A` in an invoke |
| `RB0021` | A block of branches that does not start with `if`, or has a branch after `else`. | `on Go { else -> A }` |
| `RB0022` | Chained comparisons. | `if a < b < c` |
| `RB0023` | A `///` comment that documents nothing. | `/// note` above a transition |
| `RB0024` | A lower-case type name that is not a built-in type. | `x: integer` |
| `RB0025` | Nesting more than 64 levels deep. | 65 nested states, or `((((…))))` |

The compiler's codes:

| Code | Meaning | Example |
| --- | --- | --- |
| `RB0101` | An unknown type or error name. | `order: Ordr` |
| `RB0102` | An unknown task or machine in `invoke`. | `invoke chrgeCard(order)` |
| `RB0103` | An unknown function. | `if isVald(order)` |
| `RB0104` | An unknown action in `do`. | `do audt("x")` |
| `RB0105` | An unknown variable. | `set attemps = 1` |
| `RB0106` | An event the machine does not declare. | `on Cancl -> Done` |
| `RB0107` | Two declarations with the same name in one compilation, or an event named like a type, error or machine. | `type Order` in two files, `event Order` |
| `RB0108` | Two fields, parameters, variants, variables, events or sibling states with the same name, or a name imported twice. | `state A {}` twice in one parent, `import { B, B }` |
| `RB0109` | A name used as something it is not. | `invoke audit(x)` where `audit` is an action |
| `RB0110` | `event`, `result` or `error` where it is not in scope. | `entry set n = result` |
| `RB0111` | An import of a file that cannot be read. | `import { A } from "./missing.road"` |
| `RB0112` | An import of a name the file does not declare. | `import { Ordr } from "./types.road"` |
| `RB0113` | An enum variant that does not exist. | `Role.Admn` |
| `RB0114` | A `set` of a machine parameter. | `set token = ""` |
| `RB0115` | A `send name.E` or `on name.E` whose `name` is not an `invoke … as name` in scope or a parameter or variable. | `send uplod.Pause`, `upload` outside its invoking state |
| `RB0116` | A `send` of an event the receiving machine does not declare with `event`. | `send upload.Stop` |
| `RB0117` | An `emit` of an event the machine does not declare with `emits`, or an `on name.E` of one its sender does not. | `emit Progres`, `on upload.Pause` |
| `RB0201` | A transition target that does not resolve. | `-> Dne` |
| `RB0202` | A compound state or machine without `initial`, or a machine with no states. | `state A { state B {} }`, `machine M() { initial A }` |
| `RB0203` | An `initial` that is not a child, is written twice, or is in a state without children. | `initial Strat` |
| `RB0204` | An `invoke` that leaves a declared error unhandled. | see above |
| `RB0205` | The same trigger twice in one state or one `invoke`. | `on Go -> A` and `on Go -> B` |
| `RB0206` | A second `invoke` in a state. | two `invoke`s in `state A` |
| `RB0207` | An outcome for an error the task or machine does not declare. | `fail Timeout -> A` |
| `RB0208` | `output` or `fail` outside a top-level final state, or two of them. | `state A { output 1 }` |
| `RB0209` | A top-level final state with no `output` or `fail` in a machine that returns a value. | `final state Done {}` in `machine M() -> int` |
| `RB0210` | A final state with children, or a parallel state with none. | `final state A { state B {} }` |
| `RB0211` | A transition or `invoke` in a final state. | `final state Done { on Go -> A }` |
| `RB0212` | A `done` transition in a state with no child states, which can never be taken. | `state A { done -> B }` |
| `RB0213` | A `send` to an invoked child where it is not running. | `entry send upload.Pause` in the invoking state, `on Cancel -> Idle  send upload.Stop` |
| `RB0214` | `as` on an invoke of a task. | `invoke fetch() as f` |
| `RB0301` | A value of the wrong type. | `var ratio: float = 1` |
| `RB0302` | The wrong number of arguments. | `retryDelay(1, 2)` |
| `RB0303` | An operator on operands it is not defined on. | `attempts + 1.5`, `order == other` |
| `RB0304` | A field the record, error or event does not have. | `order.idd` |
| `RB0305` | A record or error value without one of its fields. | `CardDeclined {}` |
| `RB0306` | A field read from a value that has none, or that is optional. | `attempts.size`, `draft.coupon.code`, `event.x` in `on Cancel` where `event Cancel` has no fields |
| `RB0307` | `result` after a task or machine that returns nothing. | `done set n = result` |
| `RB0308` | `error` in a `fail *` that covers several errors, or none. | `fail * set why = error.reason` |
| `RB0309` | `event` in `on *`. | `on * do log(event)` |
| `RB0310` | `output` in a machine that returns nothing. | `machine M() { … output 1 }` |
| `RB0311` | A `fail` value that is not a declared error of the machine. | `fail "late"` |
| `RB0312` | An `int` or `duration` literal beyond ±(2^53 − 1). | `9007199254740992` |
| `RB0313` | A type alias that refers to itself. | `type A = A[]` |
| `RB0314` | A type made optional twice. | `note: string??` |
| `RB0315` | A `send` or `on name.E` through a value that is not a `ref`, or a `ref` that is optional or a list. | `send count.Ping` for `count: int` |
| `RB0316` | A `ref` in a field of a record, error or event, or in what a function, task or machine returns. | `type Holder { peer: ref Peer }` |
| `RB0401` | An unguarded cycle of `always` transitions. | `A { always -> B }`, `B { always -> A }` |
| `RB0402` | A machine that invokes itself without waiting. | `state A { invoke M() { … } }` as `M`'s initial state |
| `RB0901` | A state that can never be entered (warning). | a state nothing targets |
| `RB0902` | A non-final state with no way out (warning). | `state Stuck {}` |
| `RB0903` | An event, function, action or task nothing uses (warning). | `event Unused` |
| `RB0904` | An `invoke` with no `after` around it (warning, off by default). | `state A { invoke fetch() { … } }` |
| `RB0905` | An `emits` the machine never emits (warning). | `emits Progress` with no `emit Progress` |
| `RB0906` | An `on name.E` for an event its sender never emits (warning). | `on upload.Progress` when `Upload` has no `emit Progress` |
| `RB0907` | An `emits` that nothing in the checked files receives (warning). | `emits Progress` with no `on upload.Progress` anywhere |

## Grammar

```ebnf
file        = { import | decl } ;
import      = "import" "{" name { "," name } "}" "from" string ;
decl        = typeDecl | enumDecl | errorDecl | externDecl
            | fnDecl | actionDecl | taskDecl | machineDecl ;

externDecl  = "extern" "type" Upper ;
typeDecl    = "type" Upper ( "=" type | fields ) ;
enumDecl    = "enum" Upper "{" Upper { "," Upper } "}" ;
errorDecl   = "error" Upper [ fields ] ;
fields      = "{" [ field { "," field } ] "}" ;
field       = lower ":" type ;
type        = ( Upper | "string" | "int" | "float" | "bool" | "duration" | "json"
              | "ref" Upper )
              { "[]" | "?" } ;

fnDecl      = "fn" lower params "->" type ;
actionDecl  = "action" lower params ;
taskDecl    = "task" lower params [ "->" type ] [ errors ] ;
params      = "(" [ field { "," field } ] ")" ;
errors      = "!" Upper { "|" Upper } ;

machineDecl = "machine" Upper params [ "->" type ] [ errors ] "{" { machineItem } "}" ;
machineItem = varDecl | eventDecl | emitsDecl | commonItem ;
varDecl     = "var" lower ":" type [ "=" expr ] ;
eventDecl   = "event" Upper [ fields ] ;
emitsDecl   = "emits" Upper [ fields ] ;

stateDecl   = [ "final" | "parallel" ] "state" Upper "{" { stateItem } "}" ;
stateItem   = commonItem
            | "done" branches
            | "invoke" name "(" [ expr { "," expr } ] ")" [ "as" lower ] "{" outcome { outcome } "}"
            | "output" expr
            | "fail" expr ;
commonItem  = "initial" Upper
            | stateDecl
            | "entry" effects
            | "exit" effects
            | "on" ( Upper | "*" | lower "." Upper ) branches
            | "after" expr branches
            | "always" branches ;
outcome     = "done" branches
            | "fail" ( Upper | "*" ) branches ;

branches    = [ "if" expr ] action
            | "{" "if" expr action { "if" expr action } [ "else" action ] "}" ;
action      = "->" target [ effects ]
            | effects ;
effects     = effect { effect } ;
effect      = "set" assign { "," assign }
            | "do" call { "," call }
            | "send" lower "." Upper [ recordBody ]
            | "emit" Upper [ recordBody ] ;
recordBody  = "{" [ lower ":" expr { "," lower ":" expr } ] "}" ;
assign      = lower "=" expr ;
call        = lower "(" [ expr { "," expr } ] ")" ;
target      = Upper { "." Upper } ;

expr        = orExpr ;
orExpr      = andExpr { "or" andExpr } ;
andExpr     = notExpr { "and" notExpr } ;
notExpr     = [ "not" ] cmpExpr ;
cmpExpr     = addExpr [ ( "==" | "!=" | "<" | "<=" | ">" | ">=" ) addExpr ] ;
addExpr     = mulExpr { ( "+" | "-" ) mulExpr } ;
mulExpr     = unary { "*" unary } ;
unary       = [ "-" ] postfix ;
postfix     = primary { "." lower } ;
primary     = int | float | duration | string | "true" | "false"
            | call
            | lower
            | Upper [ "." Upper | recordBody ]
            | "(" expr ")" ;

name        = Upper | lower ;
Upper       = /[A-Z][A-Za-z0-9]*/ ;
lower       = /[a-z][A-Za-z0-9]*/ ;
int         = /[0-9]+/ ;
float       = /[0-9]+\.[0-9]+/ ;
duration    = /[0-9]+(ms|s|m|h)/ ;
string      = /"([^"\\\n]|\\["\\nt])*"/ ;
comment     = "//" … end of line ;        doc comment = "///" … end of line ;
```

Rules the grammar does not express:

- Every comma-separated list accepts a trailing comma.
- All keywords are reserved: `import type enum error extern fn action task machine var event emits initial state final parallel entry exit on after always done invoke output fail if else set do send emit and or not true false`. `from`, `as`, `ref` and the built-in type names are not reserved, because they only have a meaning in a position where no name can appear: after an import list, after an invoke's arguments, and at the start of a type. `event`, `error` and `result` are the implicit names inside transitions and cannot be used for variables or parameters.
- A `///` comment documents the declaration, machine, state, event or variable that follows it. A `///` anywhere else is an error: use `//`.
- A single-line transition and a multi-branch block are told apart by the `{` after the trigger. For that reason a record value in an `after` delay must be parenthesised, as with `if` in Rust. A `{` after the event of a `send` or `emit` is always its record body.
- `on name.Event` is told from `on Event` by the lower-case name and the `.` after it.
- In a final state, `fail expr` is the machine's failure. Inside `invoke { }`, `fail Upper` is an outcome. The two never appear in the same position.
- Two `after` transitions in one state are the same trigger only if their delays are the same expression, written the same way once formatted: `after 1s` and `after 1000ms` are different triggers.
- A type is optional at most once: `T??` is an error.
- Nesting is at most 64 levels deep, counting states, parentheses, operators (each `+` in `a + b + c` is a level), field reads and type suffixes (`[]`, `?`), so every tool that walks the tree stays within its stack.
- A final state holds only `entry`, `exit`, `output` and `fail`.
- Declaration names are unique among the files compiled together: an entry file and everything it imports, directly or not, because the IR is keyed by name. Two charts that do not import one another may use the same names. A file can use only what it declares or imports. An event cannot have the name of a type, error or machine either, because the IR names the type of `event` in `on Name` by the event's name.

Common mistakes, each reported by the compiler but easy to make:

- There is no day unit: 14 days is `336h`.
- Keywords cannot be names, so a field for an event's kind is `kind`, not `type`; and `event`, `error` and `result` cannot be variables or parameters, since they are the implicit names.
- A state names its own child through its own name: `-> Parent.Child` written in `Parent`, because a bare `-> Child` is looked up among `Parent`'s siblings.
- `int` and `float` do not mix: `ratio * 2` is an error for a `float` `ratio`; write `2.0`.
- A record value as an `after` delay needs parentheses: `after (Cfg { ms: 5 }) -> A`.

Most of these rules, the split between `machineItem` and `stateItem`, and the requirement that a branch does something came from writing the spike's parser against the first draft of this grammar. That parser was deleted once the Rust compiler replaced it.

## Not in the first version

Each of these has a planned shape, and the grammar leaves room for it.

- **Keyed `spawn`.** A dynamic number of children, sketched on the [actors and messaging](actors.md#many-children) page. `send`, `emit`, `ref` and `invoke … as` are in the language (see [messages between machines](#messages-between-machines)); the runtimes do not run them yet.
- **History states.** `history` and `deep history` pseudo-states, as in SCXML.
- **Generic types and maps.** `map<string, T>` and user-defined generics.
- **Retry sugar.** `invoke chargeCard(order) retry 3` as shorthand for the `Backoff` pattern above. The explicit states stay available and are what the diagram shows either way.
