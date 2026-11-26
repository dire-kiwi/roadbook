## Receiver

`spec/conformance/charts/pair.road:8`

```text
Must get First before Second.
```

```mermaid
stateDiagram-v2
  [*] --> WaitingFirst
  WaitingFirst --> WaitingSecond: First
  WaitingSecond --> Done: Second
  Done --> [*]
```

## Sender

`spec/conformance/charts/pair.road:22`

```text
Sends First, then Second, in one transition, to the same peer named two ways.
```

```mermaid
stateDiagram-v2
  [*] --> Idle
  Idle --> Sent: Go / do note(peer), send peer.First, send target.Second
  Sent --> [*]
```
