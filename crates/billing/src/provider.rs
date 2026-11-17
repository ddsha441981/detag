//! detag-billing::provider — the `BillingProvider` trait (P1-059, FR-P-05).
//!
//! # Why a trait, and why this shape
//!
//! The kit fixes the provider in two places that disagree if read
//! naively: the tech stack (`docs/03-ARCHITECTURE/02-tech-stack.md`)
//! says **async-stripe behind a `BillingProvider` trait**, and FR-P-05
//! says the trait **SHALL allow a Razorpay adapter behind a feature
//! flag without touching core code**. The architecture ADR (in
//! `docs/03-ARCHITECTURE/05-adrs.md`) adds the third leg: **entitlements
//! live in our DB, not the provider** — the provider is a payment
//! instrument, never the source of what a user may do.
//!
//! The trait is therefore a narrow, provider-agnostic contract over
//! domain types only — no Stripe or Razorpay type ever crosses it:
//!
//! * **Checkout / portal / subscription / invoices / customers** are
//!   the five verbs the api needs (FR-P-03 + the api-spec billing
//!   routes `POST /v1/billing/checkout`, `POST /v1/billing/portal`,
//!   `GET /v1/billing/subscription`, `GET /v1/billing/invoices`).
//! * **`map_webhook` is sync and pure** — it parses an already
//!   signature-verified envelope into a [`DomainEvent`]. No I/O, so no
//!   async, so a wrong mapping is a unit test, not a live call.
//!   It returns `Option`: `None` means "a well-formed event this app
//!   ignores" (invoice events until the sync job lands, test pings) —
//!   ignored events are never errors, so the webhook answers 200 and
//!   the provider stops retrying.
//! * **`verify_webhook_signature` is on the trait** so the api webhook
//!   handler stays provider-agnostic (FR-P-04): the handler takes the
//!   raw body + signature header, asks the configured provider to
//!   verify, and only then calls `map_webhook`. Signature failure is
//!   [`BillingError::SignatureInvalid`] — reject outright, never map.
//! * **Statuses mirror the DB exactly** (`trialing` / `active` /
//!   `past_due` / `canceled` — the `subscriptions.status` CHECK).
//!   [`SubscriptionStatus::from_db_str`] fails closed on anything
//!   else; an unknown status is never read as `active`.
//! * **The trait is `dyn`-compatible** (`#[async_trait]`, `Send +
//!   Sync`) so `AppState` can hold `Arc<dyn BillingProvider>` — the
//!   same shape it already uses for `Arc<dyn Clock>`. The `PlanStore`
//!   pattern (native `async fn` + generics) was NOT reused here on
//!   purpose: generics would infect `AppState` and every handler with
//!   a provider type parameter, and the provider is chosen at runtime
//!   from config (Stripe now, Razorpay behind a flag), not at compile
//!   time. `async-trait` 0.1.92 was already in the lockfile as a
//!   transitive dependency; this ticket promotes it to a direct one.
//! * **No `async-stripe` / Razorpay SDK in this crate yet.** The trait
//!   is the contract; the Stripe adapter lands in P1-060, the Razorpay
//!   adapter behind its flag after it. Neither touches core code — the
//!   api depends on this trait, core does not depend on billing at all.
//!
//! A `DomainEvent` carries ids + status only. The handler resolves the
//! plan from our own catalog (the `plans` table, P1-049) and writes the
//! `subscriptions` row — the provider never tells us what a user is
//! entitled to.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::error::BillingError;

/// The provider behind a [`BillingProvider`], as our own DB records it.
///
/// Must match the `subscriptions.provider` CHECK (`stripe` |
/// `razorpay`). Adapters report it via
/// [`BillingProvider::provider_name`]; webhook routes key off it.
pub type ProviderName = &'static str;

/// `stripe`.
pub const PROVIDER_STRIPE: ProviderName = "stripe";
/// `razorpay`.
pub const PROVIDER_RAZORPAY: ProviderName = "razorpay";

/// A subscription's lifecycle state — exactly the four values the
/// `subscriptions.status` CHECK admits. No other value exists in our
/// system; [`SubscriptionStatus::from_db_str`] is the only way in from
/// a string, and it fails closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionStatus {
    /// Trial period — grants the plan (newest live wins, P1-049).
    Trialing,
    /// Paying and current — grants the plan.
    Active,
    /// Payment failed — grants nothing (P1-049, ADR-0025).
    PastDue,
    /// Ended — grants nothing.
    Canceled,
}

