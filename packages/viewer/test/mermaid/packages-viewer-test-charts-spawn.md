## Session

`packages/viewer/test/charts/spawn.road:16`

```text
One connection.
```

```mermaid
stateDiagram-v2
  Left: output seen
  Dropped: fail Lost { reason#58; "idle" }
  [*] --> Open
  Open --> Open: Notify / set seen = seen + 1, emit Typing { text#58; event.text }
  Open --> Left: Leave
  Open --> Dropped: after 1h
  Left --> [*]
  Dropped --> [*]
```

## Job

`packages/viewer/test/charts/spawn.road:34`

```text
A job with no result.
```

```mermaid
stateDiagram-v2
  [*] --> Running
  Running --> Done: after 1s
  Done --> [*]
```

## Hub

`packages/viewer/test/charts/spawn.road:41`

```text
Many sessions, one per connection.
```

```mermaid
stateDiagram-v2
  [*] --> Hub_machine
  state "Hub" as Hub_machine
  state Hub_machine {
    Serving: entry spawn jobs[Lane.Slow] = Job(Lane.Slow)
    Serving: on Connected / set open = open + 1, spawn sessions[event.id] = Session(event.id, "hi"), send sessions[event.id].Notify { text#58; "welcome" }
    Serving: on Disconnected / stop sessions[event.id]
    Serving: on Broadcast / send sessions.*.Notify { text#58; event.text }
    Serving: on sessions.Typing / set last = event.text, do note(key, open)
    Serving: on sessions.done / set open = open - 1, do note(key, result)
    Serving: on sessions.fail Lost [error.reason == "idle"] / do log(key)
    Serving: on sessions.fail * / do log(key)
    Serving: on jobs.done / stop jobs[key]
    Closed: output open
    [*] --> Idle
    Idle --> Serving: Open / spawn jobs[Lane.Fast] = Job(Lane.Fast)
    Serving --> Closed: Close
    Closed --> [*]
  }
  note right of Hub_machine
    exit send sessions.*.Leave
  end note
```
