//! detag-billing::razorpay — the Razorpay adapter (P1-073, FR-P-05).
//!
//! [`RazorpayAdapter`] implements [`BillingProvider`](crate::provider::BillingProvider)
//! over plain HTTPS (`reqwest` 0.12 + rustls, no Razorpay SDK
//! exists for Rust in the kit) with HTTP Basic auth
//! (`key_id:key_secret`) against `https://api.razorpay.com/v1`.
//! Domain types only cross the trait — no Razorpay JSON ever
//! leaves this module except through the domain shapes.
//!
//! # Test mode is enforced at construction
//!
//! `RazorpayAdapter::new` accepts only `rzp_test_...` key ids,
//! plus `rzp_live_...` when `allow_live` is true. A live key
//! without the flag is [`BillingError::Configuration`] — the
//! adapter refuses to exist, so no code path can charge a real
//! card by accident. `allow_live` is the owner flag (owner
//! decision #3, "live payment keys — TEST MODE only", still
//! pending): it must come from an explicit owner decision, and
//! it defaults to false wherever the adapter is constructed.
//! The webhook secret is a THIRD credential (set in the
//! Razorpay dashboard's webhook settings) — never the API key
//! secret; mixing them up is Razorpay's documented most-common
//! webhook mistake, so the constructor takes it separately
//! (`PF_RAZORPAY_WEBHOOK_SECRET`).
//!
//! # Checkout is a subscription create (ADR-0038)
//!
//! Razorpay has no Stripe-style hosted checkout session.
//! `create_checkout` POSTs `/v1/subscriptions` with
//! `plan_id` = the caller's `price_id` (the caller passes the
//! provider-appropriate id — `plans.razorpay_plan_id`, P1-073),
//! `total_count` = 1200 (100 years of monthly cycles,
//! Razorpay's documented maximum; Razorpay subscriptions must
//! be bounded, ours are indefinite), `quantity` 1,
//! `customer_notify` true, the caller's `customer_id` when it
//! has one, and `notes: {detag_user_id}` (the Stripe
//! `client_reference_id` precedent — the webhook reads it
//! back). It returns the created subscription's id and its
//! `short_url` — the authorization link the buyer opens.
//! `success_url` / `cancel_url` are unused: a Razorpay
//! subscription carries no merchant redirect. A bad plan id
//! (Razorpay 400 "The id provided does not exist") maps to
//! [`BillingError::InvalidPrice`], like Stripe's
//! `resource_missing`.
//!
//! # Subscription mapping, best-effort (ADR-0038)
//!
//! Razorpay statuses (`created` / `authenticated` / `active` /
//! `pending` / `halted` / `cancelled` / `completed` / `expired`,
//! plus `paused` from the pause endpoint) map to our four DB
//! states: `active` → Active; `authenticated` → Trialing (the
//! mandate is authorized, the first charge made — grants like a
//! trial); `pending` / `halted` → PastDue (charge failed and
//! Razorpay is retrying, or retries are exhausted);
//! `cancelled` / `completed` / `expired` → Canceled;
//! `created` / `paused` → [`BillingError::UnknownStatus`],
//! fail-closed (not authorized yet — do not grant; a deliberate
//! pause must neither keep granting nor trigger dunning — the
//! row keeps its last good state until resume/cancel).
//! `map_webhook` takes the subscription entity as its payload
//! (the Stripe `data.object` precedent — the handler extracts
//! `payload.subscription.entity`): `subscription.authenticated`
//! → `CheckoutCompleted` (the user from `notes.detag_user_id`);
//! `subscription.activated` / `.updated` / `.resumed` → the
//! status change; `subscription.cancelled` / `.completed` /
//! `.expired` → `SubscriptionDeleted`; `subscription.charged`,
//! `subscription.paused`, and all payment/invoice events →
//! `None` (ignored — the renewal's period end arrives via the
//! subscription events, and dunning runs off
//! `pending`/`halted`, not `payment.failed`, whose entity
//! carries no subscription link we can trust).
//!
//! # What is (and is not) tested here
//!
//! The live-key guard, the webhook signature verification
//! (against a real HMAC-SHA256 header built in the test), the
//! whole `map_webhook` mapping, the subscription-create body,
//! the customer-name derivation, and the subscription/invoice
//! field extraction from real response shapes are unit tests
//! in this module — they need no network. The HTTP legs
//! (`create_checkout`, `fetch_subscription`, `list_invoices`,
//! `create_customer`, `fetch_customer`) are proven against
//! a hand-rolled local mock server in this module (P1-075):
//! the adapter runs against `http://127.0.0.1:<port>/v1`
//! through the test-only `new_with_base_url` seam, so the
//! request wire format (method, path, Basic auth, JSON body)
//! and the response/error mapping are proven with real HTTP
//! and no Razorpay key. The seam is private to this module —
//! only these tests can call it — so production construction
//! always uses [`RazorpayAdapter::new`] and the real base. The mock
//! proves the wire format, not Razorpay's real behaviour:
//! its fixtures are the real response shapes this module
//! already unit-tests (ASSUMPTION-037). A flag-on checkout
//! with a test-mode key is still the first run against the
//! real API (ASSUMPTION-035).
//! Razorpay answers a missing id with 400 ("The id provided
//! does not exist" / "could not be found"), not 404 — the
//! fetchers map those descriptions to the typed
//! `UnknownSubscription` / `UnknownCustomer` errors; anything
//! else is `BillingError::Provider` with Razorpay's own
//! description (never a secret or a request payload).
//!
//! # No portal
//!
//! Razorpay has no hosted customer portal: `create_portal`
//! returns [`BillingError::Provider`] with a static message,
//! and the portal endpoint 500s while Razorpay is the active
//! provider (P1-077 lists it in the parity doc).

use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use serde_json::{json, Value};
use sha2::Sha256;
use uuid::Uuid;

use crate::error::BillingError;
use crate::provider::{
    BillingEvent, BillingProvider, CheckoutRequest, CheckoutSession, Customer, DomainEvent,
    Invoice, PortalRequest, PortalSession, ProviderName, SubscriptionState, SubscriptionStatus,
    PROVIDER_RAZORPAY,
};

/// The Razorpay API base (all calls are `/v1/...` under it).
const BASE_URL: &str = "https://api.razorpay.com/v1";

/// Billing cycles per subscription: 100 years of monthly
/// cycles — Razorpay's documented maximum duration. Our plans
/// are indefinite, so the subscription is bounded only by the
/// provider's own maximum; a real cancel still ends it.
const TOTAL_COUNT_MAX: u32 = 1200;

/// The subscription/customer `notes` key carrying our user id
/// (the Stripe `client_reference_id` precedent, ADR-0038).
const NOTES_USER_ID_KEY: &str = "detag_user_id";

/// The subscription `notes` key carrying the live subscription
/// an upgrade checkout replaces (the Stripe
/// `detag_upgrade_from` precedent — the webhook does not read
/// it yet, ASSUMPTION-032).
const NOTES_UPGRADE_FROM_KEY: &str = "detag_upgrade_from";

/// The subscription `notes` key carrying Detag's own GSTIN
/// (P1-076): the seller's GSTIN, so the provider's invoices
/// carry it. The webhook does not read it — it is passthrough
/// metadata, not a link.
const NOTES_GSTIN_KEY: &str = "detag_gstin";

/// Fallback customer name when the email local part cannot
/// stand as one (Razorpay requires a 3–50 char name).
const FALLBACK_CUSTOMER_NAME: &str = "Detag Customer";

/// The Razorpay implementation of [`BillingProvider`].
///
/// Built only through [`RazorpayAdapter::new`], which enforces
/// test mode (see the module docs). Holds the `reqwest` client
/// and the three credentials; none is ever logged.
pub struct RazorpayAdapter {
    client: reqwest::Client,
    key_id: String,
    key_secret: String,
    webhook_secret: String,
    /// The API base the calls go to — [`BASE_URL`] in
    /// production. The mock suite (P1-075) points it at a
    /// local server through the test-only
    /// [`RazorpayAdapter::new_with_base_url`].
    base_url: String,
}

// Manual `Debug`: the derived form would print the key id,
// the key secret, and the webhook secret. Nothing secret is
// ever printed.
impl std::fmt::Debug for RazorpayAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RazorpayAdapter").finish_non_exhaustive()
    }
}

impl RazorpayAdapter {
    /// Build the adapter. `key_id` must start with `rzp_test_`
    /// — or with `rzp_live_` when `allow_live` is true (the
    /// owner flag, owner decision #3). `key_secret` and
    /// `webhook_secret` must be non-empty: no call can
    /// authenticate without the secret, and no signature can
    /// verify without the webhook secret. Anything else is
    /// [`BillingError::Configuration`].
    pub fn new(
        key_id: &str,
        key_secret: &str,
        webhook_secret: &str,
        allow_live: bool,
    ) -> Result<Self, BillingError> {
        Self::new_with_base_url(key_id, key_secret, webhook_secret, allow_live, BASE_URL)
    }