impl SubscriptionStatus {
    /// The DB string for this status (`trialing` / `active` /
    /// `past_due` / `canceled`).
    #[must_use]
    pub fn as_db_str(self) -> &'static str {
        match self {
            Self::Trialing => "trialing",
            Self::Active => "active",
            Self::PastDue => "past_due",
            Self::Canceled => "canceled",
        }
    }

    /// Parse a DB (or provider) status string. Anything else is
    /// [`BillingError::UnknownStatus`] — never coerced, never defaulted.
    pub fn from_db_str(s: &str) -> Result<Self, BillingError> {
        match s {
            "trialing" => Ok(Self::Trialing),
            "active" => Ok(Self::Active),
            "past_due" => Ok(Self::PastDue),
            "canceled" => Ok(Self::Canceled),
            other => Err(BillingError::UnknownStatus(other.to_owned())),
        }
    }

    /// Whether this status grants the plan (the P1-049 resolution rule:
    /// only the newest live `trialing` / `active` subscription grants).
    #[must_use]
    pub fn grants_plan(self) -> bool {
        matches!(self, Self::Trialing | Self::Active)
    }
}

/// What the provider knows about one subscription — the shape
/// `GET /v1/billing/subscription` returns, and the shape a webhook
/// [`DomainEvent`] updates. Amounts and plan codes are NOT here: the
/// plan comes from our own catalog, the amount from the `plans` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscriptionState {
    /// The provider's subscription id (`sub_...` / Razorpay equivalent).
    pub provider_subscription_id: String,
    /// The provider's customer id, when the subscription has one.
    pub provider_customer_id: Option<String>,
    /// The lifecycle state.
    pub status: SubscriptionStatus,
    /// End of the current paid period, when the provider reports one.
    pub current_period_end: Option<DateTime<Utc>>,
    /// True when the subscriber asked to cancel at period end.
    pub cancel_at_period_end: bool,
}

/// The input to [`BillingProvider::create_checkout`]. The `price_id`
/// comes from the plan config (`plans.stripe_price_id`, P1-058) — the
/// trait never picks a price itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckoutRequest {
    /// Our user, for the subscription row the checkout will create.
    pub user_id: Uuid,
    /// Existing provider customer, if the user already has one.
    pub customer_id: Option<String>,
    /// The provider price to buy (`price_...`).
    pub price_id: String,
    /// Where the provider sends the buyer on success.
    pub success_url: String,
    /// Where the provider sends the buyer on cancel.
    pub cancel_url: String,
}

/// What `create_checkout` returns: the hosted session to redirect the
/// buyer to. The `url` is the provider's hosted page, not ours.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CheckoutSession {
    /// The provider's checkout session id.
    pub id: String,
    /// The hosted checkout URL.
    pub url: String,
}

/// The input to [`BillingProvider::create_portal`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortalRequest {
    /// The provider customer whose billing page this is.
    pub customer_id: String,
    /// Where the provider sends the user when they leave the portal.
    pub return_url: String,
}

/// What `create_portal` returns: the hosted billing-portal URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortalSession {
    /// The hosted billing-portal URL.
    pub url: String,
}

/// A provider customer — one row per user per provider, created once
/// at first checkout and reused after. The `subscriptions` table
/// carries the same id as `provider_customer_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Customer {
    /// The provider's customer id (`cus_...` / Razorpay equivalent).
    pub provider_customer_id: String,
    /// The email the customer was created with.
    pub email: String,
}

/// One invoice, as `GET /v1/billing/invoices` lists it. Amounts are
/// the provider's smallest unit (paise for INR); `status` is the
/// provider's own string (`paid` / `open` / `void` / ...), kept opaque
/// on purpose — each provider spells these differently.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invoice {
    /// The provider's invoice id.
    pub id: String,
    /// The human invoice number, when the provider assigns one.
    pub number: Option<String>,
    /// Total in the smallest currency unit.
    pub amount: i64,
    /// Three uppercase letters (`INR`).
    pub currency: String,
    /// The provider's status string.
    pub status: String,
    /// When the provider created the invoice.
    pub created_at: DateTime<Utc>,
    /// The hosted invoice page, when the provider offers one.
    pub hosted_url: Option<String>,
}

