# Provider parity — Stripe vs Razorpay (P1-077)

> Status: current as of P1-077 (after P1-073–P1-076). Stripe is the
> default provider; Razorpay is behind `PF_FEATURE_RAZORPAY` (OFF by
> default) and is a SINGLE active provider — enabling it makes Razorpay
> THE provider (ADR-0038 §7). Neither provider has had a live run on
> this VM yet: no test-mode key exists here, so every leg below is
> proven by unit shape, mock server, or the fake provider — never by a
> real provider call (ASSUMPTION-023/035).

## How to read this doc

- **PARITY** — the capability works on both providers through the same
  `BillingProvider` trait leg, and the suite proves it.
- **PARTIAL** — the leg exists on both but behaves differently in a way
  a caller can observe.
- **STUBBED** — the trait leg exists but the Razorpay side refuses or
  does nothing useful; the endpoint's answer is documented, not hidden.
- **NOT BUILT** — no leg, no endpoint behaviour; a follow-up owns it.

## Capability table

| Capability | Stripe | Razorpay | Verdict |
| --- | --- | --- | --- |
| Checkout (`POST /v1/billing/checkout`) | Subscription-mode Checkout Session (`{id, url}`); `subscription_data.metadata` carries `detag_upgrade_from` + `detag_gstin` (P1-069, P1-076); success/cancel URLs are the console contract (P1-062) | Subscription create (`POST /v1/subscriptions`, `plan_id` = `razorpay_plan_id`, `total_count` 1200, `quantity` 1, `customer_notify` true, `notes` carry `detag_user_id` / `detag_upgrade_from` / `detag_gstin`); returns `{id: sub_…, url: short_url}` — the authorization link (P1-073, P1-076) | **PARTIAL** — same endpoint and same `{id, url}` shape, but Razorpay has no merchant redirect: `success_url`/`cancel_url` are unused, there is no Checkout Session, and the buyer authorizes a mandate, not a one-off session |
| Customer portal (`POST /v1/billing/portal`) | Hosted portal session; return URL `{PF_PUBLIC_BASE_URL}/billing` (P1-067) | **No hosted portal exists** — `create_portal` returns `BillingError::Provider` ("razorpay has no customer portal") and the endpoint answers 500 while Razorpay is active (ADR-0038 §9, P1-073) | **STUBBED** — a Razorpay subscriber manages payment methods and cancels through support (P1-071); there is no self-serve portal until a portal-side flow exists |
| Subscription fetch (`fetch_subscription`) | Full status + period end + price ids (P1-060) | Full status table mapped off the wire (`active`→Active, `authenticated`→Trialing, `pending`/`halted`→PastDue, `cancelled`/`completed`/`expired`→Canceled, `created`/`paused`→`UnknownStatus` fail-closed, P1-073) | **PARITY** |
| Invoices list (`GET /v1/billing/invoices`) | `hosted_url` = `hosted_invoice_url`, `download_url` = `invoice_pdf` (P1-060, P1-068) | `hosted_url` = `short_url`, `download_url` = `None` — a Razorpay invoice has a hosted page, no PDF file (P1-073). Items filtered client-side by `customer_id` as a backstop (the documented filter is `subscription_id`, P1-073) | **PARTIAL** — same shape, but no PDF download on Razorpay |
| Customer create / fetch | Customer create + fetch (P1-060) | Customer create (name derived from the email local part, 3–50 chars, fallback "Detag Customer") + fetch (P1-073) | **PARITY** |
| Webhook receiver (`POST /v1/webhooks/{stripe,razorpay}`) | HMAC-SHA256 via the Stripe SDK, 5-min tolerance; ledger `UNIQUE (provider, event_id)`; dispatch seam (P1-063, P1-065) | HMAC-SHA256 (`hex(hmac_sha256(webhook_secret, raw_body))`, constant-time, no timestamp/tolerance — Razorpay signs the body alone); ledger key is the SHA-256 hex of the raw body (Razorpay sends no event id); same `store_and_dispatch` flow (P1-074) | **PARITY** |
| Webhook event mapping | `checkout.session.completed` creates the row; status/invoice events through the state machine (P1-065) | `subscription.authenticated` → `CheckoutCompleted`; `activated`/`updated`/`resumed`/`pending`/`halted` → `SubscriptionStatusChanged`; `cancelled`/`completed`/`expired` → `SubscriptionDeleted`; `charged`/`paused`/payment/invoice events → `None` (P1-073, P1-074) | **PARITY** (mapping tables differ by provider shape, as they must) |
| Plan linking | `plans.stripe_price_id` (P1-060) | `plans.razorpay_plan_id` (migration `20261118182355`, P1-073); webhook resolves the plan trying `by_stripe_price_id` then `by_razorpay_plan_id` (P1-074) | **PARITY** — except the admin plans editor does NOT expose `razorpay_plan_id` yet (set via SQL; follow-up, ADR-0038 §6) |
| Catalog sync (`POST /v1/admin/billing/catalog-sync`) | `CatalogProvider` implemented; drift report as JSON (P1-061) | **Not implemented** — `CatalogProvider` is Stripe-only; a Razorpay plan is created in the Razorpay dashboard and its id recorded in the catalog (ADR-0038 §6) | **STUBBED** |
| Proration (upgrade now / downgrade at period end) | Preview math + scheduled change + webhook applier (P1-069) | Same code path — proration is provider-agnostic; the checkout writes `detag_upgrade_from` to Razorpay `notes` the same way it writes Stripe metadata (P1-069, P1-073) | **PARITY** — with the same two honest gaps on both providers (below) |
| Dunning (day 3/7 reminders, day-14 cancel) | State machine + `past_due_since` + guarded steps (P1-064, P1-070) | Same code path — dunning runs off the shared state machine and the `pending`/`halted` → PastDue mapping (P1-070, P1-073) | **PARITY** |
| GSTIN passthrough | `subscription_data.metadata` `detag_gstin` (P1-076) | Subscription `notes` `detag_gstin` (P1-076) | **PARITY** |
| Test-mode enforcement | Only `sk_test_…` at construction; `sk_live_…` only with the owner flag (P1-060) | Only `rzp_test_…` at construction; `rzp_live_…` only with the owner flag (P1-073) | **PARITY** |
| Flag-off behaviour | n/a (Stripe is the default) | Flag off → `POST /v1/webhooks/razorpay` 404s; flag on with no provider → 503 `PF-5005` (P1-073, P1-074) | **PARITY** (by construction — the flag exists so Razorpay can be off) |

