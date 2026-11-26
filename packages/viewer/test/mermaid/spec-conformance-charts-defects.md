## Main

`spec/conformance/charts/defects.road:13`

```text
Loads a number or asks Sub for one.
```

```mermaid
stateDiagram-v2
  Idle: entry do note(memo)
  Loading: invoke load()
  Calling: invoke machine Sub()
  Finished: output 0
  [*] --> Idle
  Idle --> Loading: UseLoad
  Idle --> Calling: UseSub
  Loading --> Finished: done [result > limit]
  Loading --> Idle: done [else]
  Loading --> Idle: after 1s
  Calling --> Finished: done
  Calling --> Idle: fail Nope
  Finished --> [*]
```

## Sub

`spec/conformance/charts/defects.road:51`

```text
Stubbed in the cases that use it.
```

```mermaid
stateDiagram-v2
  Answer: output 1
  [*] --> Answer
  Answer --> [*]
```
