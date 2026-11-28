# XState and Roadbook side by side, in code

This page gives the code behind the rows of [Roadbook compared with XState](xstate.md) where the difference is easiest to see in an example. Each section shows XState v5, then the XState v6 beta where it differs, then Roadbook as it is today. Where Roadbook would need new syntax, the section ends with a **proposed** form. Proposed syntax is not in the language. It shows the shape of an addition, and the task that builds it decides the exact form. [What to add](xstate-plan.md) says which of these are recommended.

The XState v6 code follows Stately's [migration guide](https://stately.ai/docs/xstate/v6/xstate-v5-to-v6). XState is MIT-licensed.

## A choice between states

A state whose only job is to pick the next state.

**XState v5** writes a transient state with `always`. It is an ordinary state: it is entered, its `always` branches are tried, and it is left.

```ts
routing: {
  always: [
    { guard: ({ context }) => context.isVip, target: 'vipFlow' },
    { guard: ({ context }) => context.overBudget, target: 'review' },
    { target: 'standardFlow' },
  ],
},
```

**XState v6** adds a choice state. It may not have `invoke`, `after`, `on`, `entry` or `exit`, and its function must return a target: one that returns nothing throws at run time.

```ts
routing: {
  type: 'choice',
  choice: ({ context }) => {
    if (context.isVip) return { target: 'vipFlow' };
    if (context.overBudget) return { target: 'review' };
    return { target: 'standardFlow' };
  },
},
```

**Roadbook** has a choice state (since IR 0.14, SM-30). Its body is only branches, and the compiler checks that it ends with an `else` and that every branch has a target (`RB0227`, `RB0228`), so the run always leaves it: what v6 throws for at run time is a compile error here.

```
choice Routing {
  if isVip(user)       -> VipFlow
  if overBudget(order) -> Review        do flag(order)
  else                 -> StandardFlow
}
```

A branch may have effects and pass a target's arguments, as any transition's can. The viewer and Mermaid draw it as a diamond. Unlike v6's, it is not a pseudo-state the transition passes through in one step: the run enters it and leaves it again, as a v5 transient state, so the trace shows `Routing` entered and exited, and the guards see what the transition into it assigned. A transient state written with `always { if … else … }` still works, and is the form to use when the state does more than choose (an `entry` effect, say).

## Route states: jump to a state by an event

**XState v6** lets a state with an `id` declare `route`. Sending the built-in `xstate.route` event with a `to` then jumps straight to that state from wherever the machine is. The `route` function is a guard: returning `false` blocks the jump. A state without `route` cannot be jumped to.

```ts
const machine = createMachine({
  id: 'flow',
  initial: 'a',
  states: {
    a: {},
    b: { id: 'b', route: {} },                             // can always be jumped to
    c: { id: 'c', route: () => false },                    // blocked
    d: { id: 'd', route: ({ context }) => context.ready }, // when ready
  },
});

actor.send({ type: 'xstate.route', to: '#b' }); // now in b
```

It is meant for user interfaces, where the URL decides which screen is shown.

**XState v5** has no route states. It writes one event per destination on the root.

**Roadbook** writes the same thing as a transition in the machine body, with one declared event per destination. A transition in the machine body is taken from any state, and a guard blocks it:

```
machine Wizard(ready: bool) {
  event GoToShipping
  event GoToPayment

  on GoToShipping          -> Shipping
  on GoToPayment if ready  -> Payment

  initial Basket
  state Basket   { }
  state Shipping { }
  state Payment  { }
}
```

The difference is that host code sends a declared, typed event (`GoToPayment`) rather than naming a state id. In Roadbook state ids are the chart's own business: renaming a state does not break host code.

## Targeting your own state or a child of it

A `Session` state has a 30-minute timeout and two children. The question is what a transition does when it is written in `Session` and targets `Session` or one of its children.

**XState v5** does not leave a state when a transition written in it targets one of its children. `reenter: true` makes it leave and enter again.

```ts
session: {
  after: { 1800000: 'timedOut' },
  initial: 'browsing',
  states: {
    browsing: { on: { OPEN_CART: 'cart' } },
    cart: {},
  },
  on: {
    // session is not left: its 30-minute timer keeps running
    HOME: { target: '.browsing' },
    // session is left and entered again: the timer starts over
    RESTART: { target: '.browsing', reenter: true },
  },
},
```

**XState v6** keeps the same default. Its `reenter` also re-enters a target that resolves to the state the machine is already in.

**Roadbook** leaves and re-enters, as SCXML's external transitions do, unless the target is written with a leading dot (SM-24). Transitions written in the machine body never leave the machine.