/// A signature-verified webhook envelope, ready for
/// [`BillingProvider::map_webhook`]. The handler builds this AFTER
/// [`BillingProvider::verify_webhook_signature`] passes — mapping
/// never sees an unverified event.
#[derive(Debug, Clone, PartialEq)]
pub struct BillingEvent {
    /// Which provider sent it (`stripe` / `razorpay`).
    pub provider: String,
    /// The provider's event id — the idempotency key (FR-P-04).
    pub event_id: String,
    /// The provider's event type (`checkout.session.completed`, ...).
    pub event_type: String,
    /// The decoded JSON payload.
    pub payload: serde_json::Value,
}

/// What our app does with a webhook event, in our own words. The
/// handler applies this to the `subscriptions` table (via the plan
/// catalog — never trusting a plan code from the payload).
#[derive(Debug, Clone, PartialEq)]
pub enum DomainEvent {
    /// A checkout completed: link this customer to this subscription.
    CheckoutCompleted {
        /// The provider's customer id.
        customer_id: String,
        /// The provider's subscription id.
        subscription_id: String,
    },
    /// A subscription changed state (created / updated / renewed /
    /// past-due / resumed): upsert the row with this state.
    SubscriptionStatusChanged {
        /// The provider's subscription id.
        subscription_id: String,
        /// The new lifecycle state.
        status: SubscriptionStatus,
        /// End of the current paid period, when reported.
        current_period_end: Option<DateTime<Utc>>,
        /// True when the subscriber asked to cancel at period end.
        cancel_at_period_end: bool,
    },
    /// A subscription was deleted at the provider: the row goes
    /// `canceled` (rows are never deleted — the audit trail needs them).
    SubscriptionDeleted {
        /// The provider's subscription id.
        subscription_id: String,
    },
    /// An invoice was paid. The subscription link is `None` for
    /// one-off invoices; the handler ignores those for lifecycle.
    InvoicePaid {
        /// The provider's invoice id.
        invoice_id: String,
        /// The subscription it belongs to, when it has one.
        subscription_id: Option<String>,
    },
}

/// The billing contract (FR-P-05). One implementation per provider
/// (Stripe in P1-060, Razorpay behind its flag after it); the api
/// holds `Arc<dyn BillingProvider>` and never names an implementation.
///
/// Object-safe (`Arc<dyn BillingProvider>` is the state field)
/// and `Debug` (so `AppState` can derive `Debug`/`Clone` —
/// the same shape as [`Clock`](detag_core::clock::Clock)).
/// Implementors that hold a secret (the Stripe adapter's
/// client) must hand-write `Debug` and print nothing.
///
/// All methods are provider calls except [`BillingProvider::map_webhook`]
/// and [`BillingProvider::verify_webhook_signature`], which are pure.
#[async_trait]
pub trait BillingProvider: Send + Sync + std::fmt::Debug {
    /// `stripe` or `razorpay` — the `subscriptions.provider` value.
    fn provider_name(&self) -> ProviderName;

    /// Start a hosted checkout for `req.price_id`; returns the session
    /// to redirect the buyer to. Fails
    /// [`BillingError::InvalidPrice`] when the price is not configured.
    async fn create_checkout(&self, req: CheckoutRequest) -> Result<CheckoutSession, BillingError>;

    /// Open the hosted billing portal for this customer; returns the
    /// URL to redirect to.
    async fn create_portal(&self, req: PortalRequest) -> Result<PortalSession, BillingError>;

    /// Fetch one subscription's current state from the provider.
    /// Fails [`BillingError::UnknownSubscription`] when the provider
    /// has no such subscription.
    async fn fetch_subscription(
        &self,
        provider_subscription_id: &str,
    ) -> Result<SubscriptionState, BillingError>;

    /// List this customer's invoices, newest first, at most `limit`.
    async fn list_invoices(
        &self,
        customer_id: &str,
        limit: u32,
    ) -> Result<Vec<Invoice>, BillingError>;

    /// Mint a provider customer for this user (first checkout). The
    /// caller stores the returned id on the subscription row.
    async fn create_customer(&self, user_id: Uuid, email: &str) -> Result<Customer, BillingError>;

    /// Fetch a customer by provider id.
    async fn fetch_customer(&self, provider_customer_id: &str) -> Result<Customer, BillingError>;

