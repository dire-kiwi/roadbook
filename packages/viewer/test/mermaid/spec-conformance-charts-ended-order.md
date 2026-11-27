## Desk

`spec/conformance/charts/ended-order.road:9`

```text
Takes a reservation and keeps the call pending until Answer.
```

```mermaid
stateDiagram-v2
  [*] --> Open
  Open --> Holding: reserve / set pending = call
  Open --> Closed: Close / emit Closing
  Holding --> Open: Answer / reply pending done "ok"
  Holding --> Closed: Close / emit Closing
  Closed --> [*]
```

## Booker

`spec/conformance/charts/ended-order.road:35`

```text
Asks the desk for a reservation and waits for the desk to end.
```

```mermaid
stateDiagram-v2
  Asking: invoke desk.reserve("o1")
  Asking: on desk.Closing / do heard()
  [*] --> Asking
  Asking --> Booked: done
  Asking --> Waiting: fail Unanswered
  Waiting --> Gone: desk.ended
  Booked --> [*]
  Gone --> [*]
```