```
state Session {
  initial Browsing
  after 30m -> TimedOut

  state Browsing { on OpenCart -> Cart }
  state Cart     { on Home -> Browsing }  // written in Cart: Session is not left, the timer keeps running

  on Restart -> Session.Browsing          // written in Session: Session is left and entered, the timer starts over
  on Refresh -> Session                   // the same, and Session starts again from its initial state
}
```

Before SM-24, `on Home` written in `Session` couldn't avoid restarting the timer: to keep it running, the chart wrote `on Home` in each child that needed it, as `Cart` does above. A leading dot targets a child without leaving the state the transition is written in:

```
state Session {
  initial Browsing
  after 30m -> TimedOut
  state Browsing { on OpenCart -> Cart }
  state Cart     { }
  on Home    -> .Browsing          // Session is not left; only Cart is exited and Browsing entered
  on Restart -> Session.Browsing   // Session is left and entered
}
```

This is SCXML's `type="internal"`. The domain becomes the source state itself, so only states below it are exited. Like XState, a dotted target that is already active is entered again: `on Home -> .Browsing` taken in `Browsing` restarts `Browsing`, not `Session`.

## Several targets in one transition

Yes: one transition enters one state in each of several regions of a parallel state. A player has two regions, playback and volume. `SLEEP` should pause and mute at once.

**XState** (v5 and v6):

```ts
player: {
  type: 'parallel',
  states: {
    playback: { initial: 'playing', states: { playing: {}, paused: {} } },
    volume:   { initial: 'normal',  states: { normal: {},  muted: {} } },
  },
  on: {
    SLEEP: { target: ['.playback.paused', '.volume.muted'] },
  },
},
```

**Roadbook** before SM-24 had one target per transition. An event is offered to every region, and each region can take its own transition for it, so the same effect was written once per region:

```
parallel state Player {
  state Playback {
    initial Playing
    state Playing {
      on Pause -> Paused
      on Sleep -> Paused
    }
    state Paused { on Play -> Playing }
  }
  state Volume {
    initial Normal
    state Normal {
      on Mute  -> Muted
      on Sleep -> Muted
    }
    state Muted { on Unmute -> Normal }
  }
}
```

What that couldn't write is entering `Player` from outside directly into `Paused` and `Muted`: a transition `-> Player.Playback.Paused` enters `Volume` at its initial state, `Normal`. A transition can now have several targets:

```
on WakeIntoQuiet -> Player.Playback.Paused, Player.Volume.Muted   // from outside Player
on Sleep -> .Playback.Paused, .Volume.Muted                       // written in Player, which is kept
```

The compiler checks that the targets are in different regions of a parallel state (`RB0222`), and that either all of them have the dot or none does (`RB0221`).

## A transition into one region of a parallel state

This is the guide's own example. A parallel state `P` has regions `A` and `B`. On event `T`, region `A` re-enters itself, and `B` moves from `B1` to `B2`.

**XState v5** ends in `{ A: 'A1', B: 'B1' }`. Re-entering `A` left all of `P`, so `B` was reset, and its move to `B2` was lost.

**XState v6** ends in `{ A: 'A1', B: 'B2' }`: only `A` is left. To get v5's reset, a v6 chart targets the parallel state itself, or lists a target in every region. XState's SCXML package keeps SCXML's rule.

**Roadbook** follows SCXML for an external transition, so this chart ends as v5 does. An external transition never has a parallel state as its domain, so re-entering a region leaves `P` and enters it again with every region. `B`'s transition conflicts with that one and is dropped, and the compiler warns that `B` starts over (`RB0913`):

```
parallel state P {
  state A {
    initial A1
    state A1 { }
    on T -> A          // leaves P, enters P again: A at A1, B back at B1
  }
  state B {
    initial B1
    state B1 { on T -> B2 }   // conflicts with A's transition, which wins as the earlier region
    state B2 { }
  }
}
```

The same rule applies to a transition from one region to another: `on Blast -> Player.Volume.Normal` written in `Player.Playback.Paused` leaves `Player`, so `Playback` goes back to `Playing`.

With a `.Child` target, `on T -> .A1` written in `A` re-enters only `A1`, leaves `B` alone, and gives v6's result. An internal transition written in `P` itself, `on T -> .A.A1`, exits and enters only region `A`, as v6 does; SCXML would take it as external from a parallel state.

## Several invokes in one state

"Invokes per state" counts how many pieces of work one state can start. A loading screen fetches a user and their preferences at the same time.

**XState** (v5 and v6) takes an array:

```ts
loading: {
  invoke: [
    { src: 'fetchUser',  input: ({ context }) => context.id,
      onDone: { actions: assign({ user: ({ event }) => event.output }) },
      onError: 'failed' },
    { src: 'fetchPrefs', input: ({ context }) => context.id,
      onDone: { actions: assign({ prefs: ({ event }) => event.output }) },
      onError: 'failed' },
  ],
  always: { guard: ({ context }) => context.user && context.prefs, target: 'ready' },
},
```

