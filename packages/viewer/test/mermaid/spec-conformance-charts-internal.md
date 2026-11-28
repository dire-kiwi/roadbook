## Kept

`spec/conformance/charts/internal.road:9`

```text
`Session` keeps its timer and its invoke when an internal transition changes the states
below it; an external one restarts both.
```

```mermaid
stateDiagram-v2
  state Session {
    state "Browsing" as Session_Browsing
    Session_Browsing: entry do log("enter Browsing")
    Session_Browsing: exit do log("exit Browsing")
    state "Cart" as Session_Cart
    state Session_Cart {
      state "Editing" as Session_Cart_Editing
      state "Reviewing" as Session_Cart_Reviewing
      [*] --> Session_Cart_Editing
    }
    note right of Session_Cart
      entry do log("enter Cart")
      exit do log("exit Cart")
    end note
    [*] --> Session_Browsing
    Session_Browsing --> Session_Cart: Open
  }
  note right of Session
    invoke keepAlive()
    entry do log("enter Session")
    exit do log("exit Session")
  end note
  [*] --> Session
  Session --> TimedOut: done
  Session --> TimedOut: after 30m
  Session --> Session_Browsing: Home
  Session --> Session_Cart_Reviewing: Review
  Session --> Session: Restart
  TimedOut --> [*]
```

## Again

`spec/conformance/charts/internal.road:50`

```text
An internal transition that exits a final state takes back its parent's completion: the
first time `Work` ends, `S`'s `always` goes back into it before `S`'s `done` is tried.
```

```mermaid
stateDiagram-v2
  state S {
    state "Work" as S_Work
    state "Done" as S_Done
    S_Done: entry do log("done")
    [*] --> S_Work
    S_Work --> S_Done: Finish / set tries = tries + 1
    S_Done --> [*]
  }
  [*] --> S
  S --> S_Work: always [tries == 1] / set tries = 2
  S --> Out: done
  Out --> [*]
```