    /// Build the adapter against a different API base. The
    /// mock suite (P1-075) uses this to point the adapter at
    /// a local server; the validation is identical to
    /// [`Self::new`], so a test adapter is a real adapter
    /// that happens to talk to localhost. Private to this
    /// module on purpose — only these tests can call it, so
    /// no production path may ever point the adapter
    /// anywhere but `api.razorpay.com`.
    fn new_with_base_url(
        key_id: &str,
        key_secret: &str,
        webhook_secret: &str,
        allow_live: bool,
        base_url: &str,
    ) -> Result<Self, BillingError> {
        if key_id.is_empty() {
            return Err(BillingError::Configuration(
                "razorpay key id is empty".to_owned(),
            ));
        }
        if key_secret.is_empty() {
            return Err(BillingError::Configuration(
                "razorpay key secret is empty".to_owned(),
            ));
        }
        if webhook_secret.is_empty() {
            return Err(BillingError::Configuration(
                "razorpay webhook secret is empty".to_owned(),
            ));
        }
        let live = key_id.starts_with("rzp_live_");
        let test = key_id.starts_with("rzp_test_");
        if !live && !test {
            return Err(BillingError::Configuration(
                "razorpay key id must start with rzp_test_ or rzp_live_".to_owned(),
            ));
        }
        if live && !allow_live {
            return Err(BillingError::Configuration(
                "live razorpay key rejected: test mode only (owner decision #3)".to_owned(),
            ));
        }
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|err| {
                BillingError::Configuration(format!("razorpay http client failed: {err}"))
            })?;
        Ok(Self {
            client,
            key_id: key_id.to_owned(),
            key_secret: key_secret.to_owned(),
            webhook_secret: webhook_secret.to_owned(),
            base_url: base_url.to_owned(),
        })
    }

    /// POST `path` (under the adapter's base URL) with a JSON body,
    /// Basic-authenticated. A non-2xx answer is a provider
    /// error built from Razorpay's error envelope; a transport
    /// failure is [`BillingError::Provider`] (the URL carries
    /// no secret — auth rides in the header, not the query).
    async fn post(&self, path: &str, body: Value) -> Result<Value, BillingError> {
        let url = format!("{}{path}", self.base_url);
        let res = self
            .client
            .post(&url)
            .basic_auth(&self.key_id, Some(&self.key_secret))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(serde_json::to_string(&body).map_err(|err| {
                BillingError::Provider(format!("razorpay request is not JSON: {err}"))
            })?)
            .send()
            .await
            .map_err(|err| BillingError::Provider(format!("razorpay request failed: {err}")))?;
        read_response(res).await
    }

    /// GET `path` (under the adapter's base URL), Basic-authenticated.
    /// Same error contract as [`Self::post`].
    async fn get(&self, path: &str) -> Result<Value, BillingError> {
        let url = format!("{}{path}", self.base_url);
        let res = self
            .client
            .get(&url)
            .basic_auth(&self.key_id, Some(&self.key_secret))
            .send()
            .await
            .map_err(|err| BillingError::Provider(format!("razorpay request failed: {err}")))?;
        read_response(res).await
    }
}

/// Read a Razorpay response: 2xx decodes as JSON (via text +
/// `serde_json` — the workspace `reqwest` has no `json`
/// feature); anything else parses the `{"error":
/// {"description", "field"}}` envelope into a provider error.
/// A body that is not JSON at all degrades to the status line
/// — never the request.
async fn read_response(res: reqwest::Response) -> Result<Value, BillingError> {
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    if status.is_success() {
        return serde_json::from_str(&body).map_err(|err| {
            BillingError::Provider(format!("razorpay response is not JSON: {err}"))
        });
    }
    Err(api_failure(status, &body))
}

/// Build the provider error for a non-2xx body: the envelope's
/// `description` (falling back to its `code`, then to the
/// status line). The description is Razorpay's own error text.
fn api_failure(status: reqwest::StatusCode, body: &str) -> BillingError {
    let parsed: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let error = parsed.get("error");
    let description = error
        .and_then(|e| e.get("description"))
        .and_then(Value::as_str)
        .or_else(|| error.and_then(|e| e.get("code")).and_then(Value::as_str))
        .map(str::to_owned)
        .unwrap_or_else(|| format!("razorpay api error: HTTP {status}"));
    let field = error
        .and_then(|e| e.get("field"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    BillingError::Provider(
        ApiFailureDisplay {
            status,
            description,
            field,
        }
        .to_string(),
    )
}

/// The display of a failed Razorpay call as a
/// `BillingError::Provider` message — kept as a tiny struct so
/// the message is built in exactly one place. (Not a
/// `BillingError` variant: the callers map the failure to the
/// typed errors themselves.)
struct ApiFailureDisplay {
    status: reqwest::StatusCode,
    description: String,
    field: Option<String>,
}

impl std::fmt::Display for ApiFailureDisplay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The description is Razorpay's own text; the field
        // names the offending input when Razorpay names one.
        match &self.field {
            Some(field) => write!(
                f,
                "razorpay api error (HTTP {}): {} (field: {})",
                self.status, self.description, field
            ),
            None => write!(
                f,
                "razorpay api error (HTTP {}): {}",
                self.status, self.description
            ),
        }
    }
}

/// Does this provider error say the named id does not exist?
/// Razorpay answers a missing id with 400 ("The id provided
/// does not exist" / "The ID provided is invalid or could not
/// be found"), not 404 — so the fetchers match on the
/// description, not the status. The match is on the lowercased
/// description so a wording change in case alone cannot slip
/// a missing id past as a generic provider error.
fn is_not_found_message(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("does not exist")
        || lower.contains("could not be found")
        || lower.contains("not found")
}

/// A required string field of a Razorpay object, or
/// [`BillingError::InvalidPayload`] naming the missing field.
fn required_str<'a>(object: &'a Value, field: &str) -> Result<&'a str, BillingError> {
    object
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| BillingError::InvalidPayload(format!("missing {field}")))
}

/// An optional string field of a Razorpay object (`null` and
/// absent both read as `None`).
fn optional_str(object: &Value, field: &str) -> Option<String> {
    object.get(field).and_then(Value::as_str).map(str::to_owned)
}

/// A unix-seconds timestamp field of a Razorpay object, or
/// [`BillingError::InvalidPayload`] when it is missing or out
/// of range.
fn required_timestamp(object: &Value, field: &str) -> Result<DateTime<Utc>, BillingError> {
    let secs = object
        .get(field)
        .and_then(Value::as_i64)
        .ok_or_else(|| BillingError::InvalidPayload(format!("missing {field}")))?;
    DateTime::from_timestamp(secs, 0)
        .ok_or_else(|| BillingError::InvalidPayload(format!("{field} out of range")))
}

/// A nullable unix-seconds timestamp field (`null` and absent
/// both read as `None` — a subscription before its first
/// charge has no current period yet).
fn optional_timestamp(object: &Value, field: &str) -> Result<Option<DateTime<Utc>>, BillingError> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            let secs = value.as_i64().ok_or_else(|| {
                BillingError::InvalidPayload(format!("{field} is not a timestamp"))
            })?;
            Ok(Some(DateTime::from_timestamp(secs, 0).ok_or_else(
                || BillingError::InvalidPayload(format!("{field} out of range")),
            )?))
        }
    }
}

/// Map a Razorpay subscription status string to our state
/// (ADR-0038 §4). Anything else — `created`, `paused`, or a
/// status Razorpay invents later — fails closed with
/// [`BillingError::UnknownStatus`], never coerced.
fn map_subscription_status(status: &str) -> Result<SubscriptionStatus, BillingError> {
    match status {
        "active" => Ok(SubscriptionStatus::Active),
        "authenticated" => Ok(SubscriptionStatus::Trialing),
        "pending" | "halted" => Ok(SubscriptionStatus::PastDue),
        "cancelled" | "completed" | "expired" => Ok(SubscriptionStatus::Canceled),
        other => Err(BillingError::UnknownStatus(other.to_owned())),
    }
}