**Roadbook** lets a state run several invokes (SM-37), but each has its own outcomes and none waits for the others, so a join goes in the regions of a parallel state. `done` on the parallel state is taken when both regions reach a final state:

```
parallel state Loading {
  state User {
    initial Fetching
    state Fetching {
      invoke fetchUser(id) {
        done          -> Got     set user = result
        fail NotFound -> Failed
      }
    }
    final state Got { }
  }
  state Prefs {
    initial Fetching
    state Fetching {
      invoke fetchPrefs(id) {
        done     -> Got     set prefs = result
        fail *   -> Failed
      }
    }
    final state Got { }
  }
  done -> Ready
}
```

`-> Failed` resolves outward to a sibling of `Loading`, so a failure in either region leaves `Loading` and cancels the other fetch. It is longer than XState's array, but the diagram shows both fetches and the join, and the compiler proves `user` and `prefs` are assigned in `Ready`.

Several invokes in one state, each named with `as`, are for a background task beside the main one, like a heartbeat, rather than for a join:

```
state Watching {
  invoke heartbeat(server) as beat {
    fail Lost -> Offline
  }
  invoke syncFiles(folder) as sync {
    done   -> Synced
    fail * -> Failed
  }
}
```

They start in the order written. `sync`'s `done` leaves the state, which cancels `beat`, and `beat` failing `Lost` cancels `sync` the same way.

## Invoked work: Roadbook's tasks and v6's async logic

**XState v6** writes long-running work with `createAsyncLogic`. It has a timeout, an `AbortSignal` for cancelling, can emit events while it runs, and journals named steps for its durable mode:

```ts
const fetchUser = createAsyncLogic({
  id: 'fetchUser',
  timeout: '10s',
  run: async ({ input, signal }, enq) => {
    const user = await enq.step('fetch', () =>
      fetch(`/users/${input.id}`, { signal }).then((r) => r.json()),
    );
    enq.emit({ type: 'userLoaded', user });
    return user;
  },
});
```

**Roadbook** declares a task in the chart and implements it in host code, as an Effect in TypeScript or a future in Rust:

```
task fetchUser(id: UserId) -> User ! NotFound | Unavailable

state Loading {
  invoke fetchUser(id) {
    done             -> Ready    set user = result
    fail NotFound    -> Missing
    fail Unavailable -> Offline
  }
  after 10s -> TimedOut
}
```

They are comparable. Here is how they line up:

| | XState v6 async logic | Roadbook task |
| --- | --- | --- |
| Cancelling | An `AbortSignal` the code must check | Interrupting the Effect, or dropping the Rust future, when the state is left |
| Timeout | `timeout` on the logic, failing with `TimeoutError` | `after` on the invoking state, which is a transition the chart names |
| Errors | Untyped, `onError` | Declared, and each one has a branch the compiler requires |
| Events while running | `enq.emit` | `emits` on the task, received as `on name.E` through `invoke … as name` (SM-26) |
| Durable mode | Each `enq.step` is journaled | The whole task is one journaled activity, so a crash re-runs the task from the start |

## Delayed and self-sent events

**XState v5**, a search box that waits for a pause in typing:

```ts
typing: {
  entry: raise({ type: 'SEARCH' }, { delay: 500, id: 'debounce' }),
  on: {
    KEY: { actions: [cancel('debounce'), raise({ type: 'SEARCH' }, { delay: 500, id: 'debounce' })] },
    SEARCH: 'searching',
  },
},
```

**XState v6** writes the same with `enq.raise(event, { delay, id })` and `enq.cancel(id)`, and can declare `internalEvents` that host code cannot send.

**Roadbook** (SM-28) writes the same with a delayed raise. A raise whose name is pending replaces the pending one, so each key starts the delay again, and `Typing` is never left:

```
internal event Search   // only the machine raises it

state Typing {
  on Key    set query = event.text  raise Search after 500ms as debounce
  on Search -> Searching
}
```

Before SM-28 a debounce was a self-transition, which re-enters the state and so restarts its timer, as well as its `entry`, `exit` and invoke:

```
state Typing {
  on Key -> Typing        // re-enters Typing: the 500ms timer starts over
  after 500ms -> Searching
}
```

