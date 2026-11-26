## Inventory

`spec/conformance/charts/calls.road:19`

```text
The stock of one warehouse, shared by every order.
```

```mermaid
stateDiagram-v2
  [*] --> Inventory_machine
  state "Inventory" as Inventory_machine
  state Inventory_machine {
    Reserving: invoke reserveStock(warehouse, orderId, items)
    Reserving: on reserve / reply call fail OutOfStock { reason#58; "busy" }
    Reserving: on Twice / reply pending fail OutOfStock { reason#58; "first" }, reply pending fail OutOfStock { reason#58; "second" }
    [*] --> Ready
    Ready --> Reserving: reserve / set pending = call, set orderId = event.orderId, set items = event.items
    Ready --> Paused: Pause
    Ready --> Closed: Close
    Reserving --> Ready: done / set held = held + 1, reply pending done Reservation { orderId#58; orderId, count#58; result }
    Reserving --> Ready: fail OutOfStock / reply pending fail error
    Reserving --> Closed: Close
    Paused --> Ready: Resume
    Closed --> [*]
  }
  note right of Inventory_machine
    on count / reply call done held
  end note
```

## Order

`spec/conformance/charts/calls.road:64`

```text
Reserve the items with the warehouse, giving up after 5s.
```

```mermaid
stateDiagram-v2
  Reserving: invoke inventory.reserve(id, items)
  Reserved: output reservation
  Failed: fail problem
  [*] --> Reserving
  Reserving --> Reserved: done / set reservation = result
  Reserving --> Failed: fail OutOfStock / set problem = error
  Reserving --> Failed: fail Unanswered / set problem = OutOfStock { reason#58; "unanswered" }
  Reserving --> Failed: after 5s / set problem = OutOfStock { reason#58; "timed out" }
  Reserved --> [*]
  Failed --> [*]
```

## Counter

`spec/conformance/charts/calls.road:84`

```text
Ask the warehouse how many reservations it holds; 0 if it cannot say.
```

```mermaid
stateDiagram-v2
  Asking: invoke inventory.count()
  Got: output n
  Gave: output 0
  [*] --> Asking
  Asking --> Got: done / set n = result
  Asking --> Gave: fail *
  Got --> [*]
  Gave --> [*]
```
