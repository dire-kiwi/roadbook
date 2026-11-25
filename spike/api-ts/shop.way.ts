/**
 * What `waymark gen` would write for examples/shop. Hand-written for the sketch.
 * GENERATED FILE: do not edit.
 */
import { Schema } from "effect"
import type { Duration } from "effect"
import * as Waymark from "./waymark.js"

// ---- types -----------------------------------------------------------------

export const Role = Schema.Literals(["Customer", "Admin"])
export type Role = typeof Role.Type

export const LineItem = Schema.Struct({ sku: Schema.String, qty: Schema.Int })
export interface LineItem extends Schema.Schema.Type<typeof LineItem> {}

export const OrderDraft = Schema.Struct({
  items: Schema.Array(LineItem),
  coupon: Schema.NullOr(Schema.String)
})
export interface OrderDraft extends Schema.Schema.Type<typeof OrderDraft> {}

export const Order = Schema.Struct({ id: Schema.String, items: Schema.Array(LineItem), totalCents: Schema.Int })
export interface Order extends Schema.Schema.Type<typeof Order> {}

export const Receipt = Schema.Struct({ chargeId: Schema.String, amountCents: Schema.Int })
export interface Receipt extends Schema.Schema.Type<typeof Receipt> {}

export const User = Schema.Struct({ id: Schema.String, role: Role })
export interface User extends Schema.Schema.Type<typeof User> {}

export const ApiResponse = Schema.Struct({ status: Schema.Int, body: Schema.Json })
export interface ApiResponse extends Schema.Schema.Type<typeof ApiResponse> {}

// ---- errors ----------------------------------------------------------------

export class Unauthenticated extends Schema.TaggedError<Unauthenticated>()("Unauthenticated", {}) {}
export class ValidationFailed extends Schema.TaggedError<ValidationFailed>()("ValidationFailed", {
  issues: Schema.Array(Schema.String)
}) {}
export class CardDeclined extends Schema.TaggedError<CardDeclined>()("CardDeclined", { reason: Schema.String }) {}
export class GatewayUnavailable extends Schema.TaggedError<GatewayUnavailable>()("GatewayUnavailable", {}) {}

// ---- machine: Checkout -----------------------------------------------------

export interface CheckoutDef extends Waymark.MachineDef {
  readonly name: "Checkout"
  readonly input: { readonly order: Order; readonly user: User }
  readonly output: Receipt
  readonly error: CardDeclined | GatewayUnavailable
  readonly event: never
  readonly state: "Checkout.Charging" | "Checkout.Backoff" | "Checkout.Charged" | "Checkout.Declined" | "Checkout.GaveUp"
  readonly fns: {
    readonly retryDelay: { readonly args: { readonly attempt: number }; readonly returns: Duration.Duration }
  }
  readonly actions: {}
  readonly tasks: {
    readonly chargeCard: {
      readonly args: { readonly order: Order; readonly user: User }
      readonly success: Receipt
      readonly error: CardDeclined | GatewayUnavailable
    }
  }
  readonly machines: {}
}

declare const ir: unknown // the compiled chart, inlined by the generator

export const Checkout = Waymark.make<CheckoutDef>(ir, "Checkout")

// ---- machine: PlaceOrder ---------------------------------------------------

export const Cancel = Schema.TaggedStruct("Cancel", {})
export type Cancel = typeof Cancel.Type

export interface PlaceOrderDef extends Waymark.MachineDef {
  readonly name: "PlaceOrder"
  readonly input: { readonly token: string; readonly draft: OrderDraft }
  readonly output: ApiResponse
  readonly error: never
  readonly event: Cancel
  readonly state:
    | "PlaceOrder.Authenticating"
    | "PlaceOrder.Processing"
    | "PlaceOrder.Processing.Validating"
    | "PlaceOrder.Processing.Pricing"
    | "PlaceOrder.Processing.Paying"
    | "PlaceOrder.Processing.Confirming"
    | "PlaceOrder.Confirmed"
    | "PlaceOrder.Unauthorized"
    | "PlaceOrder.Invalid"
    | "PlaceOrder.PaymentFailed"
    | "PlaceOrder.Cancelled"
    | "PlaceOrder.TimedOut"
  readonly fns: {
    readonly confirmation: { readonly args: { readonly order: Order; readonly receipt: Receipt }; readonly returns: ApiResponse }
    readonly invalid: { readonly args: { readonly issues: ReadonlyArray<string> }; readonly returns: ApiResponse }
    readonly problem: { readonly args: { readonly status: number; readonly code: string }; readonly returns: ApiResponse }
  }
  readonly actions: {
    readonly sendConfirmationEmail: { readonly user: User; readonly order: Order; readonly receipt: Receipt }
    readonly audit: { readonly what: string; readonly order: Order }
  }
  readonly tasks: {
    readonly authenticate: { readonly args: { readonly token: string }; readonly success: User; readonly error: Unauthenticated }
    readonly validateOrder: { readonly args: { readonly draft: OrderDraft }; readonly success: Order; readonly error: ValidationFailed }
    readonly priceOrder: { readonly args: { readonly order: Order; readonly user: User }; readonly success: Order; readonly error: never }
  }
  readonly machines: {
    readonly Checkout: {
      readonly args: { readonly order: Order; readonly user: User }
      readonly success: Receipt
      readonly error: CardDeclined | GatewayUnavailable
    }
  }
}

export const PlaceOrder = Waymark.make<PlaceOrderDef>(ir, "PlaceOrder")
