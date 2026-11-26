//! Type-level sketch of the proposed Rust API. Nothing is implemented: the
//! runtime entry points are `todo!()`. The point is that the shapes compile.
//!
//! - `roadbook`  : what the runtime crate would export.
//! - `shop`     : what `roadbook::include!("examples/shop/place_order.road")`
//!                would expand to. Hand-written here.
//! - `app`      : what a developer (or an agent) writes.

#![allow(dead_code, unused_variables)]

pub mod roadbook {
    use std::marker::PhantomData;

    /// A running machine instance that accepts events.
    pub struct Actor<Event, State, Output, Error> {
        _marker: PhantomData<(Event, State, Output, Error)>,
    }

    impl<Event, State: Clone, Output, Error> Actor<Event, State, Output, Error> {
        /// Queue an event. Events are processed one at a time, in order.
        pub async fn send(&self, event: Event) {
            todo!()
        }
        /// The current state.
        pub fn state(&self) -> State {
            todo!()
        }
        /// Wait for the next state change.
        pub async fn changed(&mut self) -> State {
            todo!()
        }
        /// Wait for a top-level final state. Dropping the actor stops the machine.
        pub async fn result(self) -> Result<Output, Error> {
            todo!()
        }
    }
}

pub mod shop {
    use std::future::Future;
    use std::time::Duration;

    // ---- types -------------------------------------------------------------

    #[derive(Debug, Clone, PartialEq)]
    pub struct LineItem {
        pub sku: String,
        pub qty: i64,
    }
    #[derive(Debug, Clone, PartialEq)]
    pub struct OrderDraft {
        pub items: Vec<LineItem>,
        pub coupon: Option<String>,
    }
    #[derive(Debug, Clone, PartialEq)]
    pub struct Order {
        pub id: String,
        pub items: Vec<LineItem>,
        pub total_cents: i64,
    }
    #[derive(Debug, Clone, PartialEq)]
    pub struct Receipt {
        pub charge_id: String,
        pub amount_cents: i64,
    }
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Role {
        Customer,
        Admin,
    }
    #[derive(Debug, Clone, PartialEq)]
    pub struct User {
        pub id: String,
        pub role: Role,
    }
    #[derive(Debug, Clone, PartialEq)]
    pub struct ApiResponse {
        pub status: i64,
        pub body: String, // `json` maps to serde_json::Value in the real thing
    }

    // ---- errors ------------------------------------------------------------

    #[derive(Debug, Clone, PartialEq)]
    pub struct Unauthenticated;
    #[derive(Debug, Clone, PartialEq)]
    pub struct ValidationFailed {
        pub issues: Vec<String>,
    }
    #[derive(Debug, Clone, PartialEq)]
    pub struct CardDeclined {
        pub reason: String,
    }
    #[derive(Debug, Clone, PartialEq)]
    pub struct GatewayUnavailable;

    /// Rust has no anonymous unions, so each fallible task and machine gets an
    /// enum of exactly the errors it declared. `From` impls make `?` work.
    #[derive(Debug, Clone, PartialEq)]
    pub enum AuthenticateError {
        Unauthenticated(Unauthenticated),
    }
    #[derive(Debug, Clone, PartialEq)]
    pub enum ValidateOrderError {
        ValidationFailed(ValidationFailed),
    }
    #[derive(Debug, Clone, PartialEq)]
    pub enum ChargeCardError {
        CardDeclined(CardDeclined),
        GatewayUnavailable(GatewayUnavailable),
    }
    #[derive(Debug, Clone, PartialEq)]
    pub enum CheckoutError {
        CardDeclined(CardDeclined),
        GatewayUnavailable(GatewayUnavailable),
    }
    impl From<Unauthenticated> for AuthenticateError {
        fn from(e: Unauthenticated) -> Self {
            Self::Unauthenticated(e)
        }
    }
    impl From<ValidationFailed> for ValidateOrderError {
        fn from(e: ValidationFailed) -> Self {
            Self::ValidationFailed(e)
        }
    }
    impl From<CardDeclined> for ChargeCardError {
        fn from(e: CardDeclined) -> Self {
            Self::CardDeclined(e)
        }
    }
    impl From<GatewayUnavailable> for ChargeCardError {
        fn from(e: GatewayUnavailable) -> Self {
            Self::GatewayUnavailable(e)
        }
    }
    impl From<CardDeclined> for CheckoutError {
        fn from(e: CardDeclined) -> Self {
            Self::CardDeclined(e)
        }
    }
    impl From<GatewayUnavailable> for CheckoutError {
        fn from(e: GatewayUnavailable) -> Self {
            Self::GatewayUnavailable(e)
        }
    }

    // ---- machine: Checkout ---------------------------------------------------

    pub mod checkout {
        use super::*;
        use crate::roadbook::Actor;

        /// What the chart needs from host code. One method per declared
        /// function, action, task and child machine that the chart uses.
        pub trait Handlers: Send + Sync {
            fn retry_delay(&self, attempt: i64) -> Duration;
            fn charge_card(
                &self,
                order: &Order,
                user: &User,
            ) -> impl Future<Output = Result<Receipt, ChargeCardError>> + Send;
        }

        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum State {
            Charging,
            Backoff,
            Charged,
            Declined,
            GaveUp,
        }

        /// This machine accepts no events.
        #[derive(Debug, Clone, PartialEq)]
        pub enum Event {}

        /// Run one instance to its final state.
        pub async fn run<H: Handlers>(handlers: &H, order: Order, user: User) -> Result<Receipt, CheckoutError> {
            todo!()
        }

