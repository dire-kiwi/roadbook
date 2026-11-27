## Speaker

`spec/conformance/charts/speakers.road:7`

```text
Says two things when told to, and ends when told to quit.
```

```mermaid
stateDiagram-v2
  Talking: on Speak / emit Ping { n#58; 1 }, emit Pong { n#58; 2 }
  [*] --> Talking
  Talking --> Gone: Quit
  Gone --> [*]
```

## Listener

`spec/conformance/charts/speakers.road:24`

```text
Hears its speakers' `Ping`, from every key but "quiet", and never their `Pong`.
```

```mermaid
stateDiagram-v2
  Listening: on Add / spawn speakers[event.id] = Speaker(event.id)
  Listening: on Tell / send speakers[event.id].Speak
  Listening: on speakers.Ping [key != "quiet"] / set heard = heard + event.n
  [*] --> Listening
```

## Foreman

`spec/conformance/charts/speakers.road:43`

```text
Strict, with spawned children: what they emit or end with that it does not take is
dropped, and only its own events can be `unhandled`.
```

```mermaid
stateDiagram-v2
  Working: on Hire / spawn speakers[event.id] = Speaker(event.id)
  Working: on Tell / send speakers[event.id].Speak
  Working: on Finish / send speakers[event.id].Quit
  Working: on speakers.Ping [event.n > 5] / set heard = heard + event.n
  Working: on speakers.done [key == "boss"] / set heard = 0
  [*] --> Working
```
