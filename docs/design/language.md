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
- **Transitions** read `trigger [if guard] -> Target [set ...] [do ...]`. `-> .Child` keeps the state it is written in, and `-> A, B` enters two regions of a parallel state at once. Several guarded branches for one trigger go in a block and are tried in order.
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
| `task chargeCard(order: Order) -> Receipt ! CardDeclined` | Effectful, fallible, cancellable work. Only run by `invoke` or `spawn`. | Host |
| `task upload(file: File) -> Url emits Progress { percent: int }` | A task that emits events while it runs (see [tasks that emit](#tasks-that-emit)). | Host |
| `machine Checkout(...) -> Receipt ! ... { }` | A statechart with a task's signature. | The chart |

Built-in types are `string`, `int` (64-bit signed), `float`, `bool`, `duration` and `json` (any JSON value). `T?` is optional, `T[]` is a list and `map<string, T>` a map from strings to `T`. `ref Machine` is a handle to a running machine (see [messages between machines](#messages-between-machines)). A record or alias may take type parameters, `Page<Order>` (see [maps and generic types](#maps-and-generic-types)).

An alias of an error type is an ordinary alias: it can be used wherever a type goes. Error lists (`! CardDeclined | Gone`) and `fail E` outcomes name the errors themselves, not aliases of them, since an error is matched by its name.

Names starting with an upper-case letter are types, errors, events, states and machines. Names starting with a lower-case letter are variables, fields, functions, actions and tasks. The grammar enforces this, which keeps `invoke chargeCard(...)` (a task) and `invoke Checkout(...)` (a machine) visibly different.

Host functions receive exactly the arguments the chart passes them. They never see the machine's variables or the current event. This is the main difference from xstate, where every guard and action receives the whole context: here the chart is the complete wiring diagram of both control and data, and the host code is a set of small typed functions that know nothing about the machine that calls them.

## Inside a machine

| Item | Meaning |
| --- | --- |
| `var name: T [= expr]` | A machine variable. `sensitive var` keeps its value out of traces (see [sensitive values](#sensitive-values)). Written in a state, it is the state's own (see [data local to a state](#data-local-to-a-state)). |
| `event Name [{ fields }]` | An event the machine accepts from outside. |
| `internal event Name [{ fields }]` | An event only the machine itself produces, with `raise`: host code and other machines cannot send it (see [events a machine raises](#events-a-machine-raises)). |
| `emits Name [{ fields }]` | An event the machine emits, to whoever invoked or spawned it and every machine holding a reference to it, of those that have an `on` for it. |
| `call name(params) [-> T] [! E]` | A request the machine answers for any machine holding a reference to it (see [calls between machines](#calls-between-machines)). |
| `children name: Machine by Key`, `children name: task by Key` | A keyed collection of child machines, or of tasks, started with `spawn` (see [many children](#many-children)). |
| `initial Name` | The child state entered first. Required, once, in a state that has children, except a `parallel state`, which has none: all its children are entered. |
| `state Name { ... }` | A state. Atomic if it has no children, compound if it does. `state Name(params) { ... }` takes parameters, which every transition into it passes: `-> Name(args)` (see [data local to a state](#data-local-to-a-state)). |
| `parallel state Name { ... }` | All children are active at once (regions). A region may be any kind of state. |
| `final state Name { ... }` | Ends the enclosing compound state, or the machine at the top level. |
| `history Name`, `deep history Name` | In a state with children: a [history state](#history-states), a target that returns to where the state was when it was last left. |
| `check property` | Only directly in a machine body: an assertion about the machine's states, `never A and B`, `reachable A`, `A only after B` or `can finish`, for `roadbook check` to verify (see [checking properties](#checking-properties)). A state path is written from the machine body, like a target at machine level, and may name any state but a history state. Checks are not part of the IR. |

## Inside a state

| Item | Meaning |
| --- | --- |
| `var name: T [= expr]` | A state variable: it gets its initial value each time the state is entered and is gone when the state is left, and only the state and the states inside it can read it (see [data local to a state](#data-local-to-a-state)). |
| `entry set ... ` / `entry do ...` | Run when the state is entered. |
| `exit set ...` / `exit do ...` | Run when the state is left. |
| `on Event ...` | Transition on one of the machine's own events: an external one, or one the machine [raises](#events-a-machine-raises). `on *` matches any of the machine's own events, internal ones included, that this state has no `on` for. |
| `on name.Event ...` | Transition on an event emitted by the machine or task `name` addresses: an invoked child or task (`invoke … as name`), a `ref` parameter or variable, or any child of a `children` collection. |
| `on name.done ...` / `on name.fail Err ...` | Transition when a spawned child of the collection `name` ends (see [many children](#many-children)). |
| `on name.ended ...` | Transition when the run a `ref` parameter or variable `name` holds ends (see [when a peer ends](#when-a-peer-ends)). |
| `on Event ignore` | Take the event and do nothing. See [strict machines](#strict-machines). |
| `on name ...` | Transition taking a call of the machine's own `call name`, from another machine. |
| `after 30s ...` | Transition after the state has been active for a duration. The delay is an expression. |
| `always ...` | Transition taken as soon as its guard holds (no event). |
| `done ...` | Transition taken when a compound state reaches a final child, or every region of a parallel state does. |
| `invoke name(args) [as name] [{ done ...  fail Err ... }]` | Run a task or machine while the state is active. At most one per state. `fail *` matches the declared errors not listed. `as` names an invoked machine, so that the state can send to it and receive what it emits, or a task that declares `emits`, so that the state can receive what it emits ([tasks that emit](#tasks-that-emit)). The block of outcomes may be left out when there is nothing it must handle. `invoke x.name(args)` calls the call `name` of the machine the `ref` `x` holds. |
| `invoke name(args) retry N [on Err] [after delay] { ... }` | The same, run again when it fails, up to `N` more times (see [retrying an invoke](#retrying-an-invoke)). |
| `output expr` / `fail expr` | Final states at the top level only: the machine's result. |

A transition has up to four parts, all optional except that it must do something (or, for an `on`, say `ignore`):

```
on Submit if isValid(order) -> Paying  set attempts = 0  do audit("submitted", order)
   ^trigger ^guard             ^target ^assignments      ^actions
```

With no `-> Target` the transition has no target: the state is not left and re-entered. The effects are `set` assignments, `do` actions, the messages `send name.Event { … }` and `emit Event { … }` (see [messages between machines](#messages-between-machines)), `reply name done [value]` and `reply name fail error`, which answer a call (see [calls between machines](#calls-between-machines)), `spawn` and `stop` (see [many children](#many-children)), and `raise` and `cancel`, which give the machine one of its own events, at once or after a delay (see [events a machine raises](#events-a-machine-raises)). `set` assignments run first: every `set`, in the order written, then the `do`, `send`, `emit`, `reply`, `spawn`, `stop`, `raise` and `cancel` effects, in the order written among themselves, wherever they are written, so an action or a message sees the values its transition's `set`s assign. The same goes for `entry` and `exit`. The formatter writes every `set` clause before the others, so the source shows the order they run in.

Inside a transition, these names are in scope besides the machine's parameters and variables and the [locals](#data-local-to-a-state) of its state and the states around it: `event` (in `on`, also `on name.Event`, `on name` and `on name.ended`), `result` (in an invoke's `done` and in `on name.done`), `error` (in an invoke's `fail` and in `on name.fail`), `call` (in `on name`, the call being taken) and `key` (in `on name.Event`, `on name.done` and `on name.fail` through a `children` collection).

A target is resolved like a name in a block-structured language: first among the siblings of the state the transition is written in, then among its parent's siblings, and so on outward. A dotted path such as `Processing.Paying` then descends into a nested state, which is also how a state targets one of its own children. A transition written at machine level resolves among the top-level states. A state with parameters takes its arguments in the path, after its name: `-> Confirming(token)`, `-> Editing(doc).Changing` ([data local to a state](#data-local-to-a-state)). An unknown target is a compile error, and so is a duplicate state name among siblings.

**A target with a leading dot keeps the source.** `-> .Browsing` resolves `Browsing` among the children of the state the transition is written in, and the transition is **internal**: that state is not left, so its `entry` and `exit` do not run and its `invoke` and `after` timers keep going; only the states below it change.

```
state Session {
  invoke keepAlive() {
    done -> LoggedOut
  }
  after 30m  -> TimedOut
  on Home    -> .Browsing // Session stays: the invoke and the timer keep running
  on Restart -> Session // Session is left and entered again: both restart
  initial Browsing
  state Browsing { on Open -> Cart }
  state Cart {
    on Review -> .Reviewing // Cart's own children
    initial Editing
    state Editing   {}
    state Reviewing {}
  }
}
```

- `-> .Cart.Reviewing` goes deeper. The state the dotted target names is still entered again if it is active: `on Home -> .Browsing` taken in `Browsing` restarts `Browsing`, not `Session`.
- **In a parallel state** an internal transition exits and enters only the regions its targets are in, and the other regions keep going: `on Mute -> .Volume.Muted` written in `parallel state Player` leaves `Playback` where it is. It is the way to reset one region: `-> .Volume` enters `Volume` again by its `initial`. An internal transition written in a region, `-> .Muted` in `Volume`, stays inside the region, so another region's transition for the same event is taken too.
- **In the machine body** `-> .Idle` means the same as `-> Idle`, since the machine itself is never left; both compile to the same IR.
- A history state is a child like any other: `on Undo -> .Previous` in `Working` restores what `Working` was in when it was last left, without leaving it.
- In a state with a [`retry`](#retrying-an-invoke) the dot names nothing: such a state has no children of its own, and the states the retry writes out cannot be named.

**Several targets** are written with commas, `-> Player.Playback.Paused, Player.Volume.Muted`: the transition enters the parallel state with each target in its region, and every other region by its `initial`. Any two of the targets must be in different regions of a parallel state, at any depth: two in one compound state, or one inside another, is an error (`RB0222`). The dot belongs to the transition, so either every target has it or none does (`RB0221`): `on Sleep -> .Playback.Paused, .Volume.Muted` written in `Player`.

`entry`, `exit`, `on`, `after` and `always` may also be written directly in the machine body, where they apply to the whole machine.

### Which transition is taken

The precise rules are in the [semantics](semantics.md#execution-model) and [`spec/execution.md`](../../spec/execution.md); these are the ones a chart most often depends on.

- **Innermost first.** An event is offered to the active states from the innermost outward. The first state that has an `on` for it tries that `on`'s branches in order, and the first whose guard holds is taken. If none holds, or the state has no `on` for the event, its parent is tried, and so on up to the machine body. So when a state and its ancestor both have `on Resume`, the state's wins whenever its guard holds. A state's `on *` is tried only if that state has no `on` for the event at all. An invoke's outcome is offered only to the state that invokes, and an `after` only to the state it is written in.
- **A transition with a target is external, unless the target has a dot.** It leaves its source state and enters its target, even when the target is the source itself or one of its children: `-> Self` written in `Self`, or `-> Parent.Child` written in `Parent`, exits `Self` (or `Parent`) and enters it again, running `exit` and `entry` and restarting its `invoke` and its `after` timers. `-> .Child` written in `Parent` does not: it is internal, and only `Parent`'s active children are left. The machine body is the one place where both are the same: a transition written there does not leave the machine, so the machine's own `entry`, `exit` and timers are left alone.
- **A transition between the regions of a parallel state leaves the parallel state.** `-> P.B.Y` written in region `A` of `P` exits `P` and enters it again, so every region but `B` starts over by its `initial` (warning `RB0913`). To change one region and keep the others, write the transition in that region or in `P` with a dot, or list a target in every region.
- **A transition with no target** leaves and enters nothing, and nothing is restarted: only its effects run. An `after` timer starts when its state is entered and is used up when it fires, so an `after` with no target fires once per entry into its state.
- **The order of a step.** States are exited innermost first, then the transition's effects run, then states are entered outermost first, each running its `entry`. Only then are `always` guards evaluated, and `done` transitions and the events the step [raised](#events-a-machine-raises) taken, until none applies: an `always` sees what the `entry` effects assigned, and is taken before a `done` or a raised event. Invokes and timers start last, for the states still active.
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
- **Which errors.** The errors listed after `on`, each one the task, machine or [call](#calls-between-machines) declares; without `on`, every error it declares. Nothing else is retried: not `done`, and not a host defect (a thrown exception, a panic), which is not flow and fails the run as it does without `retry`. Nor is a call's built-in `Unanswered`, with `on` or without: it is not a declared error, so it cannot be listed (`RB0207`), and it keeps its own `fail Unanswered` outcome, which a `fail *` that handles a retried error cannot be (`RB0218`). A callee that is not running or has ended would not answer the next attempt either; one that may be busy should answer with a declared error.
- **When the retries run out.** A failure that is not retried, an error not in the list or the failure after the last retry, goes to its outcome as it would without `retry`, with `error` the last failure. Every declared error still needs an outcome (`RB0204`): it says what happens when retrying did not help.
- **Delays.** `after delay` is how long to wait between a failure and the next attempt; without it, the next attempt starts at once. The delay is a `duration` expression, evaluated when the wait starts, like an `after` transition's. In it, `attempt` is the number of attempts that have failed so far, an `int`: 1 before the first retry, 2 before the second. So `after 2s` waits the same each time and `after retryDelay(attempt)` lets a host function grow the delay (doubling, a cap, jitter): the language has no arithmetic on durations, so backoff is the function's choice. `attempt` is in scope only in a retry's delay, and it is the only implicit name there: the delay cannot read `error`, so a wait chosen by the failure (a server's retry-after) needs the explicit form. `attempt` is not reserved, so a machine may have a parameter or variable named `attempt`, and a state a local of that name, but then it cannot use `attempt` in a retry delay where that name is in scope (`RB0120`).
- **The state stays active.** While it retries, the invoking state is still active, through every attempt and every wait. Its own `on`, `after` and `always` transitions apply all the time, so `after 30s -> TimedOut` written in the state is a deadline for all the attempts together, and `on Cancel` stops a wait as well as an attempt. Its `entry` and `exit` run once, not once per attempt. Leaving the state and entering it again starts counting from zero.
- **Locals.** In a state with [parameters or variables of its own](#data-local-to-a-state), they live through every attempt and every wait, since the state stays active, and the invoke's arguments and the delay can read them. They are reset when the state is entered again, as the count is.
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
- `+`, `-`, `*` and unary `-` take two `int`s or two `float`s and give the same type. `<`, `<=`, `>` and `>=` compare two `int`s, two `float`s or two `duration`s. `==` and `!=` compare two values of the same scalar type (`string`, `int`, `float`, `bool`, `duration`) or the same enum; records, lists, maps, optionals and `json` cannot be compared ([equality](#equality)). `and`, `or` and `not` take `bool`s.
- A `T` can be used where a `T?` is expected, but not the reverse, and a field cannot be read from a `T?`: an optional value goes to a `fn` that handles the absent case. A `T[]` can be used where a `U[]` is expected if a `T` can be used as a `U`, and so can a `map<string, T>` where a `map<string, U>` is, and a `Page<T>` where a `Page<U>` is.
- Any value can be used where `json` is expected.
- An alias is the type it names: `type UserId = string` makes `UserId` and `string` interchangeable. An alias cannot refer to itself, even through `[]`, `?`, `map` or a type argument (`type Tree = map<string, Tree>` is an error; declare a record). A type is optional at most once, aliases expanded and type arguments substituted: `string??` is an error, and so is `S?` after `type S = string?`, and `Box<int?>` after `type Box<T> { value: T? }`.
- An `int` or `duration` literal must be within ±(2^53 − 1) (milliseconds, for a duration), the integers the IR's JSON carries exactly.

The implicit names have these types:

- `event`, in `on E`, has type `E`, and `event.field` the type of the event's field; in `on name.E`, the event `E` that `name`'s machine or task emits. In `on *` it matches events of different types and cannot be used.
- `result`, in an invoke's `done`, has the return type of the task or machine. If it returns nothing, `result` cannot be used.
- `error`, in an invoke's `fail E`, has type `E`. In `fail *` it has a type only if exactly one declared error is left for `*` to cover. For a call, `Unanswered` is among the errors `fail *` covers.
- `event`, in `on name` for a call, is the call's arguments, read only through their fields: `event.items` has the type of the parameter `items`, and `event` alone is an error (`RB0309`). `call` has the type `call name`.
- `event`, in `on name.ended`, is read only through its one field: `event.status` has the built-in enum type `Ended`, and `event` alone is an error (`RB0309`), as is any other field (`RB0304`).

Outside those transitions these names are not in scope.

A `ref Machine` is a handle, not data. It can be a parameter of a machine, function, action or task and a `var`, and be passed as an argument to an invoked machine and to host functions, actions and tasks. It cannot be a field of a record, error or event, a parameter of a `call`, or what a function, task or machine returns (`RB0316`): a machine gets a handle only from its own signature or from `invoke … as`, so the charts' signatures show who can talk to whom. It has no JSON form, so it cannot be passed where `json` is expected, and `==` and `!=` are not defined on it. `ref A` and `ref B` are different types for different machines; a `ref A` can be used where a `ref A?` is expected, as for any `T`.

A `call name` is a pending call of the machine's own call `name`. It can only be the type of a machine variable: not optional, not in a list or an alias, and never a parameter, a field or a result (`RB0317`), so a pending call cannot leave its run. It has no initial value, so it starts unassigned and [definite assignment](#definite-assignment) applies to it; `set pending = call` in an `on name` gives it one. Like a `ref` it has no JSON form and no `==`.

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

- **`map<K, V>`** maps keys to values of type `V`. Its keys are strings: `K` is `string` or an alias of it (`map<UserId, Order>`), which documents what the keys are (`RB0323` otherwise). Its JSON form is an object (see [values](../../spec/execution.md#values)). A chart has no map literal and no operation on a map: lookups give a value that may be absent, which a chart cannot narrow, so they belong in a `fn`, as list operations do. A map is passed around whole: in variables, parameters, fields and results, and to `fn`s (`fn emptyCounts() -> map<string, int>` gives an initial value). `map<string, ref Inventory>` holds handles where `ref Inventory[]` could.
- **Generic types.** A `type` record or alias may take type parameters, upper-case names in `<>` after its name: `type Page<T> { … }`, `type Pair<A, B> { first: A, second: B }`, `type Dict<T> = map<string, T>`. Enums, errors, events and extern types cannot, and neither can functions, tasks or machines (a `json` parameter already accepts any value). A use gives a type for each parameter, `Page<Order>`; the declaration's fields (or the aliased type) are then those of the declaration with each parameter replaced by its argument, so `page.items` is an `Order[]`. A generic alias is transparent like any alias: `Dict<Order>` is `map<string, Order>`.
- **Record values** of a generic type are written without the arguments, `Page { items: orders, next: cursor }`. The arguments come from the type the value is expected to have, where there is one (a `set`, an argument, a field, `output`), else from its field values. A value whose arguments neither tells (fields that disagree, as in `Pair { first: 1, second: "one" }` for `type Pair<T> { first: T, second: T }`, or the value of `event`) is an error (`RB0327`): use it where its type is known.
- **Rules.** A use gives exactly as many arguments as the type has parameters, and a type that is not generic takes none (`RB0324`). Every parameter is used by its declaration (`RB0325`), and none has the name of a declaration (`RB0107`). A type argument cannot hold a `ref` (`RB0316`), since it may become a field. A generic type can refer to itself, directly or through other types, only with its own parameters in order (`Tree<T>` in `Tree<T>`; `type Nested<T> { inner: Nested<T[]>? }` is `RB0326`), so that it has finitely many instances. Precisely: in the declaration of a generic type, every use of a generic type that refers back to it passes the declaration's own parameters, in order, so types that refer to each other have the same parameters (`type A<T> { b: B<T>? }` with `type B<T> { a: A<T>? }`); a type that is not generic can use any instance.

### Equality

`==` and `!=` stay limited to scalars and enums. No chart in the repository compares records or lists, or works around not being able to: correlating by an id field (`event.orderId == id`) is the idiom. It was decided on 27 November 2026 to leave it so until a chart needs more, and what a later version adds is written down here so that both runtimes add the same thing.

- **Equality is structural and directed by the type.** Records, errors, events, lists and optionals compare part by part, recursively, as long as every part can be compared. `json`, `extern type` and `ref` stay excluded. A `float` is always finite, so `NaN` raises no question.
- **The runtimes compare by the type, not by their host form.** Relaxing only the check would make them disagree: the TypeScript core compares with `===`, which compares objects by identity, and the Rust core compares JSON values. Each needs a comparison that walks the type, so that, for example, an `int` field compares as a number whatever its JSON spelling.
- **What it changes:** `RB0303` and the check behind it, both cores, the rule in [`spec/execution.md`](../../spec/execution.md), and a conformance case. Not the IR, the hashes, the viewer or any chart.

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

- **`send name.Event { fields }`** queues the event for another machine and carries on. `name` is an `invoke … as name` in scope, or a parameter or variable of type `ref Machine`. The event must be one the machine declares with `event`, with every field and the right types, as a record value: not one of its `internal event`s, which only it can raise (`RB0116`). A field-less event is written bare: `send upload.Pause`.
- **`emit Event { fields }`** emits one of the machine's own `emits`, to whoever invoked or spawned it and to every machine holding a reference to it, of those that have an `on` for it ([who receives emitted events](actors.md#who-receives-emitted-events)). The machine does not receive its own emitted events.
- **`on name.Event`** is taken for an event that `name`'s machine emits. `name` is resolved as for `send`, and `Event` must be one that machine declares with `emits`.
- **An `as` name is in scope in the invoking state and the states inside it**, which is while the child runs: elsewhere it is an error (`RB0115`). A `send` to the child must also find it running (`RB0213`): not in the invoking state's own `entry` (the child starts once the step has settled), `exit` (it has been stopped) or invoke outcomes (it has ended), and not in a transition that leaves the invoking state, since leaving stops the child before the transition's effects run. A transition with no target, an internal one (`-> .X`) written in the invoking state or inside it, or one written in a state inside the invoking state whose targets are all inside it too, can send. Two states may use the same `as` name when neither contains the other.
- **Names.** An `as` name shares the machine's namespace with its parameters and variables, and cannot be the name of an `as` around it. It is not a value: it is used only in `send name.Event` and `on name.Event`. A machine's `event`, `internal event` and `emits` names share one namespace (`RB0108`), and like events, internal and emitted events cannot have the name of a type, error or machine.
- **Two namespaces of triggers.** The events a machine accepts are its own: `on Cancel`. The events it receives from others are named through their sender: `on upload.Cancel` is a different trigger from `on Cancel`, and from `on other.Cancel`. `on *` matches only the machine's own events, never an emitted one.
- **A `ref` variable** addresses whichever machine it holds when the `send` runs or the event arrives.

### Tasks that emit

A task can say how its work is going while it runs. It declares the events it emits after its signature, and the invoking state receives them through an `as` name, as it receives a child machine's:

```
task upload(file: File) -> Url ! UploadFailed
  /// How much of the file is stored.
  emits Progress { percent: int }
  emits Stage    { name: string }

task ticks(every: duration) emits Tick

state Uploading {
  invoke upload(draft.file) as up {
    done              -> Done    set url = result
    fail UploadFailed -> Failed  set failure = error
  }
  on up.Progress set percent = event.percent
}
```

- **`emits Name [{ fields }]`** clauses come last in a task's declaration, after its errors, one for each event. They belong to the task: two tasks, or a task and a machine, may each emit an event named `Progress`, and they are different events. Within one task the names are unique (`RB0108`), and an emitted event cannot have the name of a type, error or machine (`RB0107`), as for a machine's `emits`. Fields may be `sensitive`, and each clause may have a doc comment.
- **`as` on a task** is allowed when the task declares at least one `emits` (`RB0214` otherwise, and always on a call). It follows the rules of `as` on a machine invoke: it is in scope in the invoking state and the states inside it (`RB0115`), it shares the machine's namespace with parameters, variables, collections and other `as` names, and it cannot be `key` (`RB0122`). `retry` cannot be combined with it (`RB0216`).
- **`on up.Progress`** is taken for a `Progress` the task's invoke emits. `Progress` must be one of the task's `emits` (`RB0117`). `event` has the type `upload.Progress`, the task's name, a dot and the event's, which a chart never writes, as it never writes `Upload.Progress`; `event.percent` has the field's type.
- **A task takes no events.** `send up.Pause` is an error (`RB0116`). Its end is its invoke's outcome, so `on up.ended` is an error too (`RB0124`).
- **Without `as`**, an emitting task is invoked like any other, and what it emits is not delivered, as for a machine invoked without `as`.
- **Host code decides when** a task emits: its implementation is given an emitter as it starts ([TypeScript](typescript.md), [Rust](rust.md)). So the warnings about a machine's own `emit` effects (`RB0905`, `RB0906`) do not apply to tasks. `RB0907` does: a task's `emits` that no `on x.E` through an address of that task receives is reported.
- **Order and staleness.** What a task emits is taken in the order it was emitted, and before the task's outcome. Once the state is left, and the invoke cancelled, nothing more of it is taken: what it had emitted just before is `stale` in the trace. An emitted event the machine has no enabled transition for is dropped, in a [strict machine](#strict-machines) too, as for any emitted event ([`spec/execution.md`](../../spec/execution.md#tasks-that-emit)).

**Stream tasks.** A task that returns nothing, emits, and whose host implementation runs until its invoke is cancelled is a **stream task**. It is not a separate kind of declaration: `ticks` above is an ordinary task. If its implementation does end, its outcome is an ordinary `done` with no result, which the chart may handle or leave to be dropped, and if it fails, an ordinary failure or host defect. It covers what XState's `fromCallback` (except `receive`), `fromObservable` and v6's `createAsyncLogic` with `enq.emit` do.

```
state Waiting {
  invoke ticks(1s) as clock
  on clock.Tick          set seconds = seconds + 1
  on Stop       -> Idle
}
```

**An invoke with no outcomes.** `invoke ticks(1s) as clock` has no `{ … }`: an invoke may leave its block out. When the braces are written, they hold at least one outcome. Every declared error must still be handled (`RB0204`, and for a call `Unanswered`), so in practice only an invoke of a task or machine that declares no errors goes without a block; `done` has never been required. The formatter writes no braces for an invoke with no outcomes. `retry` without a block is allowed by the grammar, but such a retry retries nothing, which is already an error (`RB0217`).

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
- **`on name.Event`** is taken for an event any child of the collection emits; `key` is that child's key. **`on name.done`** is taken when a child ends with an output (`result`, `key`), **`on name.fail E`** or **`on name.fail *`** when it fails (`error`, `key`). An outcome nothing takes is dropped, in a [strict machine](#strict-machines) too: unlike an invoke's, a spawned child's errors need not all be handled.
- **A child's defect** (a host failure, not a declared error) ends the parent with the same defect, as for an invoke.
- **When the machine ends**, finished or cancelled, its remaining children are stopped, after its own `exit` effects. A message those effects send a child is likely never taken, since a stop goes ahead of what the child has queued: to let children finish cleanly, tell them before the machine's last step and wait for their `on name.done`.
- There are no expressions over a collection: a chart cannot count its children or list their keys. Keep a count in a variable, or have the children emit what the parent needs.
- **Names.** A collection's name shares the machine's namespace with its parameters, variables and `as` names. `key` is an implicit name, like `event`, so it cannot name a parameter, variable, `as` name or collection.

**Collections of tasks.** A collection can hold tasks instead of machines: a lower-case name after the `:` is a task, an upper-case one a machine, as in `invoke`.

```
children uploads: upload by string

state Open {
  on Add              spawn uploads[event.file.name] = upload(event.file)
  on Cancel           stop uploads[event.name]
  on uploads.Progress do showProgress(key, event.percent)
  on uploads.done     set stored = stored + 1  do showUploaded(key, result)
  on uploads.fail *   do reportFailure(key, error.reason)
}
```

- **`spawn uploads[key] = upload(args)`** starts the task under `key`. The callee must be the collection's task (`RB0220`), and the arguments are checked as for an invoke.
- **`stop`, `on uploads.done`, `on uploads.fail E` and `on uploads.fail *`** behave as for a collection of machines, with `result`, `error` and `key`. **`on uploads.Progress`** is taken for an event in the task's `emits` from any of its spawned tasks: `event` has the type `upload.Progress`, and `key` is that task's key. A spawned task's emitted events are always delivered to its parent, since the collection names them, as long as the parent has an `on` for them.
- **A task takes no events**, so `send uploads[k].E` and `send uploads.*.E` are errors (`RB0116`). `on uploads.ended` is an error as for any collection (`RB0124`).
- **When the machine ends** it stops its spawned tasks as it stops machines. A spawned task's host failure is the parent's defect, and a failure with an error the task does not declare is an `undeclared` defect.

The rules at run time are in [`spec/execution.md`](../../spec/execution.md#spawned-children).

## Events a machine raises

A machine can give itself one of its own events with `raise`: at once, as SCXML's `<raise>` does, or after a delay, as SCXML's `<send>` with a delay to the machine itself does. An event declared `internal event` can come only from a `raise`. An order that reminds its customer every 24 hours until they confirm, whatever state it is in, and lapses after three days:

```
action remind(order: string)

machine Order(id: string) {
  event Checkout
  event Pack
  event Confirm
  /// Raised by the order itself: host code and other machines cannot send it.
  internal event Remind { attempt: int }

  initial Cart

  // In the machine body: taken whatever state the order is in.
  on Remind {
    if event.attempt < 3            do remind(id)  raise Remind { attempt: event.attempt + 1 } after 24h as reminder
    else                 -> Lapsed
  }

  state Cart { on Checkout -> Placed  raise Remind { attempt: 1 } after 24h as reminder }
  state Placed {
    on Pack    -> Packing
    on Confirm -> Confirmed  cancel reminder
  }
  state Packing      { on Confirm -> Confirmed  cancel reminder }
  state Confirmed    { … }
  final state Lapsed {}
}
```

- **`internal event Name [{ fields }]`** declares an event that only the machine itself raises. It is not part of the machine's interface: host code cannot send it ([TypeScript](typescript.md#what-gets-generated), [Rust](rust.md#what-gets-generated)), and a `send` of it from another machine is an error (`RB0116`). Otherwise it is an event like any other: it may have fields, `sensitive` ones among them, and a doc comment, `on Name` and `on *` take it, and `event` has its type. It is declared directly in the machine body (`RB0015` in a state), and its name shares one namespace with the machine's `event` and `emits` names (`RB0108`). An internal event that no `raise` of the machine raises can never happen (warning `RB0914`).
- **`raise Event [{ fields }]`** puts one of the machine's own events, internal or declared with `event`, on its internal queue. An event it emits, another machine's or an unknown one is an error (`RB0125`). The value is written as for `send` and `emit`, and a field-less event is written bare.
- **When it is taken.** In the same step, before the step ends: after the `always` transitions that are enabled, and after the completions (`done`) and raised events queued before it, and before any input the run takes next, even an external event that was already waiting. It is selected as the same event from outside would be: from the innermost active state outward, by `on Event` and then `on *`, in several regions at once if it enables transitions there. It belongs to the run, not to the state whose effect raised it, so it is still taken when that state was left in the same step.
- **Nothing is kept for later.** A raised event that enables no transition when it is taken is dropped, and in a [strict machine](#strict-machines) it is the `unhandled` defect. A `raise` of an event that no `on Event` and no `on *` of the machine takes is never taken (warning `RB0915`). A raised event whose transition raises it again, without end, ends the run with the `limit` defect after 1000 microsteps, as an `always` loop does.
- **`raise Event [{ fields }] after delay [as name]`** is a **delayed raise**: a timer that belongs to the run, not to a state. The delay, a `duration` expression, and then the value are evaluated when the effect runs, so `event.attempt + 1` above is computed when the reminder is scheduled. Leaving the state that started it changes nothing. When it is due, its event is queued as an `after` timer's expiry is, behind the inputs that arrived before it, and is taken as an external event is, by whatever states are active then: dropped if none takes it, and `unhandled` in a strict machine. `after` goes on with a raise only on the line where its event (or the event's `{ … }`) ends: on a later line it starts an `after` transition.
- **`as name`** names a delayed raise. A delayed raise whose name is pending replaces it: the pending one is cancelled, and the new one starts with its whole delay. Several raises may use one name, as the two above do, so that each starts the same reminder again. The name shares the machine's namespace with its parameters, variables and collections (`RB0108`). An unnamed delayed raise can be neither replaced nor cancelled. `as` on a raise without `after` is an error (`RB0028`): an immediate raise is taken before the step ends, so there is nothing to cancel.
- **`cancel name`** cancels the pending delayed raise of that name. If none is pending (it never started, it has fired, or it was cancelled already), it does nothing. A name that no `raise … after … as name` of the machine uses is an error (`RB0126`). An expiry already queued when its raise was cancelled or replaced is `stale` when it is taken, so it does nothing.
- **When the run ends.** A run that finishes, or is cancelled (by host code, or by its parent leaving the invoking state), cancels its pending delayed raises after its `exit` effects and after stopping its spawned children. What the `exit` effects of that last step raise at once is traced and never taken. A run that ends with a defect drops its delayed raises with no record, as it does its timers.
- **Saving.** A delayed raise is saved and restored as an `after` timer is: a run waiting only on delayed raises is idle, so `suspend` and `resume` save it, with each pending raise's due time, and restoring starts it again with the time that remains; one that fell due while the run was saved fires as soon as it is restored. A snapshot with no pending raise is written as before.
- **The trace** writes `raised` as an immediate raise runs, followed by its `event` (or `dropped`) record with the input `raised`; `raise.scheduled` as a delayed raise starts, and `raise.cancelled` as one is cancelled or replaced, or the run ends. A delayed raise's expiry is an `after` input with its timer's id ([`spec/trace.md`](../../spec/trace.md)).
- **Compared with `after`.** An `after` belongs to its state: it starts when the state is entered and stops when the state is left, and starting it again means leaving and entering the state. A delayed raise is started by any effect, outlives the state that started it, is stopped by a `cancel` anywhere in the machine, and is started again by a raise of the same name without leaving anything. A timeout of a state stays an `after`.

**A debounce.** Each key starts the delay again, because each raise replaces the pending one of its name. `Typing` is never left, so its own `entry`, `exit`, invoke and timers are left alone, where a self-transition `on Key -> Typing` with an `after 500ms` in `Typing` would restart them all:

```
machine Finder() {
  event Key             { text: string }
  internal event Search
  var query: string = ""

  initial Typing

  state Typing {
    on Key                  set query = event.text  raise Search after 500ms as debounce
    on Search -> Searching
  }
  state Searching { … }
}
```

**Signalling between regions.** An immediate raise lets one region of a parallel state tell another that something has happened, without a variable kept in step and an `always` that waits on it:

```
task warmCache()

machine Setup() {
  internal event Ready

  initial Starting

  parallel state Starting {
    done -> Running
    state Cache {
      initial Warming
      state Warming {
        invoke warmCache() {
          done -> Warm  raise Ready
        }
      }
      final state Warm {}
    }
    state Server {
      initial Waiting
      state Waiting       { on Ready -> Serving }
      final state Serving {}
    }
  }
  state Running { … }
}
```

`Ready` is raised by the transition out of `Warming` and taken in the same step, where it moves `Server` to `Serving`; both regions are then final, so `Starting`'s `done` is taken too, still in that step. Had `Server` been in a state with no `on Ready`, the event would have been dropped: a raised event is an event, not a flag, and a region that must learn of something later needs a variable.

There is no delayed `send` to another machine: a delayed raise whose transition sends does the same, and keeps the timer in the chart that owns it.

Decided on 27 November 2026 and built on 28 November 2026 in IR 0.11 (SM-28): `internalEvents` is a member of each [machine](../../spec/ir.md#machines), and `raise` and `cancel` are [effects](../../spec/ir.md#effects). A machine that uses none of them keeps its hash. The rules at run time are in [`spec/execution.md`](../../spec/execution.md#raised-events).

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
- **Restoring re-enters states.** The states a history state restores are entered like any others: their `entry` effects run, and their `invoke` and `after` timers start again from the beginning, so an invoked machine starts over. The machine's variables are not restored: they keep the values they have. A restored state's own [parameters and variables](#data-local-to-a-state) are: it gets back the ones it had when it was left.
- **In a parallel state**, a shallow history state restores every region with its default entry, which is what a transition to the parallel state does; a deep one restores each region as it was.
- **Saving.** What each history state recorded is part of a [saved snapshot](../../spec/execution.md#the-saved-snapshot), so a restored run returns to the same states.

The [semantics](../../spec/execution.md#a-microstep) give the precise rules. The viewer draws a history state as a small circle marked `H`, or `H*` for a deep one, inside its parent.

## Data local to a state

A machine's `var` lives as long as the run. Data that only one part of the chart needs can belong to a state instead: a **state variable**, declared with `var` inside the state, or a **state parameter**, given by the transition that enters the state. Together they are the state's **locals**.

```
machine PasswordReset(email: string) -> ApiResponse {
  event Submit { code: string }

  initial Requesting

  state Requesting {
    invoke requestReset(email) {
      done -> Confirming(result) // result is a ResetToken
    }
  }

  /// The token is only needed while the code is being confirmed.
  state Confirming(token: ResetToken) {
    var tries: int = 0 // 0 each time Confirming is entered
    on Submit {
      if matches(token, event.code) -> Resetting(token)
      if tries < 2                                                 set tries = tries + 1
      else                          -> Rejected("too many tries")
    }
  }

  state Resetting(token: ResetToken) {
    invoke resetPassword(token) {
      done -> Changed
    }
  }

  final state Changed                  { output noContent() }
  final state Rejected(reason: string) { output problem(422, reason) }
}
```

- **Declaring.** `var name: T [= expr]` and `sensitive var` may be written in any state, as in a machine body. Parameters are written after the state's name, `state Confirming(token: ResetToken, sensitive code: string)`, on a `state`, a `parallel state` or a `final state`, as fields are: a `sensitive` mark, a name and a type. A state's parameters and variables share one namespace (`RB0108`).
- **Entering with arguments.** A transition into a state with parameters passes them after the state's name, positionally, as `do` and `invoke` pass theirs: `-> Confirming(result)`. Every transition that enters the state passes them, with the declared types (`RB0224` if it does not, `RB0302` for the wrong number and `RB0301` for a wrong type), and one that does not enter it passes none (`RB0225`). A state with parameters cannot be a compound state's `initial` or a region of a parallel state (`RB0223`): those are entered by default, with no transition to pass anything. So every way into the state is a transition the compiler sees.
- **Paths.** The arguments go on the segment of the target path that names the state, wherever it is in the path: `-> Editing(doc).Changing` enters `Editing` with `doc` and goes on to its child `Changing`. Arguments are written exactly for the states the transition enters: from inside `Editing`, `-> Changing` keeps `Editing` and its locals, and `-> Editing(doc).Changing` there is `RB0225`. `-> .Child(x)` passes arguments to a child the transition enters while its source is kept. With several targets, a state on more than one target's path (the parallel state they meet in, say) gets its arguments once, on any one of them.
- **Scope.** A local can be read in the state that declares it and in every state inside it: their `entry`, `exit`, guards, effects and arguments, `invoke` arguments, `after` delays, `output` and `fail`, and the initial values of later variables. Outside them it does not exist (`RB0105`). A parameter is read-only (`RB0114`); a variable is assigned with `set` like a machine's.
- **No shadowing.** A local cannot have the name of anything else that is in scope where it is, or that it would be in scope with: a machine parameter or variable, a `children` collection, the name of a delayed raise, an `as` name, or a local of a state around it or inside it (`RB0127`); nor `key` (`RB0122`). So a name means the same thing wherever it is read. States that are not nested may have locals of the same name: `tries` in two sibling states, or in two regions, are two variables.
- **Types.** A local cannot have a `ref` or `call` type (`RB0328`): a handle to a peer or a pending call belongs to the machine.

### When locals get their values

A step runs as before: exits, innermost first; the transition's effects; entries, outermost first. Locals fit into it like this:

1. **The arguments are evaluated after the transition's effects**, before any state is entered. They see what the transition's `set`s assigned, as a `send` does, and they can still read the locals of the states the transition leaves: `-> Confirming(token)` in `Confirming` passes its token on, and a state `Backoff(attempt: int)` can go on with `-> Backoff(attempt + 1)`.
2. **The locals of the states it left are then discarded.** So their `exit` effects, and the transition's own effects and arguments, read them; nothing after does.
3. **Each state entered gets its locals**, after it is entered and before its `entry` effects run: its parameters their arguments, then each variable, in order, its initial value (an optional variable with none starts as `null`, as a machine's does). An initial value can read the state's parameters, the variables before it, and the locals of the states around it, which are entered first. Then the `entry` effects run and see them all.

Locals are reset on every entry. **Re-entering** a state, `-> Self(n)` or a transition from inside it to the state itself, discards its locals and gives it new ones, its initial values evaluated again; its old values can be read in the arguments. A transition that keeps the state, one with no target or an internal one written in it or above it (`-> .Child`), keeps its locals. An assignment in a transition to a local of a state that the transition leaves is discarded with it, which is a warning (`RB0916`): `on Retry -> Trying  set tries = tries + 1` written in `Trying` counts nothing. Use a transition with no target, a parameter (`-> Trying(tries + 1)`), or a machine variable.

`retry` works unchanged in a state with locals: the retry's attempts and waits happen inside the state, which stays active, so its locals live through every attempt, and its invoke's arguments can read its parameters. The retry's counter is still a machine variable ([what retry compiles to](#what-retry-compiles-to)); making it a variable of the retrying state is left for later, since it would change the IR, the hash and the trace of every chart that uses `retry`.

A **history state** restores locals with the states it restores: each state it enters again from its record gets back the parameters and variables it had when it was left, instead of new arguments and initial values. The states below them that are entered by default start afresh, as does everything when there is no record yet. Moving a variable from the machine into a state therefore does not change what a history state returns to. The parent itself, when the transition enters it, takes its arguments from the transition: `-> Editing(doc).Previous`.

**In a trace** a state's locals are written as `set` records after its `entered` record, one for each parameter and each variable that starts with a value, and their name is the state's id, a dot and the local's name: `{"kind": "set", "var": "PasswordReset.Confirming.tries", "value": 0}`. A `set` of a state's variable is written the same way. A machine variable's initial value is not traced; a state's locals are, since they come from the step that enters the state. A snapshot holds the locals of the active states, and a saved snapshot saves them, so a restored run goes on with them ([`spec/execution.md`](../../spec/execution.md#data-local-to-a-state)).

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

- **Where.** On any field of a record, error, event or emitted event; any parameter of a machine, state, task, action, function or call; any `var`, a state's among them. In an `on name` transition that takes a call, `event.pin` is sensitive when the call's parameter `pin` is. The mark is part of the declaration, so it holds wherever the declaration is used: through aliases, in other files and in other packages.
- **What it does.** Every trace record shows the value of a marked field, parameter or variable as the string `"<redacted>"`, at any depth: a `Credentials` in a list in an optional shows `{"user": "ada", "password": "<redacted>"}` there. A variable marked `sensitive` is shown `"<redacted>"` whole. The records keep their form; only their values change. The rules, which both runtimes follow byte for byte, are in [`spec/trace.md`](../../spec/trace.md#redaction).
- **What it does not.** Nothing but traces changes. The machine, host code and **saved snapshots see and keep the real values**: a restored run goes on with them, so a stored snapshot needs the protection the data needs. Types do not change either, in the generated TypeScript and Rust or anywhere else. On a function's parameter the mark changes nothing, since function calls are not traced; it can still say what the function handles. A marked field does not change a machine's [hash](../../spec/ir.md#the-machine-hash), so marking one later does not invalidate saved snapshots.
- **Keeping it marked.** A runtime redacts a value by its declared type. So a value read from a sensitive place (`password`, `event.pin`, a field of a sensitive variable) can only go to a place that is also sensitive, or that no trace shows: a guard, a function's argument. Assigning it to a variable that is not sensitive, passing it to an action, task, machine, call or state parameter that is not, putting it in a field that is not, or making it an `output`, a `fail`, a call's reply (the value of `reply … done` or the error of `reply … fail`, which a `replied` record shows), an `after` delay, a retry's delay or a spawned child's key is an error (`RB0320`). So is an operator's result with a sensitive operand: `pin + 0`, `not flag` and `pin == 1234` are sensitive too. A value whose type has sensitive fields cannot go where `json` is expected, since `json` has no fields to redact by (`RB0321`), unless that place is itself sensitive; that includes a map or list of such values, an instance such as `Page<Login>`, and a `Page<Login>` where a `Page<json>` is expected. Redaction follows maps and type arguments: the `password` of every `Login` in a `map<string, Login>` or a `Page<Login>` is redacted. A function's result is not sensitive: a `fn` is how a chart derives what may be shown (`last4(card)`), on purpose.
- **What is never covered.** A host defect's message is whatever host code raised, and the trace shows it as it is: a function that is given a sensitive value should not put it in an exception (`JSON.parse` quotes its input). And a run redacts by the IR it was built from: runs that talk to each other should be built from the same charts, since a mark does not change the hash their handles are checked by.

## Checking properties

A machine can state what must be true of its states, as `check` lines in its body, and `roadbook check` proves or refutes each one. They are assertions about the chart, not run-time assertions: nothing runs, and they are not part of the IR, so adding or changing one changes no machine hash.

```
machine PaymentWebhook(body: string, signature: string, secret: string) -> ApiResponse {
  …
  /// A refund is recorded only for a payload that was checked for duplicates.
  check Applying.RecordingRefund only after CheckingDuplicate
  /// Nothing is applied before the payload is parsed.
  check Applying only after Parsing
  check reachable Applying.RecordingRefund
  check can finish
}
```

| Check | Holds when |
| --- | --- |
| `check never S` | `S` is never active. |
| `check never A and B …` | The states are never all active at once: useful with the regions of a parallel state. |
| `check S only after T` | Whenever `S` becomes active, `T` has been active at some point before, or is active in the same configuration. |
| `check reachable S` | `S` can be active. |
| `check can finish` | From every configuration the machine can be in, some path leads to a top-level final state: it has no dead end and no trap it can't leave. |

A state is written as a path from the machine body, as a target written at machine level is (`Applying.RecordingRefund`). It may name any state, including a region, a final state, or the `Attempt` and `Backoff` states a [`retry`](#what-retry-compiles-to) writes out, but not a history state, which is never active. A check is written only directly in a machine body (`RB0015` in a state, `RB0016` at the top level of a file), and an unknown state is `RB0123`. A `///` doc comment documents the check, and is shown in `--checks` and the JSON.

### How checks are decided

The compiler explores every configuration the machine can be in, following [the execution rules](../../spec/execution.md) exactly: the walks that select transitions in each region, conflicts, exit and entry sets, history recorded on exit, settling (`always` before the internal queue, completions and raised events in the order they were queued), timers and invokes started on entry and consumed by their expiry or outcome, the delayed raises pending in the run (replaced, cancelled and consumed by their expiry), events emitted by children and peers, peers' ends (`on name.ended`), calls the machine makes and those it takes, spawned children's outcomes, and the transitions written in the machine body. In a strict machine, `ignore` transitions are explored like any other. A configuration passed through while settling counts: a state entered by an `always` and left at once was still active. What it does not know is data. Every guard may hold or fail each time it is evaluated, every declared event may arrive at any time (an internal one only when the machine raises it), a task may finish or fail with any of its declared errors, a call it makes may be answered `done`, with any of its declared errors or with `Unanswered`, a call of its own may arrive at any time, a peer may end at any time (and more than once, since the compiler does not follow which runs a `ref` holds), a child machine may do anything its declaration allows, and any running timer may expire.

That set of paths includes every path a real run can take, and more. So:

- **A `never` or `only after` that passes is a proof**, whatever the data: no run started from the chart can do it.
- **A `reachable` that fails is a proof** that no run can reach the state.
- **A `reachable` that passes is `proved`** when the path it found relies on nothing the chart can't show: only the machine's own events, task outcomes, and timers that are sure to expire first (the only one running, an `after` timer's or a delayed raise's, or an `after` with a literal delay shorter than the literal delays of the `after`s written after it in the same state), each taken where no other machine can act first. Otherwise it is **`conditional`**, and the report lists what the path assumes: a guard holding or failing, a timer or a delayed raise expiring before the other running timers (its delay might not be the shortest), another machine finishing, failing or emitting, answering a call one way or making one, or a step coming before another machine that could act at that point (a child still running, a collection that may have children, a peer whose events an active state listens for, a callee still to answer, a caller whose call an active state takes) ends, emits, answers or calls ("`` `Child` (invoked in `A`) does not end or emit first ``"). So with `invoke Child() { done -> D }` and `after 1s -> T` in one state, `reachable T` is conditional: the child may always finish within the second.
- **A `can finish` that passes is `proved`** when, from every configuration, the host can bring the run to a top-level final state whatever the guards decide and whatever other machines do first. The host picks the inputs it controls (the machine's own events, task outcomes, timers sure to expire first) and the data picks every guard's outcome, including all of them failing so that the input takes nothing; while the run settles nobody picks. Precisely: a final configuration is won; a settling one is won when every step from it leads to a won one; and a stable one is won when some input the host controls leads to a won configuration whatever its guards decide, and every move another machine can make from it (an outcome or event of a child, a spawned child or a peer, an answer to a call it made, a call it takes) leads to a won configuration too, since that machine may act first. So an `if approved() -> Charged` with an `else -> Declined`, both finishing, is proved, while the same `if` with no `else` is not: the guard may keep failing. Otherwise it is **`conditional`**: every configuration has a path to a final state, but something the chart can't show could block it, and the report lists what the path assumes from the first configuration (nearest the start) where the host can't force the end, together with the path to it and on to a final state.
- **A failing check is an error**, and the error shows a counterexample: of those with the fewest assumptions in all, the shortest. When the counterexample relies on assumptions, the error lists them: the path may be blocked at run time by a guard, but the chart can't show that, so to pass, the chart must change.

The checks cover runs started from the chart. A snapshot restored after a [migration](../../spec/execution.md#restoring-a-snapshot) can be in any configuration the migration wrote, reachable or not.

Checks are evaluated only for an entry file that compiles without errors: an entry with errors gets its diagnostics, and its checks are left out of the summary, `--checks` and the JSON until it compiles.

A machine with more than 100,000 configurations, or one too large to explore for another reason (an event expected from a peer through more than 8 `ref` names, since each set of them is an input of its own, or more than 1.6 million combinations of guard outcomes tried across its regions), is not checked: each of its checks gets the warning `RB0912` and the verdict `unchecked`. Only machines with checks are explored, so a chart without checks costs nothing. Checks are evaluated for the files `roadbook check` is given, never for another package's charts.

### Reports

Checks are evaluated by `roadbook check`, by the language server (a failing check is a diagnostic in the editor, with its path as related information), and by `@dire-kiwi/roadbook-cli check`. `gen`, `export`, `viz`, `paths` and the Rust macro don't evaluate them, so a failing check fails CI, not a build.

```
fn approved() -> bool

machine Payment() {
  event Pay
  event Refund

  initial Idle

  state Idle {
    on Pay {
      if approved() -> Charged
      else          -> Declined
    }
  }
  state Charged        { on Refund -> Refunded }
  final state Refunded {}
  final state Declined {}

  check never Refunded
  check reachable Declined
}
```

`roadbook check --checks payment.road` prints:

```
error[RB0501]: `never Refunded` does not hold: `Refunded` can be active
  --> payment.road:19:3
   |
11 |       if approved() -> Charged
   |       ------------------------ 1. Pay [approved()] -> Charged
...
15 |   state Charged        { on Refund -> Refunded }
   |                          --------------------- 2. Refund -> Refunded
...
19 |   check never Refunded
   |   ^^^^^^^^^^^^^^^^^^^^
   |
   = help: Idle --Pay [approved()]--> Charged --Refund--> Refunded, if `approved()` holds on the way

1 error
payment.road:19: Payment: check never Refunded: failed
payment.road:20: Payment: check reachable Declined: conditional
    if `approved()` fails on the way
    Idle --Pay [else]--> Declined
checks: 1 conditional, 1 failed
```

Each label is a transition on the counterexample, numbered in path order, at that transition in the source, and so is each guard the path assumes of a transition it doesn't take (`3. after 1s [ok()]: assumed to fail`), as when a timer expires and its guard fails, or a guarded `always` is skipped; the help is the whole path in [`roadbook paths`](roadmap.md#command-line)' style, each configuration written as its active states joined with `+` when there are regions. When any check was evaluated, `roadbook check` ends with a summary line on standard error, such as `checks: 7 proved, 4 conditional`, and `roadbook check --checks` lists every check above it, with its doc comment beneath it as `///` lines and, for a `conditional` one, its assumptions and path. The exit code is 1 when a check fails, as for any error. `--json` has every verdict in its `checks` member ([below](#diagnostics-as-json)).

A project's CI needs nothing more than the `roadbook check` (or `npx @dire-kiwi/roadbook-cli check`) it already runs.

The codes are `RB0501` to `RB0504` for the four kinds of failure and `RB0912` for a machine not checked, listed with the others under [diagnostic codes](#diagnostic-codes).

## Calls between machines

A request with a reply is a call, as [asking a peer](actors.md#asking-a-peer) describes. The callee declares the calls it answers; a machine holding a `ref` to it invokes one like a task, and the reply goes to that caller alone.

```
machine Inventory(warehouse: string) {
  /// Hold the items for an order, or fail if they cannot all be supplied.
  call reserve(orderId: string, items: Item[]) -> Reservation ! OutOfStock

  var pending: call reserve
  // …
  state Ready {
    on reserve -> Reserving  set pending = call, orderId = event.orderId, items = event.items
  }
  state Reserving {
    invoke reserveStock(warehouse, orderId, items) {
      done            -> Ready  reply pending done Reservation { orderId: orderId, count: result }
      fail OutOfStock -> Ready  reply pending fail error
    }
    on reserve reply call fail OutOfStock { reason: "busy" }
  }
}

state Reserving {
  invoke inventory.reserve(id, items) {
    done            -> Reserved  set reservation = result
    fail OutOfStock -> Failed    set problem = error
    fail Unanswered -> Failed    set problem = OutOfStock { reason: "unanswered" }
  }
  after 5s -> Failed
}
```

- **`call name(params) [-> T] [! E | F]`** in a machine body declares a call. Its name is lower-case and unique among the machine's calls, and its parameters, result and errors follow the rules for a task's, except that a parameter cannot be a `ref` (`RB0316`): the arguments reach the callee as an event's fields do, and a machine holds only the runs it was started with.
- **`invoke x.name(args) { … }`** calls it. `x` is a parameter or variable of type `ref M` (directly or through an alias; not optional, not a list), and `name` one of `M`'s calls. The arguments, `done` (with `result` of the call's return type) and `fail` outcomes are checked as for a task, and the invoke must also handle the built-in error `Unanswered` (`RB0204`), which `fail *` covers. There is no `as`.
- **`on name`**, a lower-case name with no dot, takes a call of the machine's own call `name`, in a state or the machine body, with guards and branches as for an event. In its guard and effects `event` is the call's arguments, read only through their fields (`event.items`), and `call` is the pending call. `on *` never takes a call.
- **`reply x done [value]`** and **`reply x fail error`** answer a pending call: `x` is `call` or a variable of type `call name`. `done` takes a value exactly when the call declares a result, of that type; `fail` takes one of the call's declared errors, never `Unanswered`. `reply` is an effect, so it can answer in the step that took the call or in a later one, through a variable. The value after `done` is on the same line as `done`, so `reply pending done` at the end of a line has none.
- **`Unanswered`** is a built-in error with no fields: the callee was not running, ended before answering, or had no transition for the call. It is reserved: nothing can be declared with its name, and it appears only in a call invoke's `fail Unanswered` outcome, never in a `!` list, as a type or as a value (`RB0119`).

What a runtime does with a call is in [execution](../../spec/execution.md#calls-between-runs).

## When a peer ends

A machine that holds a `ref` can be told when the run it holds ends, however it ends, with `on name.ended`:

```
machine Order(id: string, draft: OrderDraft, inventory: ref Inventory) -> Receipt ! OutOfStock {
  initial Reserving

  // In the machine body, like any `on`: it is taken in whatever state the order is in.
  on inventory.ended {
    if event.status == Ended.Defect -> Alarm
    else                            -> Unavailable
  }

  state Reserving {
    invoke inventory.reserve(id, draft.items) {
      done            -> Paying
      fail Unanswered -> Unavailable
    }
    after 5s -> Unavailable
  }
  // …
}
```

- **`on name.ended`** is taken once, when the run that `name` holds ends: it finished, failed, was cancelled, ended with a defect, or was suspended. It is written in a state or the machine body, with guards and branches, like any `on`, and searched from the active states like an event: a state with no `on name.ended` lets an outer one take it, and if no active state has one it is dropped. `on name.ended ignore` takes it and does nothing, which keeps an outer `on name.ended` from taking it.
- **`event.status`** says how the run ended, a variant of the built-in enum **`Ended { Done, Failed, Cancelled, Defect, Suspended }`**. Nothing else comes with it: not the peer's output, its error or its defect, which stay its own, so the holder depends only on what the peer accepts, emits and answers. `Ended` is a type like any enum: `Ended.Defect` is a value, `==` compares it, and a variable, a function or an action can take one (`action log(status: Ended)`). It is built in, so nothing can be declared with its name (`RB0013`).
- **`name`** is a parameter or variable of type `ref M`, `ref M?` or `ref M[]`, or an alias of one (`RB0315` otherwise, `RB0115` for an unknown name). For an optional one, it names the run it holds, if any; for a list, any run in it. A variable names whichever run it holds when the end arrives, so the end of a run it held before is dropped.
- **Only peers.** An invoked child's end is its invoke's outcome, which `done` and `fail` inside its `invoke { }` handle, and a spawned child's is `on name.done` or `on name.fail E` (`RB0124` for `on upload.ended` or `on sessions.ended`). After `name.`, a lower-case word other than `done`, `fail` and `ended` is an error, with the likely word suggested (`RB0027`): `ended` means something only there, and stays a name everywhere else.
- **Who is told.** Only a holder whose machine has an `on name.ended` for a `ref` to the peer's machine, in any state: writing it is the subscription. Others are not told and their traces show nothing. A holder that is told while none of its active states has an `on` for it writes `dropped`, also in a [strict machine](#strict-machines), which is not required to handle a peer's end. A run held under two names is told once.
- **When.** After everything else the peer sends it: what the peer's last step sends and emits, then the `Unanswered` answers to the holder's pending calls, then the end. A holder that starts after its peer has ended is told right after its own first step. Each holder is told once per peer: a variable later pointed at a run whose end was already taken is not told again.

The rules at run time are in [execution](../../spec/execution.md#messages-between-runs).

## Strict machines

By default an input that enables no transition is dropped and written as `dropped` in the trace, and that stays the default: the events real charts get race with their state (a `Cancel` while an order authenticates, a `Pause` while an import validates), every example machine that accepts events has states where some event is not handled, and a holder of a reference keeps only its own replies by a guard. A machine can opt into failing instead, and say where it ignores an event on purpose:

```
strict machine Lock() {
  event Open
  event Close
  event Knock { loud: bool }

  initial Closed

  on Knock if event.loud ignore

  state Closed {
    on Open  -> Opened
    on Close ignore
  }

  state Opened {
    on Close -> Closed
    on Knock ignore
  }
}
```

- **What is strict.** In a `strict machine`, one of its own events (from host code, or a `send` from another machine) that enables no transition ends the run with an `unhandled` defect instead of being dropped. The defect names the input, with its sender for a message ([`spec/execution.md`](../../spec/execution.md#defects)). Above, a quiet `Knock` while `Closed` is a defect: the machine body's `on Knock` takes only a loud one. A [call](#calls-between-machines) that no `on name` takes is the same defect, and the callee's caller gets `Unanswered` as the callee ends. So is an event the machine [raises](#events-a-machine-raises) itself, at once or when a delayed raise is due, that enables no transition, internal or not.
- **What is not.** Emitted events from other machines (a spawned child's among them) and from tasks, a peer's end that no `on name.ended` takes, invoke outcomes whose guarded branches all fail, a spawned child's outcome that no `on name.done` or `on name.fail` takes, and guarded `after` expiries are still dropped and traced (an `after` timer's, not a delayed raise's). They arrive when another machine or the clock decides, so failing on them would make an ordinary race fatal. A `send` from a strict machine that reaches no running machine is [undelivered](actors.md#semantics), as from any machine: strict is about what a machine receives, and a peer that has ended is the same kind of race as an emitted event.
- **`ignore`.** `on Event ignore` takes the event and does nothing: nothing is left or entered, and no effect runs. It can have a guard (`on Knock if event.loud ignore`) and be a branch of a block (`if busy ignore`), and it stands alone: a branch with `ignore` has no target and no effects. It is a transition like any other, so the trace shows the event as taken, with a `transition` record that goes nowhere, and an inner state's `ignore` is taken before an outer state's `on` for the same event, as `Opened`'s `on Knock ignore` is above. Only an event's `on` transition (an emitted event's and a [peer's end](#when-a-peer-ends) among them) can ignore (`RB0026`): a timer, `always` or `done` with nothing to do can be removed, and a call is answered with `reply`, so `on reserve ignore` is an error too. `ignore` is allowed in any machine, not only a strict one; elsewhere it documents that an event is meant to be ignored, and turns a `dropped` record into a taken one.
- **The viewer** writes `strict` in the machine's signature, shows an `ignore` inside its state like any internal transition, and in simulate mode says of an event no active state handles that the run would end with a defect rather than drop it.

Decided on 27 November 2026 and built in IR 0.5: `strict` is the machine's `strict` member in the [IR](../../spec/ir.md#machines), so adding or removing it changes the machine's hash, and an `ignore` is a transition with no target and no effects.

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
- A state's [parameters and variables](#data-local-to-a-state) used wrongly: one read outside the state and the states inside it; one whose name is already in scope there, or is a local of a state inside it; one of a `ref` or `call` type; a state with parameters as an `initial` or as a region of a parallel state; a transition that enters a state with parameters without passing its arguments, or with arguments of the wrong number or types; and arguments for a state that takes none or that the transition does not enter, or for one state twice.
- A machine that returns `T` with a top-level final state that has no `output` or `fail`, and `output` or `fail` anywhere but a final state at the top level.
- A transition or `invoke` in a final state. A final state ends its parent as soon as it is entered, so they could never run; it holds only `entry`, `exit`, `output` and `fail`, and variables of its own.
- A `done` transition in a state with no child states: it waits for a final child, so it could never be taken. (An invoke's `done` goes inside `invoke { }`.)
- A history state in a state with no child states, or in a final state, and an `initial` that names a history state.
- An unguarded cycle of `always` transitions: states each with an `always` whose first branch has no `if` (an `else` counts as guarded, since it runs only when the guards before it fail), each leading to the next, where entering a state also enters its `initial` chain (and every region of a parallel state), and a transition to a state's own ancestor leaves and re-enters it. A transition to a history state counts as entering any state it could restore: every child of its parent with that child's default entry for a shallow one, every state below the parent for a deep one. An unguarded `always` with no target is the shortest such cycle, and so is an unguarded internal one (`always -> .X`): it never leaves its state, so it is taken again after every step, even when it enters a final child, since an `always` is taken before a `done`. So is an unguarded `always` written in the machine body, whatever its target, unless that target is a top-level final state: the machine itself is never left, so its `always` would be taken again after every step.
- `spawn`, `stop`, `send name[key]`, `send name.*`, `on name.done` and `on name.fail` through a name that is not a `children` collection, and a `send name.E` to a collection with no `[key]` or `.*`; a `spawn` of a machine or task other than the collection's; a key of the wrong type; a collection whose key type is not `string`, `int` or an enum, or whose upper-case name is not a machine or lower-case name not a task; `key` as the name of a parameter, variable, `as` or collection; `on name.fail E` for an error the collection's machine or task does not declare.
- `send`, `emit` and `on name.Event` that do not match a declaration: an unknown or out-of-scope `name`, or one that is not a `ref` (a `ref` that is optional or a list cannot be sent to); an event the receiver does not declare with `event`, or the sender (a machine or a task) with `emits`; a missing, extra or wrongly typed field. A `send` to a task, through its `as` name or a collection of tasks. A `send` to an invoked child where it is not running. An `as` on a call or on a task that declares no `emits`, or one that clashes with a variable, parameter or enclosing `as`. A `ref` to something that is not a machine, or in a field or a result.
- `on name.ended` through a name that is not a parameter or variable of type `ref M`, `ref M?` or `ref M[]` (an invoked child or a collection among them), and a lower-case word after `name.` other than `done`, `fail` and `ended`; `event` read in it other than as `event.status`; a declaration or event named `Ended`.
- `raise` of an event that is not one of the machine's own (`event` or `internal event`): an event it emits, another machine's, or an unknown one. `cancel` of a name that no `raise … after … as name` of the machine uses, and `as` on a raise without `after`. A `send` of another machine's `internal event`.
- Calls that do not match their declaration: an `invoke x.name` whose `x` is not a `ref` or whose machine has no call `name`; an `on name` or a `call name` type naming no call of the machine; arguments, `done` and `fail` outcomes checked as for a task, with `Unanswered` one more error to handle; a `reply` to something that is not a pending call, or that answers otherwise than its call is declared. A `call name` type anywhere but a machine variable's, `Unanswered` anywhere but a call invoke's `fail`, and `call` outside an `on name`.
- A sensitive value going where a trace would show it as it is, and a value with sensitive fields going where `json` is expected (see [sensitive values](#sensitive-values)).
- A machine that invokes itself, directly or through other machines, without an `after` or event in between (unbounded recursion). Precisely: machine `M` *starts* machine `N` if a state of `M` that invokes `N` can be entered from `M`'s initial states by taking only `always`, `done` and invoke outcome transitions, whatever their guards; or if a `spawn` of `N` is in the `entry` of such a state, in the effects of such a transition taken in such a state, or in the `exit` of the state such a transition is written in or of a state around it that the transition leaves (a transition to a top-level final state ends the run, so what it spawns never starts). An `on name.done` or `on name.fail` is an `on`, and counts as waiting: a supervisor that spawns a child again when it fails is fine. Only `on` and `after` transitions count as waiting; an invoke's outcome does not, since a task can finish at once. A transition to a history state counts as entering its parent's default entry: what a history state restores was entered before. A cycle of *starts* is an error.

Warnings:

- States that cannot be reached from the initial state, following every transition whatever its guard. Only the outermost unreachable state is reported. A transition to a history state reaches its parent's default entry, since a history state only restores states that were entered before.
- Non-final states with no way out: an atomic state that is reachable, where neither it nor any state containing it has a transition with a target (a `done` does not count: it needs a final child).
- Events, functions, actions or tasks that are declared but never used: an event no `on` of its machine names, or a function, action or task that nothing in the compilation calls, runs or invokes (a use in a `send` or `emit` counts). Only declarations in the files being checked are reported, and a use from any file checked at the same time counts, so checking a file of shared declarations on its own reports everything only other files use; check them together (`roadbook check` with no paths checks the whole project).
- A state with an `invoke` and no timeout anywhere above it: neither the state nor any state containing it has an `after` (off by default; `roadbook check --warn invoke-timeout`).
- An `emits E` that no `emit E` of its machine sends, and an `on name.E` for an `E` its sender declares but never emits, which can never be taken.
- A `children` collection that no `spawn` fills.
- An `emits E` that no `on name.E` in the checked files receives: a machine's, or a task's that no `on x.E` through an address of that task receives. As for unused declarations, a receiver in any file checked at the same time counts; host code holding a handle may still receive it.
- A `call` that no `on name` of its machine takes, so every such call is answered `Unanswered`, and a `call name` variable that no `reply` answers.
- A history state that no transition targets.
- A `set` of a state's variable in a transition that leaves that state, so that the value is discarded with the state's locals before anything can read it.
- An `internal event` that no `raise` of its machine raises, and a `raise` of an event that no `on E` and no `on *` of the machine takes.
- A transition that leaves a parallel state it is written in, to a target inside it, and so starts some of its regions over by default entry: one from a region to another region, or from a region to a state inside itself. The message names the regions that restart; a transition that targets the parallel state itself, lists a target in every region, or is internal does not warn.

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
  "fatal": null,                                 // or the message of a usage or I/O error
  "checks": [
    {
      "machine": "Pay",
      "check": "reachable Charged",              // the property, as formatted after `check`
      "doc": "A card can be charged.",           // its doc comment, or null
      "file": "examples/pay.road",
      "line": 30, "col": 3, "endLine": 30, "endCol": 26,
      "verdict": "conditional",                  // "proved", "conditional", "failed" or "unchecked"
      "assumptions": ["`ok()` holds"],           // what the path relies on that the chart does not decide
      "path": [ { "from": "Pay.Idle", "index": 0, "trigger": "Go", "guard": "ok()", "to": "Pay.Charged" } ]
    }
  ]
}
```

Ranges are as in the IR: lines and columns are 1-based, columns count Unicode scalar values, and the end is exclusive. A fix is a set of edits that together resolve the diagnostic: replace each range with its `text` (an empty range inserts). The command exits with 0 when there are no errors (warnings allowed), 1 when there are errors and 2 on a usage or I/O error, such as a file that does not exist. In that last case the object has no diagnostics and `fatal` holds the error's message, which is also printed on standard error. `checks` lists every `check` line of the checked files that compile without errors with its verdict, whether it passed or not (the checks of an entry with errors are not evaluated): a `failed` one is also an error among the diagnostics (`RB0501`–`RB0504`), and an `unchecked` one a warning (`RB0912`). `path` is the path the verdict rests on, from the initial configuration, as the transitions it takes, shaped as the steps of `roadbook paths --json` (`to` is `[]` for a transition with no target): the counterexample of a failed check, the witness of a `reachable` that holds, and for a conditional `can finish`, the way to the first configuration from which the host can't force the end and on from there to a final state. It lists only the transitions taken, leaving out the steps that take none (an expiry or outcome whose guards all failed, the end of settling), though what those assume is in `assumptions`. It is `[]` when there is none, as for a proved `never`. `assumptions` are what that path relies on that the chart does not decide (for `can finish`, the part from that configuration): a guard's outcome ("`` `approved()` holds ``", "`` `ready` fails ``"), a timer expiring while others run ("`` `after 5s` in `A` expires before the other running timers ``"), what another machine does ("`` `Checkout` (invoked in `Paying`) ends `done` ``", "`` peer `inventory` emits `Reserved` ``"), or that a step comes before the other machines that could act at that point ("`` `Child` (invoked in `A`) does not end or emit first ``"). A `reachable` is `proved` only by a path that relies on none, and a `can finish` only when the host can bring the run to a final state from every configuration whatever the guards decide and whatever other machines do first. Checks cover runs started from the chart: a run restored from a snapshot made by an earlier version of the chart may be in a configuration this one cannot reach, and a passing check says nothing about it.

### Definite assignment

A `var` with no initial value and a type that is not optional starts unassigned. Every other variable always has a value: a parameter, a variable with an initial value, and an optional variable with none, which starts as `null`. The same holds for a [state's](#data-local-to-a-state) parameters and variables, each time their state is entered: a parameter always has its argument, and a state variable with no initial value and a type that is not optional starts unassigned on every entry. The compiler proves that every read of a variable happens after the variable has a value, wherever the read is: a guard, a `set` value, the arguments of a `do`, the value of a `send` or `emit` and the `ref` variable a `send` goes through, the arguments of an `invoke`, an `after` delay, `output` and `fail`, and `entry` and `exit` effects. A read it cannot prove is an error (`RB0403`) at the read, with a label on a way in that leaves the variable unassigned. An `on name.Event` through a `ref` variable is not a read: an unassigned variable names no sender.

The analysis is a forward dataflow over the chart. A variable is assigned when a state is entered if it is assigned on every way into the state. A way in is the start of the machine or a transition with a target, and along it, in the order a step runs them, these assign:

- the source state, if the variable is assigned whenever the source is active (see below);
- the `exit` of the source and of each state containing it that the transition leaves (none for an internal transition);
- the transition's `set`s;
- the `entry` of each state entered before this one in the same step: the states containing it, outermost first, and the regions of a parallel state before its own, in document order.

So a `set` in a transition is seen by its target's `entry`, by the `entry` of the target's children and by the target's invoke. Within one `entry`, `exit` or transition the `set`s run first, in order, so a `set` sees the `set`s before it and a `do`, `send` or `emit` sees them all.

What each read sees:

- An `entry` sees what is assigned on every way into its state, and its own `set`s as above. `output` and `fail` see that and everything the final state's `entry` assigns.
- A guard and the effects of a transition, the state's invoke arguments and `after` delays, and its `exit` run while the state is active, and see what is assigned whenever it is: for an atomic state, what its `entry` left; for a compound state, what is assigned in the state itself or in every child, since it is always in one of them; for a parallel state, what is assigned in the state itself or in any region, since it is in all of them. A transition's effects also see the `exit`s that run before them.
- A `done` sees what is assigned in each final child it can complete through, and for a parallel state what is assigned in any region once every region has reached a final state.
- A machine variable is never unassigned again, so the `always` and `done` transitions taken while a step settles, and the invokes and timers started after it, see at least this much. A state variable is unassigned again only by entering its state again, and is read only inside that state.
- A state variable is analysed like a machine variable inside its state, with one difference: a way in that enters its state (one whose path from the domain to the target passes through it, or one that starts the machine) leaves it unassigned when its state is entered, whatever was assigned before, unless it has an initial value or an optional type. A way in that keeps its state active (a transition written inside the state, or an internal one written in it or above it, whose domain is the state or below it) carries what was assigned along it, as for a machine variable. So `set`s of a state variable in a transition that leaves its state assign nothing a later read can see (`RB0916` warns).
- An initial value sees the parameters and the variables declared before it that have an initial value or an optional type: initial values are computed in the order the variables are declared.

The analysis is conservative: it may reject a chart that never reads an unassigned variable when it runs. Where:

- Guards are not evaluated. Every transition whose state can be active is assumed to be taken, so a variable assigned only on a branch whose guard happens to hold is not assigned after the others.
- A transition with no target assigns only for its own effects, and an internal one only for its effects and the states it enters: the state can be left before it is taken, so the state's other transitions and its `exit` cannot rely on it.
- An `exit`, an invoke's arguments and an `after` delay see what was assigned when their state was entered, not what a transition taken inside it assigned later.
- Inside a parallel state, a region relies on what was assigned before it was entered (by the states containing it and by the regions before it, in order) and on its own transitions. What another region assigns after that does not count, since the regions take their transitions independently. The transitions written on the parallel state itself and its `done` see every region.
- Of the states a transition leaves, only the `exit`s of its source and the states containing it count, since which of the source's children is active is not known.
- States that can never be entered are not checked; they are already warned about (`RB0901`).
- A transition to a history state is a way into its parent's default entry, as if it targeted the parent (the parent itself is entered only if the transition leaves it). The states a history state restores add no way in: each was entered before, and a variable assigned then is still assigned. That holds for state variables too, since a restored state gets back the locals it had.

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
- **Spacing.** One space around binary operators, `->`, `=`, `!` and `|`, after `:` and `,`, and inside the braces of a one-line list or record value; none inside parentheses or the angle brackets of type arguments (`map<string, int>`, `type Page<T>`). A `retry` clause is written `retry 3 on A | B after delay`, one space between its parts. In a transition the target (several separated by `, `) and each `set`, `do`, `send`, `emit`, `reply`, `spawn` or `stop` clause are separated by two spaces: `-> Paying  set attempts = 0  do audit(order)`. Every `set` comes first, then the other clauses in the order written; consecutive clauses of one kind are merged for `set` and `do`: `do f()  set a = 1  set b = 2` is written `set a = 1, b = 2  do f()`. Each `send`, `emit`, `reply`, `spawn` and `stop` is a clause of its own, and an empty body is dropped: `send upload.Pause {}` is written `send upload.Pause`. No space goes inside the brackets of a key: `sessions[event.id]`, `sessions.*`.
- **Blank lines** between items are kept, at most one. There are none at the start or end of a block or file.
- **Lists.** No trailing commas. A list in a declaration (import names, fields, enum variants, parameters) with a line break right after its opening bracket stays multi-line: its elements are indented one level, keep the author's line breaks between them, and the closing bracket goes on its own line. Every other list is printed on one line.
- **Tasks that emit.** A task with exactly one `emits` clause keeps it on the line of its signature, `task ticks(every: duration) emits Tick`, unless the clause has a doc comment, which must stay right above it. A task with several writes each on a line of its own, indented two spaces, under the signature, aligned as `emits` lines are. There is no rule by width. An `invoke` with no outcomes is written with no braces: `invoke ticks(1s) as clock`.
- **One-line states.** A state whose body is empty, or is a single item other than a state, an `invoke` or a block of branches, and contains no comment, is written on one line: `final state Charged { output receipt }`. Every other state is written as a block. A history state is written on a line of its own: `history Previous`, `deep history Previous`.
- **Expressions** have only the parentheses precedence needs, plus `not (…)` around a comparison or arithmetic operand, and parentheses around a record value used as an `after` delay or a retry's delay. Numbers are written as parsed (`007` becomes `7`, `1.50` becomes `1.5`). `error E {}` is written `error E`, and a record value with no fields `E {}` is written `E`.
- **Comments** are kept as written, trailing spaces removed. A comment on its own line stays on its own line, indented like the code that follows it. A comment after code stays at the end of that line, including one after the closing bracket of a multi-line list, which ends the line that bracket starts (`) { // c`, `} from "./x.road" // c`). A comment inside something printed on one line moves to its own line before it; for the line a multi-line list's closing bracket starts (`) -> T ! E {`), that is into the list, before the bracket. Comments after that line belong to what follows it.
- **Alignment.** An alignment group is a run of consecutive lines of one kind with no blank line or comment line between them. The kinds and what they align:
  - one-line `type`, `enum`, `error` and `extern type` declarations: what follows the name and its type parameters (`{` or `=`);
  - one-line `fn`, `action` and `task` declarations, and a machine's `call` lines: `->` and `!`;
  - `var` lines: the type;
  - `children` lines: the machine, and `by`;
  - `event` and `emits` lines, together: the field list;
  - one-line states: the `{`;
  - transitions and invoke outcomes, together with the header, branches and closing `}` of a block of branches among them: `->` and the first `set` or `do`, at the same column for nested branches as for the lines around them.

  Each aligned column starts one space after the widest text of the column before it, measured from the start of the line, over every line of the group (closing `}` lines excepted), including lines that end before that column. In transitions the effects column starts two spaces after the widest target. Lines are not padded at the end.

## Diagnostic codes

Codes are `RB` and four digits, grouped by range: `RB00xx` syntax, `RB01xx` names, `RB02xx` structure, `RB03xx` types, `RB04xx` analyses, `RB05xx` properties (a `check` line that fails, which only `roadbook check`, the language server and `@dire-kiwi/roadbook-cli check` evaluate) and `RB09xx` warnings. The syntax codes:

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
| `RB0013` | A keyword, or `result`, used as a name, or a declaration or event named `Unanswered` or `Ended`. | `var on: int`, `var result: int`, `type Evt { type: string }`, `error Unanswered`, `enum Ended { A }` |
| `RB0014` | `done`, `invoke`, `output`, `fail` or a history state directly in a machine body. | `machine M() { done -> A }`, `machine M() { history H }` |
| `RB0015` | `event`, `internal event`, `emits`, `call`, `children` or `check` inside a state. (`var` is allowed there: a [state variable](#data-local-to-a-state).) | `state A { event Go }`, `state A { check can finish }` |
| `RB0016` | A machine item at the top level of a file. | `state A {}` or `check can finish` outside a machine |
| `RB0017` | A transition with no target, `set`, `do`, `send`, `emit`, `reply`, `spawn`, `stop`, `raise`, `cancel` or `ignore`. | `on Go` |
| `RB0018` | A record value as an `after` delay, or a retry's delay, without parentheses. | `after Cfg { ms: 5 } -> A` |
| `RB0019` | An invoke outcome outside `invoke { }`. | `fail CardDeclined -> Declined` in a state |
| `RB0020` | Something other than an outcome inside `invoke { }`. | `fail declined`, `on Go -> A` in an invoke |
| `RB0021` | A block of branches that does not start with `if`, or has a branch after `else`. | `on Go { else -> A }` |
| `RB0022` | Chained comparisons. | `if a < b < c` |
| `RB0023` | A `///` comment that documents nothing. | `/// note` above a transition |
| `RB0024` | A lower-case type name that is not a built-in type. | `x: integer` |
| `RB0025` | Nesting more than 64 levels deep. | 65 nested states, or `((((…))))` |
| `RB0026` | `ignore` outside an event's `on` transition (a call's too), or with a target or effects. | `after 5s ignore`, `on Go ignore -> A` |
| `RB0027` | `on name.word` with a lower-case `word` other than `done`, `fail` and `ended`. | `on inventory.end`, `on peer.reserved` |
| `RB0028` | `as name` on a `raise` without `after`: only a delayed raise has a name. | `raise Ping as ping` |

The compiler's codes:

| Code | Meaning | Example |
| --- | --- | --- |
| `RB0101` | An unknown type or error name. | `order: Ordr` |
| `RB0102` | An unknown task or machine in `invoke`. | `invoke chrgeCard(order)` |
| `RB0103` | An unknown function. | `if isVald(order)` |
| `RB0104` | An unknown action in `do`. | `do audt("x")` |
| `RB0105` | An unknown variable, or a state's parameter or variable read outside that state. | `set attemps = 1`, `tries` in `B` for `var tries` in its sibling `A` |
| `RB0106` | An event the machine does not declare. | `on Cancl -> Done` |
| `RB0107` | Two declarations with the same name in one compilation, or an event or type parameter named like a type, error or machine. | `type Order` in two files, `event Order`, `type Page<Order>` |
| `RB0108` | Two fields, parameters, type parameters, variants, variables, events or sibling states with the same name, or a name imported twice. A machine's `event`, `internal event` and `emits` names are one namespace, and the name of a delayed raise shares one with its parameters, variables and collections. | `state A {}` twice in one parent, `import { B, B }`, `event Go` and `internal event Go`, `raise Go after 1s as attempts` beside `var attempts: int` |
| `RB0109` | A name used as something it is not. | `invoke audit(x)` where `audit` is an action |
| `RB0110` | `event`, `result`, `error`, `call` or `key` where it is not in scope. | `entry set n = result`, `on Go set p = call` |
| `RB0111` | An import of a file that cannot be read. | `import { A } from "./missing.road"` |
| `RB0112` | An import of a name the file does not declare. | `import { Ordr } from "./types.road"` |
| `RB0113` | An enum variant that does not exist. | `Role.Admn` |
| `RB0114` | A `set` of a machine's or a state's parameter. | `set token = ""` |
| `RB0115` | A `send name.E` or `on name.E` whose `name` is not an `invoke … as name` in scope or a parameter or variable, or an `invoke name.call(…)` or `on name.ended` whose `name` is not a parameter or variable. | `send uplod.Pause`, `upload` outside its invoking state, `invoke invntory.reserve()`, `on invntory.ended` |
| `RB0116` | A `send` of an event the receiving machine does not declare with `event` (an `internal event` of it among them, which only it can raise), or a `send` to a task, which takes no events. | `send upload.Stop`, `send child.Ping` for `internal event Ping` in the child, `send up.Pause` for `invoke upload(f) as up`, `send uploads[k].Pause` for `children uploads: upload by string` |
| `RB0117` | An `emit` of an event the machine does not declare with `emits`, or an `on name.E` of one its sender, a machine or a task, does not. | `emit Progres`, `on upload.Pause`, `on up.Progres` |
| `RB0118` | A call the machine does not declare: in `invoke x.name`, `on name` or a `call name` type. | `invoke inventory.reserv(id)`, `on reserv` |
| `RB0119` | `Unanswered` where it cannot be: in a `!` list, as a type or a value, or in a `fail` outcome of an invoke that is not a call. | `call reserve() ! Unanswered`, `fail Unanswered` after a task |
| `RB0120` | `attempt` in a retry delay of a machine with a parameter or variable named `attempt`, or of a state where a state's local of that name is in scope. | `retry 3 after backoff(attempt)` with `var attempt: int` |
| `RB0121` | A `spawn`, `stop`, `send name[key]`, `send name.*`, `on name.done` or `on name.fail` through a name that is not a `children` collection, or a `send name.E` to a collection without a key or `*`. | `stop upload[1]`, `send sessions.Notify` |
| `RB0122` | `key` as the name of a machine or state parameter, a variable, an `as` name or a collection. | `var key: string` |
| `RB0123` | A state path in a `check` that names no state of the machine, or names a history state. | `check reachable Applyng`, `check never Work.Last` for `history Last` |
| `RB0124` | `on name.ended` through an `invoke … as name` child or task, or a `children` collection, whose end is its outcome. | `on upload.ended`, `on up.ended`, `on sessions.ended` |
| `RB0125` | A `raise` of an event that is not one of the machine's own events or internal events: an event it emits, another machine's, or an unknown one. | `raise Progress` for `emits Progress`, `raise Unknown` |
| `RB0126` | A `cancel` of a name that no `raise … after … as name` of the machine uses. | `cancel reminder` with no `raise … as reminder` |
| `RB0127` | A state's parameter or variable whose name is already in scope in the state, or is a local of a state inside it: a machine parameter or variable, a collection, the name of a delayed raise, an `as` name in scope, or a local of a state around it. | `state Retrying(attempts: int)` with `var attempts: int`, `var n` in both `A` and `A.B` |
| `RB0201` | A transition target that does not resolve, or a path through a history state. | `-> Dne`, `-> Working.Previous.Editing` |
| `RB0202` | A compound state or machine without `initial`, or a machine with no states. | `state A { state B {} }`, `machine M() { initial A }` |
| `RB0203` | An `initial` that is not a child state, is written twice, or is in a state without children. | `initial Strat`, `initial Previous` for `history Previous` |
| `RB0204` | An `invoke` that leaves a declared error unhandled, or for a call `Unanswered`. | see above |
| `RB0205` | The same trigger twice in one state or one `invoke`. | `on Go -> A` and `on Go -> B` |
| `RB0206` | A second `invoke` in a state. | two `invoke`s in `state A` |
| `RB0207` | An outcome, or a retry, for an error the task, machine or call does not declare (a call's `Unanswered` cannot be retried). | `fail Timeout -> A`, `retry 3 on Timeout` |
| `RB0208` | `output` or `fail` outside a top-level final state, or two of them. | `state A { output 1 }` |
| `RB0209` | A top-level final state with no `output` or `fail` in a machine that returns a value. | `final state Done {}` in `machine M() -> int` |
| `RB0210` | A final state with children, or a parallel state with none. | `final state A { state B {} }` |
| `RB0211` | A transition or `invoke` in a final state. | `final state Done { on Go -> A }` |
| `RB0212` | A `done` transition in a state with no child states, which can never be taken. | `state A { done -> B }` |
| `RB0213` | A `send` to an invoked child where it is not running. | `entry send upload.Pause` in the invoking state, `on Cancel -> Idle  send upload.Stop` |
| `RB0214` | `as` on an invoke of a call, or of a task that declares no `emits`. | `invoke fetch() as f` for `task fetch() -> int`, `invoke inventory.reserve(id) as r` |
| `RB0215` | `retry` on the invoke of a state with child states. | `state A { invoke f() retry 2 { … }  initial B  state B {} }` |
| `RB0216` | `retry` on an invoke with `as`. | `invoke Upload(file) as upload retry 2` |
| `RB0217` | A `retry` that retries nothing, or names an error twice. | `retry 0`, `retry 2` of a task that declares no errors, `retry 2 on Busy \| Busy` |
| `RB0218` | A retried error handled by a `fail *` that also handles an error not retried (for a call, `Unanswered` too). | `retry 2 on Busy` with only `fail * -> Failed` for `! Busy \| Gone` |
| `RB0219` | A history state in a state with no child states, or in a final state. | `state A { history H }` |
| `RB0220` | A `spawn` of a machine or task that is not its collection's. | `spawn sessions[id] = Upload(f)` for `children sessions: Session by string`, `spawn uploads[id] = fetch()` for `children uploads: upload by string` |
| `RB0221` | Several targets of which some have the leading dot and some do not. | `-> .Playback.Paused, Volume.Muted` |
| `RB0222` | Several targets that are not each in a different region of a parallel state: two in one compound state, one inside another, or the same one twice. | `-> Cart.Editing, Cart.Reviewing` |
| `RB0223` | A state with parameters as a compound state's `initial` or as a region of a parallel state, which are entered with no arguments. | `initial Confirming` for `state Confirming(token: ResetToken)` |
| `RB0224` | A transition that enters a state with parameters without passing its arguments. | `-> Confirming`, `-> Editing.Changing` from outside `Editing(doc: Doc)` |
| `RB0225` | Arguments for a state that takes none, for one the transition does not enter, or for one state twice. | `-> Idle(1)`, `-> Editing(doc).Changing` written inside `Editing` |
| `RB0301` | A value of the wrong type. | `var ratio: float = 1` |
| `RB0302` | The wrong number of arguments. | `retryDelay(1, 2)` |
| `RB0303` | An operator on operands it is not defined on. | `attempts + 1.5`, `order == other` |
| `RB0304` | A field the record, error or event does not have. | `order.idd` |
| `RB0305` | A record or error value without one of its fields. | `CardDeclined {}` |
| `RB0306` | A field read from a value that has none, or that is optional. | `attempts.size`, `draft.coupon.code`, `event.x` in `on Cancel` where `event Cancel` has no fields |
| `RB0307` | `result` after a task or machine that returns nothing. | `done set n = result` |
| `RB0308` | `error` in a `fail *` that covers several errors, or none. | `fail * set why = error.reason` |
| `RB0309` | `event` in `on *`, or used whole in an `on name` that takes a call or an `on name.ended`. | `on * do log(event)`, `on reserve do log(event)`, `on peer.ended do log(event)` |
| `RB0310` | `output` in a machine that returns nothing. | `machine M() { … output 1 }` |
| `RB0311` | A `fail` value that is not a declared error of the machine. | `fail "late"` |
| `RB0312` | An `int` or `duration` literal beyond ±(2^53 − 1). | `9007199254740992` |
| `RB0313` | A type alias that refers to itself. | `type A = A[]` |
| `RB0314` | A type made optional twice. | `note: string??` |
| `RB0315` | A `send`, `on name.E` or `invoke name.call(…)` through a value that is not a `ref`, or a `ref` that is optional or a list; an `on name.ended` through one that is not a `ref M`, `ref M?` or `ref M[]`. | `send count.Ping` for `count: int` |
| `RB0316` | A `ref` in a field of a record, error or event, in a parameter of a `call`, in what a function, task or machine returns, or in a type argument. | `type Holder { peer: ref Peer }`, `Box<ref Peer>` |
| `RB0317` | A `call name` type anywhere but a machine variable's, or a variable of it with an initial value. | `var p: call reserve?`, `fn f(p: call reserve) -> int` |
| `RB0318` | A `reply` to something that is not a pending call. | `reply count done` for `count: int` |
| `RB0319` | A `reply` that does not answer as its call is declared. | `reply call done` for a call that returns `int`, `reply call fail Gone` |
| `RB0320` | A value read from a sensitive field, parameter or variable (or computed from one by an operator), going where a trace shows it: a variable, a parameter of an action, task, machine, call or state, or a field that is not `sensitive`, or an `output`, a `fail`, a call's reply, an `after` delay or a retry's delay. | `set note = event.password` for `var note: string` |
| `RB0321` | A value whose type has `sensitive` fields, going where `json` is expected in a place a trace shows. | `do log(creds)` for `action log(what: json)` |
| `RB0322` | A `children` key type that is not `string`, `int` or an enum (or an alias of one). | `children s: Session by float` |
| `RB0323` | A `map` whose key type is not `string` or an alias of it. | `map<int, Order>` |
| `RB0324` | A generic type without its type arguments or with the wrong number of them, or type arguments on a type that takes none. | `page: Page`, `Order<int>` |
| `RB0325` | A type parameter its declaration does not use. | `type Id<T> = string` |
| `RB0326` | A generic type that refers to itself, directly or through other types, with other arguments than its own parameters. | `type Nested<T> { inner: Nested<T[]>? }` |
| `RB0327` | A record value of a generic type whose type arguments neither the expected type nor its fields tell. | `describe(Pair { first: 1, second: "one" })` |
| `RB0328` | A state's parameter or variable of a `ref` or `call` type, which only the machine's can have. | `state Talking(peer: ref Inventory)` |
| `RB0401` | An unguarded cycle of `always` transitions. | `A { always -> B }`, `B { always -> A }`, `always -> .Child` |
| `RB0402` | A machine that invokes or spawns itself without waiting. | `state A { invoke M() { … } }` as `M`'s initial state |
| `RB0403` | A read of a variable that might not be assigned yet. | `entry do log(n)` in the initial state, for `var n: int` |
| `RB0501` | A `check never` whose state can be active, or whose states can be active together. | `check never Refunded` |
| `RB0502` | A `check reachable` whose state no path reaches, even with every guard, timer order and other machine free. | `check reachable Charged` with nothing targeting `Charged` |
| `RB0503` | A `check S only after T` where `S` can be active before `T` has been. | `check Refunded only after Charged` |
| `RB0504` | A `check can finish` where the machine can reach a configuration from which no top-level final state can be reached. | `state Loop { on Go -> Loop }` |
| `RB0901` | A state that can never be entered (warning). | a state nothing targets |
| `RB0902` | A non-final state with no way out (warning). | `state Stuck {}` |
| `RB0903` | An event, function, action or task nothing uses (warning). | `event Unused` |
| `RB0904` | An `invoke` with no `after` around it (warning, off by default). | `state A { invoke fetch() { … } }` |
| `RB0905` | An `emits` the machine never emits (warning). | `emits Progress` with no `emit Progress` |
| `RB0906` | An `on name.E` for an event its sender never emits (warning). | `on upload.Progress` when `Upload` has no `emit Progress` |
| `RB0907` | An `emits` that nothing in the checked files receives (warning): a machine's, or a task's that no `on x.E` through an address of that task receives. | `emits Progress` with no `on upload.Progress` anywhere |
| `RB0908` | A `call` no `on name` of its machine takes (warning). | `call count()` with no `on count` |
| `RB0909` | A `call name` variable no `reply` answers (warning). | `var pending: call reserve` with no `reply pending …` |
| `RB0910` | A history state that no transition targets (warning). | `history Previous` with no `-> Working.Previous` |
| `RB0911` | A `children` collection that no `spawn` fills (warning). | `children sessions: Session by string` with no `spawn sessions[…]` |
| `RB0912` | A machine whose checks were not evaluated, as too large to explore: more than 100000 configurations, a peer's event expected through more than 8 `ref` names, or regions whose guard outcomes combine in too many ways (warning). | many parallel regions, each with deep history |
| `RB0913` | A transition that leaves a parallel state and enters it again, starting regions over that it does not target (warning). | `on Cross -> P.B.Y` in region `A` of `P` |
| `RB0914` | An `internal event` that no `raise` of the machine raises, so it can never happen (warning). | `internal event Ping` with no `raise Ping` |
| `RB0915` | A `raise E` where no `on E` and no `on *` of the machine takes `E`, so it is always dropped, or `unhandled` in a strict machine (warning). | `raise Ping` with no `on Ping` |
| `RB0916` | A `set` of a state's variable in a transition that leaves that state, so the value is discarded with it (warning). | `on Retry -> Trying  set tries = tries + 1` written in `Trying` |

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
              | "ref" Upper | "call" lower | "map" "<" type "," type ">" )
              { "[]" | "?" } ;
typeArgs    = "<" type { "," type } ">" ;

fnDecl      = "fn" lower params "->" type ;
actionDecl  = "action" lower params ;
taskDecl    = "task" lower params [ "->" type ] [ errors ] { emitsDecl } ;
params      = "(" [ field { "," field } ] ")" ;
errors      = "!" Upper { "|" Upper } ;

machineDecl = [ "strict" ] "machine" Upper params [ "->" type ] [ errors ] "{" { machineItem } "}" ;
machineItem = varDecl | eventDecl | emitsDecl | callDecl | childrenDecl | checkDecl | commonItem ;
varDecl     = [ "sensitive" ] "var" lower ":" type [ "=" expr ] ;
eventDecl   = [ "internal" ] "event" Upper [ fields ] ;
emitsDecl   = "emits" Upper [ fields ] ;
callDecl    = "call" lower params [ "->" type ] [ errors ] ;
childrenDecl = "children" lower ":" name "by" type ;
checkDecl   = "check" property ;
property    = "never" statePath { "and" statePath }
            | "reachable" statePath
            | statePath "only" "after" statePath
            | "can" "finish" ;
statePath   = Upper { "." Upper } ;

stateDecl   = [ "final" | "parallel" ] "state" Upper [ params ] "{" { stateItem } "}" ;
stateItem   = commonItem
            | varDecl
            | [ "deep" ] "history" Upper
            | "done" branches
            | "invoke" [ lower "." ] name "(" [ expr { "," expr } ] ")" [ "as" lower ] [ retry ]
              [ "{" outcome { outcome } "}" ]
            | "output" expr
            | "fail" expr ;
commonItem  = "initial" Upper
            | stateDecl
            | "entry" effects
            | "exit" effects
            | "on" ( Upper | "*" | lower "." ( Upper | "done" | "fail" ( Upper | "*" ) | "ended" ) | lower ) branches
            | "after" expr branches
            | "always" branches ;
retry       = "retry" int [ "on" Upper { "|" Upper } ] [ "after" expr ] ;
outcome     = "done" branches
            | "fail" ( Upper | "*" ) branches ;

branches    = [ "if" expr ] action
            | "{" "if" expr action { "if" expr action } [ "else" action ] "}" ;
action      = "->" targets [ effects ]
            | effects
            | "ignore" ;
effects     = effect { effect } ;
effect      = "set" assign { "," assign }
            | "do" call { "," call }
            | "send" lower [ "[" expr "]" | "." "*" ] "." Upper [ recordBody ]
            | "emit" Upper [ recordBody ]
            | "reply" ( "call" | lower ) ( "done" [ expr ] | "fail" expr )
            | "spawn" lower "[" expr "]" "=" name "(" [ expr { "," expr } ] ")"
            | "stop" lower "[" expr "]"
            | "raise" Upper [ recordBody ] [ "after" expr [ "as" lower ] ]
            | "cancel" lower ;
recordBody  = "{" [ lower ":" expr { "," lower ":" expr } ] "}" ;
assign      = lower "=" expr ;
call        = lower "(" [ expr { "," expr } ] ")" ;
targets     = target { "," target } ;
target      = [ "." ] segment { "." segment } ;
segment     = Upper [ "(" [ expr { "," expr } ] ")" ] ;

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
            | lower | "event" | "error" | "call"
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
- All keywords are reserved: `import type enum error extern fn action task machine strict var event emits call initial state final parallel entry exit on after always done invoke output fail if else set do send emit reply ignore and or not true false`. `from`, `as`, `retry`, `ref`, `map`, `sensitive`, `history`, `deep`, `children`, `by`, `spawn`, `stop`, `raise`, `cancel`, `internal`, `check`, `never`, `reachable`, `only`, `can`, `finish` and the built-in type names are not reserved, because they only have a meaning in a position where no name can appear: after an import list, after an invoke's arguments (`as` and `retry`) and after a delayed raise's delay (`as`), at the start of a type, and before a field or parameter name and its `:`, or before `var` (so `sensitive: bool` is a field named `sensitive`), at the start of an item of a state body (`history`, `deep`), in `children name:` at the start of a machine item, after a collection's machine, in `spawn name[`, `stop name[`, `raise Name` and `cancel name` at the start of an effect (so `action stop()` is still a valid action), in `internal event` at the start of a machine item, and in a check: `check` followed by a name at the start of a machine item, and the words of its property after it (so `var check: int` is still a variable). `event`, `error` and `result` are the implicit names inside transitions and cannot be used for variables or parameters; nor can `key`, though it can name a field. (`call` is a keyword.)
- In a type, the `>` closing type arguments may be written as the first character of a `>=`: `var m: map<string, int>= f()` reads as written (the formatter writes `> =` with a space).
- A `///` comment documents the declaration, machine, state, history state, event, variable or check that follows it. A `///` anywhere else is an error: use `//`.
- A single-line transition and a multi-branch block are told apart by the `{` after the trigger. For that reason a record value in an `after` delay must be parenthesised, as with `if` in Rust, and so must one in a retry's delay, which the invoke's `{` follows. A `{` after the event of a `send`, `emit` or `raise` is always its record body.
- `on name.Event` is told from `on Event` by the lower-case name and the `.` after it, and `on name`, which takes a call, by a lower-case name with no `.`. Likewise `invoke x.name(…)` is a call, told from `invoke name(…)` by the `.`.
- In `reply x done value`, the value is parsed only if it starts on the same line as `done`.
- `after` goes on with a `raise` only on the line where the raised event, or its `{ … }`, ends. On a later line it starts an `after` transition. `as` names a raise only after its delay (`RB0028` without one).
- In a final state, `fail expr` is the machine's failure. Inside `invoke { }`, `fail Upper` is an outcome. The two never appear in the same position.
- Two `after` transitions in one state are the same trigger only if their delays are the same expression, written the same way once formatted: `after 1s` and `after 1000ms` are different triggers.
- A type is optional at most once: `T??` is an error.
- Nesting is at most 64 levels deep, counting states, parentheses, operators (each `+` in `a + b + c` is a level), field reads and type suffixes (`[]`, `?`), so every tool that walks the tree stays within its stack.
- A final state holds only `entry`, `exit`, `output`, `fail` and variables.
- A target's arguments follow the state's name directly; the `(` after a target's name always starts them, since effects start with a keyword.
- `ignore` is an action only in an `on` transition (`on Event`, `on *`, `on name.Event`).
- Declaration names are unique among the files compiled together: an entry file and everything it imports, directly or not, because the IR is keyed by name. Two charts that do not import one another may use the same names. A file can use only what it declares or imports. An event cannot have the name of a type, error or machine either, because the IR names the type of `event` in `on Name` by the event's name.

Common mistakes, each reported by the compiler but easy to make:

- There is no day unit: 14 days is `336h`.
- Keywords cannot be names, so a field for an event's kind is `kind`, not `type`; and `event`, `error` and `result` cannot be variables or parameters, since they are the implicit names.
- A state names its own child through its own name: `-> Parent.Child` written in `Parent`, because a bare `-> Child` is looked up among `Parent`'s siblings.
- `int` and `float` do not mix: `ratio * 2` is an error for a `float` `ratio`; write `2.0`.
- A record value as an `after` delay needs parentheses: `after (Cfg { ms: 5 }) -> A`.

Most of these rules, the split between `machineItem` and `stateItem`, and the requirement that a branch does something came from writing the spike's parser against the first draft of this grammar. That parser was deleted once the Rust compiler replaced it.
