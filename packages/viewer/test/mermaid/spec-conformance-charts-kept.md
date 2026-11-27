## Tally

`spec/conformance/charts/kept.road:9`

```text
Counts what its parent sends it, and naps after five seconds without news.
```

```mermaid
stateDiagram-v2
  Counting: on Add / set n = n + event.by, emit Added { n#58; n }
  Napping: entry do log("nap")
  Done: output n
  [*] --> Counting
  Counting --> Napping: after 5s
  Counting --> Done: Finish
  Napping --> Counting: Add / set n = n + event.by, emit Added { n#58; n }
  Napping --> Done: Finish
  Done --> [*]
```

## Desk

`spec/conformance/charts/kept.road:36`

```text
Keeps a Tally, passes on what it is told, and hears what it counts.
```

```mermaid
stateDiagram-v2
  Open: invoke machine Tally(start) as tally
  Open: on Bump / send tally.Add { by#58; event.by }
  Open: on Close / send tally.Finish
  Open: on tally.Added / set heard = event.n
  Closed: output heard
  [*] --> Open
  Open --> Closed: done / set heard = result
  Closed --> [*]
```

## Office

`spec/conformance/charts/kept.road:58`

```text
Keeps a Desk: two levels of child runs.
```

```mermaid
stateDiagram-v2
  Working: invoke machine Desk(1) as desk
  Working: on Bump / send desk.Bump { by#58; event.by }
  Working: on Close / send desk.Close
  [*] --> Working
  Working --> Shut: done / do log("shut")
  Shut --> [*]
```

## Fetcher

`spec/conformance/charts/kept.road:77`

```text
Fetches a number, then holds it until it is released.
```

```mermaid
stateDiagram-v2
  Fetching: invoke fetch(n)
  Done: output got
  [*] --> Fetching
  Fetching --> Holding: done / set got = result
  Holding --> Done: Release
  Done --> [*]
```

## Errand

`spec/conformance/charts/kept.road:96`

```text
Keeps a Fetcher, and releases it when told.
```

```mermaid
stateDiagram-v2
  Waiting: invoke machine Fetcher(3) as fetcher
  Waiting: on Release / send fetcher.Release
  [*] --> Waiting
  Waiting --> Finished: done
  Finished --> [*]
```

## Safe

`spec/conformance/charts/kept.road:113`

```text
Keeps a secret given to it, and what it was last told.
```

```mermaid
stateDiagram-v2
  Locked: on Tell / set last = event.note
  [*] --> Locked
  Locked --> Gone: Shut
  Gone --> [*]
```

## Vault

`spec/conformance/charts/kept.road:130`

```text
Keeps a Safe, and passes on what it is told.
```

```mermaid
stateDiagram-v2
  Holding: invoke machine Safe(code) as safe
  Holding: on Tell / send safe.Tell { note#58; event.note }
  Holding: on Shut / send safe.Shut
  [*] --> Holding
  Holding --> Empty: done
  Empty --> [*]
```
