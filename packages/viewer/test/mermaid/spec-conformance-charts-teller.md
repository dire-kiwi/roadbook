## Teller

`spec/conformance/charts/teller.road:7`

```text
Answers questions from whoever invoked it with what it was asked.
```

```mermaid
stateDiagram-v2
  Listening: on Ask / emit Told { answer#58; event.question, to#58; owner.user }
  [*] --> Listening
  Listening --> Gone: Bye
  Gone --> [*]
```

## Keeper

`spec/conformance/charts/teller.road:23`

```text
Invokes a Teller and asks it a question: before it runs, and once it does.
```

```mermaid
stateDiagram-v2
  state Asking {
    state "Ready" as Asking_Ready
    Asking_Ready: entry send teller.Ask { question#58; code }
    Asking_Ready: on Go / send teller.Ask { question#58; event.question }
    [*] --> Asking_Ready
  }
  note right of Asking
    invoke machine Teller(code, owner) as teller
    on teller.Told / set answer = event.answer
  end note
  [*] --> Asking
  Asking --> Finished: done
  Asking --> Finished: Stop
  Finished --> [*]
```

## Caller

`spec/conformance/charts/teller.road:48`

```text
Asks a Teller it holds a `ref` to, and keeps the answer.
```

```mermaid
stateDiagram-v2
  Asking: entry send teller.Ask { question#58; secret }
  Asking: on teller.Told / set heard = event.answer
  Asking: on Hang / send teller.Ask { question#58; secret }
  [*] --> Asking
  Asking --> Done: Leave
  Done --> [*]
```
