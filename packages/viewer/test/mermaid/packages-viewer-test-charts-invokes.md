## Helper

`packages/viewer/test/charts/invokes.road:13`

```text
Answers a poke.
```

```mermaid
stateDiagram-v2
  Idle: on Poke / emit Poked
  [*] --> Idle
  Idle --> Gone: Quit
  Gone --> [*]
```

## Watch

`packages/viewer/test/charts/invokes.road:29`

```text
Watches a server while it syncs a folder, with a helper beside them.
```

```mermaid
stateDiagram-v2
  Watching: invoke heartbeat(server) as beat
  Watching: invoke syncFiles(folder) as sync
  Watching: invoke machine Helper() as helper
  Watching: on helper.Poked / set synced = 0
  Synced: output synced
  Offline: output 0
  [*] --> Watching
  Watching --> Offline: beat#58; fail Lost
  Watching --> Synced: sync#58; done / set synced = result
  Watching --> Offline: sync#58; fail *
  Watching --> Offline: Stop
  Synced --> [*]
  Offline --> [*]
```
