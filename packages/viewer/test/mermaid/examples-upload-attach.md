## Attach

`examples/upload/attach.road:26`

```text
Attach a file to a message: store it with a task that reports its progress, then wait for
the user to confirm, showing how long they have waited.
```

```mermaid
stateDiagram-v2
  Storing: invoke storeFile(file) as store
  Storing: on store.Progress / do showProgress(event.percent)
  Storing: on store.Retrying / do showRetrying(event.attempt)
  Confirming: invoke ticks(1s) as clock
  Confirming: on clock.Tick / set waited = waited + 1, do showWaiting(waited)
  Attached: output url
  Failed: fail failure
  [*] --> Storing
  Storing --> Confirming: done / set url = result
  Storing --> Failed: fail UploadFailed / set failure = error
  Confirming --> Attached: Confirm
  Attached --> [*]
  Failed --> [*]
```

## AttachMany

`examples/upload/attach.road:58`

```text
Attach several files at once: one `storeFile` task per file, keyed by the file's name, in a
collection of tasks. Counts the files stored.
```

```mermaid
stateDiagram-v2
  Open: on Add / spawn stores[event.file.name] = storeFile(event.file)
  Open: on Cancel / stop stores[event.name]
  Open: on stores.Progress / do showFileProgress(key, event.percent)
  Open: on stores.done / set stored = stored + 1
  Open: on stores.fail UploadFailed / do showFileProgress(key, 0)
  Closed: output stored
  [*] --> Open
  Open --> Closed: Done
  Closed --> [*]
```

## Upload

`examples/upload/upload.road:23`

```text
Upload a file in chunks. Can be paused, and reports its progress to whoever invoked it.
```

```mermaid
stateDiagram-v2
  Sending: invoke sendChunk(file, offset)
  Finishing: invoke finishUpload(file)
  Done: output url
  Failed: fail failure
  [*] --> Sending
  Sending --> Finishing: always [complete(file, offset)]
  Sending --> Sending: done / set offset = result, emit Progress { percent#58; percentOf(file, result) }
  Sending --> Failed: fail UploadFailed / set failure = error
  Sending --> Paused: Pause
  Paused --> Sending: Resume
  Finishing --> Done: done / set url = result
  Finishing --> Failed: fail UploadFailed / set failure = error
  Done --> [*]
  Failed --> [*]
```