/// Map a Razorpay subscription object (a fetched subscription,
/// or the `payload.subscription.entity` of a webhook) to its
/// domain state. The `price_ids` carry the subscription's
/// `plan_id` — the handler resolves the plan from it
/// (`plans.by_razorpay_plan_id`, P1-074). `cancel_at_period_end`
/// is always `false`: Razorpay reports no such flag on the
/// object (a cancel-at-cycle-end is a scheduled change, and
/// `has_scheduled_changes` does not say what the change is),
/// so the adapter never claims a pending period-end cancel.
fn subscription_state_from_object(object: &Value) -> Result<SubscriptionState, BillingError> {
    Ok(SubscriptionState {
        provider_subscription_id: required_str(object, "id")?.to_owned(),
        provider_customer_id: optional_str(object, "customer_id"),
        status: map_subscription_status(required_str(object, "status")?)?,
        current_period_end: optional_timestamp(object, "current_end")?,
        cancel_at_period_end: false,
        price_ids: vec![required_str(object, "plan_id")?.to_owned()],
    })
}

/// Map a Razorpay invoice object to our shape. `amount` is
/// already the smallest unit (paise); `status` is the
/// provider's own string, kept opaque on purpose.
/// `download_url` is `None`: a Razorpay invoice has a hosted
/// page (`short_url`), no PDF file.
fn invoice_from_object(object: &Value) -> Result<Invoice, BillingError> {
    Ok(Invoice {
        id: required_str(object, "id")?.to_owned(),
        number: optional_str(object, "invoice_number"),
        amount: object
            .get("amount")
            .and_then(Value::as_i64)
            .ok_or_else(|| BillingError::InvalidPayload("missing amount".to_owned()))?,
        currency: required_str(object, "currency")?.to_ascii_uppercase(),
        status: required_str(object, "status")?.to_owned(),
        created_at: required_timestamp(object, "date")?,
        hosted_url: optional_str(object, "short_url"),
        download_url: None,
    })
}

/// The subscription-create body for a checkout (pure, so the
/// mapping is a unit test even though the call itself needs
/// the network). `plan_id` is the caller's `price_id` — the
/// Razorpay plan id for the catalog plan. `total_count` is
/// [`TOTAL_COUNT_MAX`] (ADR-0038 §3); `notes` carries our user
/// id so the `subscription.authenticated` webhook can link
/// the subscription back to the user, and the superseded
/// subscription id for an upgrade (the Stripe metadata
/// precedent — the webhook does not read it yet), plus
/// Detag's GSTIN when configured (P1-076 — passthrough, the
/// webhook does not read it either).
fn subscription_create_body(req: &CheckoutRequest) -> Value {
    let mut notes = serde_json::Map::new();
    notes.insert(
        NOTES_USER_ID_KEY.to_owned(),
        Value::String(req.user_id.to_string()),
    );
    if let Some(from) = &req.upgrade_from_provider_subscription_id {
        notes.insert(
            NOTES_UPGRADE_FROM_KEY.to_owned(),
            Value::String(from.clone()),
        );
    }
    if let Some(gstin) = &req.gstin {
        notes.insert(NOTES_GSTIN_KEY.to_owned(), Value::String(gstin.clone()));
    }
    let mut body = json!({
        "plan_id": req.price_id,
        "total_count": TOTAL_COUNT_MAX,
        "quantity": 1,
        "customer_notify": true,
        "notes": Value::Object(notes),
    });
    if let Some(customer_id) = &req.customer_id {
        body["customer_id"] = Value::String(customer_id.clone());
    }
    body
}

/// Derive a Razorpay customer `name` from the email (pure).
/// Razorpay requires a name (3–50 chars; alphanumerics plus
/// space, period, apostrophe, parentheses) and the trait
/// carries no name — only the email. The local part is kept
/// with every disallowed character turned into a space
/// (whitespace collapsed, ends trimmed); a local part that
/// cannot stand as a name (under 3 chars after sanitizing,
/// or empty) falls back to [`FALLBACK_CUSTOMER_NAME`]. The
/// email itself is still sent as `email`, and
/// `notes.detag_user_id` is the real link — the name is
/// cosmetic at Razorpay.
fn customer_name_from_email(email: &str) -> String {
    let local = email.split('@').next().unwrap_or("");
    let mut cleaned = String::with_capacity(local.len());
    for ch in local.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, ' ' | '.' | '\'' | '(' | ')') {
            cleaned.push(ch);
        } else {
            cleaned.push(' ');
        }
    }
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() < 3 {
        return FALLBACK_CUSTOMER_NAME.to_owned();
    }
    // Over-long names truncate at 50 chars (Razorpay's max),
    // with any trailing space from the cut trimmed off.
    if collapsed.chars().count() > 50 {
        collapsed
            .chars()
            .take(50)
            .collect::<String>()
            .trim_end()
            .to_owned()
    } else {
        collapsed
    }
}

/// Our user id off a subscription's `notes` (pure): the
/// `subscription.authenticated` mapping reads it the way
/// Stripe's mapping reads `client_reference_id`. Absent means
/// the subscription was not created by our checkout — the
/// handler logs it instead of guessing a user. A
/// present-but-unparseable value is our own bug, so it fails
/// mapping loudly.
fn user_id_from_notes(object: &Value) -> Result<Option<Uuid>, BillingError> {
    match object
        .get("notes")
        .and_then(|notes| notes.get(NOTES_USER_ID_KEY))
        .and_then(Value::as_str)
    {
        None => Ok(None),
        Some(raw) => Some(raw.parse::<Uuid>().map_err(|_| {
            BillingError::InvalidPayload(format!("{NOTES_USER_ID_KEY} is not a user id"))
        }))
        .transpose(),
    }
}

#[async_trait]
impl BillingProvider for RazorpayAdapter {
    fn provider_name(&self) -> ProviderName {
        PROVIDER_RAZORPAY
    }

    async fn create_checkout(&self, req: CheckoutRequest) -> Result<CheckoutSession, BillingError> {
        if req.price_id.is_empty() {
            return Err(BillingError::InvalidPrice(req.price_id));
        }
        let body = subscription_create_body(&req);
        let created = self.post("/subscriptions", body).await.map_err(|err| {
            // A bad plan id is the one checkout failure the
            // caller can fix by configuring the catalog — the
            // other 400s stay generic provider errors.
            if let BillingError::Provider(message) = &err {
                if message.contains("does not exist") && message.contains("plan") {
                    return BillingError::InvalidPrice(req.price_id.clone());
                }
            }
            err
        })?;
        Ok(CheckoutSession {
            id: required_str(&created, "id")?.to_owned(),
            url: required_str(&created, "short_url")?.to_owned(),
        })
    }

    async fn create_portal(&self, _req: PortalRequest) -> Result<PortalSession, BillingError> {
        // Razorpay has no hosted customer portal (ADR-0038
        // §9): there is no URL to return, and inventing one
        // would send the buyer to a page that does not exist.
        // The portal endpoint 500s while Razorpay is active.
        Err(BillingError::Provider(
            "razorpay has no customer portal".to_owned(),
        ))
    }

    async fn fetch_subscription(
        &self,
        provider_subscription_id: &str,
    ) -> Result<SubscriptionState, BillingError> {
        let object = self
            .get(&format!("/subscriptions/{provider_subscription_id}"))
            .await
            .map_err(|err| {
                if let BillingError::Provider(message) = &err {
                    if is_not_found_message(message) {
                        return BillingError::UnknownSubscription(
                            provider_subscription_id.to_owned(),
                        );
                    }
                }
                err
            })?;
        subscription_state_from_object(&object)
    }

