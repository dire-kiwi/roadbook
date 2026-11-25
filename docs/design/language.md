# The Waymark language

A `.way` file declares data shapes, the host functions a machine needs, and the machines themselves. It contains no implementation code. The compiler turns it into the [IR](semantics.md#the-ir), which everything else consumes.

The examples on this page come from [`examples/shop`](../../examples/shop), which the spike compiles and renders.

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

Everything a machine refers to is declared, with types. Declarations live at the top level of any file and are shared with `import`.

| Declaration | Meaning | Implemented by |
| --- | --- | --- |
| `type User { id: string, role: Role }` | A record. | Generated |
| `type UserId = string` | An alias. | Generated |
| `enum Role { Customer, Admin }` | A closed set of names. | Generated |
| `error CardDeclined { reason: string }` | A tagged error. Also usable as a type. | Generated |
| `extern type Money` | A type that exists only in host code. Opaque to the compiler. | Host, bound per language in `waymark.toml` |
| `fn retryDelay(attempt: int) -> duration` | A pure, synchronous function. Used in guards and expressions. | Host |
| `action audit(what: string, order: Order)` | A side effect that cannot fail. Awaited, in order. | Host |
| `task chargeCard(order: Order) -> Receipt ! CardDeclined` | Effectful, fallible, cancellable work. Only run by `invoke`. | Host |
| `machine Checkout(...) -> Receipt ! ... { }` | A statechart with a task's signature. | The chart |

Built-in types are `string`, `int` (64-bit signed), `float`, `bool`, `duration` and `json` (any JSON value). `T?` is optional and `T[]` is a list.

Names starting with an upper-case letter are types, errors, events, states and machines. Names starting with a lower-case letter are variables, fields, functions, actions and tasks. The grammar enforces this, which keeps `invoke chargeCard(...)` (a task) and `invoke Checkout(...)` (a machine) visibly different.

Host functions receive exactly the arguments the chart passes them. They never see the machine's variables or the current event. This is the main difference from xstate, where every guard and action receives the whole context: here the chart is the complete wiring diagram of both control and data, and the host code is a set of small typed functions that know nothing about the machine that calls them.

## Inside a machine

| Item | Meaning |
| --- | --- |
| `var name: T [= expr]` | A machine variable. |
| `event Name [{ fields }]` | An event the machine accepts from outside. |
| `initial Name` | The child state entered first. Required, once, when a state has children. |
| `state Name { ... }` | A state. Atomic if it has no children, compound if it does. |
| `parallel state Name { ... }` | All children are active at once (regions). |
| `final state Name { ... }` | Ends the enclosing compound state, or the machine at the top level. |

## Inside a state

| Item | Meaning |
| --- | --- |
| `entry set ... ` / `entry do ...` | Run when the state is entered. |
| `exit set ...` / `exit do ...` | Run when the state is left. |
| `on Event ...` | Transition on an external event. `on *` matches any event this state has no `on` for. |
| `after 30s ...` | Transition after the state has been active for a duration. The delay is an expression. |
| `always ...` | Transition taken as soon as its guard holds (no event). |
| `done ...` | Transition taken when a compound state reaches a final child, or every region of a parallel state does. |
| `invoke name(args) { done ...  fail Err ... }` | Run a task or machine while the state is active. At most one per state. `fail *` matches the declared errors not listed. |
| `output expr` / `fail expr` | Final states at the top level only: the machine's result. |

A transition has up to four parts, all optional except that it must do something:

```
on Submit if isValid(order) -> Paying  set attempts = 0  do audit("submitted", order)
   ^trigger ^guard             ^target ^assignments      ^actions
```

With no `-> Target` the transition is internal: the state is not left and re-entered. `set` assignments run before `do` actions.

Inside a transition, three names are in scope besides the machine's variables: `event` (in `on`), `result` (in an invoke's `done`) and `error` (in an invoke's `fail`).

A target is resolved like a name in a block-structured language: first among the siblings of the state the transition is written in, then among its parent's siblings, and so on outward. A dotted path such as `Processing.Paying` then descends into a nested state, which is also how a state targets one of its own children. A transition written at machine level resolves among the top-level states. An unknown target is a compile error, and so is a duplicate state name among siblings.

`entry`, `exit`, `on`, `after` and `always` may also be written directly in the machine body, where they apply to the whole machine.

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

## What the compiler checks

Errors (the build fails):

- Unknown or duplicate names: states, events, variables, functions, actions, tasks, types.
- Type errors in arguments, assignments, guards, `output` and `fail`.
- A compound state without `initial`, or an `initial` that is not a child.
- An `invoke` that does not handle every declared error of the task or machine.
- The same trigger written twice in one state. Several branches go in one block, where their order is explicit and only the last may be unguarded, so a branch can never be shadowed by an earlier one.
- A read of a variable that might not be assigned yet. <a id="definite-assignment"></a>
- A machine that returns `T` with a top-level final state that has no `output` or `fail`.
- An unguarded cycle of `always` transitions.
- A machine that invokes itself, directly or through other machines, without an `after` or event in between (unbounded recursion).

Warnings:

- States that cannot be reached from the initial state.
- Non-final states with no way out (no transition, no invoke).
- Events, functions, actions or tasks that are declared but never used.
- A state with an `invoke` and no timeout anywhere above it (off by default).

Diagnostics carry a code, a source span and, where possible, a fix. `waymark check --json` prints them in a machine-readable form so an agent can act on them without parsing prose.

```
error[W0204]: `invoke chargeCard` does not handle `GatewayUnavailable`
  --> examples/shop/checkout.way:14:5
   |
14 |     invoke chargeCard(order, user) {
   |     ^^^^^^^^^^^^^^^^^ declared as `-> Receipt ! CardDeclined | GatewayUnavailable`
   |
   = help: add `fail GatewayUnavailable -> <State>` or `fail * -> <State>`
```

### Definite assignment

A `var` with no default and a non-optional type starts unassigned. The compiler runs a forward dataflow analysis over the chart: a variable is assigned in a state if every transition that can enter that state either assigns it or leaves from a state where it was already assigned. Reading a variable in a state where it is not assigned is an error.

In `PlaceOrder`, `order` is assigned on the only way into `Pricing`, so `invoke priceOrder(order, user)` is accepted. Change `Validating`'s `done` branch to drop `set order = result` and the compiler points at the read in `Pricing`.

This is what lets host functions take plain non-optional arguments. In xstate the same machine would declare `order: Order | undefined` and every reader would assert it is present.

## Formatting

`waymark fmt` is canonical: one accepted layout, no options. Agents and humans produce the same text, so diffs show only real changes.

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
type        = ( Upper | "string" | "int" | "float" | "bool" | "duration" | "json" )
              { "[]" | "?" } ;

fnDecl      = "fn" lower params "->" type ;
actionDecl  = "action" lower params ;
taskDecl    = "task" lower params [ "->" type ] [ errors ] ;
params      = "(" [ field { "," field } ] ")" ;
errors      = "!" Upper { "|" Upper } ;

machineDecl = "machine" Upper params [ "->" type ] [ errors ] "{" { machineItem } "}" ;
machineItem = varDecl | eventDecl | commonItem ;
varDecl     = "var" lower ":" type [ "=" expr ] ;
eventDecl   = "event" Upper [ fields ] ;

stateDecl   = [ "final" | "parallel" ] "state" Upper "{" { stateItem } "}" ;
stateItem   = commonItem
            | "done" branches
            | "invoke" name "(" [ expr { "," expr } ] ")" "{" outcome { outcome } "}"
            | "output" expr
            | "fail" expr ;
commonItem  = "initial" Upper
            | stateDecl
            | "entry" effects
            | "exit" effects
            | "on" ( Upper | "*" ) branches
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
            | "do" call { "," call } ;
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
            | Upper [ "." Upper | "{" [ lower ":" expr { "," lower ":" expr } ] "}" ]
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
- All keywords are reserved. `event`, `error` and `result` are the implicit names inside transitions and cannot be used for variables or parameters.
- A `///` comment documents the declaration, machine, state, event or variable that follows it.
- A single-line transition and a multi-branch block are told apart by the `{` after the trigger. For that reason a record value in an `after` delay must be parenthesised, as with `if` in Rust.
- In a final state, `fail expr` is the machine's failure. Inside `invoke { }`, `fail Upper` is an outcome. The two never appear in the same position.
- Two `after` transitions in one state are the same trigger only if their delays are the same expression.
- Declaration names are unique across a project, because the IR is keyed by name. A file can use only what it declares or imports.

Most of these rules, the split between `machineItem` and `stateItem`, and the requirement that a branch does something came from writing the spike's parser against the first draft of this grammar. The spike still implements that draft, which accepts everything above and is slightly more lenient.

## Not in the first version

Each of these has a planned shape, and the grammar leaves room for it.

- **History states.** `history` and `deep history` pseudo-states, as in SCXML.
- **Talking to a running child.** `send` to an invoked machine and `emit` from it. The first version gives a child its arguments and gets back its result.
- **Many children at once.** `spawn` for a dynamic number of child machines. `invoke` covers exactly one per state.
- **Generic types and maps.** `map<string, T>` and user-defined generics.
- **Retry sugar.** `invoke chargeCard(order) retry 3` as shorthand for the `Backoff` pattern above. The explicit states stay available and are what the diagram shows either way.
