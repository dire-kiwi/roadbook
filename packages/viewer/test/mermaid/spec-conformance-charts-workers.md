## Logbook

`spec/conformance/charts/workers.road:8`

```text
Takes notes from every machine that holds a reference to it.
```

```mermaid
stateDiagram-v2
  Open: on Note / do log(event.text)
  [*] --> Open
  Open --> Closed: Close
  Closed --> [*]
```

## Worker

`spec/conformance/charts/workers.road:23`

```text
Works until it is cancelled, then says so to the logbook and to its parent.
```

```mermaid
stateDiagram-v2
  [*] --> Worker_machine
  state "Worker" as Worker_machine
  state Worker_machine {
    [*] --> Working
    Working --> Finished: Finish
    Finished --> [*]
  }
  note right of Worker_machine
    exit send book.Note { text#58; "worker stopped" }
    exit emit Stopping
  end note
```

## Boss

`spec/conformance/charts/workers.road:37`

```text
Watches a Worker, which it can restart or leave.
```

```mermaid
stateDiagram-v2
  [*] --> Boss_machine
  state "Boss" as Boss_machine
  state Boss_machine {
    Watching: invoke machine Worker(book) as worker
    Watching: on worker.Stopping / do log("worker stopping")
    [*] --> Watching
    Watching --> Idle: done
    Watching --> Watching: Restart
    Watching --> Idle: Leave
    Idle --> Watching: Restart
  }
  note right of Boss_machine
    exit send book.Note { text#58; "boss stopped" }
  end note
```
