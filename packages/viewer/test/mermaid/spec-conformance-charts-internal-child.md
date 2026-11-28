## Echo

`spec/conformance/charts/internal-child.road:4`

```text
A child that answers every `Ping` with a `Pong`.
```

```mermaid
stateDiagram-v2
  Ready: on Ping / emit Pong
  [*] --> Ready
  Ready --> Stopped: Stop
  Stopped --> [*]
```

## Caller

`spec/conformance/charts/internal-child.road:21`

```text
An internal transition written in the invoking state does not leave it, so its child keeps
running and can be sent to from the transition itself.
```

```mermaid
stateDiagram-v2
  state Talking {
    state "Waiting" as Talking_Waiting
    state "Heard" as Talking_Heard
    [*] --> Talking_Waiting
  }
  note right of Talking
    invoke machine Echo() as echo
  end note
  [*] --> Talking
  Talking --> Over: done
  Talking --> Talking_Waiting: Ping / send echo.Ping
  Talking --> Talking_Heard: echo.Pong
  Over --> [*]
```
