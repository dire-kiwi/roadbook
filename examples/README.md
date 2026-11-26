# Examples

Each directory is one project: `roadbook check examples/<dir>` checks it as a whole.

- [`shop/`](shop): `POST /orders` (authenticate, validate, price, pay, confirm) and the `Checkout` payment machine it invokes, with retry and backoff.
- [`password-reset/`](password-reset): `POST /password-reset/confirm`. Token lookup, a pure password policy, hashing, the update, session revocation and a 5 second budget.
- [`webhook/`](webhook): a payment provider's webhook. Signature guard, parsing, idempotency, routing on an enum, and a retry loop for `DatabaseBusy`.
- [`import-job/`](import-job): a long-lived CSV import that accepts `Pause`, `Resume` and `Cancel` and processes batches with timed retries.
- [`upload/`](upload): messaging with an invoked child. `Publish` invokes `Upload` as `upload`, sends it `Pause` and `Resume`, and shows the `Progress` it emits. `Batch` spawns an `Upload` per file into `children uploads: Upload by string`, keyed by the file's name: it pauses and resumes them all with `send uploads.*`, cancels one with `stop`, and hears each one's progress and outcome with `key`.
- [`inventory/`](inventory) and [`orders/`](orders): messaging with a peer, across packages. An `Order` takes `inventory: ref Inventory`, sends it `Reserve` and waits for the `Reserved` or `Rejected` it emits. `orders` imports from the package `inventory` (`"inventory/inventory.road"`), which the root `roadbook.toml` maps to `examples/inventory` for the command line and the TypeScript generator. Both are also Rust crates: `inventory` includes its chart, and `orders` depends on it and includes its own, whose import the `inventory` crate's `roadbook::include!` answers, so `orders`'s generated code uses `inventory`'s types. Both runtimes run these three examples: TypeScript in [`packages/effect/test/examples.test.ts`](../packages/effect/test/examples.test.ts), Rust in `examples/orders/tests` (which also checks at compile time that host code sends only `inventory::Event`s) and `crates/roadbook/tests/include_messaging.rs`.
- [`large/`](large): a support ticket's lifecycle with about 50 states, used to test the diagram viewer's layout. It has three levels of nesting, a parallel state and an invoked child machine (`SatisfactionSurvey`).

## Writing charts from prose

The four charts above were written as an experiment. An agent got a prose description of each one and had read only [the language page](../docs/design/language.md) and `shop/`: no compiler source and no tests. It wrote each chart once and checked it. It then fixed the chart using only the diagnostics, re-checking until it was clean. The untouched first attempts were kept to compare against.

| Chart | States | First-attempt errors by code | Rounds to clean | `fmt` changes |
| --- | --- | --- | --- | --- |
| `password-reset` | 12 | none | 0 | 6 of 98 lines (alignment) |
| `webhook` | 15 | none | 0 | 6 of 131 lines (alignment) |
| `import-job` | 9 | none | 0 | 12 of 101 lines (alignment; one state collapsed to one line) |
| `large` | 46 + 5 in the child machine | none | 0 | 51 of 352 lines (alignment; five states collapsed to one line) |

States are counted from `roadbook export ir`, not counting the machine itself. Every first attempt also had no warnings. With `--warn invoke-timeout`, `webhook`, `import-job` and `large` report RB0904. I left those alone because the prose gives none of their steps a deadline. Every formatter change was whitespace.

Zero errors does not mean the charts are correct, for two reasons:

- **The check most likely to catch an agent's bugs did not exist yet.** The compiler did not yet do definite assignment (milestone 6). Deleting `set reset = result` from `password-reset` still checked clean, so I checked every read of a `var` with no default by hand. Since milestone 6 that deletion is reported as `RB0403`.
- **The page was read in full and its traps were avoided on purpose.** Each trap is documented, but an agent writing quickly would hit it. The list below says what a natural first draft would have contained and what the compiler does with it. I probed each case with a scratch file after the experiment.

### Unclear points in the language page

Since this experiment, [the language page](../docs/design/language.md) answers points 1, 2, 5, 8, 9 and 11 (see "Which transition is taken" there), documents the `fn` idiom for lists (3), says that there is no `none` literal and that absence which changes the flow is a task error (4), says that an invoked child receives no events (6), and lists the small traps of 10 under "Common mistakes". The suggestions that would change the language remain open: built-in list functions (3), a `none` literal (4), a warning on invoking a machine that declares events (6), a warning on an `always` block with no `else` and an exhaustive `match` (7), and a `d` duration unit (10).

1. **Which transition wins when a state and its ancestor both handle an event.** `import-job` has `on Resume` on `Importing` and `on Resume -> RunningBatch` on its child `Paused`, and its correctness depends on the child winning. The page never says that descendants take priority, how an ancestor's `on` interacts with a child's `on *`, or in what order an ancestor's `after` and a child's invoke outcome are considered.
   *Suggestion:* add a short "Which transition is taken" section (innermost state first, then outward; branches in order), with an example.
