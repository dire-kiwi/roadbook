## Counter

`spec/conformance/charts/saved.road:13`

```text
A counter that waits for events in a state with no work.
```

```mermaid
stateDiagram-v2
  Counting: on Add / set n = n + event.by, do count("added", n)
  Done: output n
  [*] --> Counting
  Counting --> Done: Finish
  Done --> [*]
```

## Shop

`spec/conformance/charts/saved.road:30`

```text
Pays through a child machine.
```

```mermaid
stateDiagram-v2
  Paying: invoke machine Pay(amount)
  Paid: output receipt
  [*] --> Paying
  Paying --> Paid: done / set receipt = result
  Paid --> [*]
```

## Pay

`spec/conformance/charts/saved.road:45`

```text
Quotes a price, then charges it.
```

```mermaid
stateDiagram-v2
  Quoting: invoke quote(amount)
  Charging: invoke charge(price)
  Charged: output receipt
  [*] --> Quoting
  Quoting --> Charging: done / set price = result
  Charging --> Charged: done / set receipt = result
  Charged --> [*]
```

## Reminder

`spec/conformance/charts/saved.road:67`

```text
Rings a second after it starts waiting.
```

```mermaid
stateDiagram-v2
  Ringing: entry do log("ring")
  [*] --> Waiting
  Waiting --> Ringing: after 1s
  Ringing --> Waiting: Snooze
```

## Order

`spec/conformance/charts/saved.road:83`

```text
An order kept as a snapshot between requests: it waits three days for
payment, ships, and waits for delivery.
```

```mermaid
stateDiagram-v2
  Shipping: invoke ship(id)
  Expired: on PaymentReceived / do refund(id, event.amount)
  [*] --> AwaitingPayment
  AwaitingPayment --> Expired: after 72h
  AwaitingPayment --> Shipping: PaymentReceived / set paid = event.amount
  Shipping --> AwaitingDelivery: done / set tracking = result
  AwaitingDelivery --> Complete: Delivered
  Complete --> [*]
```
