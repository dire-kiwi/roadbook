## Ticker

`packages/viewer/test/charts/stress.road:11`

```text
Ticks on its own.
```

```mermaid
stateDiagram-v2
  Done: output n
  [*] --> Ticking
  Ticking --> Ticking: after 1s / do note("tick")
  Ticking --> Stopping: after 1m
  Stopping --> Done: always
  Done --> [*]
```

## Stress

`packages/viewer/test/charts/stress.road:22`

```text
Every shape the viewer has to lay out, at once.
```

```mermaid
stateDiagram-v2
  [*] --> Stress_machine
  state "Stress" as Stress_machine
  state Stress_machine {
    Start: on * / do note("ignored")
    state Running {
      state "Left" as Running_Left
      state Running_Left {
        state "A" as Running_Left_A
        state "B" as Running_Left_B
        Running_Left_B: invoke step(n)
        state "C" as Running_Left_C
        state "LeftDone" as Running_Left_LeftDone
        [*] --> Running_Left_A
        Running_Left_A --> Running_Left_A: Poke / do note("a")
        Running_Left_A --> Running_Left_B: after 5s
        Running_Left_B --> Running_Left_C: done / set n = result
        Running_Left_B --> Running_Left_B: fail Flaky [n < 3] / set n = n + 1
        Running_Left_B --> Running_Left_A: fail Flaky [else]
        Running_Left_C --> Running_Left_LeftDone: always
        Running_Left_LeftDone --> [*]
      }
      --
      state "Right" as Running_Right
      state Running_Right {
        state "X" as Running_Right_X
        state Running_Right_X {
          state "X1" as Running_Right_X_X1
          state "X2" as Running_Right_X_X2
          state Running_Right_X_X2 {
            state "Deep" as Running_Right_X_X2_Deep
            state Running_Right_X_X2_Deep {
              state "Deeper" as Running_Right_X_X2_Deep_Deeper
              [*] --> Running_Right_X_X2_Deep_Deeper
            }
            [*] --> Running_Right_X_X2_Deep
          }
          [*] --> Running_Right_X_X1
          Running_Right_X_X1 --> Running_Right_X_X1: Poke
          Running_Right_X_X1 --> Running_Right_X_X2: Go
          Running_Right_X_X2 --> Running_Right_X_X1: Poke
        }
        state "Y" as Running_Right_Y
        Running_Right_Y: invoke machine Ticker(n)
        state "RightDone" as Running_Right_RightDone
        [*] --> Running_Right_X
        Running_Right_X --> Running_Right_Y: Stop
        Running_Right_Y --> Running_Right_RightDone: done / set n = result
        Running_Right_RightDone --> [*]
      }
    }
    Finished: output n
    Failed: output 0 - 1
    Expired: output 0 - 2
    Quit1: output 1
    Quit2: output 2
    Quit3: output 3
    Quit4: output 4
    Quit5: output 5
    [*] --> Start
    Start --> Running: Go
    Start --> Start: Poke / set n = n + 1
    Start --> Exits: Stop
    Running --> Stopped: Stop
    Running --> Finished: done
    Running_Left_B --> Failed: fail *
    Running_Right_X_X2_Deep_Deeper --> Running_Left_C: Go
    Stopped --> Running_Right_X_X2: Go
    Exits --> Quit1: Go
    Exits --> Quit2: Poke
    Exits --> Quit3: Stop
    Exits --> Quit4: after 1s
    Exits --> Quit5: always
    Finished --> [*]
    Failed --> [*]
    Expired --> [*]
    Quit1 --> [*]
    Quit2 --> [*]
    Quit3 --> [*]
    Quit4 --> [*]
    Quit5 --> [*]
  }
  Stress_machine --> Start: Reset
  Stress_machine --> Expired: after 1h
```
