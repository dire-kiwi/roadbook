## Desk

`spec/conformance/charts/strict-calls.road:6`

```text
A help desk that answers questions while it is open.
```

```mermaid
stateDiagram-v2
  state Staffed {
    state "Free" as Staffed_Free
    state "Held" as Staffed_Held
    [*] --> Staffed_Free
    Staffed_Free --> Staffed_Held: hold / set pending = call
    Staffed_Held --> Staffed_Free: Release / reply pending done answered
  }
  note right of Staffed
    on ask / set answered = answered + 1, reply call done answered
  end note
  [*] --> Staffed
  Staffed --> Shut: Close
  Shut --> Staffed: Reopen
```

## Asker

`spec/conformance/charts/strict-calls.road:37`

```text
Ask the desk a question; 0 if it does not answer.
```

```mermaid
stateDiagram-v2
  Asking: invoke desk.ask()
  Got: output n
  Gave: output 0
  [*] --> Asking
  Asking --> Got: done / set n = result
  Asking --> Gave: fail Unanswered
  Got --> [*]
  Gave --> [*]
```

## Holder

`spec/conformance/charts/strict-calls.road:54`

```text
Hold a place with the desk; 0 if it does not answer.
```

```mermaid
stateDiagram-v2
  Holding: invoke desk.hold()
  Got: output n
  Gave: output 0
  [*] --> Holding
  Holding --> Got: done / set n = result
  Holding --> Gave: fail Unanswered
  Got --> [*]
  Gave --> [*]
```
