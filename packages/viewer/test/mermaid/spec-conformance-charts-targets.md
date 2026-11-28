## Player

`spec/conformance/charts/targets.road:9`

```text
Several targets enter several regions of a parallel state at once, the other regions by
default entry; written with dots in the parallel state, they exit and enter only those
regions.
```

```mermaid
stateDiagram-v2
  state On {
    state "Playback" as On_Playback
    state On_Playback {
      state "H" as On_Playback_Last
      state "Playing" as On_Playback_Playing
      state "Paused" as On_Playback_Paused
      [*] --> On_Playback_Playing
      On_Playback_Paused --> On_Playback_Playing: Play
    }
    --
    state "Volume" as On_Volume
    state On_Volume {
      state "Normal" as On_Volume_Normal
      state "Muted" as On_Volume_Muted
      state "Loud" as On_Volume_Loud
      [*] --> On_Volume_Normal
      On_Volume_Muted --> On_Volume_Normal: Unmute
    }
    --
    state "Light" as On_Light
    On_Light: entry do log("enter Light")
    On_Light: exit do log("exit Light")
    On_Light: after 1s / do log("tick")
  }
  note right of On
    entry do log("enter On")
    exit do log("exit On")
  end note
  [*] --> Idle
  Idle --> On_Playback_Paused: Wake
  Idle --> On_Volume_Muted: Wake
  Idle --> On_Playback_Last: Resume
  Idle --> On_Volume_Loud: Resume
  On --> On_Playback_Paused: Sleep
  On --> On_Volume_Muted: Sleep
  On --> Idle: Off
```
