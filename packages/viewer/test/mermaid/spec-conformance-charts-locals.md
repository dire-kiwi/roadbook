## Counter

`spec/conformance/charts/locals.road:15`

```text
State variables: bound each time their state is entered, in order, an initial value
reading the ones before it and a child's reading its parent's.
```

```mermaid
stateDiagram-v2
  state Counting {
    state "Inner" as Counting_Inner
    Counting_Inner: var m#58; int = n * 2
    [*] --> Counting_Inner
  }
  note right of Counting
    var base#58; int = 10
    var n#58; int = base + 1
    var note#58; string?
    entry do log("counting")
    on Tick / set n = n + 1, set total = total + 1
  end note
  [*] --> Counting
  Idle --> Counting: Back
  Counting --> Idle: Leave
```

## Fetch

`spec/conformance/charts/locals.road:42`

```text
State parameters: arguments from `result`, from the left state's own locals on
re-entry, and from what the transition assigned.
```

```mermaid
stateDiagram-v2
  Loading: invoke fetch(id)
  state "Showing(body#58; string, count#58; int)" as Showing
  Showing: var seen#58; int = count * 10
  Showing: exit do log(body)
  [*] --> Loading
  Loading --> Showing: done -> Showing(result, 1)
  Showing --> Showing: Again -> Showing(body, count + 1)
  Showing --> Showing: Grow / set last = seen + count -> Showing(body, last)
```

## Editor

`spec/conformance/charts/locals.road:65`

```text
A target through a state with parameters, a transition inside it that keeps it, an
internal one, and one in the machine body.
```

```mermaid
stateDiagram-v2
  [*] --> Editor_machine
  state "Editor" as Editor_machine
  state Editor_machine {
    state "Editing(doc#58; string)" as Editing
    state Editing {
      state "Viewing" as Editing_Viewing
      state "Changing(step#58; int)" as Editing_Changing
      [*] --> Editing_Viewing
      Editing_Viewing --> Editing_Changing: Next -> Changing(edits)
      Editing_Changing --> Editing_Viewing: Next
    }
    note right of Editing
      var edits#58; int = 0
      exit do log(doc)
    end note
    [*] --> Idle
    Idle --> Editing_Changing: Open -> Editing(event.doc).Changing(1)
    Editing --> Editing_Changing: Jump / set edits = edits + 1 -> .Changing(edits + 5)
  }
  Editor_machine --> Idle: Reset
```

## Verdict

`spec/conformance/charts/locals.road:89`

```text
A final state's parameter and variable as the machine's output.
```

```mermaid
stateDiagram-v2
  state "Passed(score#58; int)" as Passed
  Passed: output score
  state "Low(score#58; int)" as Low
  Low: var doubled#58; int = score * 2
  Low: output doubled
  [*] --> Waiting
  Waiting --> Passed: Judge [event.score > 5] -> Passed(event.score)
  Waiting --> Low: Judge [else] -> Low(event.score)
  Passed --> [*]
  Low --> [*]
```

## Courier

`spec/conformance/charts/locals.road:110`

```text
A retry in a state with parameters: its locals live through every attempt.
```

```mermaid
stateDiagram-v2
  state "Sending(to#58; string)" as Sending
  state Sending {
    state "Attempt" as Sending_Attempt
    Sending_Attempt: invoke deliver(to)
    state "Backoff" as Sending_Backoff
    [*] --> Sending_Attempt
    Sending_Attempt --> Sending_Backoff: fail Busy [sendingAttempt < 2] / set sendingAttempt = sendingAttempt + 1
    Sending_Backoff --> Sending_Attempt: after wait(sendingAttempt)
  }
  note right of Sending
    entry set sendingAttempt = 0
    entry do log(to)
  end note
  [*] --> Idle
  Idle --> Sending: Go -> Sending(event.to)
  Sending_Attempt --> Idle: done
  Sending_Attempt --> Idle: fail Busy [else]
```

## Overflow

`spec/conformance/charts/locals.road:127`

```text
An argument that overflows: the step is atomic and the run ends with a defect.
```

```mermaid
stateDiagram-v2
  A: var twice#58; float = big * 2.0
  state "B(n#58; float)" as B
  [*] --> A
  A --> B: Go -> B(twice * 10.0)
  B --> [*]
```

## Vault

`spec/conformance/charts/locals.road:141`

```text
Sensitive parameters and variables are redacted in `set` records.
```

```mermaid
stateDiagram-v2
  state "Open(sensitive pin#58; int, user#58; string)" as Open
  Open: sensitive var guess#58; int = pin
  Open: var shown#58; string = user
  Open: on Wrong / set guess = guess + 1
  [*] --> Locked
  Locked --> Open: Unlock -> Open(event.pin, event.user)
  Open --> Locked: Lock
```
