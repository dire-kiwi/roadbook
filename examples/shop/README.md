# The shop example

`POST /orders` as a chart: authenticate, validate, price, take payment, confirm. `place_order.road` is the request; `checkout.road` is the payment, invoked as a child machine; `types.road` holds the shared data shapes, errors and host functions.

The same charts are served by two applications, which must answer every request identically:

- TypeScript with Effect v4 and Express, in this directory's pnpm package.
- Rust with axum, in `axum/`.

## The HTTP interface

`POST /orders` with an `Authorization` header and a JSON body that is an `OrderDraft`: `{"items": [{"sku": "apple", "qty": 2}], "coupon": null}` (`coupon` may be omitted, meaning `null`). The response's status is the chart's `ApiResponse.status` and its JSON body is `ApiResponse.body`. A body that does not decode as an `OrderDraft` is answered `400 {"code": "bad_request"}` without running the machine. A run that ends with a defect is answered `500 {"code": "internal"}`.

## Behaviour of the example services

Both applications implement the chart's host functions with these fake services, so their answers can be compared.

| Host function | Behaviour |
| --- | --- |
| `authenticate(token)` | `"Bearer good"` gives `{"id": "u1", "role": "Customer"}`; `"Bearer admin"` gives `{"id": "u2", "role": "Admin"}`; anything else fails with `Unauthenticated`. |
| `validateOrder(draft)` | No items fails with `ValidationFailed { issues: ["order has no items"] }`. Otherwise every item with `qty < 1` adds the issue `"<sku>: quantity must be at least 1"`, in item order, and any issue fails with `ValidationFailed`. Otherwise succeeds with `{"id": "ord_1", "items": <the items>, "totalCents": 0}`. |
| `priceOrder(order, user)` | `totalCents` = the sum of `qty * 1250` over the items, less 10% (rounded down) for an `Admin`. |
| `chargeCard(order, user)` | An item with sku `"declined"` fails with `CardDeclined { reason: "insufficient_funds" }`. An item with sku `"flaky"` fails with `GatewayUnavailable` every time. Otherwise succeeds with `{"chargeId": "ch_" + order.id, "amountCents": order.totalCents}`. |
| `retryDelay(attempt)` | `10ms * 2^attempt`. |
| `confirmation(order, receipt)` | `{"status": 201, "body": {"orderId": order.id, "chargeId": receipt.chargeId, "totalCents": order.totalCents}}` |
| `invalid(issues)` | `{"status": 422, "body": {"code": "invalid", "issues": issues}}` |
| `problem(status, code)` | `{"status": status, "body": {"code": code}}` |
| `sendConfirmationEmail`, `audit` | Log a line. |

## Requests every application is tested with

| Request | Response |
| --- | --- |
| `Bearer good`, `{"items": [{"sku": "apple", "qty": 2}]}` | `201 {"orderId": "ord_1", "chargeId": "ch_ord_1", "totalCents": 2500}` |
| `Bearer admin`, `{"items": [{"sku": "apple", "qty": 2}]}` | `201 {"orderId": "ord_1", "chargeId": "ch_ord_1", "totalCents": 2250}` |
| `Bearer nope`, `{"items": [{"sku": "apple", "qty": 2}]}` | `401 {"code": "unauthenticated"}` |
| no `Authorization` header, same body | `401 {"code": "unauthenticated"}` |
| `Bearer good`, `{"items": []}` | `422 {"code": "invalid", "issues": ["order has no items"]}` |
| `Bearer good`, `{"items": [{"sku": "a", "qty": 0}, {"sku": "b", "qty": -1}]}` | `422 {"code": "invalid", "issues": ["a: quantity must be at least 1", "b: quantity must be at least 1"]}` |
| `Bearer good`, `{"items": [{"sku": "declined", "qty": 1}]}` | `402 {"code": "payment_failed"}` |
| `Bearer good`, `{"items": [{"sku": "flaky", "qty": 1}]}` | `402 {"code": "payment_failed"}`, after three retries |
| `Bearer good`, `{"items": "nope"}` | `400 {"code": "bad_request"}` |
| `Bearer good`, `{"items": [{"sku": "apple", "qty": 9007199254740991}]}` | `500 {"code": "internal"}`: pricing overflows `int` in both runtimes |