Nor could a chart write a timer that outlives the state that started it, short of moving the `after` up to a parent state. Two examples are "remind the customer in 24 hours unless they confirm, whatever else happens" and "after setting up, tell the other region to start". A delayed raise is a timer of the run, not of a state, and an immediate one is taken before the step ends, by every region that has an `on` for it ([events a machine raises](language.md#events-a-machine-raises)):

```
internal event Remind         // cannot be sent from outside

state Ordered {
  entry raise Remind after 24h as reminder
}
on Confirm cancel reminder
on Remind do sendReminder(order)
```

XState's delayed `sendTo` has no counterpart: a delayed raise whose transition sends does the same.

## Asking what state the machine is in from a guard

**XState v5:**

```ts
on: { VOLUME_UP: { guard: not(stateIn('#player.volume.muted')), actions: 'louder' } }
```

**XState v6:**

```ts
on: { VOLUME_UP: ({ self }, enq) => { if (!self.getSnapshot().matches('volume.muted')) enq(louder) } }
```

**Roadbook** keeps a variable in step with the state:

```
var muted: bool = false
state Muted {
  entry set muted = true
  exit  set muted = false
}
on VolumeUp if not muted do louder()
```

```
// proposed
on VolumeUp if not in(Player.Volume.Muted) do louder()
```

## Data that belongs to a state

**XState v6** passes `input` on the transition into a state, typed by the state's schema. It is gone once the state is left:

```ts
idle:    { on: { LOAD: { target: 'loading', input: { userId: 'u1' } } } },
loading: { entry: ({ input }) => console.log(input.userId) },
```

**Roadbook** (SM-29) has state parameters and state variables. A parameter is passed on the transition into the state, and a `var` declared in a state is set again each time the state is entered; both are readable only in that state and the states inside it, and both are gone once it is left:

```
state Requesting {
  invoke requestReset(email) { done -> Confirming(result) }
}
state Confirming(token: ResetToken) {
  var tries: int = 0           // set to 0 each time Confirming is entered, gone when it is left
  on Confirm if tries < 3 -> Resetting(token)
}
```

The compiler checks that every transition into `Confirming` passes a `ResetToken`. Before SM-29, `token` was a machine variable, set by `done -> Confirming  set token = result`, which definite assignment proved set before it was read but which lived on in every later state.

## Callback actors, and tasks that report progress

**XState v5**, a callback actor that pushes events into its parent:

```ts
invoke: {
  src: fromCallback(({ sendBack }) => {
    const id = setInterval(() => sendBack({ type: 'TICK' }), 1000);
    return () => clearInterval(id);
  }),
},
on: { TICK: { actions: 'count' } },
```

**Roadbook** (SM-26) has tasks that declare what they emit. A **stream task** returns nothing and emits until its state is left, which covers callback and observable actors except that it cannot be sent events. The host code gets an `emit` function, the second argument in TypeScript and an `Emitter` in Rust.

```
task upload(file: File) -> Url ! UploadFailed emits Progress { percent: int }
task ticks(every: duration) emits Tick

state Waiting {
  invoke ticks(1s) as clock
  on clock.Tick set seconds = seconds + 1
}

state Uploading {
  invoke upload(file) as up {
    done              -> Done    set url = result
    fail UploadFailed -> Failed
  }
  on up.Progress set percent = event.percent
}
```

## Reducer actors

**XState v5** makes an actor from a reducer:

```ts
const counter = fromTransition((state, event) =>
  event.type === 'INC' ? { count: state.count + 1 } : state, { count: 0 });
```

**Roadbook** writes this as a machine with one state, which is the same thing with typed events:

```
machine Counter() {
  event Inc
  emits Changed { count: int }
  var count: int = 0
  initial Counting
  state Counting {
    on Inc set count = count + 1  emit Changed { count: count }
  }
}
```

## Reacting to a child's snapshot

**XState v5** runs an action each time a child's snapshot changes:

```ts
invoke: { src: 'upload', onSnapshot: { actions: assign({ percent: ({ event }) => event.snapshot.context.percent }) } },
```

**Roadbook** has the child emit what the parent needs, received as `on upload.Progress`. That uses the [messages between machines](https://github.com/dire-kiwi/roadbook/blob/main/docs/design/language.md#messages-between-machines) the language already has. The parent then depends only on the child's declared `emits`, not on its variables or states.

## Learning that a peer ended

**XState v6** subscribes to another actor's lifecycle:

```ts
enq.subscribeTo(inventory, {
  done:  (output) => ({ type: 'INVENTORY_DONE', output }),
  error: (err)    => ({ type: 'INVENTORY_FAILED', err }),
});
```

**Roadbook** (SM-25) has a trigger for it. Writing the `on` is the subscription, so with SM-18's filter only holders that have one are told:

```
machine Orders(inventory: ref Inventory) {
  state Waiting {
    on inventory.Reserved -> Paying
    on inventory.ended {
      if event.status == Ended.Defect -> InventoryCrashed
      else                            -> InventoryGone
    }
    after 30s -> TimedOut
  }
}
```

The lower-case `ended` keeps it apart from `on inventory.Event`. `event.status` says how the peer ended (`Done`, `Failed`, `Cancelled`, `Defect` or `Suspended`). Unlike XState's `done` and `error`, it doesn't carry the peer's output or error: a holder depends only on the peer's interface.
