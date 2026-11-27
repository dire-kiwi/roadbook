## Inventory

`spec/conformance/charts/ended.road:16`

```text
The stock of one warehouse, which ends when asked to: done on Close, failed
on Fail, with a defect on Crash (when `crash` has one).
```

```mermaid
stateDiagram-v2
  [*] --> Inventory_machine
  state "Inventory" as Inventory_machine
  state Inventory_machine {
    Open: on Crash / do crash()
    Closed: output warehouse
    Broken: fail Shut
    [*] --> Open
    Open --> Closed: Close
    Open --> Broken: Fail
    Closed --> [*]
    Broken --> [*]
  }
  note right of Inventory_machine
    on Reserve ignore
  end note
```

## Order

`spec/conformance/charts/ended.road:38`

```text
Asks the warehouse for stock and waits for it, until it ends or 5s pass.
How the warehouse ended decides where the order goes.
```

```mermaid
stateDiagram-v2
  Waiting: entry send inventory.Reserve { orderId#58; id }
  [*] --> Waiting
  Waiting --> Closed: inventory.ended [event.status == Ended.Done]
  Waiting --> Alarm: inventory.ended [event.status == Ended.Defect] / do note(event.status)
  Waiting --> Gone: inventory.ended [else] / do note(event.status)
  Waiting --> TimedOut: after 5s
  Closed --> [*]
  Alarm --> [*]
  Gone --> [*]
  TimedOut --> [*]
```

## Clerk

`spec/conformance/charts/ended.road:59`

```text
Holds a `ref` to the warehouse but has no `on inventory.ended`: it is not
told when the warehouse ends.
```

```mermaid
stateDiagram-v2
  [*] --> Open
  Open --> Stopped: Stop
  Stopped --> [*]
```

## Picker

`spec/conformance/charts/ended.road:71`

```text
Hears the warehouse's end only while it is Watching: an end that arrives
while it is Idle is dropped.
```

```mermaid
stateDiagram-v2
  [*] --> Idle
  Idle --> Watching: Go
  Watching --> Gone: inventory.ended
  Gone --> [*]
```

## Guard

`spec/conformance/charts/ended.road:85`

```text
As Picker, but strict: an end that no transition takes is still dropped,
never `unhandled`.
```

```mermaid
stateDiagram-v2
  [*] --> Idle
  Idle --> Watching: Go
  Watching --> Gone: inventory.ended
  Gone --> [*]
```

## Switcher

`spec/conformance/charts/ended.road:99`

```text
Watches the warehouse its variable `current` holds: `a`, then `b` after
Swap. The end of a run `current` no longer holds is dropped.
```

```mermaid
stateDiagram-v2
  Watching: on Swap / set current = b
  [*] --> Watching
  Watching --> Gone: current.ended / do note(event.status)
  Gone --> [*]
```
