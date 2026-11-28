## Sides

`spec/conformance/charts/internal-parallel.road:8`

```text
Internal transitions in a parallel state: one written in a region stays in it, so another
region takes the same event too; one written in the parallel state exits and enters only
the regions it targets.
```

```mermaid
stateDiagram-v2
  state P {
    state "A" as P_A
    state P_A {
      state "A1" as P_A_A1
      state "A2" as P_A_A2
      [*] --> P_A_A1
    }
    note right of P_A
      entry do log("enter A")
      exit do log("exit A")
    end note
    --
    state "B" as P_B
    state P_B {
      state "B1" as P_B_B1
      state "B2" as P_B_B2
      [*] --> P_B_B1
      P_B_B1 --> P_B_B2: T
    }
    note right of P_B
      entry do log("enter B")
      exit do log("exit B")
    end note
  }
  note right of P
    entry do log("enter P")
    exit do log("exit P")
  end note
  [*] --> P
  P --> P_A_A2: Redo
  P --> P_B: Reset
  P_A --> P_A_A1: T
```