    /// Verify a webhook's signature over the raw body (FR-P-04).
    /// `signature` is the provider's signature header as received.
    /// Pure: no I/O. Fails [`BillingError::SignatureInvalid`] — the
    /// caller must reject the event outright on this error.
    fn verify_webhook_signature(&self, payload: &[u8], signature: &str)
        -> Result<(), BillingError>;

    /// Map a verified [`BillingEvent`] to what our app does with it.
    /// Pure: no I/O. Returns `None` for a well-formed event type this
    /// app ignores (the handler answers 200 and moves on); fails
    /// [`BillingError::InvalidPayload`] when the payload is malformed
    /// for its type.
    fn map_webhook(&self, event: BillingEvent) -> Result<Option<DomainEvent>, BillingError>;
}

/// `Arc` forwards, so one shared provider serves every handler.
#[async_trait]
impl<T: BillingProvider + ?Sized> BillingProvider for Arc<T> {
    fn provider_name(&self) -> ProviderName {
        self.as_ref().provider_name()
    }

    async fn create_checkout(&self, req: CheckoutRequest) -> Result<CheckoutSession, BillingError> {
        self.as_ref().create_checkout(req).await
    }

    async fn create_portal(&self, req: PortalRequest) -> Result<PortalSession, BillingError> {
        self.as_ref().create_portal(req).await
    }

    async fn fetch_subscription(
        &self,
        provider_subscription_id: &str,
    ) -> Result<SubscriptionState, BillingError> {
        self.as_ref()
            .fetch_subscription(provider_subscription_id)
            .await
    }

    async fn list_invoices(
        &self,
        customer_id: &str,
        limit: u32,
    ) -> Result<Vec<Invoice>, BillingError> {
        self.as_ref().list_invoices(customer_id, limit).await
    }

    async fn create_customer(&self, user_id: Uuid, email: &str) -> Result<Customer, BillingError> {
        self.as_ref().create_customer(user_id, email).await
    }

    async fn fetch_customer(&self, provider_customer_id: &str) -> Result<Customer, BillingError> {
        self.as_ref().fetch_customer(provider_customer_id).await
    }

    fn verify_webhook_signature(
        &self,
        payload: &[u8],
        signature: &str,
    ) -> Result<(), BillingError> {
        self.as_ref().verify_webhook_signature(payload, signature)
    }

    fn map_webhook(&self, event: BillingEvent) -> Result<Option<DomainEvent>, BillingError> {
        self.as_ref().map_webhook(event)
    }
}

