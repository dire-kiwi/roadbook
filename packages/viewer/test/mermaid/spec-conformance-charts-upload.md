## Upload

`spec/conformance/charts/upload.road:11`

```text
Sends chunks until `size` bytes are stored, reporting each one. Can be paused.
```

```mermaid
stateDiagram-v2
  Sending: invoke sendChunk(offset)
  Done: output offset
  [*] --> Sending
  Sending --> Done: always [offset >= size]
  Sending --> Sending: done / set offset = result, emit Progress { offset#58; result }
  Sending --> Paused: Pause
  Paused --> Sending: Resume
  Done --> [*]
```

## Publish

`spec/conformance/charts/upload.road:34`

```text
Uploads, pausing and resuming the child when asked, and shows its progress.
```

```mermaid
stateDiagram-v2
  Uploading: invoke machine Upload(size) as upload
  Uploading: on PauseClicked / send upload.Pause
  Uploading: on ResumeClicked / send upload.Resume
  Uploading: on upload.Progress / set shown = event.offset, do showProgress(event.offset)
  Published: output uploaded
  [*] --> Uploading
  Uploading --> Published: done / set uploaded = result
  Published --> [*]
```

## Quiet

`spec/conformance/charts/upload.road:56`

```text
Invokes Upload without `as`, so it receives nothing the child emits.
```

```mermaid
stateDiagram-v2
  Uploading: invoke machine Upload(size)
  Over: output uploaded
  [*] --> Uploading
  Uploading --> Over: done / set uploaded = result
  Over --> [*]
```

## Eager

`spec/conformance/charts/upload.road:71`

```text
Sends to its child before the child has started, and after its outcome was taken.
```

```mermaid
stateDiagram-v2
  state Uploading {
    state "Starting" as Uploading_Starting
    Uploading_Starting: entry send upload.Pause
    [*] --> Uploading_Starting
  }
  note right of Uploading
    invoke machine Upload(size) as upload
    done / set uploaded = result
    on PauseClicked / send upload.Pause
  end note
  Over: output uploaded
  [*] --> Uploading
  Uploading --> Over: Finish
  Over --> [*]
```
