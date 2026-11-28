## Watch

`spec/conformance/charts/invokes.road:25`

```text
Three invokes and a timer in one state, and one more invoke in its child state.
```

```mermaid
stateDiagram-v2
  state Watching {
    state "Inner" as Watching_Inner
    Watching_Inner: invoke work(2)
    [*] --> Watching_Inner
  }
  note right of Watching
    invoke heartbeat(server) as beat
    invoke copy(folder) as files
    invoke work(1) as extra
    files#58; fail Busy / set busy = busy + 1
  end note
  Copied: output copied
  Offline: output busy
  Stopped: output busy
  [*] --> Watching
  Watching --> Offline: beat#58; fail Lost
  Watching --> Copied: files#58; done / set copied = result
  Watching --> Stopped: files#58; fail *
  Watching --> Stopped: after 1m
  Watching --> Stopped: Stop
  Copied --> [*]
  Offline --> [*]
  Stopped --> [*]
```

## Lookup

`spec/conformance/charts/invokes.road:58`

```text
The same task twice in one state, told apart by `as`: `a`'s `fail *` is not `b`'s.
```

```mermaid
stateDiagram-v2
  Both: invoke fetch("a") as a
  Both: invoke fetch("b") as b
  Both: b#58; done / set got = result
  Both: b#58; fail Busy / set got = "b was busy"
  Found: output got
  Gave: output got
  [*] --> Both
  Both --> Found: a#58; done / set got = result
  Both --> Gave: a#58; fail *
  Found --> [*]
  Gave --> [*]
```

## Ticker

`spec/conformance/charts/invokes.road:79`

```text
A stream task beside the work that ends the state.
```

```mermaid
stateDiagram-v2
  Running: invoke ticks(1s) as clock
  Running: invoke work(3) as job
  Running: on clock.Tick / set ticks = ticks + 1
  Over: output ticks
  [*] --> Running
  Running --> Over: job#58; done
  Over --> [*]
```

## Kid

`spec/conformance/charts/invokes.road:96`

```text
Answers a poke, and ends when told to quit.
```

```mermaid
stateDiagram-v2
  Idle: on Poke / emit Poked
  [*] --> Idle
  Idle --> Gone: Quit
  Gone --> [*]
```

## Twins

`spec/conformance/charts/invokes.road:113`

```text
Two children in one state: each is sent to and heard from by its name, and the outcome of
one may send to the other, which is still running.
```

```mermaid
stateDiagram-v2
  Both: invoke machine Kid() as left
  Both: invoke machine Kid() as right
  Both: left#58; done / send right.Poke
  Both: on Go / send left.Poke
  Both: on Next / send left.Quit
  Both: on left.Poked / set heard = heard + 1
  Over: output heard
  [*] --> Both
  Both --> Over: right#58; done
  Both --> Over: right.Poked / set heard = heard + 10
  Over --> [*]
```
