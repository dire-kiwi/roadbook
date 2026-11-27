## Door

`spec/conformance/charts/strict-messages.road:6`

```text
Takes a loud knock while shut and any knock while ajar; tells its visitors when it opens.
```

```mermaid
stateDiagram-v2
  Shut: on Knock [event.loud] ignore
  Ajar: on Knock ignore
  [*] --> Shut
  Shut --> Ajar: Open / emit Opened
  Ajar --> Shut: Close
```

## Visitor

`spec/conformance/charts/strict-messages.road:27`

```text
Knocks on its door when told to. Hears `Opened` only while it waits for it.
```

```mermaid
stateDiagram-v2
  [*] --> Visitor_machine
  state "Visitor" as Visitor_machine
  state Visitor_machine {
    [*] --> Idle
    Idle --> Waiting: Wait
    Waiting --> Idle: door.Opened
  }
  note right of Visitor_machine
    on Go / send door.Knock { loud#58; event.loud }
  end note
```