2. **Whether a targeted transition to the state itself, or to its own child, leaves and re-enters it.** The page says that a transition with no target is internal, so the reverse is only implied. `large` relies on `after 2h -> WithAgent` re-entering, to restart the timer and re-run `entry`. It also has `on Assign -> Triage.Assigned` written on `Triage`. If that leaves and re-enters `Triage`, it restarts the triage SLA timer, and the page does not say whether it does. For SLA timers this is the difference between right and wrong.
   *Suggestion:* state that `-> Self` is external (exit, entry, invoke and timers restart). Say what happens to the source state when the target is one of its own descendants.
3. **No way to test a list for emptiness, and no list literal.** The password policy returns `string[]`, where "empty means fine". Lists cannot be compared, there is no `[]` literal and there is no `len`. Without generics, every element type needs its own `fn isEmpty(items: T[]) -> bool`.
   *Suggestion:* add built-in `isEmpty`/`len` for lists, or document the `fn` idiom next to the comparison rules.
4. **Optionals have no narrowing and no "none" literal.** The page says "an optional value goes to a `fn` that handles the absent case". A chart cannot branch on presence and then use the value, so "the token may not exist" only works as a declared error on the task (`! TokenNotFound`). Also, a record with a `T?` field cannot be built in the chart without a value for that field, which is why `large` builds `Closure` in a `fn`.
   *Suggestion:* say outright that absence that changes the flow should be a task error. Add a `none` literal, or say there isn't one.
5. **`initial` in a parallel state.** The "Inside a machine" table says `initial` is "Required, once, when a state has children", which is false for `parallel state`. The page also does not say whether a region may be atomic, or what `done` means then.
   *Suggestion:* add "(not in a parallel state)" to the table, and say what a region may be. The compiler's RB0203 here is clear: "`M.P` is parallel: all of its children are entered".
6. **An invoked machine that declares events.** Messaging is not in the first version, so nothing can ever send events to an invoked child. Even so, `invoke Child()` where `Child` waits on `on Poke` is accepted with no warning, and that state can never be left. `large`'s survey polls instead of waiting for a `Rated` event for this reason.
   *Suggestion:* say on the page whether events reach invoked children. Until they do, warn on an invoke of a machine that declares events.
7. **Branch blocks with no `else`, and no exhaustive routing on an enum.** `always { if k == K.A -> D }` is accepted silently. When the guard is false the state is stuck, and RB0902 does not fire because a transition with a target exists. In `webhook`, `else -> RecordingRefund` stands for `Refunded` and would silently catch a fourth event kind.
   *Suggestion:* warn on an `always` block with no `else` in a state with no other way out. Longer term, add an exhaustive `match` on enums.
8. **What happens when a task fails by a defect.** "`hashPassword` can fail only by a defect" is modelled as a task with no errors. The page never says what an undeclared failure does: whether it fails the machine, crashes the parent, or something else.
   *Suggestion:* add one sentence and link to the semantics page.
9. **The order of `entry` against `always`.** `password-reset` computes `problems` in `entry` and tests it in `always` on the same state. That only works if `entry` runs before the `always` guard is evaluated, which is likely but not stated.
   *Suggestion:* give the microstep order (exit, transition `set`/`do`, entry, then eventless transitions).
10. **Small traps, all documented but easy to miss.** There is no `d` duration unit, so 14 days is `336h`. `event`, `error` and `result` cannot be variables, so the parsed webhook event is `payment`. `type` is reserved, so the event-kind field is `kind`. A parent naming its own child needs `Parent.Child`.
    *Suggestion:* add a `d` unit. Collect these traps in a short "common mistakes" list.
11. **A timer with no target on a compound state.** `after triageSla(priority) do pageOnCall(...)` has no target, and the page does not say whether it fires once or repeatedly.

### Diagnostics

- **Hard to act on: a keyword used as a field name.** `type Evt { type: string }` gives `RB0011 expected '}' to close the fields of 'type', found keyword 'type'` and then a follow-on `RB0010`. The page assigns this mistake to `RB0013` ("A keyword, or `result`, used as a name"). The message points at brace matching rather than at the name.
  *Suggestion:* report `RB0013` with "`type` is a keyword; rename the field (for example `kind`)", and recover so that no `RB0010` follows. *Done:* a keyword followed by `:` now gives one `RB0013` with a rename hint.
- **Easy to act on: an own-child target.** `RB0201` for `on Go -> C` written on `A` says "`C` is a child of `A`; a state names its own children through its name: `A.C`". The plain unknown-target case lists the states in scope. Both made the fix obvious.
- **Easy to act on: `initial` in a parallel state.** `RB0203` names the problem directly.

### Formatter

- Alignment groups run until a blank line. One long signature pushes `->` to column 60 or more for every short `fn` in the run: in `webhook`, `fn ok()` is padded to match `signatureValid`. Split groups with blank lines if that matters.
- In a block of branches, the `fail TransientBatchError {` header is in the same alignment group as its branches, so short guards get padded far out (`if result == 0         ->`). This follows the documented rule but looks accidental.
- `var` lines align the type but not `=`, so hand-aligned defaults are undone.
