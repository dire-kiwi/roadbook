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
    state "CheckingPolicy(reset#58; ResetToken)" as Working_CheckingPolicy
    Working_CheckingPolicy: var problems#58; string[] = passwordProblems(newPassword)
    state "Hashing(reset#58; ResetToken)" as Working_Hashing
    Working_Hashing: invoke hashPassword(newPassword)
    state "Updating(reset#58; ResetToken, hash#58; string)" as Working_Updating
    Working_Updating: invoke updatePassword(reset.userId, hash)
    state "Notifying(userId#58; UserId)" as Working_Notifying
    Working_Notifying: entry do revokeSessions(userId)
    Working_Notifying: entry do sendPasswordChangedEmail(userId)
    [*] --> Working_LookingUp
    Working_LookingUp --> Working_CheckingPolicy: done -> CheckingPolicy(result)
    Working_CheckingPolicy --> Working_Hashing: always [isEmpty(problems)] -> Hashing(reset)
    Working_Hashing --> Working_Updating: done -> Updating(reset, result)
    Working_Updating --> Working_Notifying: done -> Notifying(reset.userId)
  }
  Changed: output noContent()
  UnknownToken: output problem(404, "unknown_token")
  ExpiredToken: output problem(410, "token_expired")
  state "WeakPassword(problems#58; string[])" as WeakPassword
  WeakPassword: output weakPassword(problems)
  UserVanished: output problem(409, "user_gone")
  TimedOut: output problem(503, "timeout")
  [*] --> Working
  Working --> TimedOut: after 5s
  Working_LookingUp --> UnknownToken: fail TokenNotFound
  Working_LookingUp --> ExpiredToken: fail TokenExpired
  Working_CheckingPolicy --> WeakPassword: always [else] -> WeakPassword(problems)
  Working_Updating --> UserVanished: fail UserGone
  Working_Notifying --> Changed: always
  Changed --> [*]
  UnknownToken --> [*]
  ExpiredToken --> [*]
  WeakPassword --> [*]
  UserVanished --> [*]
  TimedOut --> [*]
```
