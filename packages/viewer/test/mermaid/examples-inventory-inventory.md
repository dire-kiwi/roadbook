## Inventory

`examples/inventory/inventory.road:19`

```text
The stock of one warehouse, shared by every order. An order asks it to reserve items, and
it answers by emitting `Reserved` or `Rejected` with the order's id, which every holder of
a reference to it receives.
```

```mermaid
stateDiagram-v2
  [*] --> Inventory_machine
  state "Inventory" as Inventory_machine
  state Inventory_machine {
    Reserving: invoke reserveStock(warehouse, orderId, items)
    Reserving: on Reserve / emit Rejected { orderId#58; event.orderId, reason#58; "busy" }
    [*] --> Ready
    Ready --> Reserving: Reserve / set orderId = event.orderId, set items = event.items
    Ready --> Closed: Close
    Reserving --> Ready: done / emit Reserved { orderId#58; orderId }
    Reserving --> Ready: fail OutOfStock / emit Rejected { orderId#58; orderId, reason#58; error.reason }
    Closed --> [*]
  }
  note right of Inventory_machine
    on Release / do releaseStock(warehouse, event.orderId)
    on Restock / do addStock(warehouse, event.sku, event.qty)
  end note
```
