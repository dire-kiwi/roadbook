## Restore

`spec/conformance/charts/internal-history.road:7`

```text
An internal transition to a history state of its own source restores what the source was
in when it was last left, without leaving it.
```

```mermaid
stateDiagram-v2
  state C {
    state "H" as C_H
    state "A" as C_A
    state "B" as C_B
    [*] --> C_A
    C_A --> C_B: Next
    C_B --> C_A: Prev
  }
  note right of C
    entry do log("enter C")
    exit do log("exit C")
  end note
  [*] --> C
  C --> Out: Leave
  C --> C_H: Undo
  Out --> C_H: Back
```
