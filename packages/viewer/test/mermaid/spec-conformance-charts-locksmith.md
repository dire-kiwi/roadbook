## Locksmith

`spec/conformance/charts/locksmith.road:13`

```text
Cuts keys for its callers.
```

```mermaid
stateDiagram-v2
  Open: on cut [event.label == ""] / reply call fail Refused { reason#58; event.pin }
  Open: on cut [else] / reply call done Key { label#58; event.label, code#58; event.pin }
  [*] --> Open
  Open --> Closed: Close
  Closed --> [*]
```

## Client

`spec/conformance/charts/locksmith.road:33`

```text
Ask the locksmith for a key; give its label, or "" if it refused.
```

```mermaid
stateDiagram-v2
  Asking: invoke locksmith.cut(pin, label)
  Got: output cut.label
  Gave: output ""
  [*] --> Asking
  Asking --> Got: done / set cut = result
  Asking --> Gave: fail Refused
  Asking --> Gave: fail Unanswered
  Got --> [*]
  Gave --> [*]
```
