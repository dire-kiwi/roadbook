## PlaceOrder

`examples/shop/place_order.road:11`

```text
POST /orders: authenticate, validate, price, take payment, confirm.
Every final state is one HTTP response.
```

```mermaid
stateDiagram-v2
  Authenticating: invoke authenticate(token)
  state Processing {
    state "Validating" as Processing_Validating
    Processing_Validating: invoke validateOrder(draft)
    state "Pricing" as Processing_Pricing
    Processing_Pricing: invoke priceOrder(order, user)
    state "Paying" as Processing_Paying
    Processing_Paying: invoke machine Checkout(order, user)
    state "Confirming" as Processing_Confirming
    Processing_Confirming: entry do sendConfirmationEmail(user, order, receipt)
    [*] --> Processing_Validating
    Processing_Validating --> Processing_Pricing: done / set order = result
    Processing_Pricing --> Processing_Paying: done / set order = result
    Processing_Paying --> Processing_Confirming: done / set receipt = result
  }
  Confirmed: output confirmation(order, receipt)
  Unauthorized: output problem(401, "unauthenticated")
  Invalid: output invalid(issues)
  PaymentFailed: output problem(402, "payment_failed")
  Cancelled: output problem(499, "cancelled")
  TimedOut: output problem(504, "timeout")
  [*] --> Authenticating
  Authenticating --> Processing: done / set user = result
  Authenticating --> Unauthorized: fail Unauthenticated
  Processing --> Cancelled: Cancel
  Processing --> TimedOut: after 30s
  Processing_Validating --> Invalid: fail ValidationFailed / set issues = error.issues
  Processing_Paying --> PaymentFailed: fail CardDeclined / do audit("card_declined", order)
  Processing_Paying --> PaymentFailed: fail GatewayUnavailable / do audit("gateway_down", order)
  Processing_Confirming --> Confirmed: always
  Confirmed --> [*]
  Unauthorized --> [*]
  Invalid --> [*]
  PaymentFailed --> [*]
  Cancelled --> [*]
  TimedOut --> [*]
```

## Checkout

`examples/shop/checkout.road:4`

```text
Charge the customer's card, retrying while the payment gateway is down.
```

```mermaid
stateDiagram-v2
  Charging: invoke chargeCard(order, user)
  Backoff: entry set attempts = attempts + 1
  Charged: output receipt
  Declined: fail declined
  GaveUp: fail GatewayUnavailable
  [*] --> Charging
  Charging --> Charged: done / set receipt = result
  Charging --> Declined: fail CardDeclined / set declined = error
  Charging --> Backoff: fail GatewayUnavailable [attempts < 3]
  Charging --> GaveUp: fail GatewayUnavailable [else]
  Backoff --> Charging: after retryDelay(attempts)
  Charged --> [*]
  Declined --> [*]
  GaveUp --> [*]
```
