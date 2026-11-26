## Panels

`spec/conformance/charts/history-regions.road:5`

```text
History states of a parallel state.
```

```mermaid
stateDiagram-v2
  state Both {
    state "H*" as Both_Deep
    state "H" as Both_Shallow
    state "L" as Both_L
    state Both_L {
      state "L1" as Both_L_L1
      state "L2" as Both_L_L2
      [*] --> Both_L_L1
      Both_L_L1 --> Both_L_L2: Left
    }
    --
    state "R" as Both_R
    state Both_R {
      state "R1" as Both_R_R1
      state "R2" as Both_R_R2
      state Both_R_R2 {
        state "R3" as Both_R_R2_R3
        [*] --> Both_R_R2_R3
      }
      [*] --> Both_R_R1
      Both_R_R1 --> Both_R_R2: Right
    }
  }
  [*] --> Both
  Both --> Out: Away
  Out --> Both_Deep: Back
  Out --> Both_Shallow: BackShallow
```

## Region

`spec/conformance/charts/history-regions.road:43`

```text
The history state of a compound state in one region of a parallel state.
```

```mermaid
stateDiagram-v2
  state P {
    state "A" as P_A
    state P_A {
      state "H*" as P_A_H
      state "A1" as P_A_A1
      state "A2" as P_A_A2
      [*] --> P_A_A1
      P_A_A1 --> P_A_A2: Go
    }
    --
    state "B" as P_B
    state P_B {
      state "B1" as P_B_B1
      state "B2" as P_B_B2
      [*] --> P_B_B1
      P_B_B1 --> P_B_B2: Go
    }
  }
  [*] --> P
  P --> Away: Out
  Away --> P_A_H: Back
```