## What is proven, and how (evidence)

| Leg | Stripe evidence | Razorpay evidence |
| --- | --- | --- |
| HTTP legs (checkout / fetch / invoices / customer) | Unit shape only (P1-060) — the calls are built and parsed, but no test exercises the real `reqwest` call against anything | Mock-server suite (P1-075): 13 tests against a hand-rolled localhost server asserting method, path, Basic auth, and JSON body off the wire |
| Webhook signature | Real HMAC path end to end with a fake `sk_test_` key + test secret (P1-063) | Real HMAC path end to end with the real adapter + a delegating `fetch_subscription` double (P1-074) |
| Idempotency / replay | Ledger `UNIQUE (provider, event_id)`, replay is a no-op (P1-063) | Same ledger; Razorpay replay proven (one row, unmoved stamp, one subscription row, P1-074) |
| First live run | **Not done** — no test-mode key on this VM (ASSUMPTION-023) | **Not done** — no test-mode key on this VM (ASSUMPTION-035) |

The asymmetry is honest and recorded, not hidden: Razorpay's HTTP legs
are better proven than Stripe's (mock suite vs unit shape alone), and
Stripe's mock-server equivalent was never a ticket. The first live run
for EITHER provider is still a flag-on checkout with a test-mode key.

## Stubbed / not built (the gap list)

1. **No Razorpay portal** (STUBBED, ADR-0038 §9) — `POST /v1/billing/portal`
   500s while Razorpay is active. Mitigation today: support cancel
   (P1-071) + the Razorpay dashboard. A portal-side flow is a follow-up.
2. **No Razorpay catalog sync** (STUBBED, ADR-0038 §6) — plan ids are
   recorded by hand (SQL) after creating the plan in the Razorpay
   dashboard. The admin plans editor does not expose `razorpay_plan_id`.
3. **No provider-side cancel** (NOT BUILT, ASSUMPTION-034 follow-up, both
   providers) — support cancel moves OUR row only; the provider
   subscription must also be canceled in the dashboard, or billing
   continues. Same gap on Stripe and Razorpay.
4. **No provider-side price switch on downgrade** (NOT BUILT,
   ASSUMPTION-032 follow-up, both providers) — the entitlement flips on
   time; the provider keeps billing the old price past the switch until
   a subscription schedule lands.
5. **Upgrade supersede not wired** (NOT BUILT, ASSUMPTION-032 follow-up,
   both providers) — checkout writes `detag_upgrade_from` (Stripe
   metadata / Razorpay notes) but the webhook does not read it yet; the
   old row stays live-but-stale and newest-live-wins (P1-049) carries
   the entitlement.
6. **Single active provider only** (NOT BUILT, ADR-0038 §7) — enabling
   Razorpay disables Stripe sales and the Stripe webhook fails
   signature (400) while it is on. Dual-provider (both webhooks live)
   needs a second state field.
7. **Buyer's GSTIN not collected** (NOT BUILT, ASSUMPTION-038 follow-up,
   both providers) — only Detag's own GSTIN rides the checkout.
8. **Razorpay success/cancel URLs unused** (PARTIAL, ADR-0038 §3) — a
   Razorpay subscription carries no merchant redirect; the console
   success/cancel pages (Phase 4) have nothing to land on for Razorpay
   until a return flow is designed.

## Launch sequencing (summary — ADR-0042 owns the decision)

1. Stripe stays the default provider; the Razorpay flag stays OFF.
2. First live run: a flag-on checkout with a **test-mode** key, proving
   the real wire for Razorpay (and, separately, for Stripe).
3. P1-078 (enable runbook + owner checklist) lands before any enable.
4. No live key until owner decision #3 (live payment keys) is answered.
5. The portal gap (#1 above) must be accepted by the owner or closed
   before Razorpay takes real subscribers — there is no self-serve
   cancel/payment-method flow on Razorpay today.
6. The money-impact gaps (#3, #4, #5) must be closed or explicitly
   accepted before Razorpay takes real subscribers — they are the same
   gaps Stripe already carries, so they do not block a test-mode pilot.

## Related docs

- Pricing config — `10-pricing-config.md` (portal section: the 500
  under Razorpay; refund & cancel policy)
- ADRs — ADR-0038 (Razorpay adapter), ADR-0039 (webhook receiver),
  ADR-0040 (mock suite), ADR-0041 (GSTIN), ADR-0042 (launch sequencing)
- Assumptions — ASSUMPTION-023 (Stripe honesty), ASSUMPTION-035
  (Razorpay honesty), ASSUMPTION-032 (proration follow-ups),
  ASSUMPTION-034 (cancel follow-up), ASSUMPTION-038 (GSTIN),
  ASSUMPTION-039 (this doc's verdicts)
- Test strategy — `docs/05-QUALITY/01-test-strategy.md` (P1-073/074/075
  sections: what proves each leg)