    async fn list_invoices(
        &self,
        customer_id: &str,
        limit: u32,
    ) -> Result<Vec<Invoice>, BillingError> {
        // The list is asked for this customer's invoices — and
        // then filtered client-side by `customer_id` as a
        // backstop: the documented invoice-list filter is
        // `subscription_id`, not `customer_id`, so if Razorpay
        // ignores the query parameter the response is the
        // merchant's newest invoices and the filter is what
        // keeps another customer's invoices out of the answer
        // (at most `limit` survive — a merchant with many
        // invoices may need a second page, a follow-up).
        let listed = self
            .get(&format!(
                "/invoices?customer_id={customer_id}&count={limit}"
            ))
            .await?;
        let items = listed
            .get("items")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                BillingError::Provider("razorpay invoice list has no items".to_owned())
            })?;
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            if item.get("customer_id").and_then(Value::as_str) != Some(customer_id) {
                continue;
            }
            out.push(invoice_from_object(item)?);
        }
        Ok(out)
    }

    async fn create_customer(&self, user_id: Uuid, email: &str) -> Result<Customer, BillingError> {
        let created = self
            .post(
                "/customers",
                json!({
                    "name": customer_name_from_email(email),
                    "email": email,
                    "notes": { NOTES_USER_ID_KEY: user_id.to_string() },
                }),
            )
            .await?;
        Ok(Customer {
            provider_customer_id: required_str(&created, "id")?.to_owned(),
            // The response echoes the email; fall back to the
            // requested one (our own input, not provider data)
            // rather than failing a working call on a field we
            // supplied.
            email: optional_str(&created, "email").unwrap_or_else(|| email.to_owned()),
        })
    }

    async fn fetch_customer(&self, provider_customer_id: &str) -> Result<Customer, BillingError> {
        let object = self
            .get(&format!("/customers/{provider_customer_id}"))
            .await
            .map_err(|err| {
                if let BillingError::Provider(message) = &err {
                    if is_not_found_message(message) {
                        return BillingError::UnknownCustomer(provider_customer_id.to_owned());
                    }
                }
                err
            })?;
        Ok(Customer {
            provider_customer_id: required_str(&object, "id")?.to_owned(),
            email: optional_str(&object, "email").unwrap_or_default(),
        })
    }

    fn verify_webhook_signature(
        &self,
        payload: &[u8],
        signature: &str,
    ) -> Result<(), BillingError> {
        // Razorpay signs the raw body: `hex(hmac_sha256(
        // webhook_secret, body))`, sent as
        // `X-Razorpay-Signature`. The comparison is
        // constant-time (`verify_slice`); a non-hex header is
        // not a signature at all.
        let expected = hex::decode(signature).map_err(|_| BillingError::SignatureInvalid)?;
        let mut mac = Hmac::<Sha256>::new_from_slice(self.webhook_secret.as_bytes())
            .map_err(|_| BillingError::SignatureInvalid)?;
        mac.update(payload);
        mac.verify_slice(&expected)
            .map_err(|_| BillingError::SignatureInvalid)
    }

    fn map_webhook(&self, event: BillingEvent) -> Result<Option<DomainEvent>, BillingError> {
        if event.provider != PROVIDER_RAZORPAY {
            return Err(BillingError::InvalidPayload(
                "event is not a razorpay event".to_owned(),
            ));
        }
        match event.event_type.as_str() {
            // The first payment: the subscription is authorized
            // and the row is created (the Stripe
            // `checkout.session.completed` precedent). The user
            // comes off `notes.detag_user_id` (this ticket
            // writes it at checkout).
            "subscription.authenticated" => Ok(Some(DomainEvent::CheckoutCompleted {
                customer_id: required_str(&event.payload, "customer_id")?.to_owned(),
                subscription_id: required_str(&event.payload, "id")?.to_owned(),
                user_id: user_id_from_notes(&event.payload)?,
            })),
            "subscription.activated"
            | "subscription.updated"
            | "subscription.resumed"
            | "subscription.pending"
            | "subscription.halted" => {
                let state = subscription_state_from_object(&event.payload)?;
                Ok(Some(DomainEvent::SubscriptionStatusChanged {
                    subscription_id: state.provider_subscription_id,
                    status: state.status,
                    current_period_end: state.current_period_end,
                    cancel_at_period_end: state.cancel_at_period_end,
                }))
            }
            "subscription.cancelled" | "subscription.completed" | "subscription.expired" => {
                Ok(Some(DomainEvent::SubscriptionDeleted {
                    subscription_id: required_str(&event.payload, "id")?.to_owned(),
                }))
            }
            // A charge writes no row (the renewal's period end
            // arrives via the subscription events — the P1-065
            // `invoice.paid` no-op precedent); a pause changes
            // no state our machine can store; payment/invoice
            // events carry no subscription link to act on.
            // Unknown-but-well-formed types are ignored, not
            // errors — the handler answers 200 and Razorpay
            // stops retrying.
            _ => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const TEST_KEY_ID: &str = "rzp_test_00000000000000";
    const TEST_KEY_SECRET: &str = "test_secret_00000000000000000000";
    const WEBHOOK_SECRET: &str = "whsec_test00000000000000000000000000";

    fn adapter() -> RazorpayAdapter {
        RazorpayAdapter::new(TEST_KEY_ID, TEST_KEY_SECRET, WEBHOOK_SECRET, false)
            .expect("test key is valid")
    }

    /// Build a real Razorpay signature header for `payload`:
    /// `hex(hmac_sha256(webhook_secret, raw_body))`.
    fn razorpay_signature_header(payload: &[u8]) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(WEBHOOK_SECRET.as_bytes())
            .expect("hmac accepts any key length");
        mac.update(payload);
        hex::encode(mac.finalize().into_bytes())
    }

    #[test]
    fn provider_name_is_razorpay() {
        assert_eq!(
            BillingProvider::provider_name(&adapter()),
            PROVIDER_RAZORPAY
        );
    }

    #[test]
    fn test_key_accepted_and_malformed_keys_rejected() {
        assert!(RazorpayAdapter::new(TEST_KEY_ID, TEST_KEY_SECRET, WEBHOOK_SECRET, false).is_ok());
        for (key_id, key_secret, webhook_secret) in [
            ("", TEST_KEY_SECRET, WEBHOOK_SECRET),
            ("not-a-key", TEST_KEY_SECRET, WEBHOOK_SECRET),
            ("rzp_live_00000000000000", TEST_KEY_SECRET, WEBHOOK_SECRET),
            (
                "sk_test_51Fake0000000000000000",
                TEST_KEY_SECRET,
                WEBHOOK_SECRET,
            ),
            (TEST_KEY_ID, "", WEBHOOK_SECRET),
            (TEST_KEY_ID, TEST_KEY_SECRET, ""),
        ] {
            let err = RazorpayAdapter::new(key_id, key_secret, webhook_secret, false)
                .expect_err("malformed key must fail");
            assert!(
                matches!(err, BillingError::Configuration(_)),
                "bad key {key_id:?} gave {err:?}"
            );
            // The error never leaks a credential value.
            assert!(
                key_id.is_empty() || !err.to_string().contains(key_id),
                "error leaks the key id"
            );
            assert!(
                key_secret.is_empty() || !err.to_string().contains(key_secret),
                "error leaks the key secret"
            );
        }
    }

    #[test]
    fn live_key_rejected_without_owner_flag_and_accepted_with_it() {
        let err = RazorpayAdapter::new(
            "rzp_live_00000000000000",
            TEST_KEY_SECRET,
            WEBHOOK_SECRET,
            false,
        )
        .expect_err("live key without the flag must fail");
        assert_eq!(
            err,
            BillingError::Configuration(
                "live razorpay key rejected: test mode only (owner decision #3)".to_owned()
            )
        );
        assert!(!err.to_string().contains("rzp_live_"));
        assert!(RazorpayAdapter::new(
            "rzp_live_00000000000000",
            TEST_KEY_SECRET,
            WEBHOOK_SECRET,
            true
        )
        .is_ok());
    }

    #[test]
    fn signature_verification_accepts_a_real_razorpay_header() {
        let payload = br#"{"event":"subscription.activated","payload":{"subscription":{"entity":{"id":"sub_123","status":"active"}}}}"#;
        let header = razorpay_signature_header(payload);
        assert!(adapter().verify_webhook_signature(payload, &header).is_ok());
    }

    #[test]
    fn signature_verification_rejects_tampered_payload_wrong_secret_and_garbage() {
        let payload = br#"{"event":"subscription.activated"}"#;
        let header = razorpay_signature_header(payload);
        let adapter = adapter();
        // Same header, different body: the signature no longer covers it.
        assert_eq!(
            adapter
                .verify_webhook_signature(b"{}", &header)
                .expect_err("tampered payload must fail"),
            BillingError::SignatureInvalid
        );
        // A header built for a different secret.
        let other = RazorpayAdapter::new(
            TEST_KEY_ID,
            TEST_KEY_SECRET,
            "whsec_different0000000000000000000000",
            false,
        )
        .expect("second adapter builds");
        assert_eq!(
            other
                .verify_webhook_signature(payload, &header)
                .expect_err("wrong secret must fail"),
            BillingError::SignatureInvalid
        );
        // Garbage header, empty header, non-hex header.
        assert_eq!(
            adapter
                .verify_webhook_signature(payload, "deadbeef")
                .expect_err("garbage signature must fail"),
            BillingError::SignatureInvalid
        );
        assert_eq!(
            adapter
                .verify_webhook_signature(payload, "")
                .expect_err("empty signature must fail"),
            BillingError::SignatureInvalid
        );
        assert_eq!(
            adapter
                .verify_webhook_signature(payload, "zzzz-not-hex")
                .expect_err("non-hex signature must fail"),
            BillingError::SignatureInvalid
        );
    }

    fn razorpay_event(event_type: &str, payload: Value) -> BillingEvent {
        BillingEvent {
            provider: PROVIDER_RAZORPAY.to_owned(),
            event_id: "evt_test".to_owned(),
            event_type: event_type.to_owned(),
            payload,
        }
    }

    #[test]
    fn webhook_maps_real_razorpay_event_shapes() {
        let adapter = adapter();
        // subscription.authenticated → CheckoutCompleted, user
        // from notes.detag_user_id.
        let user_id = Uuid::now_v7();
        assert_eq!(
            adapter
                .map_webhook(razorpay_event(
                    "subscription.authenticated",
                    json!({"id": "sub_123", "customer_id": "cust_123", "status": "authenticated", "plan_id": "plan_pro", "current_end": 1794844800, "notes": {"detag_user_id": user_id.to_string()}}),
                ))
                .expect("authenticated maps"),
            Some(DomainEvent::CheckoutCompleted {
                customer_id: "cust_123".to_owned(),
                subscription_id: "sub_123".to_owned(),
                user_id: Some(user_id),
            })
        );
        // No notes: the user is None and the handler logs it
        // instead of guessing (the Stripe no-reference-id rule).
        assert_eq!(
            adapter
                .map_webhook(razorpay_event(
                    "subscription.authenticated",
                    json!({"id": "sub_123", "customer_id": "cust_123", "status": "authenticated", "plan_id": "plan_pro"}),
                ))
                .expect("authenticated without notes maps"),
            Some(DomainEvent::CheckoutCompleted {
                customer_id: "cust_123".to_owned(),
                subscription_id: "sub_123".to_owned(),
                user_id: None,
            })
        );
        // Activated / updated / resumed → status change, with
        // the period end converted from unix seconds.
        assert_eq!(
            adapter
                .map_webhook(razorpay_event(
                    "subscription.activated",
                    json!({"id": "sub_123", "customer_id": "cust_123", "status": "active", "plan_id": "plan_pro", "current_end": 1794844800}),
                ))
                .expect("activated maps"),
            Some(DomainEvent::SubscriptionStatusChanged {
                subscription_id: "sub_123".to_owned(),
                status: SubscriptionStatus::Active,
                current_period_end: Some(
                    DateTime::from_timestamp(1794844800, 0).expect("in range")
                ),
                cancel_at_period_end: false,
            })
        );
        // Pending / halted are the dunning trigger.
        assert_eq!(
            adapter
                .map_webhook(razorpay_event(
                    "subscription.updated",
                    json!({"id": "sub_123", "status": "pending", "plan_id": "plan_pro"}),
                ))
                .expect("pending maps"),
            Some(DomainEvent::SubscriptionStatusChanged {
                subscription_id: "sub_123".to_owned(),
                status: SubscriptionStatus::PastDue,
                current_period_end: None,
                cancel_at_period_end: false,
            })
        );
        assert_eq!(
            adapter
                .map_webhook(razorpay_event(
                    "subscription.halted",
                    json!({"id": "sub_123", "status": "halted", "plan_id": "plan_pro"}),
                ))
                .expect("halted maps"),
            Some(DomainEvent::SubscriptionStatusChanged {
                subscription_id: "sub_123".to_owned(),
                status: SubscriptionStatus::PastDue,
                current_period_end: None,
                cancel_at_period_end: false,
            })
        );
        // Terminal events → the row is canceled, never deleted.
        for event_type in [
            "subscription.cancelled",
            "subscription.completed",
            "subscription.expired",
        ] {
            assert_eq!(
                adapter
                    .map_webhook(razorpay_event(
                        event_type,
                        json!({"id": "sub_123", "status": "cancelled", "plan_id": "plan_pro"}),
                    ))
                    .expect("terminal maps"),
                Some(DomainEvent::SubscriptionDeleted {
                    subscription_id: "sub_123".to_owned(),
                }),
                "{event_type} must delete"
            );
        }
        // Charged, paused, payment, and unknown events are
        // ignored, not errors — the handler answers 200.
        for (event_type, payload) in [
            (
                "subscription.charged",
                json!({"id": "sub_123", "status": "active", "plan_id": "plan_pro"}),
            ),
            (
                "subscription.paused",
                json!({"id": "sub_123", "status": "paused", "plan_id": "plan_pro"}),
            ),
            ("payment.captured", json!({"id": "pay_123"})),
            ("payment.failed", json!({"id": "pay_124"})),
            ("invoice.paid", json!({"id": "inv_123"})),
            ("subscription.trial_ending", json!({"id": "sub_123"})),
        ] {
            assert_eq!(
                adapter
                    .map_webhook(razorpay_event(event_type, payload))
                    .expect("ignoring a type never errors"),
                None,
                "{event_type} must be ignored"
            );
        }
    }

    #[test]
    fn webhook_mapping_fails_closed_on_malformed_and_unknown_status() {
        let adapter = adapter();
        // `created` is not authorized yet — do not grant.
        assert_eq!(
            adapter
                .map_webhook(razorpay_event(
                    "subscription.updated",
                    json!({"id": "sub_123", "status": "created", "plan_id": "plan_pro"}),
                ))
                .expect_err("created must fail closed"),
            BillingError::UnknownStatus("created".to_owned())
        );
        // A pause on the update path fails closed too (the
        // `subscription.paused` event itself is ignored, but a
        // status field our machine cannot store is never read
        // as a grant).
        assert_eq!(
            adapter
                .map_webhook(razorpay_event(
                    "subscription.updated",
                    json!({"id": "sub_123", "status": "paused", "plan_id": "plan_pro"}),
                ))
                .expect_err("paused must fail closed"),
            BillingError::UnknownStatus("paused".to_owned())
        );
        // Missing customer on the authenticated event.
        assert_eq!(
            adapter
                .map_webhook(razorpay_event(
                    "subscription.authenticated",
                    json!({"id": "sub_123", "status": "authenticated"}),
                ))
                .expect_err("missing customer must fail"),
            BillingError::InvalidPayload("missing customer_id".to_owned())
        );
        // A notes user id that is not our user id is our own
        // bug — fail mapping loudly.
        assert_eq!(
            adapter
                .map_webhook(razorpay_event(
                    "subscription.authenticated",
                    json!({"id": "sub_123", "customer_id": "cust_123", "status": "authenticated", "notes": {"detag_user_id": "not-a-uuid"}}),
                ))
                .expect_err("a bad notes user id must fail"),
            BillingError::InvalidPayload("detag_user_id is not a user id".to_owned())
        );
        // An event from another provider is not mappable here.
        assert_eq!(
            adapter
                .map_webhook(BillingEvent {
                    provider: "stripe".to_owned(),
                    event_id: "evt_s".to_owned(),
                    event_type: "customer.subscription.updated".to_owned(),
                    payload: json!({}),
                })
                .expect_err("wrong provider must fail"),
            BillingError::InvalidPayload("event is not a razorpay event".to_owned())
        );
    }

    #[test]
    fn subscription_create_body_carries_plan_total_count_and_user_note() {
        let user_id = Uuid::now_v7();
        let body = subscription_create_body(&CheckoutRequest {
            user_id,
            customer_id: Some("cust_123".to_owned()),
            price_id: "plan_pro".to_owned(),
            success_url: "https://app.example/success".to_owned(),
            cancel_url: "https://app.example/cancel".to_owned(),
            upgrade_from_provider_subscription_id: Some("sub_old_123".to_owned()),
            gstin: Some(TEST_GSTIN.to_owned()),
        });
        assert_eq!(body["plan_id"], "plan_pro");
        assert_eq!(body["total_count"], TOTAL_COUNT_MAX);
        assert_eq!(body["quantity"], 1);
        assert_eq!(body["customer_notify"], true);
        assert_eq!(body["customer_id"], "cust_123");
        assert_eq!(body["notes"]["detag_user_id"], user_id.to_string());
        assert_eq!(body["notes"]["detag_upgrade_from"], "sub_old_123");
        assert_eq!(body["notes"]["detag_gstin"], TEST_GSTIN);
        // A first purchase carries no customer and no upgrade
        // note — only the user link.
        let first = subscription_create_body(&CheckoutRequest {
            user_id,
            customer_id: None,
            price_id: "plan_pro".to_owned(),
            success_url: "https://app.example/success".to_owned(),
            cancel_url: "https://app.example/cancel".to_owned(),
            upgrade_from_provider_subscription_id: None,
            gstin: None,
        });
        assert!(first.get("customer_id").is_none());
        assert!(first["notes"].get("detag_upgrade_from").is_none());
        assert!(first["notes"].get("detag_gstin").is_none());
        assert_eq!(first["notes"]["detag_user_id"], user_id.to_string());
    }

    #[test]
    fn customer_name_from_email_sanitizes_to_razorpay_rules() {
        // The local part, kept as-is when it already fits.
        assert_eq!(
            customer_name_from_email("gaurav.kumar@example.com"),
            "gaurav.kumar"
        );
        // Disallowed characters become spaces (the `@` never
        // reaches the name — only the local part is used).
        assert_eq!(
            customer_name_from_email("first+last@example.com"),
            "first last"
        );
        assert_eq!(
            customer_name_from_email("user_name-1@example.com"),
            "user name 1"
        );
        // Too short after sanitizing falls back.
        assert_eq!(
            customer_name_from_email("ab@example.com"),
            FALLBACK_CUSTOMER_NAME
        );
        assert_eq!(
            customer_name_from_email("@example.com"),
            FALLBACK_CUSTOMER_NAME
        );
        // Over-long truncates at 50 chars.
        let long = format!("{}@example.com", "a".repeat(60));
        let name = customer_name_from_email(&long);
        assert_eq!(name.chars().count(), 50);
        assert!(name.chars().all(|c| c.is_ascii_alphanumeric() || c == ' '));
    }

    /// The subscription object as Razorpay returns it (the
    /// create-subscription response shape): the field paths
    /// the live `fetch_subscription` uses, proven here — the
    /// fetch itself stays untested live (ASSUMPTION-035), but
    /// the extraction from what it returns is proven.
    #[test]
    fn subscription_state_comes_off_a_real_subscription_shape() {
        let object = json!({
            "id": "sub_00000000000001",
            "entity": "subscription",
            "plan_id": "plan_00000000000001",
            "customer_id": "cust_00000000000001",
            "status": "active",
            "current_start": 1773461489,
            "current_end": 1776053489,
            "ended_at": null,
            "quantity": 1,
            "notes": {"detag_user_id": "019aa1b2-c3d4-7e5f-8000-000000000001"},
            "charge_at": 1773461489,
            "total_count": 1200,
            "paid_count": 3,
            "short_url": "https://rzp.io/i/PWtAiEo",
            "remaining_count": 1197
        });
        let state = subscription_state_from_object(&object).expect("the fixture maps");
        assert_eq!(state.provider_subscription_id, "sub_00000000000001");
        assert_eq!(
            state.provider_customer_id.as_deref(),
            Some("cust_00000000000001")
        );
        assert_eq!(state.status, SubscriptionStatus::Active);
        assert_eq!(
            state.current_period_end,
            Some(DateTime::from_timestamp(1776053489, 0).expect("in range"))
        );
        assert!(!state.cancel_at_period_end);
        assert_eq!(state.price_ids, vec!["plan_00000000000001".to_owned()]);
        // Before the first charge there is no customer and no
        // current period — both read as None, not an error.
        let created = json!({
            "id": "sub_00000000000002",
            "plan_id": "plan_00000000000001",
            "customer_id": null,
            "status": "authenticated",
            "current_end": null
        });
        let state = subscription_state_from_object(&created).expect("created maps");
        assert_eq!(state.provider_customer_id, None);
        assert_eq!(state.status, SubscriptionStatus::Trialing);
        assert_eq!(state.current_period_end, None);
    }

    /// The invoice object as Razorpay returns it (the
    /// fetch-all-invoices shape): every field of our shape,
    /// including the nullable ones.
    #[test]
    fn invoice_comes_off_a_real_invoice_shape() {
        let object = json!({
            "id": "inv_DAweOiQ7amIUVd",
            "entity": "invoice",
            "invoice_number": "#0961",
            "customer_id": "cust_DAtUWmvpktokrT",
            "status": "paid",
            "date": 1566891149,
            "amount": 600,
            "currency": "INR",
            "short_url": "https://rzp.io/i/K8Zg72C"
        });
        let invoice = invoice_from_object(&object).expect("the fixture maps");
        assert_eq!(invoice.id, "inv_DAweOiQ7amIUVd");
        assert_eq!(invoice.number.as_deref(), Some("#0961"));
        assert_eq!(invoice.amount, 600);
        assert_eq!(invoice.currency, "INR");
        assert_eq!(invoice.status, "paid");
        assert_eq!(
            invoice.created_at,
            DateTime::from_timestamp(1566891149, 0).expect("in range")
        );
        assert_eq!(
            invoice.hosted_url.as_deref(),
            Some("https://rzp.io/i/K8Zg72C")
        );
        assert_eq!(invoice.download_url, None);
        // Nullable number / short_url read as None.
        let bare = json!({
            "id": "inv_2",
            "invoice_number": null,
            "customer_id": "cust_1",
            "status": "issued",
            "date": 1566891149,
            "amount": 49900,
            "currency": "inr",
            "short_url": null
        });
        let invoice = invoice_from_object(&bare).expect("bare maps");
        assert_eq!(invoice.number, None);
        assert_eq!(invoice.currency, "INR");
        assert_eq!(invoice.hosted_url, None);
    }

    #[test]
    fn not_found_detection_matches_razorpay_wording() {
        // Razorpay's two missing-id wordings (400, not 404).
        assert!(is_not_found_message(
            "razorpay api error (HTTP 400): The id provided does not exist"
        ));
        assert!(is_not_found_message(
            "The ID provided is invalid or could not be found."
        ));
        assert!(is_not_found_message("subscription not found"));
        // A different failure is not a missing id.
        assert!(!is_not_found_message(
            "razorpay api error (HTTP 400): The plan id field is required."
        ));
        assert!(!is_not_found_message(
            "razorpay api error (HTTP 401): The API key/secret provided is invalid."
        ));
    }

    #[tokio::test]
    async fn create_portal_is_unsupported_not_a_url() {
        // Razorpay has no hosted customer portal — the method
        // must fail loudly, never return a URL that does not
        // exist.
        let err = adapter()
            .create_portal(PortalRequest {
                customer_id: "cust_123".to_owned(),
                return_url: "https://app.example/billing".to_owned(),
            })
            .await
            .expect_err("no portal must fail");
        assert_eq!(
            err,
            BillingError::Provider("razorpay has no customer portal".to_owned())
        );
    }

    /// One request the mock server saw, in full: what the
    /// adapter actually put on the wire.
    struct RecordedRequest {
        method: String,
        path: String,
        authorization: Option<String>,
        content_type: Option<String>,
        body: String,
    }

    /// A hand-rolled stand-in for `api.razorpay.com` (P1-075,
    /// FR-P-03): the adapter's `post`/`get` run against it
    /// through the test-only `new_with_base_url` seam, so the
    /// request wire format (method, path, Basic auth, JSON
    /// body) and the response mapping are proven with real
    /// HTTP and no Razorpay key. No new dependency —
    /// `tokio::net::TcpListener` plus a scripted response
    /// queue. Each connection serves one request and answers
    /// `Connection: close`, so reqwest never reuses a stale
    /// socket.
    struct MockRazorpay {
        base_url: String,
        requests: tokio::sync::mpsc::UnboundedReceiver<RecordedRequest>,
        responses: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<(u16, String)>>>,
        task: tokio::task::JoinHandle<()>,
    }

    impl MockRazorpay {
        async fn start() -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("the mock binds localhost");
            let port = listener
                .local_addr()
                .expect("the mock has an address")
                .port();
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            let responses =
                std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
            let task = tokio::spawn(serve_mock(listener, tx, std::sync::Arc::clone(&responses)));
            Self {
                base_url: format!("http://127.0.0.1:{port}/v1"),
                requests: rx,
                responses,
                task,
            }
        }

        fn enqueue(&self, status: u16, body: Value) {
            self.enqueue_raw(status, &body.to_string());
        }

        fn enqueue_raw(&self, status: u16, body: &str) {
            self.responses
                .lock()
                .expect("the mock queue is not poisoned")
                .push_back((status, body.to_owned()));
        }

        /// The adapter pointed at this mock — the same
        /// validation as production, only the base URL
        /// differs.
        fn adapter(&self) -> RazorpayAdapter {
            RazorpayAdapter::new_with_base_url(
                TEST_KEY_ID,
                TEST_KEY_SECRET,
                WEBHOOK_SECRET,
                false,
                &self.base_url,
            )
            .expect("the test key is valid")
        }

        /// The next request the adapter made — the test fails
        /// instead of hanging when the adapter never calls.
        async fn take_request(&mut self) -> RecordedRequest {
            tokio::time::timeout(Duration::from_secs(10), self.requests.recv())
                .await
                .expect("the adapter must call the mock within 10 s")
                .expect("the mock server is still running")
        }

        /// No request arrived (the adapter must not have
        /// called at all).
        fn no_request(&mut self) -> bool {
            self.requests.try_recv().is_err()
        }
    }

    impl Drop for MockRazorpay {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn serve_mock(
        listener: tokio::net::TcpListener,
        requests: tokio::sync::mpsc::UnboundedSender<RecordedRequest>,
        responses: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<(u16, String)>>>,
    ) {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                return;
            };
            let requests = requests.clone();
            let responses = std::sync::Arc::clone(&responses);
            tokio::spawn(async move {
                handle_mock_connection(socket, requests, responses).await;
            });
        }
    }

    async fn handle_mock_connection(
        socket: tokio::net::TcpStream,
        requests: tokio::sync::mpsc::UnboundedSender<RecordedRequest>,
        responses: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<(u16, String)>>>,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut reader = tokio::io::BufReader::new(socket);
        let mut raw = Vec::new();
        let mut buf = [0u8; 4096];
        let head_end = loop {
            match reader.read(&mut buf).await {
                Ok(0) | Err(_) => return,
                Ok(n) => {
                    raw.extend_from_slice(&buf[..n]);
                    if let Some(end) = find_subslice(&raw, b"\r\n\r\n") {
                        break end;
                    }
                    if raw.len() > 65_536 {
                        return;
                    }
                }
            }
        };
        let head = String::from_utf8_lossy(&raw[..head_end]).into_owned();
        let mut lines = head.lines();
        let Some(request_line) = lines.next() else {
            return;
        };
        let mut parts = request_line.split_whitespace();
        let (Some(method), Some(path)) = (parts.next(), parts.next()) else {
            return;
        };
        let mut authorization = None;
        let mut content_type = None;
        let mut content_length = 0usize;
        for line in lines {
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            match name.trim().to_ascii_lowercase().as_str() {
                "authorization" => authorization = Some(value.trim().to_owned()),
                "content-type" => content_type = Some(value.trim().to_owned()),
                "content-length" => content_length = value.trim().parse().unwrap_or(0),
                _ => {}
            }
        }
        let mut body = raw[head_end + 4..].to_vec();
        while body.len() < content_length {
            match reader.read(&mut buf).await {
                Ok(0) | Err(_) => return,
                Ok(n) => body.extend_from_slice(&buf[..n]),
            }
        }
        body.truncate(content_length);
        let recorded = RecordedRequest {
            method: method.to_owned(),
            path: path.to_owned(),
            authorization,
            content_type,
            body: String::from_utf8_lossy(&body).into_owned(),
        };
        let _ = requests.send(recorded);
        let (status, response_body) = responses
            .lock()
            .expect("the mock queue is not poisoned")
            .pop_front()
            .unwrap_or_else(|| {
                (
                    500u16,
                    json!({"error": {"description": "the mock has no scripted response"}})
                        .to_string(),
                )
            });
        let reason = match status {
            200 => "OK",
            201 => "Created",
            400 => "Bad Request",
            401 => "Unauthorized",
            404 => "Not Found",
            500 => "Internal Server Error",
            _ => "Status",
        };
        let mut socket = reader.into_inner();
        let _ = socket
            .write_all(
                format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response_body}",
                    response_body.len()
                )
                .as_bytes(),
            )
            .await;
        let _ = socket.flush().await;
    }

    fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    /// `Basic base64(key_id:key_secret)` for the test key —
    /// `rzp_test_00000000000000:test_secret_00000000000000000000`
    /// — computed once here so the test pins the exact header
    /// reqwest must send (there is no base64 crate in this
    /// crate to compute it at runtime).
    const TEST_BASIC_AUTH: &str =
        "Basic cnpwX3Rlc3RfMDAwMDAwMDAwMDAwMDA6dGVzdF9zZWNyZXRfMDAwMDAwMDAwMDAwMDAwMDAwMDA=";

    /// A valid GSTIN for the passthrough tests (P1-076) —
    /// the same shape the config validator accepts.
    const TEST_GSTIN: &str = "29ABCDE1234F1Z5";

    fn checkout_request(user_id: Uuid) -> CheckoutRequest {
        CheckoutRequest {
            user_id,
            customer_id: Some("cust_123".to_owned()),
            price_id: "plan_pro".to_owned(),
            success_url: "https://app.example/success".to_owned(),
            cancel_url: "https://app.example/cancel".to_owned(),
            upgrade_from_provider_subscription_id: Some("sub_old_123".to_owned()),
            gstin: Some(TEST_GSTIN.to_owned()),
        }
    }

    fn subscription_object(status: &str) -> Value {
        json!({
            "id": "sub_123",
            "customer_id": "cust_123",
            "status": status,
            "plan_id": "plan_pro",
            "current_end": 1794844800,
        })
    }

    #[tokio::test]
    async fn mock_checkout_posts_a_subscription_create_and_returns_the_short_url() {
        let mut mock = MockRazorpay::start().await;
        mock.enqueue(
            200,
            json!({
                "id": "sub_00000000000001",
                "entity": "subscription",
                "plan_id": "plan_pro",
                "status": "authenticated",
                "short_url": "https://rzp.io/i/PWtAiEo",
            }),
        );
        let user_id = Uuid::now_v7();
        let session = mock
            .adapter()
            .create_checkout(checkout_request(user_id))
            .await
            .expect("the mock checkout succeeds");
        assert_eq!(session.id, "sub_00000000000001");
        assert_eq!(session.url, "https://rzp.io/i/PWtAiEo");
        let req = mock.take_request().await;
        assert_eq!(req.method, "POST");
        assert_eq!(req.path, "/v1/subscriptions");
        assert_eq!(req.authorization.as_deref(), Some(TEST_BASIC_AUTH));
        assert_eq!(
            req.content_type.as_deref(),
            Some("application/json"),
            "checkout posts JSON"
        );
        let body: Value = serde_json::from_str(&req.body).expect("the request body is JSON");
        assert_eq!(body["plan_id"], "plan_pro");
        assert_eq!(body["total_count"], TOTAL_COUNT_MAX);
        assert_eq!(body["quantity"], 1);
        assert_eq!(body["customer_notify"], true);
        assert_eq!(body["customer_id"], "cust_123");
        assert_eq!(body["notes"]["detag_user_id"], user_id.to_string());
        assert_eq!(body["notes"]["detag_upgrade_from"], "sub_old_123");
        assert_eq!(body["notes"]["detag_gstin"], TEST_GSTIN);
    }

    #[tokio::test]
    async fn mock_checkout_without_a_customer_sends_no_customer_id() {
        let mut mock = MockRazorpay::start().await;
        mock.enqueue(
            200,
            json!({"id": "sub_1", "short_url": "https://rzp.io/i/abc"}),
        );
        let mut req_body = checkout_request(Uuid::now_v7());
        req_body.customer_id = None;
        req_body.upgrade_from_provider_subscription_id = None;
        mock.adapter()
            .create_checkout(req_body)
            .await
            .expect("the mock checkout succeeds");
        let req = mock.take_request().await;
        let body: Value = serde_json::from_str(&req.body).expect("the request body is JSON");
        assert!(body.get("customer_id").is_none());
        assert!(body["notes"].get("detag_upgrade_from").is_none());
    }

    #[tokio::test]
    async fn mock_checkout_bad_plan_id_is_an_invalid_price() {
        let mut mock = MockRazorpay::start().await;
        mock.enqueue(
            400,
            json!({"error": {
                "code": "BAD_REQUEST_ERROR",
                "description": "The plan id provided does not exist",
                "field": "plan_id",
            }}),
        );
        let err = mock
            .adapter()
            .create_checkout(checkout_request(Uuid::now_v7()))
            .await
            .expect_err("a bad plan id must fail");
        assert_eq!(err, BillingError::InvalidPrice("plan_pro".to_owned()));
        let req = mock.take_request().await;
        assert_eq!(req.method, "POST");
        assert_eq!(req.path, "/v1/subscriptions");
    }

    #[tokio::test]
    async fn mock_checkout_empty_price_id_never_calls_the_server() {
        let mut mock = MockRazorpay::start().await;
        let mut req_body = checkout_request(Uuid::now_v7());
        req_body.price_id = String::new();
        let err = mock
            .adapter()
            .create_checkout(req_body)
            .await
            .expect_err("an empty price id must fail");
        assert_eq!(err, BillingError::InvalidPrice(String::new()));
        assert!(
            mock.no_request(),
            "the adapter must validate before any HTTP call"
        );
    }

    #[tokio::test]
    async fn mock_checkout_non_json_success_is_a_provider_error() {
        let mock = MockRazorpay::start().await;
        mock.enqueue_raw(200, "this is not json");
        let err = mock
            .adapter()
            .create_checkout(checkout_request(Uuid::now_v7()))
            .await
            .expect_err("a non-JSON success must fail");
        assert!(
            matches!(err, BillingError::Provider(_)),
            "non-JSON success gave {err:?}"
        );
        assert!(err.to_string().contains("not JSON"));
    }

    #[tokio::test]
    async fn mock_fetch_subscription_maps_every_status() {
        // The whole mapping table, through the real HTTP leg:
        // each Razorpay status off the wire lands on the
        // domain state the state machine stores.
        for (wire, expected) in [
            ("active", SubscriptionStatus::Active),
            ("authenticated", SubscriptionStatus::Trialing),
            ("pending", SubscriptionStatus::PastDue),
            ("halted", SubscriptionStatus::PastDue),
            ("cancelled", SubscriptionStatus::Canceled),
            ("completed", SubscriptionStatus::Canceled),
            ("expired", SubscriptionStatus::Canceled),
        ] {
            let mut mock = MockRazorpay::start().await;
            mock.enqueue(200, subscription_object(wire));
            let state = mock
                .adapter()
                .fetch_subscription("sub_123")
                .await
                .expect("the mock fetch succeeds");
            assert_eq!(state.provider_subscription_id, "sub_123");
            assert_eq!(state.status, expected, "wire status {wire}");
            assert_eq!(
                state.current_period_end,
                Some(DateTime::from_timestamp(1794844800, 0).expect("in range"))
            );
            assert_eq!(state.price_ids, vec!["plan_pro".to_owned()]);
            assert!(!state.cancel_at_period_end);
            let req = mock.take_request().await;
            assert_eq!(req.method, "GET");
            assert_eq!(req.path, "/v1/subscriptions/sub_123");
            assert_eq!(req.authorization.as_deref(), Some(TEST_BASIC_AUTH));
        }
        // Fail-closed: a status our machine cannot store is
        // never read as a grant.
        for wire in ["created", "paused"] {
            let mock = MockRazorpay::start().await;
            mock.enqueue(200, subscription_object(wire));
            let err = mock
                .adapter()
                .fetch_subscription("sub_123")
                .await
                .expect_err("an unmappable status must fail");
            assert_eq!(err, BillingError::UnknownStatus(wire.to_owned()));
        }
    }

    #[tokio::test]
    async fn mock_fetch_subscription_unknown_id_is_typed() {
        let mock = MockRazorpay::start().await;
        mock.enqueue(
            400,
            json!({"error": {"description": "The id provided does not exist"}}),
        );
        let err = mock
            .adapter()
            .fetch_subscription("sub_nope")
            .await
            .expect_err("a missing id must fail");
        assert_eq!(
            err,
            BillingError::UnknownSubscription("sub_nope".to_owned())
        );
        // A different failure is a plain provider error, not
        // a typed unknown id.
        let mock = MockRazorpay::start().await;
        mock.enqueue(
            401,
            json!({"error": {"description": "The API key/secret provided is invalid."}}),
        );
        let err = mock
            .adapter()
            .fetch_subscription("sub_123")
            .await
            .expect_err("an auth failure must fail");
        assert!(
            matches!(err, BillingError::Provider(_)),
            "auth failure gave {err:?}"
        );
        assert!(err.to_string().contains("invalid"));
    }

    #[tokio::test]
    async fn mock_list_invoices_filters_another_customers_invoices() {
        let mut mock = MockRazorpay::start().await;
        mock.enqueue(
            200,
            json!({"items": [
                {
                    "id": "inv_1",
                    "invoice_number": "#0961",
                    "customer_id": "cust_1",
                    "status": "paid",
                    "date": 1566891149,
                    "amount": 600,
                    "currency": "INR",
                    "short_url": "https://rzp.io/i/K8Zg72C",
                },
                {
                    "id": "inv_other",
                    "invoice_number": "#0962",
                    "customer_id": "cust_2",
                    "status": "paid",
                    "date": 1566891150,
                    "amount": 99900,
                    "currency": "INR",
                    "short_url": "https://rzp.io/i/other",
                },
                {
                    "id": "inv_2",
                    "invoice_number": null,
                    "customer_id": "cust_1",
                    "status": "issued",
                    "date": 1566891149,
                    "amount": 49900,
                    "currency": "inr",
                    "short_url": null,
                },
            ]}),
        );
        let invoices = mock
            .adapter()
            .list_invoices("cust_1", 5)
            .await
            .expect("the mock list succeeds");
        assert_eq!(
            invoices.len(),
            2,
            "the other customer's invoice is filtered"
        );
        assert_eq!(invoices[0].id, "inv_1");
        assert_eq!(invoices[0].number.as_deref(), Some("#0961"));
        assert_eq!(invoices[0].amount, 600);
        assert_eq!(invoices[0].currency, "INR");
        assert_eq!(invoices[0].status, "paid");
        assert_eq!(
            invoices[0].hosted_url.as_deref(),
            Some("https://rzp.io/i/K8Zg72C")
        );
        assert_eq!(invoices[0].download_url, None);
        assert_eq!(invoices[1].id, "inv_2");
        assert_eq!(invoices[1].currency, "INR", "the currency is uppercased");
        assert_eq!(invoices[1].number, None);
        assert_eq!(invoices[1].hosted_url, None);
        let req = mock.take_request().await;
        assert_eq!(req.method, "GET");
        assert_eq!(req.path, "/v1/invoices?customer_id=cust_1&count=5");
        assert_eq!(req.authorization.as_deref(), Some(TEST_BASIC_AUTH));
    }

    #[tokio::test]
    async fn mock_list_invoices_without_items_is_a_provider_error() {
        let mock = MockRazorpay::start().await;
        mock.enqueue(200, json!({"count": 0}));
        let err = mock
            .adapter()
            .list_invoices("cust_1", 5)
            .await
            .expect_err("a list without items must fail");
        assert!(
            matches!(err, BillingError::Provider(_)),
            "no-items list gave {err:?}"
        );
    }

    #[tokio::test]
    async fn mock_create_customer_posts_name_email_and_notes() {
        let mut mock = MockRazorpay::start().await;
        mock.enqueue(
            200,
            json!({"id": "cust_123", "email": "first+last@example.com"}),
        );
        let user_id = Uuid::now_v7();
        let customer = mock
            .adapter()
            .create_customer(user_id, "first+last@example.com")
            .await
            .expect("the mock create succeeds");
        assert_eq!(customer.provider_customer_id, "cust_123");
        assert_eq!(customer.email, "first+last@example.com");
        let req = mock.take_request().await;
        assert_eq!(req.method, "POST");
        assert_eq!(req.path, "/v1/customers");
        assert_eq!(req.authorization.as_deref(), Some(TEST_BASIC_AUTH));
        let body: Value = serde_json::from_str(&req.body).expect("the request body is JSON");
        assert_eq!(body["name"], "first last");
        assert_eq!(body["email"], "first+last@example.com");
        assert_eq!(body["notes"]["detag_user_id"], user_id.to_string());
    }

    #[tokio::test]
    async fn mock_create_customer_falls_back_to_the_requested_email() {
        let mock = MockRazorpay::start().await;
        mock.enqueue(200, json!({"id": "cust_123"}));
        let customer = mock
            .adapter()
            .create_customer(Uuid::now_v7(), "plain@example.com")
            .await
            .expect("the mock create succeeds");
        assert_eq!(customer.email, "plain@example.com");
    }

    #[tokio::test]
    async fn mock_fetch_customer_returns_the_customer_and_unknown_is_typed() {
        let mut mock = MockRazorpay::start().await;
        mock.enqueue(200, json!({"id": "cust_123", "email": "buyer@example.com"}));
        let customer = mock
            .adapter()
            .fetch_customer("cust_123")
            .await
            .expect("the mock fetch succeeds");
        assert_eq!(customer.provider_customer_id, "cust_123");
        assert_eq!(customer.email, "buyer@example.com");
        let req = mock.take_request().await;
        assert_eq!(req.method, "GET");
        assert_eq!(req.path, "/v1/customers/cust_123");
        let mock = MockRazorpay::start().await;
        mock.enqueue(
            400,
            json!({"error": {"description": "The ID provided is invalid or could not be found."}}),
        );
        let err = mock
            .adapter()
            .fetch_customer("cust_nope")
            .await
            .expect_err("a missing customer must fail");
        assert_eq!(err, BillingError::UnknownCustomer("cust_nope".to_owned()));
    }

    #[tokio::test]
    async fn mock_repeated_fetch_is_deterministic() {
        // The adapter's reads are side-effect-free: the same
        // subscription fetched twice returns the same state.
        // Write-side idempotency is the webhook ledger's job
        // (P1-074), not the adapter's — the adapter sends no
        // idempotency key of its own, and these GETs carry
        // none.
        let mut mock = MockRazorpay::start().await;
        mock.enqueue(200, subscription_object("active"));
        mock.enqueue(200, subscription_object("active"));
        let adapter = mock.adapter();
        let first = adapter
            .fetch_subscription("sub_123")
            .await
            .expect("the first fetch succeeds");
        let second = adapter
            .fetch_subscription("sub_123")
            .await
            .expect("the second fetch succeeds");
        assert_eq!(first.status, second.status);
        assert_eq!(
            first.provider_subscription_id,
            second.provider_subscription_id
        );
        assert_eq!(first.current_period_end, second.current_period_end);
        assert_eq!(first.price_ids, second.price_ids);
        let first_req = mock.take_request().await;
        let second_req = mock.take_request().await;
        assert_eq!(first_req.path, second_req.path);
        assert_eq!(first_req.method, "GET");
        assert_eq!(second_req.method, "GET");
    }
}
