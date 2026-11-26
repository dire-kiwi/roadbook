## Parent

`spec/conformance/charts/cancel.road:7`

```text
Runs Child until Stop, then ends.
```

```mermaid
stateDiagram-v2
  [*] --> Parent_machine
  state "Parent" as Parent_machine
  state Parent_machine {
    Running: invoke machine Child()
    Running: exit do log("exit Running")
    [*] --> Running
    Running --> Over: done
    Running --> Over: Stop / do log("stop")
    Over --> [*]
  }
  note right of Parent_machine
    exit do log("exit Parent")
  end note
```

## Child

`spec/conformance/charts/cancel.road:27`

```text
Waits five seconds two levels down, logging every exit.
```

```mermaid
stateDiagram-v2
  [*] --> Child_machine
  state "Child" as Child_machine
  state Child_machine {
    state Outer {
      state "Inner" as Outer_Inner
      Outer_Inner: exit do log("exit Inner")
      [*] --> Outer_Inner
    }
    note right of Outer
      exit do log("exit Outer")
    end note
    [*] --> Outer
    Outer_Inner --> Finished: after 5s
    Finished --> [*]
  }
  note right of Child_machine
    exit do log("exit Child")
  end note
```
