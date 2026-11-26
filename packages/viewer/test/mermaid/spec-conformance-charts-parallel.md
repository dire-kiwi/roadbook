## Regions

`spec/conformance/charts/parallel.road:11`

```text
Enters and exits three regions of different kinds, in document order.
```

```mermaid
stateDiagram-v2
  [*] --> Regions_machine
  state "Regions" as Regions_machine
  state Regions_machine {
    state Both {
      state "Left" as Both_Left
      state Both_Left {
        state "L1" as Both_Left_L1
        Both_Left_L1: entry do log("enter L1")
        Both_Left_L1: exit do log("exit L1")
        [*] --> Both_Left_L1
      }
      note right of Both_Left
        entry do log("enter Left")
        exit do log("exit Left")
      end note
      --
      state "Right" as Both_Right
      state Both_Right {
        state "R1" as Both_Right_R1
        state Both_Right_R1 {
          state "R2" as Both_Right_R1_R2
          Both_Right_R1_R2: entry do log("enter R2")
          Both_Right_R1_R2: exit do log("exit R2")
          [*] --> Both_Right_R1_R2
        }
        [*] --> Both_Right_R1
      }
      --
      state "Lone" as Both_Lone
      Both_Lone: entry do log("enter Lone")
      Both_Lone: exit do log("exit Lone")
    }
    note right of Both
      entry do log("enter Both")
      exit do log("exit Both")
    end note
    [*] --> Both
    Both --> Away: Leave / do log("leave")
    Away --> Both: Back
  }
  note right of Regions_machine
    entry do log("enter Regions")
    exit do log("exit Regions")
  end note
```

## Fork

`spec/conformance/charts/parallel.road:62`

```text
One event taken in two regions, and `always` transitions in two regions
taken in one microstep.
```

```mermaid
stateDiagram-v2
  state P {
    state "A" as P_A
    state P_A {
      state "A1" as P_A_A1
      state "A2" as P_A_A2
      state "A3" as P_A_A3
      [*] --> P_A_A1
      P_A_A1 --> P_A_A2: Go / set n = n + 1, do count("A go", n)
      P_A_A2 --> P_A_A3: always [n > 1] / do count("A always", n)
    }
    --
    state "B" as P_B
    state P_B {
      state "B1" as P_B_B1
      state "B2" as P_B_B2
      state "B3" as P_B_B3
      P_B_B3: on Go / do count("B3 go", n)
      [*] --> P_B_B1
      P_B_B1 --> P_B_B2: Go / set n = n + 10, do count("B go", n)
      P_B_B2 --> P_B_B3: always / do log("B always")
    }
    --
    state "C" as P_C
    P_C: on Tick / do log("tick")
  }
  [*] --> P
```

## Clash

`spec/conformance/charts/parallel.road:96`

```text
Two regions' transitions for one event that would exit the same state.
```

```mermaid
stateDiagram-v2
  state P {
    state "A" as P_A
    state P_A {
      state "A1" as P_A_A1
      [*] --> P_A_A1
    }
    --
    state "B" as P_B
    state P_B {
      state "B1" as P_B_B1
      state "B2" as P_B_B2
      [*] --> P_B_B1
      P_B_B1 --> P_B_B2: Go / do log("B go")
      P_B_B1 --> P_B_B2: Swap / do log("B swap")
    }
  }
  [*] --> P
  P --> Out: Swap / do log("P swap")
  P_A_A1 --> Out: Go / do log("A go")
  Out --> P: Back
```

## Escape

`spec/conformance/charts/parallel.road:129`

```text
Transitions from inside a region: into another region, and out of the
parallel state.
```

```mermaid
stateDiagram-v2
  state P {
    state "A" as P_A
    state P_A {
      state "A1" as P_A_A1
      [*] --> P_A_A1
    }
    --
    state "B" as P_B
    state P_B {
      state "B1" as P_B_B1
      P_B_B1: exit do log("exit B1")
      state "B2" as P_B_B2
      [*] --> P_B_B1
      P_B_B1 --> P_B_B2: after 1s
    }
  }
  note right of P
    exit do log("exit P")
  end note
  [*] --> P
  P_A_A1 --> P_B_B1: Cross / do log("cross")
  P_A_A1 --> Done: Out / do log("out")
  Done --> [*]
```

## Finish

`spec/conformance/charts/parallel.road:164`

```text
A parallel state completes when every region is in a final state.
```

```mermaid
stateDiagram-v2
  state Work {
    state "Left" as Work_Left
    state Work_Left {
      state "L1" as Work_Left_L1
      state "L2" as Work_Left_L2
      [*] --> Work_Left_L1
      Work_Left_L1 --> Work_Left_L2: A
      Work_Left_L2 --> [*]
    }
    note right of Work_Left
      done / do log("left done")
    end note
    --
    state "Mid" as Work_Mid
    --
    state "Right" as Work_Right
    state Work_Right {
      state "R1" as Work_Right_R1
      state "R2" as Work_Right_R2
      [*] --> Work_Right_R1
      Work_Right_R1 --> Work_Right_R2: B
      Work_Right_R2 --> [*]
    }
    Work_Mid --> [*]
  }
  Over: output "over"
  [*] --> Work
  Work --> Over: done / do log("work done")
  Over --> [*]
```

## Jobs

`spec/conformance/charts/parallel.road:198`

```text
Invokes and timers in two regions and on the parallel state itself.
```

```mermaid
stateDiagram-v2
  state Busy {
    state "Up" as Busy_Up
    state Busy_Up {
      state "Uploading" as Busy_Up_Uploading
      Busy_Up_Uploading: invoke upload()
      state "Uploaded" as Busy_Up_Uploaded
      [*] --> Busy_Up_Uploading
      Busy_Up_Uploading --> Busy_Up_Uploaded: done
      Busy_Up_Uploading --> Busy_Up_Uploaded: after 2s
    }
    --
    state "Down" as Busy_Down
    state Busy_Down {
      state "Downloading" as Busy_Down_Downloading
      Busy_Down_Downloading: invoke download()
      state "Downloaded" as Busy_Down_Downloaded
      [*] --> Busy_Down_Downloading
      Busy_Down_Downloading --> Busy_Down_Downloaded: done
      Busy_Down_Downloading --> Busy_Down_Downloading: after 3s
    }
  }
  [*] --> Busy
  Busy --> Late: after 5s
  Busy --> Stopped: Stop
```

## Shell

`spec/conformance/charts/parallel.road:243`

```text
A parallel state inside a compound state, left by the compound state's
transition.
```

```mermaid
stateDiagram-v2
  state Outer {
    state "Split" as Outer_Split
    state Outer_Split {
      state "X" as Outer_Split_X
      Outer_Split_X: exit do log("exit X")
      --
      state "Y" as Outer_Split_Y
      state Outer_Split_Y {
        state "Y1" as Outer_Split_Y_Y1
        Outer_Split_Y_Y1: exit do log("exit Y1")
        state "Y2" as Outer_Split_Y_Y2
        [*] --> Outer_Split_Y_Y1
        Outer_Split_Y_Y1 --> Outer_Split_Y_Y2: after 1s
      }
    }
    note right of Outer_Split
      exit do log("exit Split")
    end note
    [*] --> Outer_Split
  }
  note right of Outer
    exit do log("exit Outer")
  end note
  [*] --> Outer
  Outer --> Gone: Leave / do log("leave")
```
