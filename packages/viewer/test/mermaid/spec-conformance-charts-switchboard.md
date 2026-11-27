## Line

`spec/conformance/charts/switchboard.road:7`

```text
One open line, until it is hung up.
```

```mermaid
stateDiagram-v2
  [*] --> Up
  Up --> Down: Hang
  Down --> [*]
```

## Switchboard

`spec/conformance/charts/switchboard.road:19`

```text
Opens lines, holds a lookup taken at night, and pauses to answer it, coming back to the
state it was in.
```

```mermaid
stateDiagram-v2
  [*] --> Switchboard_machine
  state "Switchboard" as Switchboard_machine
  state Switchboard_machine {
    state Working {
      state "H" as Working_Last
      state "Day" as Working_Day
      state "Night" as Working_Night
      state "Held" as Working_Held
      [*] --> Working_Day
      Working_Day --> Working_Night: Night
      Working_Night --> Working_Held: lookup / set pending = call, set asked = event.name
    }
    [*] --> Working
    Working_Held --> Paused: Pause
    Paused --> Answered: Answer / reply pending done 7
    Answered --> Working_Last: Resume
  }
  note right of Switchboard_machine
    on Open / spawn lines[event.n] = Line(event.n)
  end note
```

## Caller

`spec/conformance/charts/switchboard.road:52`

```text
Looks a name up.
```

```mermaid
stateDiagram-v2
  Asking: invoke board.lookup("ada")
  Got: output number
  [*] --> Asking
  Asking --> Got: done / set number = result
  Asking --> Got: fail Unanswered
  Got --> [*]
```