        /// Start a long-lived instance.
        pub fn spawn<H: Handlers + 'static>(
            handlers: std::sync::Arc<H>,
            order: Order,
            user: User,
        ) -> Actor<Event, State, Receipt, CheckoutError> {
            todo!()
        }
    }

    // ---- machine: PlaceOrder -------------------------------------------------

    pub mod place_order {
        use super::*;
        use crate::roadbook::Actor;
        use std::convert::Infallible;

        pub trait Handlers: Send + Sync {
            // fn
            fn confirmation(&self, order: &Order, receipt: &Receipt) -> ApiResponse;
            fn invalid(&self, issues: &[String]) -> ApiResponse;
            fn problem(&self, status: i64, code: &str) -> ApiResponse;
            // action
            fn send_confirmation_email(
                &self,
                user: &User,
                order: &Order,
                receipt: &Receipt,
            ) -> impl Future<Output = ()> + Send;
            fn audit(&self, what: &str, order: &Order) -> impl Future<Output = ()> + Send;
            // task
            fn authenticate(&self, token: &str) -> impl Future<Output = Result<User, AuthenticateError>> + Send;
            fn validate_order(
                &self,
                draft: &OrderDraft,
            ) -> impl Future<Output = Result<Order, ValidateOrderError>> + Send;
            fn price_order(&self, order: &Order, user: &User) -> impl Future<Output = Order> + Send;
            // machine: usually one line delegating to `checkout::run`
            fn checkout(
                &self,
                order: &Order,
                user: &User,
            ) -> impl Future<Output = Result<Receipt, CheckoutError>> + Send;
        }

        /// The active state. Compound states carry their active child.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum State {
            Authenticating,
            Processing(Processing),
            Confirmed,
            Unauthorized,
            Invalid,
            PaymentFailed,
            Cancelled,
            TimedOut,
        }
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Processing {
            Validating,
            Pricing,
            Paying,
            Confirming,
        }

        #[derive(Debug, Clone, PartialEq)]
        pub enum Event {
            Cancel,
        }

        /// This machine declares no errors, so `run` returns the output directly.
        pub async fn run<H: Handlers>(handlers: &H, token: String, draft: OrderDraft) -> ApiResponse {
            todo!()
        }

        pub fn spawn<H: Handlers + 'static>(
            handlers: std::sync::Arc<H>,
            token: String,
            draft: OrderDraft,
        ) -> Actor<Event, State, ApiResponse, Infallible> {
            todo!()
        }
    }
}

pub mod app {
    use crate::shop::*;
    use std::sync::Arc;
    use std::time::Duration;

    pub struct Users;
    impl Users {
        async fn by_token(&self, token: &str) -> Option<User> {
            None
        }
    }
    pub struct Payments;
    impl Payments {
        async fn charge(&self, order: &Order, user: &User) -> Result<Receipt, ChargeCardError> {
            Err(GatewayUnavailable)?
        }
    }

    /// The application's dependencies. One struct implements every chart's handlers.
    pub struct App {
        users: Users,
        payments: Payments,
    }

    impl checkout::Handlers for App {
        fn retry_delay(&self, attempt: i64) -> Duration {
            Duration::from_millis(200 * 2u64.pow(attempt as u32))
        }
        async fn charge_card(&self, order: &Order, user: &User) -> Result<Receipt, ChargeCardError> {
            self.payments.charge(order, user).await
        }
    }

    impl place_order::Handlers for App {
        fn confirmation(&self, order: &Order, receipt: &Receipt) -> ApiResponse {
            ApiResponse { status: 201, body: format!(r#"{{"orderId":"{}"}}"#, order.id) }
        }
        fn invalid(&self, issues: &[String]) -> ApiResponse {
            ApiResponse { status: 422, body: issues.join(",") }
        }
        fn problem(&self, status: i64, code: &str) -> ApiResponse {
            ApiResponse { status, body: code.to_owned() }
        }
        async fn send_confirmation_email(&self, user: &User, order: &Order, receipt: &Receipt) {}
        async fn audit(&self, what: &str, order: &Order) {}
        async fn authenticate(&self, token: &str) -> Result<User, AuthenticateError> {
            // `?` converts the declared error into the task's error enum.
            Ok(self.users.by_token(token).await.ok_or(Unauthenticated)?)
        }
        async fn validate_order(&self, draft: &OrderDraft) -> Result<Order, ValidateOrderError> {
            if draft.items.is_empty() {
                return Err(ValidationFailed { issues: vec!["order has no items".into()] })?;
            }
            Ok(Order { id: "ord_1".into(), items: draft.items.clone(), total_cents: 0 })
        }
        async fn price_order(&self, order: &Order, user: &User) -> Order {
            Order { total_cents: 4200, ..order.clone() }
        }
        async fn checkout(&self, order: &Order, user: &User) -> Result<Receipt, CheckoutError> {
            checkout::run(self, order.clone(), user.clone()).await
        }
    }

    /// One machine run per request. With axum this is the body of a handler.
    pub async fn post_orders(app: Arc<App>, token: String, draft: OrderDraft) -> ApiResponse {
        place_order::run(&*app, token, draft).await
    }

    /// Long-lived use: send events, watch state.
    pub async fn watched(app: Arc<App>, token: String, draft: OrderDraft) -> ApiResponse {
        let mut actor = place_order::spawn(app, token, draft);
        actor.send(place_order::Event::Cancel).await;
        if actor.changed().await == place_order::State::Cancelled {
            println!("cancelled");
        }
        match actor.result().await {
            Ok(response) => response,
            Err(never) => match never {},
        }
    }

    /// The futures are `Send`, so handlers can run on a multi-threaded runtime.
    fn assert_send<T: Send>(_: T) {}
    fn check(app: Arc<App>) {
        assert_send(post_orders(app, String::new(), OrderDraft { items: vec![], coupon: None }));
    }
}
