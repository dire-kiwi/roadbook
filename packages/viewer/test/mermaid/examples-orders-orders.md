## Order

`examples/orders/orders.road:18`

```text
Reserve the items with the warehouse, then take the payment. The warehouse is a peer that
lives longer than any order: whoever starts the order passes it in.
```

```mermaid
stateDiagram-v2
  Reserving: invoke inventory.reserve(id, draft.items)
  Paying: invoke charge(id, draft)
  Paid: output receipt
  Unavailable: fail problem
  Declined: fail PaymentDeclined
  [*] --> Reserving
  Reserving --> Paying: done
  Reserving --> Unavailable: fail OutOfStock / set problem = error
  Reserving --> Unavailable: fail Unanswered / set problem = OutOfStock { reason#58; "the warehouse is closed" }
  Reserving --> Unavailable: after 5s / set problem = OutOfStock { reason#58; "the warehouse did not answer" }
  Paying --> Paid: done / set receipt = result
  Paying --> Declined: fail PaymentDeclined / send inventory.Release { orderId#58; id }
  Paid --> [*]
  Unavailable --> [*]
  Declined --> [*]
```

## Inventory

`inventory/inventory.road:18`

```text
The stock of one warehouse, shared by every order. An order calls `reserve`, and the answer
goes to that order alone.
```

```mermaid
stateDiagram-v2
  [*] --> Inventory_machine
  state "Inventory" as Inventory_machine
  state Inventory_machine {
    Reserving: invoke reserveStock(warehouse, orderId, items)
    Reserving: on reserve / reply call fail OutOfStock { reason#58; "busy" }
    [*] --> Ready
    Ready --> Reserving: reserve / set pending = call, set orderId = event.orderId, set items = event.items
    Ready --> Closed: Close
    Reserving --> Ready: done / reply pending done
    Reserving --> Ready: fail OutOfStock / reply pending fail error
    Closed --> [*]
  }
  note right of Inventory_machine
    on Release / do releaseStock(warehouse, event.orderId)
    on Restock / do addStock(warehouse, event.sku, event.qty)
  end note
```
