## Batch

`examples/upload/batch.road:14`

```text
Upload any number of files at once: one `Upload` per file, keyed by the file's name. The
user can add files while others upload, cancel one, and pause and resume them all.
```

```mermaid
stateDiagram-v2
  Open: on Add / spawn uploads[event.file.name] = Upload(event.file)
  Open: on Cancel / stop uploads[event.name]
  Open: on PauseAll / send uploads.*.Pause
  Open: on ResumeAll / send uploads.*.Resume
  Open: on uploads.Progress / do showFileProgress(key, event.percent)
  Open: on uploads.done / set uploaded = uploaded + 1, do showUploaded(key, result)
  Open: on uploads.fail * / do reportFailure(key, error.reason)
  Closed: output uploaded
  [*] --> Open
  Open --> Closed: Close
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
