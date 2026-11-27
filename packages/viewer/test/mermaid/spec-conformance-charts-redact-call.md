## Issuer

`spec/conformance/charts/redact-call.road:11`

```text
Issues tokens while it is open; strict, so a call while closed is a defect.
```

```mermaid
stateDiagram-v2
  Open: on issue / reply call done Token { user#58; event.user, secret#58; event.pin }
  [*] --> Open
  Open --> Shut: Close
  Shut --> Open: Reopen
```

## Client

`spec/conformance/charts/redact-call.road:30`

```text
Asks the issuer for a token; gives the user it was issued to, or "".
```

```mermaid
stateDiagram-v2
  Asking: invoke issuer.issue(user, pin)
  Got: output user
  Gave: output ""
  [*] --> Asking
  Asking --> Got: done / set token = result
  Asking --> Gave: fail Unanswered
  Got --> [*]
  Gave --> [*]
```
