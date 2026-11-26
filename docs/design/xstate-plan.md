# What to add from XState

This page says which of the gaps in [Roadbook compared with XState](xstate.md) to close. It assumes the aim is to be exhaustive: to cover everything XState can express unless a feature works against what Roadbook is for. Roadbook is for readable, checked program flow, with host code that sees only its arguments. [The code examples](xstate-examples.md) show each feature in XState and in Roadbook.

On 27 November 2026 the user approved six additions: reacting to a peer's end, property checks, a transition that keeps its source, tasks that emit, host code receiving emitted events, and child machines in snapshots. The rest are recommendations for the user to decide on.

## Each feature

| Feature | Recommendation | Why |
| --- | --- | --- |
| Learning that a peer ended (`on peer.ended`) | **Add** (approved) | A holder of a `ref` cannot react to a peer's end today. It shares a design with keyed spawn's `on name.done` and `on name.fail`, which now exist. |
| Checking properties ("never reach `Refunded`") | **Add** (approved) | Turns the chart into something CI can hold a design to. It needs only the IR. |
| A transition that keeps its source (`-> .Child`) | **Add** (approved) | The one common case where Roadbook's default surprises: a parent's invoke and timers restart. SCXML has it as `type="internal"`. |
| Tasks that report progress (`task … emits E`) | **Add** (approved) | Progress from long tasks, in the chart, received as `on name.E` like a machine's emits. |
| Callback and observable actors | **Add, as stream tasks** | A task that emits and has no result, running until its state is left, covers both. It comes with tasks that emit, so it belongs in the same task. |
| Host code receiving a run's emitted events | **Add** (approved) | XState's `actor.on`. It is listed as not yet specified. |
| Child machines in a snapshot | **Add** (approved) | A parent with long-lived children can't be suspended between events today, and restoring one restarts its children. Running tasks still restart, since host work can't be saved. |
| Choice states | **Add, as a pseudo-state** | Cheap. The compiler requires an `else`, which beats v6's run-time throw, and the diagram draws a diamond. It compiles to today's transient state. |
| Several targets in one transition | **Add** | SCXML has it, and the core already enters several states per transition. The only new thing is entering a parallel state with a chosen state in more than one region. Low priority. |
| A transition into one region of a parallel state | **Keep SCXML's rule, and add a warning** | v6 breaks with SCXML here, and XState's own SCXML package keeps the old rule. `-> .Child` gives v6's behaviour where a chart wants it. A warning would flag a transition that leaves a parallel state and so resets its other regions, which is easy to write by accident. |
| Partial wildcards (`mouse.*`) | **Skip, add `on A \| B` instead** | Roadbook's events are declared and typed, so a prefix match over names would give `event` no useful type. A list of alternatives covers the use: `event` has the fields the events share. |
| `raise`, and internal events | **Add** | SCXML's internal queue, taken before external events. `internal event` declares an event host code cannot send, as v6's `internalEvents` does. It lets one region of a parallel state signal another without a shared variable. |
| Delayed events and `cancel` | **Add, with `raise`** | A named timer that belongs to the machine, not a state, saved in snapshots like any timer: `raise E after 24h as name` and `cancel name`. Delayed sends to other machines aren't needed: a delayed `raise` whose transition sends does the same. |
| Data local to a state | **Add** | `var` inside a state, set on entry and gone on exit, readable only in that state and below. State parameters, `state Confirming(token: T)` with `-> Confirming(result)`, are v6's state input. The compiler already tracks where a variable is assigned, so it can check both. |
| Asking what state the machine is in (`stateIn`) | **Add** | `in(Player.Volume.Muted)` as an expression. It replaces a variable kept in step by `entry` and `exit`, which is easy to get wrong. SCXML has `In()`. |
| Fire-and-forget actions | **Don't add, add spawned tasks** | A host action can already start background work and return at once, which is what XState's synchronous actions do. Nothing would track that work, though: no cancel, no failure branch, and the durable driver couldn't replay it. What a chart needs is tracked background work that isn't tied to a state: spawning a task into a `children` collection, with `on name.done` and `on name.fail` as for machines. |
| Is `invoke` comparable to v6's async logic? | **Yes, except for emitting** | Cancelling, timeouts and declared errors are covered. Emitting while running is the gap, which tasks that emit close. Journaling steps inside a task is the other difference, covered under the durable driver below. |
| Several invokes per state | **Add, each named with `as`** | Useful for a background task beside the main one, such as a heartbeat. A parallel state stays the way to join. Low priority. |
| Reducer actors | **Skip** | A machine with one state is a reducer with typed events. A host-side reducer would hide flow from the chart. |
| Reacting to a child's snapshot | **Skip** | It would tie a parent to its child's variables and states. The child's declared `emits` are the interface. It would also defeat SM-18's filter and the rule that a holder hashes only its peer's interface. |
| Tags and `meta` on states | **Add** | Host code shows a spinner while the machine is in any state tagged `busy`, without listing states. Small, and the viewer can show them. |
| A default target for a history state | **Add** | SCXML has it, as `history Previous -> Editing`. Tiny. |
| Equality on records, lists and maps | **Add** | Decided in SM-8 and not built. |
| Route states | **Skip** | A transition in the machine body, with one declared event per destination, is the same thing typed. A generic `route` event would make host code name state ids. |
| State-level `onError` | **Skip** | Roadbook keeps declared errors, which are handled on the invoke, apart from defects, which are bugs and can't be caught. Catching at a state would blur that. |
| A registry of actors | **Skip** | A deliberate choice: who talks to whom is read from the charts. |
| Journaling steps inside a task (v6's `enq.step`) | **Skip for now** | It belongs to the host-side task API, not the chart. Revisit if long tasks under the durable driver re-run too much after a crash. |

## Proposed tasks

The additions are grouped into tasks by the code they change. Most change the language, the IR and both cores, so they bump the IR version, rewrite the hashes, and touch the same files. SM-18, SM-19 and SM-20 are in that state now, so these tasks wait for them, and the language tasks run one or two at a time rather than all at once.

| Task | Covers | Waits for | Size |
| --- | --- | --- | --- |
| **Property checks** (approved) | Assertions over the IR's states and transitions, checked by `roadbook check` in CI. Guards are ignored, which over-approximates the paths, so a "never" that passes is proved. A "can reach" that passes may still be blocked by a guard, and the report says so. | Nothing | Medium, compiler and CLI only |
| **Host code receives emitted events** (approved) | A stream in the Effect driver and a subscription in the Rust driver for a run's emits, specified in `spec/execution.md`. | SM-18 | Small |
| **Transitions that keep their source** (approved) | `-> .Child`, with SCXML's internal domain rule. Recommended alongside: several targets, and the warning for a transition that resets sibling regions. All three change how targets and domains are computed. | SM-18, SM-19, SM-20 | Small to medium |
| **`on peer.ended`** (approved) | The trigger for `ref` holders, perhaps with how the peer ended, and how it relates to `on name.done` and `on name.fail` for spawned children and to SM-20's `Unanswered`. | SM-18, SM-19, SM-20 | Medium |
| **Tasks that emit** (approved) | `task … emits E`, `as` on a task invoke, `on name.E` from a task, stream tasks with no result. Recommended alongside: spawned tasks, and several invokes per state. | Transitions that keep their source | Medium |
| **Child machines in snapshots** (approved) | A child run's snapshot saved inside its parent's, with its own IR hash, restored without restarting. `suspend` allowed with running children. | `on peer.ended` | Large |
| **`raise`, internal events and delayed events** | `raise E`, `internal event`, `raise E after d as name`, `cancel name`, and the internal queue in both cores and the trace. | Tasks that emit | Medium |
| **Data local to a state** | `var` in a state, and state parameters with `-> State(args)`. | `raise` | Medium |
| **Small additions** | `choice` pseudo-states, `in(State)`, `on A \| B`, a history default target, tags and `meta`, and equality on records, lists and maps. | Data local to a state | Medium, many small parts |

Property checks can start now, and host emits once SM-18 is merged. The rest form two lanes after SM-20: one through transitions, emitting tasks, `raise`, state data and the small additions; the other through `on peer.ended` and child snapshots.
