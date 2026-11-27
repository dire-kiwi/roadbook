## Session

`spec/conformance/charts/hub.road:9`

```text
One connection. It counts what it is told, and says back what it is asked to echo.
```

```mermaid
stateDiagram-v2
  [*] --> Session_machine
  state "Session" as Session_machine
  state Session_machine {
    Open: on Notify / set count = count + 1
    Open: on Echo / emit Said { text#58; event.text }
    Open: on Crash / do boom(name)
    Left: output count
    Dropped: fail Lost { reason#58; name }
    [*] --> Open
    Open --> Left: Leave
    Open --> Dropped: Drop
    Left --> [*]
    Dropped --> [*]
  }
  note right of Session_machine
    exit emit Said { text#58; "bye" }
  end note
```

## Hub

`spec/conformance/charts/hub.road:33`

```text
Spawns, addresses and stops sessions by key.
```

```mermaid
stateDiagram-v2
  [*] --> Hub_machine
  state "Hub" as Hub_machine
  state Hub_machine {
    Serving: on Join / spawn sessions[event.id] = Session(event.name)
    Serving: on JoinTell / spawn sessions[event.id] = Session(event.name), send sessions[event.id].Echo { text#58; event.text }
    Serving: on JoinTwice / spawn sessions[event.id] = Session("first"), spawn sessions[event.id] = Session("second")
    Serving: on Kick / stop sessions[event.id]
    Serving: on Tell / send sessions[event.id].Echo { text#58; event.text }
    Serving: on Shout / send sessions.*.Notify { text#58; event.text }
    Serving: on Leave / send sessions[event.id].Leave
    Serving: on Drop / send sessions[event.id].Drop
    Serving: on Crash / send sessions[event.id].Crash
    Serving: on sessions.Said / set said = said + 1, do log(key)
    Serving: on sessions.done / do log(key)
    Serving: on sessions.fail * / do log(error.reason)
    Closed: output said
    [*] --> Serving
    Serving --> Closed: Close
    Serving --> Closed: CloseJoin / spawn sessions[event.id] = Session("late")
    Closed --> [*]
  }
  note right of Hub_machine
    exit do log("closing")
  end note
```

## Pool

`spec/conformance/charts/hub.road:73`

```text
Keyed by number: handles only its children's ending, and can be saved while they run. It
hears what they say (so it is delivered, and dropped or stale) but never takes it.
```

```mermaid
stateDiagram-v2
  Running: on Open / spawn slots[event.n] = Session(event.name)
  Running: on Close / stop slots[event.n]
  Running: on Leave / send slots[event.n].Leave
  Running: on Drop / send slots[event.n].Drop
  Running: on slots.done / do log("left")
  Running: on slots.Said [key < 0] / do log("never")
  [*] --> Running
  Running --> Finished: Finish
  Finished --> [*]
```