/// A shared provider — the type `AppState` will hold (P1-060 wires it).
pub type DynBillingProvider = Arc<dyn BillingProvider>;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A test double: canned answers, no I/O. Its `map_webhook`
    /// understands exactly one event shape — the point here is the
    /// contract (what the trait promises), not any adapter's mapping;
    /// the Stripe adapter's mapping is proven in P1-060.
    #[derive(Debug)]
    struct FakeProvider {
        name: ProviderName,
    }

    #[async_trait]
    impl BillingProvider for FakeProvider {
        fn provider_name(&self) -> ProviderName {
            self.name
        }

        async fn create_checkout(
            &self,
            req: CheckoutRequest,
        ) -> Result<CheckoutSession, BillingError> {
            if req.price_id.is_empty() {
                return Err(BillingError::InvalidPrice(req.price_id));
            }
            Ok(CheckoutSession {
                id: format!("cs_test_{}", self.name),
                url: format!("https://checkout.{}.example/sess", self.name),
            })
        }

        async fn create_portal(&self, req: PortalRequest) -> Result<PortalSession, BillingError> {
            Ok(PortalSession {
                url: format!("https://portal.{}.example/{}", self.name, req.customer_id),
            })
        }

        async fn fetch_subscription(
            &self,
            provider_subscription_id: &str,
        ) -> Result<SubscriptionState, BillingError> {
            if provider_subscription_id == "sub_missing" {
                return Err(BillingError::UnknownSubscription(
                    provider_subscription_id.to_owned(),
                ));
            }
            Ok(SubscriptionState {
                provider_subscription_id: provider_subscription_id.to_owned(),
                provider_customer_id: Some(format!("cus_test_{}", self.name)),
                status: SubscriptionStatus::Active,
                current_period_end: None,
                cancel_at_period_end: false,
            })
        }

        async fn list_invoices(
            &self,
            _customer_id: &str,
            _limit: u32,
        ) -> Result<Vec<Invoice>, BillingError> {
            Ok(vec![])
        }

        async fn create_customer(
            &self,
            _user_id: Uuid,
            email: &str,
        ) -> Result<Customer, BillingError> {
            Ok(Customer {
                provider_customer_id: format!("cus_test_{}", self.name),
                email: email.to_owned(),
            })
        }

        async fn fetch_customer(
            &self,
            provider_customer_id: &str,
        ) -> Result<Customer, BillingError> {
            if provider_customer_id == "cus_missing" {
                return Err(BillingError::UnknownCustomer(
                    provider_customer_id.to_owned(),
                ));
            }
            Ok(Customer {
                provider_customer_id: provider_customer_id.to_owned(),
                email: "buyer@example.com".to_owned(),
            })
        }

        fn verify_webhook_signature(
            &self,
            _payload: &[u8],
            signature: &str,
        ) -> Result<(), BillingError> {
            if signature == "valid" {
                Ok(())
            } else {
                Err(BillingError::SignatureInvalid)
            }
        }

        fn map_webhook(&self, event: BillingEvent) -> Result<Option<DomainEvent>, BillingError> {
            match event.event_type.as_str() {
                "subscription.status_changed" => {
                    let status = event
                        .payload
                        .get("status")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| BillingError::InvalidPayload("missing status".to_owned()))?;
                    Ok(Some(DomainEvent::SubscriptionStatusChanged {
                        subscription_id: event
                            .payload
                            .get("subscription_id")
                            .and_then(|v| v.as_str())
                            .ok_or_else(|| {
                                BillingError::InvalidPayload("missing subscription_id".to_owned())
                            })?
                            .to_owned(),
                        status: SubscriptionStatus::from_db_str(status)?,
                        current_period_end: None,
                        cancel_at_period_end: false,
                    }))
                }
                _ => Ok(None),
            }
        }
    }

    /// The FR-P-05 proof: the same caller code, written once against
    /// `&dyn BillingProvider`, works unchanged for either provider —
    /// swapping adapters touches no caller code.
    async fn checkout_url_for(provider: &dyn BillingProvider, price_id: &str) -> String {
        provider
            .create_checkout(CheckoutRequest {
                user_id: Uuid::now_v7(),
                customer_id: None,
                price_id: price_id.to_owned(),
                success_url: "https://app.example/success".to_owned(),
                cancel_url: "https://app.example/cancel".to_owned(),
            })
            .await
            .expect("fake checkout succeeds")
            .url
    }

    #[tokio::test]
    async fn same_caller_code_works_for_either_provider() {
        let stripe = FakeProvider {
            name: PROVIDER_STRIPE,
        };
        let razorpay = FakeProvider {
            name: PROVIDER_RAZORPAY,
        };
        assert_eq!(
            checkout_url_for(&stripe, "price_pro").await,
            "https://checkout.stripe.example/sess"
        );
        assert_eq!(
            checkout_url_for(&razorpay, "price_pro").await,
            "https://checkout.razorpay.example/sess"
        );
        // And through the shared handle AppState will hold.
        let shared: DynBillingProvider = Arc::new(FakeProvider {
            name: PROVIDER_STRIPE,
        });
        assert_eq!(shared.provider_name(), PROVIDER_STRIPE);
        assert_eq!(
            checkout_url_for(shared.as_ref(), "price_pro").await,
            "https://checkout.stripe.example/sess"
        );
    }

    #[tokio::test]
    async fn empty_price_is_invalid_price_not_a_provider_call() {
        let provider = FakeProvider {
            name: PROVIDER_STRIPE,
        };
        let err = provider
            .create_checkout(CheckoutRequest {
                user_id: Uuid::now_v7(),
                customer_id: None,
                price_id: String::new(),
                success_url: "https://app.example/success".to_owned(),
                cancel_url: "https://app.example/cancel".to_owned(),
            })
            .await
            .expect_err("empty price must fail");
        assert_eq!(err, BillingError::InvalidPrice(String::new()));
    }

    #[tokio::test]
    async fn unknown_subscription_and_customer_are_typed_errors() {
        let provider = FakeProvider {
            name: PROVIDER_STRIPE,
        };
        assert_eq!(
            provider
                .fetch_subscription("sub_missing")
                .await
                .expect_err("missing subscription must fail"),
            BillingError::UnknownSubscription("sub_missing".to_owned())
        );
        assert_eq!(
            provider
                .fetch_customer("cus_missing")
                .await
                .expect_err("missing customer must fail"),
            BillingError::UnknownCustomer("cus_missing".to_owned())
        );
        assert!(provider.fetch_subscription("sub_123").await.is_ok());
    }

    #[test]
    fn subscription_status_round_trips_the_db_check_values() {
        for (s, status) in [
            ("trialing", SubscriptionStatus::Trialing),
            ("active", SubscriptionStatus::Active),
            ("past_due", SubscriptionStatus::PastDue),
            ("canceled", SubscriptionStatus::Canceled),
        ] {
            assert_eq!(SubscriptionStatus::from_db_str(s).unwrap(), status);
            assert_eq!(status.as_db_str(), s);
        }
        assert!(SubscriptionStatus::from_db_str("expired").is_err());
        assert!(SubscriptionStatus::from_db_str("ACTIVE").is_err());
        // Only live statuses grant the plan (P1-049 resolution rule).
        assert!(SubscriptionStatus::Trialing.grants_plan());
        assert!(SubscriptionStatus::Active.grants_plan());
        assert!(!SubscriptionStatus::PastDue.grants_plan());
        assert!(!SubscriptionStatus::Canceled.grants_plan());
    }

    #[test]
    fn error_messages_carry_no_payload() {
        // Messages are static templates + the offending id — never a
        // webhook body, never a secret.
        assert_eq!(
            BillingError::SignatureInvalid.to_string(),
            "webhook signature verification failed"
        );
        assert_eq!(
            BillingError::UnknownStatus("expired".to_owned()).to_string(),
            "unknown subscription status: expired"
        );
    }

    #[test]
    fn webhook_mapping_known_ignored_and_malformed() {
        let provider = FakeProvider {
            name: PROVIDER_STRIPE,
        };
        // A known type maps to a domain event.
        let mapped = provider
            .map_webhook(BillingEvent {
                provider: PROVIDER_STRIPE.to_owned(),
                event_id: "evt_1".to_owned(),
                event_type: "subscription.status_changed".to_owned(),
                payload: json!({"subscription_id": "sub_123", "status": "past_due"}),
            })
            .expect("mapping a known type succeeds");
        assert_eq!(
            mapped,
            Some(DomainEvent::SubscriptionStatusChanged {
                subscription_id: "sub_123".to_owned(),
                status: SubscriptionStatus::PastDue,
                current_period_end: None,
                cancel_at_period_end: false,
            })
        );
        // An unknown-but-well-formed type is ignored, not an error —
        // the handler answers 200 and the provider stops retrying.
        let ignored = provider
            .map_webhook(BillingEvent {
                provider: PROVIDER_STRIPE.to_owned(),
                event_id: "evt_2".to_owned(),
                event_type: "invoice.finalized".to_owned(),
                payload: json!({}),
            })
            .expect("ignoring a type never errors");
        assert_eq!(ignored, None);
        // A malformed payload for a known type is an error.
        let err = provider
            .map_webhook(BillingEvent {
                provider: PROVIDER_STRIPE.to_owned(),
                event_id: "evt_3".to_owned(),
                event_type: "subscription.status_changed".to_owned(),
                payload: json!({"subscription_id": "sub_123"}),
            })
            .expect_err("missing status must fail mapping");
        assert_eq!(
            err,
            BillingError::InvalidPayload("missing status".to_owned())
        );
        // An unknown status string fails closed, never coerces.
        let err = provider
            .map_webhook(BillingEvent {
                provider: PROVIDER_STRIPE.to_owned(),
                event_id: "evt_4".to_owned(),
                event_type: "subscription.status_changed".to_owned(),
                payload: json!({"subscription_id": "sub_123", "status": "expired"}),
            })
            .expect_err("unknown status must fail mapping");
        assert_eq!(err, BillingError::UnknownStatus("expired".to_owned()));
    }

    #[test]
    fn signature_verification_is_a_gate_not_a_mapping() {
        let provider = FakeProvider {
            name: PROVIDER_STRIPE,
        };
        assert!(provider.verify_webhook_signature(b"{}", "valid").is_ok());
        assert_eq!(
            provider
                .verify_webhook_signature(b"{}", "bogus")
                .expect_err("bad signature must fail"),
            BillingError::SignatureInvalid
        );
    }
}
