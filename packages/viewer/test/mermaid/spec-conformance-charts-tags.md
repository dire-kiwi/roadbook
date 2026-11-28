## Gallery

`spec/conformance/charts/tags.road:5`

```text
An image gallery that loads in two regions that share a tag.
```

```mermaid
stateDiagram-v2
  state Route <<choice>>
  state Viewing {
    state "Loading" as Viewing_Loading
    state Viewing_Loading {
      state "Thumbs" as Viewing_Loading_Thumbs
      --
      state "Full" as Viewing_Loading_Full
    }
    state "Shown" as Viewing_Shown
    [*] --> Viewing_Loading
    Viewing_Loading --> Viewing_Shown: Loaded
  }
  [*] --> Closed
  Closed --> Route: Open / set opened = opened + 1
  Closed --> Done: Quit
  Route --> Viewing_Shown: [opened > 1]
  Route --> Viewing: [else]
  Viewing --> Closed: Close
  Done --> [*]
```
