## Booth

`spec/conformance/charts/booth.road:8`

```text
Books seats, once it has been asked enough times.
```

```mermaid
stateDiagram-v2
  Open: on reserve [tries < 2] / set tries = tries + 1, reply call fail Busy { tries#58; tries }
  Open: on reserve [else] / reply call done event.seat
  [*] --> Open
```

## Guest

`spec/conformance/charts/booth.road:25`

```text
Books a seat, retrying while the booth is busy.
```

```mermaid
stateDiagram-v2
  state Booking {
    state "Attempt" as Booking_Attempt
    Booking_Attempt: invoke booth.reserve("12A")
    state "Backoff" as Booking_Backoff
    [*] --> Booking_Attempt
    Booking_Attempt --> Booking_Backoff: fail Busy [bookingAttempt < 2] / set bookingAttempt = bookingAttempt + 1
    Booking_Backoff --> Booking_Attempt: after 1s
  }
  note right of Booking
    entry set bookingAttempt = 0
  end note
  Booked: output seat
  Gave: output ""
  [*] --> Booking
  Booking_Attempt --> Booked: done / set seat = result
  Booking_Attempt --> Gave: fail Busy [else]
  Booking_Attempt --> Gave: fail Unanswered
  Booked --> [*]
  Gave --> [*]
```
