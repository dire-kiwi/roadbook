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
| `type Page<T> { items: T[], next: string? }` | A generic record (or alias, `type Dict<T> = map<string, T>`). See [maps and generic types](#maps-and-generic-types). | Generated |
| `enum Role { Customer, Admin }` | A closed set of names. | Generated |
| `error CardDeclined { reason: string }` | A tagged error. Also usable as a type. | Generated |
| `extern type Money` | A type that exists only in host code. Opaque to the compiler. | Host, bound per language in `roadbook.toml` |
| `fn retryDelay(attempt: int) -> duration` | A pure, synchronous function. Used in guards and expressions. | Host |
| `action audit(what: string, order: Order)` | A side effect that cannot fail. Awaited, in order. | Host |
| `task chargeCard(order: Order) -> Receipt ! CardDeclined` | Effectful, fallible, cancellable work. Only run by `invoke`. | Host |
| `machine Checkout(...) -> Receipt ! ... { }` | A statechart with a task's signature. | The chart |

Built-in types are `string`, `int` (64-bit signed), `float`, `bool`, `duration` and `json` (any JSON value). `T?` is optional, `T[]` is a list and `map<string, T>` a map from strings to `T`. `ref Machine` is a handle to a running machine (see [messages between machines](#messages-between-machines)). A record or alias may take type parameters, `Page<Order>` (see [maps and generic types](#maps-and-generic-types)).

An alias of an error type is an ordinary alias: it can be used wherever a type goes. Error lists (`! CardDeclined | Gone`) and `fail E` outcomes name the errors themselves, not aliases of them, since an error is matched by its name.

Names starting with an upper-case letter are types, errors, events, states and machines. Names starting with a lower-case letter are variables, fields, functions, actions and tasks. The grammar enforces this, which keeps `invoke chargeCard(...)` (a task) and `invoke Checkout(...)` (a machine) visibly different.

Host functions receive exactly the arguments the chart passes them. They never see the machine's variables or the current event. This is the main difference from xstate, where every guard and action receives the whole context: here the chart is the complete wiring diagram of both control and data, and the host code is a set of small typed functions that know nothing about the machine that calls them.

## Inside a machine

| Item | Meaning |
| --- | --- |
| `var name: T [= expr]` | A machine variable. `sensitive var` keeps its value out of traces (see [sensitive values](#sensitive-values)). |
| `event Name [{ fields }]` | An event the machine accepts from outside. |
| `emits Name [{ fields }]` | An event the machine emits, to whoever invoked or spawned it and every machine holding a reference to it. |
| `children name: Machine by Key` | A keyed collection of child machines, started with `spawn` (see [many children](#many-children)). |
| `initial Name` | The child state entered first. Required, once, in a state that has children, except a `parallel state`, which has none: all its children are entered. |
| `state Name { ... }` | A state. Atomic if it has no children, compound if it does. |
| `parallel state Name { ... }` | All children are active at once (regions). A region may be any kind of state. |
| `final state Name { ... }` | Ends the enclosing compound state, or the machine at the top level. |
| `history Name`, `deep history Name` | In a state with children: a [history state](#history-states), a target that returns to where the state was when it was last left. |

## Inside a state

| Item | Meaning |
| --- | --- |
| `entry set ... ` / `entry do ...` | Run when the state is entered. |
| `exit set ...` / `exit do ...` | Run when the state is left. |
| `on Event ...` | Transition on an external event. `on *` matches any of the machine's own events this state has no `on` for. |
| `on name.Event ...` | Transition on an event emitted by the machine `name` addresses: an invoked child (`invoke … as name`), a `ref` parameter or variable, or any child of a `children` collection. |
| `on name.done ...` / `on name.fail Err ...` | Transition when a spawned child of the collection `name` ends (see [many children](#many-children)). |
| `after 30s ...` | Transition after the state has been active for a duration. The delay is an expression. |
| `always ...` | Transition taken as soon as its guard holds (no event). |
| `done ...` | Transition taken when a compound state reaches a final child, or every region of a parallel state does. |
| `invoke name(args) [as name] { done ...  fail Err ... }` | Run a task or machine while the state is active. At most one per state. `fail *` matches the declared errors not listed. `as` names an invoked machine, so that the state can send to it and receive what it emits. |
| `invoke name(args) retry N [on Err] [after delay] { ... }` | The same, run again when it fails, up to `N` more times (see [retrying an invoke](#retrying-an-invoke)). |
| `output expr` / `fail expr` | Final states at the top level only: the machine's result. |

A transition has up to four parts, all optional except that it must do something:

```
on Submit if isValid(order) -> Paying  set attempts = 0  do audit("submitted", order)
   ^trigger ^guard             ^target ^assignments      ^actions
```

With no `-> Target` the transition is internal: the state is not left and re-entered. The effects are `set` assignments, `do` actions, the messages `send name.Event { … }` and `emit Event { … }` (see [messages between machines](#messages-between-machines)), and `spawn` and `stop` (see [many children](#many-children)). `set` assignments run first: every `set`, in the order written, then the other effects, in the order written among themselves, wherever they are written, so an action or a message sees the values its transition's `set`s assign. The same goes for `entry` and `exit`. The formatter writes every `set` clause before the others, so the source shows the order they run in.

Inside a transition, these names are in scope besides the machine's variables: `event` (in `on`, also `on name.Event`), `result` (in an invoke's `done` and in `on name.done`), `error` (in an invoke's `fail` and in `on name.fail`) and `key` (in `on name.Event`, `on name.done` and `on name.fail` through a `children` collection).

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

## Retrying an invoke

The `Backoff` pattern at the top of this page is common enough to have a shorthand. `retry` after an invoke's arguments runs the task again when it fails:

```
state Charging {
  invoke chargeCard(order, user) retry 3 on GatewayUnavailable after retryDelay(attempt) {
    done                    -> Charged   set receipt = result
    fail CardDeclined       -> Declined  set declined = error
    fail GatewayUnavailable -> GaveUp
  }
}
```

`retry N [on E | F …] [after delay]` means:

- **How many times.** `N` is an integer literal of at least 1: the number of retries, so the task runs at most `N + 1` times. `retry 3` above makes up to four attempts.
- **Which errors.** The errors listed after `on`, each one the task or machine declares; without `on`, every error it declares. Nothing else is retried: not `done`, and not a host defect (a thrown exception, a panic), which is not flow and fails the run as it does without `retry`.
- **When the retries run out.** A failure that is not retried, an error not in the list or the failure after the last retry, goes to its outcome as it would without `retry`, with `error` the last failure. Every declared error still needs an outcome (`RB0204`): it says what happens when retrying did not help.
- **Delays.** `after delay` is how long to wait between a failure and the next attempt; without it, the next attempt starts at once. The delay is a `duration` expression, evaluated when the wait starts, like an `after` transition's. In it, `attempt` is the number of attempts that have failed so far, an `int`: 1 before the first retry, 2 before the second. So `after 2s` waits the same each time and `after retryDelay(attempt)` lets a host function grow the delay (doubling, a cap, jitter): the language has no arithmetic on durations, so backoff is the function's choice. `attempt` is in scope only in a retry's delay, and it is the only implicit name there: the delay cannot read `error`, so a wait chosen by the failure (a server's retry-after) needs the explicit form. `attempt` is not reserved, so a machine may have a parameter or variable named `attempt`, but then it cannot use `attempt` in a retry delay (`RB0118`).
- **The state stays active.** While it retries, the invoking state is still active, through every attempt and every wait. Its own `on`, `after` and `always` transitions apply all the time, so `after 30s -> TimedOut` written in the state is a deadline for all the attempts together, and `on Cancel` stops a wait as well as an attempt. Its `entry` and `exit` run once, not once per attempt. Leaving the state and entering it again starts counting from zero.
- **Where.** `retry` is allowed on the invoke of a state with no child states (`RB0215`), without `as` (`RB0216`): a retried machine is a new child run each attempt, so a name for it would address nothing during a wait. A retried error that `fail *` handles must not share that outcome with an error that is not retried (`RB0218`): give it an outcome of its own.

### What `retry` compiles to

`retry` is shorthand only. The compiler writes it out as states, a variable and transitions that a chart could contain without it, so the IR has nothing new, the runtimes run it unchanged, and the diagram, the trace and snapshots show the explicit states whichever way the chart was written. The example above compiles to exactly what this compiles to:

```
var chargingAttempt: int = 0

state Charging {
  entry set chargingAttempt = 0
  initial Attempt

  state Attempt {
    invoke chargeCard(order, user) {
      done                     -> Charged   set receipt = result
      fail CardDeclined        -> Declined  set declined = error
      fail GatewayUnavailable {
        if chargingAttempt < 3 -> Backoff   set chargingAttempt = chargingAttempt + 1
        else                   -> GaveUp
      }
    }
  }

  state Backoff {
    after retryDelay(chargingAttempt) -> Attempt
  }
}
```

- **The variable** counts the failed attempts. Its name is the state's path below the machine, joined, with a lower-case first letter, and `Attempt`: `chargingAttempt` for `Charging`, `processingPayingAttempt` for `Processing.Paying`. If a parameter, a variable, an `as` name or another retry's variable already has that name, or the delay uses it, the smallest number from 2 that makes it free is added: `chargingAttempt2`. It starts at 0, is set to 0 by the state's `entry`, before the state's own `entry` effects, and comes after the machine's own variables in the IR, in the order of the states. `attempt` in the delay reads it. Since the name depends on the state's path and on the names around it, renaming the state, or declaring a variable with the counter's name, renames the counter; like any change to a machine, that changes its hash, so snapshots saved before it need a migration.
- **The state** becomes a compound state whose `initial` is `Attempt`. It keeps its `entry`, `exit`, `on`, `after` and `always`. `Attempt` holds the invoke and its outcomes; `Backoff` holds the wait. Without `after` there is no `Backoff`: the retry branch goes `-> Attempt`, which leaves `Attempt` and enters it again, starting the next attempt.
- **The retry branch** comes first in the block of the outcome that handles the error, before the branches written there (which keep their guards), and so is tried first: `fail E { if counter < N -> Backoff  set counter = counter + 1  … }`. When `fail *` handles the retried errors, the branch goes first in the `fail *` block.
- **Targets** of the outcomes are resolved where they were written, in the state, before it gets its children: a sibling of `Charging` named `Backoff` is still what `-> Backoff` in an outcome means.
- **Names in the chart.** The generated states and the variable cannot be named in the chart: `-> Charging.Attempt` is an unknown target and `chargingAttempt` an unknown variable, since neither exists in the source.
- **Warnings.** The retry branches do not count as a way out of the state for `RB0902`, since they stop once the retries run out: an invoke whose outcomes all lack a target has no way out with `retry` as without it, and the warning names the state as written.
- **Sources.** In the IR the variable, `Backoff`, and the retry branches have the span of the `retry` clause, and `Attempt` the span of the invoke. Their docs are empty.

The pattern at the top of the page has `Backoff` as a sibling of `Charging` instead. The shorthand nests both in the state because that is what makes the count start again whenever the state is entered from elsewhere, and what keeps the state's own transitions, such as a deadline or a cancel, in force during the waits. The sibling form stays available, and is what to write when the counter is needed elsewhere, or the wait needs transitions of its own.

**In a trace** a retry is the explicit states at work: on entering `Charging`, `entered Checkout.Charging`, `set chargingAttempt 0`, `entered Checkout.Charging.Attempt` and `invoke.started`; on a retried failure, the `invoke.fail` input, the `transition` from `Checkout.Charging.Attempt` to `Checkout.Charging.Backoff`, `exited` and `set chargingAttempt` 1, `entered Checkout.Charging.Backoff` and `timer.started` with the evaluated delay; when the timer expires, the transition back to `Attempt` and a new `invoke.started` with the next invoke id. The viewer's replay and coverage modes read these as for any chart.

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
- `+`, `-`, `*` and unary `-` take two `int`s or two `float`s and give the same type. `<`, `<=`, `>` and `>=` compare two `int`s, two `float`s or two `duration`s. `==` and `!=` compare two values of the same scalar type (`string`, `int`, `float`, `bool`, `duration`) or the same enum; records, lists, maps, optionals and `json` cannot be compared (see open question 5 on the [roadmap](roadmap.md#open-questions)). `and`, `or` and `not` take `bool`s.
- A `T` can be used where a `T?` is expected, but not the reverse, and a field cannot be read from a `T?`: an optional value goes to a `fn` that handles the absent case. A `T[]` can be used where a `U[]` is expected if a `T` can be used as a `U`, and so can a `map<string, T>` where a `map<string, U>` is, and a `Page<T>` where a `Page<U>` is.
- Any value can be used where `json` is expected.
- An alias is the type it names: `type UserId = string` makes `UserId` and `string` interchangeable. An alias cannot refer to itself, even through `[]`, `?`, `map` or a type argument (`type Tree = map<string, Tree>` is an error; declare a record). A type is optional at most once, aliases expanded and type arguments substituted: `string??` is an error, and so is `S?` after `type S = string?`, and `Box<int?>` after `type Box<T> { value: T? }`.
- An `int` or `duration` literal must be within ±(2^53 − 1) (milliseconds, for a duration), the integers the IR's JSON carries exactly.

The implicit names have these types:

- `event`, in `on E`, has type `E`, and `event.field` the type of the event's field; in `on name.E`, the event `E` that `name`'s machine emits. In `on *` it matches events of different types and cannot be used.
- `result`, in an invoke's `done`, has the return type of the task or machine. If it returns nothing, `result` cannot be used.
- `error`, in an invoke's `fail E`, has type `E`. In `fail *` it has a type only if exactly one declared error is left for `*` to cover.

Outside those transitions the three names are not in scope.

A `ref Machine` is a handle, not data. It can be a parameter of a machine, function, action or task and a `var`, and be passed as an argument to an invoked machine and to host functions, actions and tasks. It cannot be a field of a record, error or event, or what a function, task or machine returns (`RB0316`): a machine gets a handle only from its own signature or from `invoke … as`, so the charts' signatures show who can talk to whom. It has no JSON form, so it cannot be passed where `json` is expected, and `==` and `!=` are not defined on it. `ref A` and `ref B` are different types for different machines; a `ref A` can be used where a `ref A?` is expected, as for any `T`.

There are no built-in functions and no list or map literals, so anything a chart asks of a list or a map goes through a host `fn`. A `json[]` parameter accepts a list of any element type, so one function serves every list: `fn isEmpty(items: json[]) -> bool`, used as `always if isEmpty(problems) -> Accept`. Optionals work the same way: there is no `none` literal and no narrowing, so a chart cannot test for presence and then use the value. When absence changes the flow, make it a declared error of the task that looks the value up (`task findToken(id: string) -> Token ! TokenNotFound`), and build a record whose optional field may be absent in a `fn`.

## Maps and generic types

```
type UserId = string

/// A page of results, of any item type.
type Page<T> { items: T[], next: string? }
type Tree<T> { value: T, children: Tree<T>[] }
type Dict<T> = map<string, T>

fn countByUser(orders: Order[]) -> map<UserId, int>
fn nextPage(after: Page<Order>) -> Page<Order>

machine Report(regions: Dict<Order[]>) -> Page<Order> {
  var page:   Page<Order>
  var counts: map<UserId, int>
  …
  final state Done { output Page { items: page.items, next: page.next } }
}
```

- **`map<K, V>`** maps keys to values of type `V`. Its keys are strings: `K` is `string` or an alias of it (`map<UserId, Order>`), which documents what the keys are (`RB0320` otherwise). Its JSON form is an object (see [values](../../spec/execution.md#values)). A chart has no map literal and no operation on a map: lookups give a value that may be absent, which a chart cannot narrow, so they belong in a `fn`, as list operations do. A map is passed around whole: in variables, parameters, fields and results, and to `fn`s (`fn emptyCounts() -> map<string, int>` gives an initial value). `map<string, ref Inventory>` holds handles where `ref Inventory[]` could.
- **Generic types.** A `type` record or alias may take type parameters, upper-case names in `<>` after its name: `type Page<T> { … }`, `type Pair<A, B> { first: A, second: B }`, `type Dict<T> = map<string, T>`. Enums, errors, events and extern types cannot, and neither can functions, tasks or machines (a `json` parameter already accepts any value). A use gives a type for each parameter, `Page<Order>`; the declaration's fields (or the aliased type) are then those of the declaration with each parameter replaced by its argument, so `page.items` is an `Order[]`. A generic alias is transparent like any alias: `Dict<Order>` is `map<string, Order>`.
- **Record values** of a generic type are written without the arguments, `Page { items: orders, next: cursor }`. The arguments come from the type the value is expected to have, where there is one (a `set`, an argument, a field, `output`), else from its field values. A value whose arguments neither tells (fields that disagree, as in `Pair { first: 1, second: "one" }` for `type Pair<T> { first: T, second: T }`, or the value of `event`) is an error (`RB0324`): use it where its type is known.
- **Rules.** A use gives exactly as many arguments as the type has parameters, and a type that is not generic takes none (`RB0321`). Every parameter is used by its declaration (`RB0322`), and none has the name of a declaration (`RB0107`). A type argument cannot hold a `ref` (`RB0316`), since it may become a field. A generic type can refer to itself, directly or through other types, only with its own parameters in order (`Tree<T>` in `Tree<T>`; `type Nested<T> { inner: Nested<T[]>? }` is `RB0323`), so that it has finitely many instances. Precisely: in the declaration of a generic type, every use of a generic type that refers back to it passes the declaration's own parameters, in order, so types that refer to each other have the same parameters (`type A<T> { b: B<T>? }` with `type B<T> { a: A<T>? }`); a type that is not generic can use any instance.

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

### Many children

`invoke` runs one child per state. A dynamic number of children (one per connection, per order, per player) are **spawned** into a keyed collection declared on the machine, so they stay typed and visible in the chart:

```
machine Hub() {
  event Connected    { id: SessionId, socket: Socket }
  event Disconnected { id: SessionId }
  event Broadcast    { text: string }

  children sessions: Session by SessionId

  initial Serving
  state Serving {
    on Connected       spawn sessions[event.id] = Session(event.socket)
    on Disconnected    stop sessions[event.id]
    on Broadcast       send sessions.*.Notify { text: event.text }
    on sessions.Typing do showTyping(key)
    on sessions.done   do logOut(key)
    on sessions.fail * do report(key, error)
  }
}
```

- **`children name: Machine by Key`** declares a collection. `Key` is `string`, `int` or an enum, or an alias of one. A collection belongs to the machine, not to a state: its children keep running whatever states the machine moves through.
- **`spawn name[key] = Machine(args)`** starts a child under `key`. `Machine` must be the collection's machine, and the arguments are checked as for `invoke`. A child already running under that key is stopped first, so a key always names the child spawned last. Like an invoke, the child starts once the step has settled.
- **`stop name[key]`** stops the child with that key, if there is one: it is cancelled, and its `exit` effects run before anything after the `stop`. Nothing happens if the key has no child.
- **`send name[key].Event { fields }`** sends to one child. It is `undelivered` if no child has that key, including one spawned in the same step, which has not started yet. **`send name.*.Event { fields }`** sends to every running child of the collection.
- **`on name.Event`** is taken for an event any child of the collection emits; `key` is that child's key. **`on name.done`** is taken when a child ends with an output (`result`, `key`), **`on name.fail E`** or **`on name.fail *`** when it fails (`error`, `key`). An outcome nothing takes is dropped: unlike an invoke's, a spawned child's errors need not all be handled.
- **A child's defect** (a host failure, not a declared error) ends the parent with the same defect, as for an invoke.
- **When the machine ends**, finished or cancelled, its remaining children are stopped, after its own `exit` effects. A message those effects send a child is likely never taken, since a stop goes ahead of what the child has queued: to let children finish cleanly, tell them before the machine's last step and wait for their `on name.done`.
- There are no expressions over a collection: a chart cannot count its children or list their keys. Keep a count in a variable, or have the children emit what the parent needs.
- **Names.** A collection's name shares the machine's namespace with its parameters, variables and `as` names. `key` is an implicit name, like `event`, so it cannot name a parameter, variable, `as` name or collection.

The rules at run time are in [`spec/execution.md`](../../spec/execution.md#spawned-children).

## History states

A history state returns to a compound or parallel state where it was when it was last left, instead of starting it over from its `initial`.

```
machine Editor() {
  event Preview
  event Edit
  event Help
  event Back

  initial Working

  state Working {
    initial Editing
    /// Where `Back` returns to.
    history Previous

    state Editing    { on Preview -> Previewing }
    state Previewing { on Edit -> Editing }
    on Help -> Help
  }

  state Help { on Back -> Working.Previous }
}
```

From `Working.Previewing`, `Help` leaves `Working`, and `Back` returns to `Working.Previewing`, not to `Working.Editing`.

- **`history Name`** declares a shallow history state of the state it is written in, its **parent**. It is a pseudo-state: it is never active, has no body, and is only ever a transition's target. Entering it enters the child of its parent that was active when the parent was last left, and that child's default entry (its `initial` chain, or every region).
- **`deep history Name`** restores the whole configuration below its parent: every state that was active below it, down to the atomic and final states, in every region.
- **Before the parent has ever been left**, there is nothing to return to, and entering the history state enters the parent's default entry, as a transition to the parent would: its `initial` chain, or every region of a parallel state.
- **What is recorded** is where the parent was the last time a transition left it. Finishing the run and cancelling it leave states too, but record nothing: the run is over.
- **Where.** In a state with child states, compound or parallel. Not in the machine body, which is never left; not in a final or atomic state (`RB0219`). A history state's name shares the namespace of its sibling states (`RB0108`), and it is targeted like one, `-> Working.Previous`, or `-> Previous` from a sibling. It cannot be the `initial`, and a path cannot go through it (`Working.Previous.Editing`). A state may have one history state of each kind, or several; they record the same exits.
- **Which states a transition leaves** does not depend on what was recorded: it is decided as if the history state were an ordinary child of its parent. So a transition from inside the parent to its own history state (`on Reset -> Previous` in `Working.Previewing`) leaves the states below the parent but not the parent itself, and goes back to where the parent was when it was last left, which is not where it is now. (SCXML computes this from the states the history restores; the difference shows only in such transitions.)
- **Restoring re-enters states.** The states a history state restores are entered like any others: their `entry` effects run, and their `invoke` and `after` timers start again from the beginning, so an invoked machine starts over. Variables are not restored: they keep the values they have.
- **In a parallel state**, a shallow history state restores every region with its default entry, which is what a transition to the parallel state does; a deep one restores each region as it was.
- **Saving.** What each history state recorded is part of a [saved snapshot](../../spec/execution.md#the-saved-snapshot), so a restored run returns to the same states.

The [semantics](../../spec/execution.md#a-microstep) give the precise rules. The viewer draws a history state as a small circle marked `H`, or `H*` for a deep one, inside its parent.

## Sensitive values

A trace shows the values a run handles: its arguments, what it assigns, the events it takes, what its tasks return. A field, parameter or variable whose value must not appear there is marked `sensitive`:

```
type Credentials { user: string, sensitive password: string }

task signIn(login: Credentials, sensitive pin: int) -> Session ! Locked

machine Vault(login: Credentials, sensitive pin: int) -> Profile ! Locked {
  event Retry { sensitive pin: int, note: string }
  sensitive var current: int = pin
  …
}
```

- **Where.** On any field of a record, error, event or emitted event; any parameter of a machine, task, action or function; any `var`. The mark is part of the declaration, so it holds wherever the declaration is used: through aliases, in other files and in other packages.
- **What it does.** Every trace record shows the value of a marked field, parameter or variable as the string `"<redacted>"`, at any depth: a `Credentials` in a list in an optional shows `{"user": "ada", "password": "<redacted>"}` there. A variable marked `sensitive` is shown `"<redacted>"` whole. The records keep their form; only their values change. The rules, which both runtimes follow byte for byte, are in [`spec/trace.md`](../../spec/trace.md#redaction).
- **What it does not.** Nothing but traces changes. The machine, host code and **saved snapshots see and keep the real values**: a restored run goes on with them, so a stored snapshot needs the protection the data needs. Types do not change either, in the generated TypeScript and Rust or anywhere else. On a function's parameter the mark changes nothing, since function calls are not traced; it can still say what the function handles. A marked field does not change a machine's [hash](../../spec/ir.md#the-machine-hash), so marking one later does not invalidate saved snapshots.
- **Keeping it marked.** A runtime redacts a value by its declared type. So a value read from a sensitive place (`password`, `event.pin`, a field of a sensitive variable) can only go to a place that is also sensitive, or that no trace shows: a guard, a function's argument. Assigning it to a variable that is not sensitive, passing it to an action, task or machine parameter that is not, putting it in a field that is not, or making it an `output`, a `fail`, an `after` delay, a retry's delay or a spawned child's key is an error (`RB0317`). So is an operator's result with a sensitive operand: `pin + 0`, `not flag` and `pin == 1234` are sensitive too. A value whose type has sensitive fields cannot go where `json` is expected, since `json` has no fields to redact by (`RB0318`), unless that place is itself sensitive; that includes a map or list of such values, an instance such as `Page<Login>`, and a `Page<Login>` where a `Page<json>` is expected. Redaction follows maps and type arguments: the `password` of every `Login` in a `map<string, Login>` or a `Page<Login>` is redacted. A function's result is not sensitive: a `fn` is how a chart derives what may be shown (`last4(card)`), on purpose.
- **What is never covered.** A host defect's message is whatever host code raised, and the trace shows it as it is: a function that is given a sensitive value should not put it in an exception (`JSON.parse` quotes its input). And a run redacts by the IR it was built from: runs that talk to each other should be built from the same charts, since a mark does not change the hash their handles are checked by.

## Projects

A project is a directory tree of `.road` files. Its root is the nearest directory holding a `roadbook.toml`, searched upward from the first path given to a command (or the current directory), else the current directory. Files are named by their path relative to the root, with `/` separators, in diagnostics and in the IR's `sources`.

Every command works file by file. `roadbook check` compiles each file it is given as an entry, together with everything it imports, and reports each diagnostic once even when several entries import the file it is about. A directory stands for every `.road` file under it, and no path for the whole project. `target`, `node_modules` and `.git` directories are never searched, and links to directories are not followed. `roadbook export ir <file>` compiles one entry.

An import path that starts with `./`, `../` or `/` names a file of the same project, or of the same package for a file in a package: relative to the importing file, or to the root for `/`. Any other path names a file in another package: `"inventory/inventory.road"` is the file `inventory.road` of the package `inventory`, and `"@shop/inventory/charts/inventory.road"` the file `charts/inventory.road` of the scoped package `@shop/inventory`. A package's charts must be shipped in it.

An import never leads out of the project, or out of the package for a file in a package. A path that goes above the root of either is an error (`RB0111`) and nothing is read: from `charts/m.road`, `"/../x.road"` and `"../../x.road"` are refused, while `"../lib/x.road"` is fine. The command line also refuses to read a file, imported or named as an entry, whose real location, with symbolic links resolved, is outside the project root, or outside the package's directory for a file of a package. So a symbolic link in the project to a file elsewhere is not followed, and a relative import in a package whose `exports` gives its files other paths cannot reach above the package's directory; a link to a file inside the project is followed, and a package's directory may itself be a link, as package managers make them. A file named on the command line outside the project root may import files no further above the root than itself.

The command line finds a package, in this order:

1. in the `[packages]` table of `roadbook.toml`, which maps a package's name to its directory;
2. as Node does, in `node_modules/<package>` of the importing file's directory and of each directory above it.

Either way, if the package's directory has a `package.json` with `exports`, the file is where `exports` maps its path, as Node resolves it; without `exports` the path is taken inside the package's directory. The part of Node's rules that is followed:

- Keys are subpaths (`"./*.road": "./charts/*.road"`); a `"."` entry for the main module may sit beside them. `exports` that is a string, an array or an object of conditions names only the main module, so it exports no chart, and a map mixing subpaths and conditions is an error, as in Node.
- An exact key (`"./inventory.road"`) wins. Otherwise, of the keys with one `*` that match, the one whose part before the `*` is longest wins, then the longest key: with `"./*.road": "./charts/*.road"` and `"./legacy/*.road": "./old/*.road"`, `"pk/legacy/x.road"` is `old/x.road`, whatever the order of the keys. Keys ending in `/` (folder mappings, which Node no longer supports) are not followed.
- A target is a path starting with `./` inside the package, where every `*` is replaced by what the key's `*` matched; `null`, which leaves the file unexported; an object of conditions, which resolves to its first member, in the order written, named `roadbook` or `default` (nested objects alike; a `roadbook` condition lets a package export its charts from somewhere else than `default` would); or an array, which resolves to its first entry that does.
- A file the map does not export cannot be imported (`RB0111`, saying so).

A file is one file however it is reached: a file of a `[packages]` package inside the project can be imported both by a relative path and as a file of the package, and packages may import each other, and it is still loaded once (the command line tells files apart by their canonical path on disk). It keeps the name it was first reached by, in load order: the entry, then depth first what it imports, in the order written. That name is the one in the IR's `sources` (a project path, or a package and a path in it), and its own imports are resolved from it.

The Rust macro does not look for files: it receives the owning crate's compiled IR (see [actors and messaging](actors.md#across-crates-and-packages)), and the compiler takes the IR's declarations as it would a parsed file's. In diagnostics a package's file is named by where it is, relative to the project root; in the IR's `sources` by its package and its path in the package. Only errors are reported in another package's charts, never warnings. Declaration names are unique among all the files compiled together, other packages' included.

`roadbook.toml` may be empty. Its keys so far:

| Key | Meaning |
| --- | --- |
| `exclude = ["glob", …]` | Files and directories that a directory search skips, as globs over project paths: `*` matches within one path segment, `**` across segments and `?` one character. A directory that matches is skipped whole. A file named on the command line is checked even if it matches. |
| `gen-exclude = ["glob", …]` | More files and directories, as for `exclude`, that only `roadbook gen` skips: charts that need no generated TypeScript module, such as fixtures read as IR. `roadbook check` and the other commands still see them, and a file named on the command line is generated even if it matches. |
| `allow-unassigned = ["glob", …]` | Files, as globs over project paths, whose machines every command compiles without the [definite assignment](#definite-assignment) check (`RB0403`). Only for fixtures that test a runtime's `unassigned` defect, which a chart can reach no other way. |
| `[packages]` | A table of `name = "directory"`: the directory of each package charts import from, relative to the root. A scoped name is quoted: `"@shop/inventory" = "packages/inventory"`. It comes after the keys above. |

```toml
exclude = ["crates/roadbook-compiler/tests/fail", "crates/roadbook/tests/ui/charts"]
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
- A read of a variable that might not be assigned yet, in an initial value or anywhere in the chart (see [definite assignment](#definite-assignment)).
- A machine that returns `T` with a top-level final state that has no `output` or `fail`, and `output` or `fail` anywhere but a final state at the top level.
- A transition or `invoke` in a final state. A final state ends its parent as soon as it is entered, so they could never run; it holds only `entry`, `exit`, `output` and `fail`.
- A `done` transition in a state with no child states: it waits for a final child, so it could never be taken. (An invoke's `done` goes inside `invoke { }`.)
- A history state in a state with no child states, or in a final state, and an `initial` that names a history state.
- An unguarded cycle of `always` transitions: states each with an `always` whose first branch has no `if` (an `else` counts as guarded, since it runs only when the guards before it fail), each leading to the next, where entering a state also enters its `initial` chain (and every region of a parallel state), and a transition to a state's own ancestor leaves and re-enters it. A transition to a history state counts as entering any state it could restore: every child of its parent with that child's default entry for a shallow one, every state below the parent for a deep one. An unguarded `always` with no target is the shortest such cycle. So is an unguarded `always` written in the machine body, whatever its target, unless that target is a top-level final state: the machine itself is never left, so its `always` would be taken again after every step.
- `spawn`, `stop`, `send name[key]`, `send name.*`, `on name.done` and `on name.fail` through a name that is not a `children` collection, and a `send name.E` to a collection with no `[key]` or `.*`; a `spawn` of a machine other than the collection's; a key of the wrong type; a collection whose key type is not `string`, `int` or an enum, or whose machine is not a machine; `key` as the name of a parameter, variable, `as` or collection; `on name.fail E` for an error the collection's machine does not declare.
- `send`, `emit` and `on name.Event` that do not match a declaration: an unknown or out-of-scope `name`, or one that is not a `ref` (a `ref` that is optional or a list cannot be sent to); an event the receiver does not declare with `event`, or the sender with `emits`; a missing, extra or wrongly typed field. A `send` to an invoked child where it is not running. An `as` on a task, or one that clashes with a variable, parameter or enclosing `as`. A `ref` to something that is not a machine, or in a field or a result.
- A sensitive value going where a trace would show it as it is, and a value with sensitive fields going where `json` is expected (see [sensitive values](#sensitive-values)).
- A machine that invokes itself, directly or through other machines, without an `after` or event in between (unbounded recursion). Precisely: machine `M` *starts* machine `N` if a state of `M` that invokes `N` can be entered from `M`'s initial states by taking only `always`, `done` and invoke outcome transitions, whatever their guards; or if a `spawn` of `N` is in the `entry` of such a state, in the effects of such a transition taken in such a state, or in the `exit` of the state such a transition is written in or of a state around it that the transition leaves (a transition to a top-level final state ends the run, so what it spawns never starts). An `on name.done` or `on name.fail` is an `on`, and counts as waiting: a supervisor that spawns a child again when it fails is fine. Only `on` and `after` transitions count as waiting; an invoke's outcome does not, since a task can finish at once. A transition to a history state counts as entering its parent's default entry: what a history state restores was entered before. A cycle of *starts* is an error.

Warnings:

- States that cannot be reached from the initial state, following every transition whatever its guard. Only the outermost unreachable state is reported. A transition to a history state reaches its parent's default entry, since a history state only restores states that were entered before.
- Non-final states with no way out: an atomic state that is reachable, where neither it nor any state containing it has a transition with a target (a `done` does not count: it needs a final child).
- Events, functions, actions or tasks that are declared but never used: an event no `on` of its machine names, or a function, action or task that nothing in the compilation calls, runs or invokes (a use in a `send` or `emit` counts). Only declarations in the files being checked are reported, and a use from any file checked at the same time counts, so checking a file of shared declarations on its own reports everything only other files use; check them together (`roadbook check` with no paths checks the whole project).
- A state with an `invoke` and no timeout anywhere above it: neither the state nor any state containing it has an `after` (off by default; `roadbook check --warn invoke-timeout`).
- An `emits E` that no `emit E` of its machine sends, and an `on name.E` for an `E` its sender declares but never emits, which can never be taken.
- A `children` collection that no `spawn` fills.
- An `emits E` that no `on name.E` in the checked files receives. As for unused declarations, a receiver in any file checked at the same time counts; host code holding a handle may still receive it.
- A history state that no transition targets.

The checks run in order: names and structure, then types, then the analyses (the cycle and recursion checks, definite assignment and the warnings). A file with syntax errors is checked no further: its declarations are left out, and the names other files import from it are not reported as unknown. The files around it are still checked. The analyses run for each entry file once its compilation (the file and everything it imports) has no other errors, so one mistake does not produce a cascade of diagnostics, and an error in one chart does not turn off the analyses of another.

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

A `var` with no initial value and a type that is not optional starts unassigned. Every other variable always has a value: a parameter, a variable with an initial value, and an optional variable with none, which starts as `null`. The compiler proves that every read of a variable happens after the variable has a value, wherever the read is: a guard, a `set` value, the arguments of a `do`, the value of a `send` or `emit` and the `ref` variable a `send` goes through, the arguments of an `invoke`, an `after` delay, `output` and `fail`, and `entry` and `exit` effects. A read it cannot prove is an error (`RB0403`) at the read, with a label on a way in that leaves the variable unassigned. An `on name.Event` through a `ref` variable is not a read: an unassigned variable names no sender.

The analysis is a forward dataflow over the chart. A variable is assigned when a state is entered if it is assigned on every way into the state. A way in is the start of the machine or a transition with a target, and along it, in the order a step runs them, these assign:

- the source state, if the variable is assigned whenever the source is active (see below);
- the `exit` of the source and of each state containing it that the transition leaves;
- the transition's `set`s;
- the `entry` of each state entered before this one in the same step: the states containing it, outermost first, and the regions of a parallel state before its own, in document order.

So a `set` in a transition is seen by its target's `entry`, by the `entry` of the target's children and by the target's invoke. Within one `entry`, `exit` or transition the `set`s run first, in order, so a `set` sees the `set`s before it and a `do`, `send` or `emit` sees them all.

What each read sees:

- An `entry` sees what is assigned on every way into its state, and its own `set`s as above. `output` and `fail` see that and everything the final state's `entry` assigns.
- A guard and the effects of a transition, the state's invoke arguments and `after` delays, and its `exit` run while the state is active, and see what is assigned whenever it is: for an atomic state, what its `entry` left; for a compound state, what is assigned in the state itself or in every child, since it is always in one of them; for a parallel state, what is assigned in the state itself or in any region, since it is in all of them. A transition's effects also see the `exit`s that run before them.
- A `done` sees what is assigned in each final child it can complete through, and for a parallel state what is assigned in any region once every region has reached a final state.
- A variable is never unassigned again, so the `always` and `done` transitions taken while a step settles, and the invokes and timers started after it, see at least this much.
- An initial value sees the parameters and the variables declared before it that have an initial value or an optional type: initial values are computed in the order the variables are declared.

The analysis is conservative: it may reject a chart that never reads an unassigned variable when it runs. Where:

- Guards are not evaluated. Every transition whose state can be active is assumed to be taken, so a variable assigned only on a branch whose guard happens to hold is not assigned after the others.
- A transition with no target assigns only for its own effects: the state can be left before it is taken, so the state's other transitions and its `exit` cannot rely on it.
- An `exit`, an invoke's arguments and an `after` delay see what was assigned when their state was entered, not what a transition taken inside it assigned later.
- Inside a parallel state, a region relies on what was assigned before it was entered (by the states containing it and by the regions before it, in order) and on its own transitions. What another region assigns after that does not count, since the regions take their transitions independently. The transitions written on the parallel state itself and its `done` see every region.
- Of the states a transition leaves, only the `exit`s of its source and the states containing it count, since which of the source's children is active is not known.
- States that can never be entered are not checked; they are already warned about (`RB0901`).
- A transition to a history state is a way into its parent's default entry, as if it targeted the parent (the parent itself is entered only if the transition leaves it). The states a history state restores add no way in: each was entered before, and a variable assigned then is still assigned.

It is fast: each state's facts only shrink, a state's transitions are looked at again only when they do, and the work grows with the size of the chart times the number of variables that start unassigned.

In `PlaceOrder`, `order` is assigned on the only way into `Pricing`, so `invoke priceOrder(order, user)` is accepted. Change `Validating`'s `done` branch to drop `set order = result` and the compiler points at the read in `Pricing`:

```
error[RB0403]: `order` might not be assigned yet when this is evaluated
  --> examples/shop/place_order.road:47:25
   |
40 |         done                  -> Pricing
   |         -------------------------------- this enters `PlaceOrder.Processing.Pricing` without assigning `order`
...
47 |       invoke priceOrder(order, user) {
   |                         ^^^^^
   |
   = help: assign `order` on every way into `PlaceOrder.Processing.Pricing`, give it an initial value, or make its type optional
```

This is what lets host functions take plain non-optional arguments. In xstate the same machine would declare `order: Order | undefined` and every reader would assert it is present.

Both runtimes keep reading an unassigned variable as a runtime error (`unassigned`, see [`spec/execution.md`](../../spec/execution.md#defects)), as a backstop: IR written by hand or by another tool, or a chart compiled with `allow-unassigned`, can still do it, and it ends the run with a defect rather than with an undefined value.

## Formatting

`roadbook fmt` is canonical: one accepted layout, no options. Agents and humans produce the same text, so diffs show only real changes.

The layout, as implemented in `roadbook-syntax`. The example files and the charts on these pages are written in it.

- **Lines.** Two-space indentation, one item per line, and no line wrapping. A file with syntax errors is not formatted.
- **Spacing.** One space around binary operators, `->`, `=`, `!` and `|`, after `:` and `,`, and inside the braces of a one-line list or record value; none inside parentheses or the angle brackets of type arguments (`map<string, int>`, `type Page<T>`). A `retry` clause is written `retry 3 on A | B after delay`, one space between its parts. In a transition the target and each `set`, `do`, `send` or `emit` clause are separated by two spaces: `-> Paying  set attempts = 0  do audit(order)`. Every `set` comes first, then the other clauses in the order written; consecutive clauses of one kind are merged for `set` and `do`: `do f()  set a = 1  set b = 2` is written `set a = 1, b = 2  do f()`. Each `send`, `emit`, `spawn` and `stop` is a clause of its own, and an empty body is dropped: `send upload.Pause {}` is written `send upload.Pause`. No space goes inside the brackets of a key: `sessions[event.id]`, `sessions.*`.
- **Blank lines** between items are kept, at most one. There are none at the start or end of a block or file.
- **Lists.** No trailing commas. A list in a declaration (import names, fields, enum variants, parameters) with a line break right after its opening bracket stays multi-line: its elements are indented one level, keep the author's line breaks between them, and the closing bracket goes on its own line. Every other list is printed on one line.
- **One-line states.** A state whose body is empty, or is a single item other than a state, an `invoke` or a block of branches, and contains no comment, is written on one line: `final state Charged { output receipt }`. Every other state is written as a block. A history state is written on a line of its own: `history Previous`, `deep history Previous`.
- **Expressions** have only the parentheses precedence needs, plus `not (…)` around a comparison or arithmetic operand, and parentheses around a record value used as an `after` delay or a retry's delay. Numbers are written as parsed (`007` becomes `7`, `1.50` becomes `1.5`). `error E {}` is written `error E`, and a record value with no fields `E {}` is written `E`.
- **Comments** are kept as written, trailing spaces removed. A comment on its own line stays on its own line, indented like the code that follows it. A comment after code stays at the end of that line, including one after the closing bracket of a multi-line list, which ends the line that bracket starts (`) { // c`, `} from "./x.road" // c`). A comment inside something printed on one line moves to its own line before it; for the line a multi-line list's closing bracket starts (`) -> T ! E {`), that is into the list, before the bracket. Comments after that line belong to what follows it.
- **Alignment.** An alignment group is a run of consecutive lines of one kind with no blank line or comment line between them. The kinds and what they align:
  - one-line `type`, `enum`, `error` and `extern type` declarations: what follows the name and its type parameters (`{` or `=`);
  - one-line `fn`, `action` and `task` declarations: `->` and `!`;
  - `var` lines: the type;
  - `children` lines: the machine, and `by`;
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
| `RB0014` | `done`, `invoke`, `output`, `fail` or a history state directly in a machine body. | `machine M() { done -> A }`, `machine M() { history H }` |
| `RB0015` | `var` or `event` inside a state. | `state A { var x: int }` |
| `RB0016` | A machine item at the top level of a file. | `state A {}` outside a machine |
| `RB0017` | A transition with no target, `set` or `do`. | `on Go` |
| `RB0018` | A record value as an `after` delay, or a retry's delay, without parentheses. | `after Cfg { ms: 5 } -> A` |
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
| `RB0107` | Two declarations with the same name in one compilation, or an event or type parameter named like a type, error or machine. | `type Order` in two files, `event Order`, `type Page<Order>` |
| `RB0108` | Two fields, parameters, type parameters, variants, variables, events or sibling states with the same name, or a name imported twice. | `state A {}` twice in one parent, `import { B, B }` |
| `RB0109` | A name used as something it is not. | `invoke audit(x)` where `audit` is an action |
| `RB0110` | `event`, `result`, `error` or `key` where it is not in scope. | `entry set n = result` |
| `RB0111` | An import of a file that cannot be read. | `import { A } from "./missing.road"` |
| `RB0112` | An import of a name the file does not declare. | `import { Ordr } from "./types.road"` |
| `RB0113` | An enum variant that does not exist. | `Role.Admn` |
| `RB0114` | A `set` of a machine parameter. | `set token = ""` |
| `RB0115` | A `send name.E` or `on name.E` whose `name` is not an `invoke … as name` in scope or a parameter or variable. | `send uplod.Pause`, `upload` outside its invoking state |
| `RB0116` | A `send` of an event the receiving machine does not declare with `event`. | `send upload.Stop` |
| `RB0117` | An `emit` of an event the machine does not declare with `emits`, or an `on name.E` of one its sender does not. | `emit Progres`, `on upload.Pause` |
| `RB0118` | `attempt` in a retry delay of a machine with a parameter or variable named `attempt`. | `retry 3 after backoff(attempt)` with `var attempt: int` |
| `RB0119` | A `spawn`, `stop`, `send name[key]`, `send name.*`, `on name.done` or `on name.fail` through a name that is not a `children` collection, or a `send name.E` to a collection without a key or `*`. | `stop upload[1]`, `send sessions.Notify` |
| `RB0120` | `key` as the name of a machine parameter, variable, `as` name or collection. | `var key: string` |
| `RB0201` | A transition target that does not resolve, or a path through a history state. | `-> Dne`, `-> Working.Previous.Editing` |
| `RB0202` | A compound state or machine without `initial`, or a machine with no states. | `state A { state B {} }`, `machine M() { initial A }` |
| `RB0203` | An `initial` that is not a child state, is written twice, or is in a state without children. | `initial Strat`, `initial Previous` for `history Previous` |
| `RB0204` | An `invoke` that leaves a declared error unhandled. | see above |
| `RB0205` | The same trigger twice in one state or one `invoke`. | `on Go -> A` and `on Go -> B` |
| `RB0206` | A second `invoke` in a state. | two `invoke`s in `state A` |
| `RB0207` | An outcome, or a retry, for an error the task or machine does not declare. | `fail Timeout -> A`, `retry 3 on Timeout` |
| `RB0208` | `output` or `fail` outside a top-level final state, or two of them. | `state A { output 1 }` |
| `RB0209` | A top-level final state with no `output` or `fail` in a machine that returns a value. | `final state Done {}` in `machine M() -> int` |
| `RB0210` | A final state with children, or a parallel state with none. | `final state A { state B {} }` |
| `RB0211` | A transition or `invoke` in a final state. | `final state Done { on Go -> A }` |
| `RB0212` | A `done` transition in a state with no child states, which can never be taken. | `state A { done -> B }` |
| `RB0213` | A `send` to an invoked child where it is not running. | `entry send upload.Pause` in the invoking state, `on Cancel -> Idle  send upload.Stop` |
| `RB0214` | `as` on an invoke of a task. | `invoke fetch() as f` |
| `RB0215` | `retry` on the invoke of a state with child states. | `state A { invoke f() retry 2 { … }  initial B  state B {} }` |
| `RB0216` | `retry` on an invoke with `as`. | `invoke Upload(file) as upload retry 2` |
| `RB0217` | A `retry` that retries nothing, or names an error twice. | `retry 0`, `retry 2` of a task that declares no errors, `retry 2 on Busy \| Busy` |
| `RB0218` | A retried error handled by a `fail *` that also handles an error not retried. | `retry 2 on Busy` with only `fail * -> Failed` for `! Busy \| Gone` |
| `RB0219` | A history state in a state with no child states, or in a final state. | `state A { history H }` |
| `RB0220` | A `spawn` of a machine that is not its collection's. | `spawn sessions[id] = Upload(f)` for `children sessions: Session by string` |
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
| `RB0316` | A `ref` in a field of a record, error or event, in what a function, task or machine returns, or in a type argument. | `type Holder { peer: ref Peer }`, `Box<ref Peer>` |
| `RB0317` | A value read from a sensitive field, parameter or variable (or computed from one by an operator), going where a trace shows it: a variable, a parameter of an action, task or machine, or a field that is not `sensitive`, or an `output`, a `fail`, an `after` delay or a retry's delay. | `set note = event.password` for `var note: string` |
| `RB0318` | A value whose type has `sensitive` fields, going where `json` is expected in a place a trace shows. | `do log(creds)` for `action log(what: json)` |
| `RB0319` | A `children` key type that is not `string`, `int` or an enum (or an alias of one). | `children s: Session by float` |
| `RB0320` | A `map` whose key type is not `string` or an alias of it. | `map<int, Order>` |
| `RB0321` | A generic type without its type arguments or with the wrong number of them, or type arguments on a type that takes none. | `page: Page`, `Order<int>` |
| `RB0322` | A type parameter its declaration does not use. | `type Id<T> = string` |
| `RB0323` | A generic type that refers to itself, directly or through other types, with other arguments than its own parameters. | `type Nested<T> { inner: Nested<T[]>? }` |
| `RB0324` | A record value of a generic type whose type arguments neither the expected type nor its fields tell. | `describe(Pair { first: 1, second: "one" })` |
| `RB0401` | An unguarded cycle of `always` transitions. | `A { always -> B }`, `B { always -> A }` |
| `RB0402` | A machine that invokes or spawns itself without waiting. | `state A { invoke M() { … } }` as `M`'s initial state |
| `RB0403` | A read of a variable that might not be assigned yet. | `entry do log(n)` in the initial state, for `var n: int` |
| `RB0901` | A state that can never be entered (warning). | a state nothing targets |
| `RB0902` | A non-final state with no way out (warning). | `state Stuck {}` |
| `RB0903` | An event, function, action or task nothing uses (warning). | `event Unused` |
| `RB0904` | An `invoke` with no `after` around it (warning, off by default). | `state A { invoke fetch() { … } }` |
| `RB0905` | An `emits` the machine never emits (warning). | `emits Progress` with no `emit Progress` |
| `RB0906` | An `on name.E` for an event its sender never emits (warning). | `on upload.Progress` when `Upload` has no `emit Progress` |
| `RB0907` | An `emits` that nothing in the checked files receives (warning). | `emits Progress` with no `on upload.Progress` anywhere |
| `RB0908` | A history state that no transition targets (warning). | `history Previous` with no `-> Working.Previous` |
| `RB0909` | A `children` collection that no `spawn` fills (warning). | `children sessions: Session by string` with no `spawn sessions[…]` |

## Grammar

```ebnf
file        = { import | decl } ;
import      = "import" "{" name { "," name } "}" "from" string ;
decl        = typeDecl | enumDecl | errorDecl | externDecl
            | fnDecl | actionDecl | taskDecl | machineDecl ;

externDecl  = "extern" "type" Upper ;
typeDecl    = "type" Upper [ typeParams ] ( "=" type | fields ) ;
typeParams  = "<" Upper { "," Upper } ">" ;
enumDecl    = "enum" Upper "{" Upper { "," Upper } "}" ;
errorDecl   = "error" Upper [ fields ] ;
fields      = "{" [ field { "," field } ] "}" ;
field       = [ "sensitive" ] lower ":" type ;
type        = ( Upper [ typeArgs ] | "string" | "int" | "float" | "bool" | "duration" | "json"
              | "ref" Upper | "map" "<" type "," type ">" )
              { "[]" | "?" } ;
typeArgs    = "<" type { "," type } ">" ;

fnDecl      = "fn" lower params "->" type ;
actionDecl  = "action" lower params ;
taskDecl    = "task" lower params [ "->" type ] [ errors ] ;
params      = "(" [ field { "," field } ] ")" ;
errors      = "!" Upper { "|" Upper } ;

machineDecl = "machine" Upper params [ "->" type ] [ errors ] "{" { machineItem } "}" ;
machineItem = varDecl | eventDecl | emitsDecl | childrenDecl | commonItem ;
varDecl     = [ "sensitive" ] "var" lower ":" type [ "=" expr ] ;
eventDecl   = "event" Upper [ fields ] ;
emitsDecl   = "emits" Upper [ fields ] ;
childrenDecl = "children" lower ":" Upper "by" type ;

stateDecl   = [ "final" | "parallel" ] "state" Upper "{" { stateItem } "}" ;
stateItem   = commonItem
            | [ "deep" ] "history" Upper
            | "done" branches
            | "invoke" name "(" [ expr { "," expr } ] ")" [ "as" lower ] [ retry ]
              "{" outcome { outcome } "}"
            | "output" expr
            | "fail" expr ;
commonItem  = "initial" Upper
            | stateDecl
            | "entry" effects
            | "exit" effects
            | "on" ( Upper | "*" | lower "." ( Upper | "done" | "fail" ( Upper | "*" ) ) ) branches
            | "after" expr branches
            | "always" branches ;
retry       = "retry" int [ "on" Upper { "|" Upper } ] [ "after" expr ] ;
outcome     = "done" branches
            | "fail" ( Upper | "*" ) branches ;

branches    = [ "if" expr ] action
            | "{" "if" expr action { "if" expr action } [ "else" action ] "}" ;
action      = "->" target [ effects ]
            | effects ;
effects     = effect { effect } ;
effect      = "set" assign { "," assign }
            | "do" call { "," call }
            | "send" lower [ "[" expr "]" | "." "*" ] "." Upper [ recordBody ]
            | "emit" Upper [ recordBody ]
            | "spawn" lower "[" expr "]" "=" Upper "(" [ expr { "," expr } ] ")"
            | "stop" lower "[" expr "]" ;
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
- All keywords are reserved: `import type enum error extern fn action task machine var event emits initial state final parallel entry exit on after always done invoke output fail if else set do send emit and or not true false`. `from`, `as`, `retry`, `ref`, `map`, `sensitive`, `history`, `deep`, `children`, `by`, `spawn`, `stop` and the built-in type names are not reserved, because they only have a meaning in a position where no name can appear: after an import list, after an invoke's arguments (`as` and `retry`), at the start of a type, and before a field or parameter name and its `:`, or before `var` (so `sensitive: bool` is a field named `sensitive`), at the start of an item of a state body (`history`, `deep`), in `children name:` at the start of a machine item, after a collection's machine, and in `spawn name[` and `stop name[` at the start of an effect (so `action stop()` is still a valid action). `event`, `error` and `result` are the implicit names inside transitions and cannot be used for variables or parameters; nor can `key`, though it can name a field.
- In a type, the `>` closing type arguments may be written as the first character of a `>=`: `var m: map<string, int>= f()` reads as written (the formatter writes `> =` with a space).
- A `///` comment documents the declaration, machine, state, history state, event or variable that follows it. A `///` anywhere else is an error: use `//`.
- A single-line transition and a multi-branch block are told apart by the `{` after the trigger. For that reason a record value in an `after` delay must be parenthesised, as with `if` in Rust, and so must one in a retry's delay, which the invoke's `{` follows. A `{` after the event of a `send` or `emit` is always its record body.
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
