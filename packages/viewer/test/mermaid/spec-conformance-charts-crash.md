## Crash

`spec/conformance/charts/crash.road:8`

```text
Runs two children and a task, one in each region.
```

```mermaid
stateDiagram-v2
  state Busy {
    state "A" as Busy_A
    Busy_A: invoke machine Child("A")
    --
    state "B" as Busy_B
    Busy_B: invoke machine Child("B")
    --
    state "C" as Busy_C
    Busy_C: invoke crash()
  }
  [*] --> Busy
  Busy_A --> Over: done
  Busy_B --> Over: done
  Busy_C --> Over: done
  Over --> [*]
```

## Child

`spec/conformance/charts/crash.road:35`

```text
Waits for Stop, which never comes, logging its exit.
```

```mermaid
stateDiagram-v2
  Waiting: exit do log(name)
  [*] --> Waiting
  Waiting --> Stopped: Stop
  Stopped --> [*]
```
