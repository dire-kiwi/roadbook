## ConfirmPasswordReset

`examples/password-reset/confirm.road:11`

```text
POST /password-reset/confirm: exchange a reset token and a new password
for a changed password. Every final state is one HTTP response.
```

```mermaid
stateDiagram-v2
  state Working {
    state "LookingUp" as Working_LookingUp
    Working_LookingUp: invoke findResetToken(token)
    state "CheckingPolicy" as Working_CheckingPolicy
    Working_CheckingPolicy: entry set problems = passwordProblems(newPassword)
    state "Hashing" as Working_Hashing
    Working_Hashing: invoke hashPassword(newPassword)
    state "Updating" as Working_Updating
    Working_Updating: invoke updatePassword(reset.userId, hash)
    state "Notifying" as Working_Notifying
    Working_Notifying: entry do revokeSessions(reset.userId)
    Working_Notifying: entry do sendPasswordChangedEmail(reset.userId)
    [*] --> Working_LookingUp
    Working_LookingUp --> Working_CheckingPolicy: done / set reset = result
    Working_CheckingPolicy --> Working_Hashing: always [isEmpty(problems)]
    Working_Hashing --> Working_Updating: done / set hash = result
    Working_Updating --> Working_Notifying: done
  }
  Changed: output noContent()
  UnknownToken: output problem(404, "unknown_token")
  ExpiredToken: output problem(410, "token_expired")
  WeakPassword: output weakPassword(problems)
  UserVanished: output problem(409, "user_gone")
  TimedOut: output problem(503, "timeout")
  [*] --> Working
  Working --> TimedOut: after 10s
  Working_LookingUp --> UnknownToken: fail TokenNotFound
  Working_LookingUp --> ExpiredToken: fail TokenExpired
  Working_CheckingPolicy --> WeakPassword: always [else]
  Working_Updating --> UserVanished: fail UserGone
  Working_Notifying --> Changed: always
  Changed --> [*]
  UnknownToken --> [*]
  ExpiredToken --> [*]
  WeakPassword --> [*]
  UserVanished --> [*]
  TimedOut --> [*]
```
