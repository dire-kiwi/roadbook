## Editor

`spec/conformance/charts/history.road:10`

```text
Shallow and deep history of one compound state.
```

```mermaid
stateDiagram-v2
  state Work {
    state "H" as Work_Shallow
    state "H*" as Work_Deep
    state "Draft" as Work_Draft
    Work_Draft: entry do log("enter Draft")
    Work_Draft: exit do log("exit Draft")
    state "Review" as Work_Review
    state Work_Review {
      state "Read" as Work_Review_Read
      state "Comment" as Work_Review_Comment
      Work_Review_Comment: invoke fetch()
      [*] --> Work_Review_Read
      Work_Review_Read --> Work_Review_Comment: Deeper
      Work_Review_Comment --> Work_Review_Read: done
      Work_Review_Comment --> Work_Review_Read: after 1s
    }
    note right of Work_Review
      entry do log("enter Review")
      exit do log("exit Review")
    end note
    [*] --> Work_Draft
    Work_Draft --> Work_Review: Next
    Work_Draft --> Work_Shallow: Jump
    Work_Review --> Work_Draft: Edit
  }
  note right of Work
    entry do log("enter Work")
    exit do log("exit Work")
  end note
  [*] --> Work
  Work --> Help: Help
  Work --> Work_Deep: Restart
  Help --> Work_Shallow: Back
  Help --> Work_Deep: BackDeep
```

## Steps

`spec/conformance/charts/history.road:66`

```text
A deep history state that restores a final state, completing its parent.
```

```mermaid
stateDiagram-v2
  state Flow {
    state "H*" as Flow_H
    state "S1" as Flow_S1
    state "S2" as Flow_S2
    [*] --> Flow_S1
    Flow_S1 --> Flow_S2: Finish
    Flow_S2 --> [*]
  }
  Paused: on Ready / set ready = true
  [*] --> Flow
  Flow --> Finished: done [ready]
  Flow --> Paused: Pause
  Paused --> Flow_H: Resume
  Finished --> [*]
```

## Bounce

`spec/conformance/charts/history.road:96`

```text
A parent left and returned to through its history state while settling.
```

```mermaid
stateDiagram-v2
  state P {
    state "H" as P_H
    state "A" as P_A
    state "B" as P_B
    [*] --> P_A
    P_A --> P_B: Go
  }
  [*] --> P
  P_B --> Q: always [n == 0] / set n = 1
  Q --> P_H: always / do log("back")
```
