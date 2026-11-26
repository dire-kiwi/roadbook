## Publish

`examples/upload/publish.road:17`

```text
Publish a post once its attachment is uploaded. The user can pause and resume the upload
and sees its progress.
```

```mermaid
stateDiagram-v2
  Uploading: invoke machine Upload(draft.file) as upload
  Uploading: on PauseClicked / send upload.Pause
  Uploading: on ResumeClicked / send upload.Resume
  Uploading: on upload.Progress / set percent = event.percent, do showProgress(event.percent)
  Publishing: invoke publishPost(draft.title, url)
  Published: output post
  Failed: fail failure
  [*] --> Uploading
  Uploading --> Publishing: done / set url = result
  Uploading --> Failed: fail UploadFailed / set failure = error
  Publishing --> Published: done / set post = result
  Published --> [*]
  Failed --> [*]
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
