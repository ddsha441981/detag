# Pricing config (P1-058)

Where the prices live, how to change them safely, and what a change does —
and does not — do. Open question #2
(`02-REQUIREMENTS/06-open-questions.md`): **Free ₹0 / Pro ₹499/mo /
Business ₹1,999/mo** (USD mirror 0/19/79), changeable in config + Stripe
dashboard. The owner owns that row; this page is the procedure for
changing it.

## The prices today (FR-P-01)

| Plan | Amount | Currency | Daily images | Video-seconds/day | Max bytes | Result TTL | Concurrency | Lane | Features |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| free | 0 | INR | 10 | 0 | 25 MiB | 6 h | 1 | standard | — |
| pro | 49,900 | INR | 500 | 3,600 | 1 GiB | 24 h | 3 | standard | api_keys, webhooks |
| business | 199,900 | INR | 5,000 | 18,000 | 4 GiB | 72 h | 10 | priority | api_keys, webhooks, batch |

Amounts are in the smallest currency unit (paise), stored as `plans.amount`
(`INTEGER`, `>= 0`). `plans.currency` is three uppercase letters.
`plans.code` is `CHECK (free|pro|business)` — there are exactly three
plans, and no plan is ever created at runtime (ASSUMPTION-022). The seven
entitlement keys are `daily_images`, `daily_video_seconds`, `max_bytes`,
`result_ttl_hours`, `concurrency`, `lane`, `features` — the full object is
required wherever entitlements are written.

**USD mirror: not implemented.** Open question #2 names a USD mirror
(0/19/79), but no USD price, no second currency, and no currency switch
exists anywhere in the code today — the seed, the catalog, and every test
pin INR only. A USD offering is a future change (new currency rows or a
per-currency price table), not a config flip. Do not quote USD prices to
customers until that lands.

## Where the defaults live

Two files, kept in sync by hand — both hold the same three rows, verbatim:

1. `migrations/20261112065140_billing.up.sql` — the seed `INSERT` for the
   `plans` table. Migrations are immutable history: never edit this file
   to change a price. It is the record of what a fresh database got on
   day one.
2. `scripts/seed.sql` — the live idempotent operation (`just seed`). Every
   `INSERT` is `ON CONFLICT (code) DO NOTHING`, so re-running the seed
   never clobbers an operator or admin edit. This file is the one to edit
   when the *default* for fresh installs changes.

The seed is run by `just seed` (after `just migrate`) and by `just dev`.
`crates/core/src/seed.rs` runs `scripts/seed.sql` and reports the row
counts. The plan catalog itself (`crates/core/src/plans.rs`,
`PlanCatalog` over `PlanStore`/`PlanCache`) reads the `plans` table, not
the seed files — the files only matter at install time.

## The Stripe side

- `plans.stripe_price_id` is `NULL` until a live price is configured.
  It is stored, validated (`price_` + alphanumerics), and shown by
  `GET /v1/admin/plans`. Since P1-061 the catalog sync writes it:
  the nightly sync (P1-082's schedule) — or `POST
  /v1/admin/billing/catalog-sync` now — creates the Stripe Product
  + recurring monthly Price for each paid plan and stores the id.
  Checkout reads it from P1-062. Setting it by hand (below) is
  still allowed, but the sync is the normal path: a hand-set id
  the sync finds drifted (amount/currency changed, or the Price
  archived in Stripe) is replaced, and the old Price archived.
- Keys live in env, never in the repo: `PF_STRIPE_SECRET_KEY`,
  `PF_STRIPE_WEBHOOK_SECRET` (test placeholders in `.env.example`;
  see `docs/06-DELIVERY/04-config-reference.md`). P1-060 enforces
  test mode — a live key is rejected without an owner flag — so a
  `sk_live_` key in dev is a refusal, not a charge.
- The Stripe dashboard is the second half of every price change: the
  Price object must exist in Stripe (same amount, same currency) before
  its id is useful. Create the Price first, copy the `price_...` id,
  then attach it (below). An id that names no Stripe Price will fail at
  checkout time, not at save time — the editor validates the shape,
  not the existence.

## The Razorpay side (P1-073)

- `plans.razorpay_plan_id` is `NULL` until a Razorpay plan is linked —
  the mirror of `stripe_price_id` for the Razorpay provider
  (ADR-0038). There is NO catalog sync for Razorpay: the plan is
  created in the Razorpay dashboard (Subscriptions must be enabled
  in test AND live mode, or the create 400s), then linked by hand:
  `UPDATE plans SET razorpay_plan_id = 'plan_...' WHERE code = 'pro';`
  The admin plans editor does not expose the column yet (follow-up) —
  SQL is the only path today, and the seed leaves it `NULL` like
  `stripe_price_id`. A plan with no Razorpay plan id is 422
  `not_purchasable` at checkout while Razorpay is the active
  provider — the same "no price configured yet" answer Stripe
  gets, never a provider call with a wrong-provider id.
- Keys live in env, never in the repo: `PF_RAZORPAY_KEY`,
  `PF_RAZORPAY_SECRET`, `PF_RAZORPAY_WEBHOOK_SECRET` (test
  placeholders in `.env.example`; see
  `docs/06-DELIVERY/04-config-reference.md`). The webhook secret
  is the THIRD credential — set in the dashboard's webhook
  settings, never the API key secret. P1-073 enforces test mode —
  a live key is rejected without an owner flag (owner decision
  #3, still pending) — so a `rzp_live_` key in dev is a refusal,
  not a charge. Enabling the flag (`PF_FEATURE_RAZORPAY=true`)
  makes Razorpay THE provider: Stripe prices are not sold and
  the Stripe webhook fails signature while it is on (one active
  provider — ADR-0038).
- The Razorpay dashboard is the second half of every price change:
  the Plan object must exist in Razorpay (same amount, same
  currency, monthly) before its id is useful. Create the Plan
  first, copy the `plan_...` id, then attach it (above). An id
  that names no Razorpay Plan will fail at checkout time, not at
  link time — the link is a plain SQL write, it validates
  nothing.

## How to change a price — the three paths

### 1. Runtime change (the normal path): the admin plan editor

`PUT /v1/admin/plans` (admin only; documented in
`docs/06-DELIVERY/07-admin-guide.md` and the api-spec admin-plans row).
Batch edit — per-plan updates plus a `remove` list — applied in one
transaction, cache invalidated after the commit, audited once as
`admin.plans.updated` with the changed codes in `after`. No deploy, no
restart: the profile, the quota pre-check, and the feature gate read the
catalog, so they see the edit at once (the 5-minute `PLAN_CACHE_TTL` is
invalidated by the handler, not waited out).

Rules the editor enforces (all 422 unless noted):

- Free stays ₹0 and takes no Stripe price.
- A plan cannot be removed while any subscription references it — live
  or historical (`plan_id` is `ON DELETE RESTRICT`) — 409 `PF-2012`.
  Free can never be removed.
- `entitlements`, when present, is the complete seven-key object.
- `amount` is paise `>= 0`; `currency` three uppercase letters; a Stripe
  price id is `price_` + alphanumerics. Unknown codes are 404 `PF-2010`.
- No plan creation: an unknown code in the update list is a 404, not a
  new plan. New plans are a seed/migration change (path 2).

### 2. Default change (fresh installs): seed + migration

To change what a *new* database gets:

1. Edit `scripts/seed.sql` (the live default).
2. Add a **new** migration that `UPDATE`s the existing rows to the new
   values — never edit the old migration. Re-seeding alone does not
   move existing deployments: `ON CONFLICT DO NOTHING` leaves existing
   rows untouched by design, so a price change that only touches the
   seed reaches new installs and nobody else.
3. Keep the two files in agreement on values: the seed is the default,
   the migration is the rollout. Both carry the FR-P-01 amounts in
   paise.

### 3. Stripe dashboard change

The normal path since P1-061 is the sync, not the dashboard:
change the amount in the catalog (path 1), then run the sync —
`POST /v1/admin/billing/catalog-sync`, or wait for the nightly
run (P1-082) — and the sync creates the new Price, archives the
old one, and stores the new id, returning a drift report that
names every action. The manual dashboard path still works:

1. In the Stripe dashboard, create the new Price (same currency as the
   plan row; the amount must match what the plan will charge).
2. Attach its id to the plan — via the editor (path 1) for a live
   system, or via the seed (path 2) for fresh installs.
3. Verify: `GET /v1/admin/plans` shows the id; the next checkout
   buys that Price. (The next sync run adopts the hand-created
   Price when its amount + currency already match — it does not
   create a duplicate.)

Order matters: Stripe Price first, attach second. The editor cannot
check that the id exists in Stripe.

## What a change does — and does not — do

- **Does:** change what the catalog reports (`GET /v1/admin/plans`,
  `GET /v1/plans`, `/v1/me` quota snapshot), what the quota pre-check
  enforces for new job creates, and what new checkouts will buy once
  checkout lands (P1-062). The change is visible immediately — no cache wait.
- **Does not:** re-price existing Stripe subscriptions. What a subscriber
  is billed is driven by the Stripe Price object their subscription
  holds, not by the catalog row. Changing the catalog amount alone
  changes the display and the next checkout, not the current bill.
  Re-pricing existing subscribers is a Stripe-side operation (price
  change on the subscription), not a catalog edit — and there is no
  proration logic in the app today.
- **Does not:** change the entitlement limits unless the entitlements
  object is edited too. Price and limits are separate fields; a price
  rise with unchanged limits is a valid edit.
- **Rollback:** the editor is transactional (a blocked removal rolls
  the whole PUT back) and audited, so a bad change is reverted with a
  second PUT carrying the old values — the audit row is the record of
  what changed. There is no undo button; keep the previous values from
  the `admin.plans.updated` audit row (or from the seed) before editing.

## Rollout checklist (any price change)

1. Owner sign-off on the new amount (open question #2 is OWNER-owned).
2. Stripe Price created in the right mode (test first; live only with
   the owner flag, P1-060) and the id copied exactly.
3. Apply via the editor (live systems) and/or seed + migration (fresh
   installs) — both halves if both apply.
4. Verify: `GET /v1/admin/plans` shows the new amount + price id;
   `GET /v1/me` on a test user of each plan shows the unchanged
   limits (or the intended new ones).
5. Check the console pricing surface when it exists (Phase 4): the
   `upgrade_url` in quota denials points at `{PF_PUBLIC_BASE_URL}/plans`
   (ASSUMPTION-020), which is not a real page until Phase 4 — a price
   change today does not reach any customer-facing page.
6. Announce: existing subscribers are unaffected (see above) — say so
   explicitly in the announcement, or the support queue will ask.

## The billing portal (P1-067)

- `POST /v1/billing/portal` returns the provider-hosted portal
  URL for the caller's own customer (see the api-spec billing
  row). The portal is where a subscriber manages payment
  methods, sees invoices, and cancels — no code of ours runs
  inside it; subscription changes made there land back with
  the webhook (P1-063/065).
- The portal's configuration is Stripe-dashboard-side, not
  code: the adapter passes `configuration: None` when it
  creates the session, so the account's **default portal
  configuration** applies. That configuration is where the
  product decisions live:
  - **Invoice display** — which invoices the customer sees
    (all, or only paid/open) and whether the hosted invoice
    page / PDF download is offered.
  - **Cancel behavior** — whether the customer may cancel in
    the portal at all, and whether cancellation is immediate
    or at period end. Our state machine treats `canceled` as
    terminal either way (P1-064); the timing only decides
    when the `customer.subscription.deleted` event arrives.
  - **Payment-method updates** — whether the customer may
    add/remove cards inside the portal.
- Change it in the Stripe dashboard (Settings → Billing →
  Customer portal) — no deploy, no migration, no code
  change. The default configuration is in force until an
  owner changes it; if the default ever stops matching the
  product (e.g. cancel must be period-end-only), that is an
  owner decision, not a ticket — see ASSUMPTION-030.
- Razorpay has NO hosted customer portal (P1-073): while
  Razorpay is the active provider, `POST /v1/billing/portal`
  answers 500 — there is no portal URL to return. A Razorpay
  subscriber manages payment methods and cancels through
  support (P1-071) until a portal-side flow exists; the
  portal section above is Stripe-only. P1-077 lists the
  parity gap.

## Upgrade/downgrade proration (P1-069)

- An upgrade is effective **now** and a downgrade at the
  **period end** — the entitlement switch timing, not the
  money timing. The preview on an upgrade (`credit` /
  `charge` / `net`, paise) is our computation for display:
  each leg is the plan amount times the unused fraction of
  the current period (whole seconds, truncated toward zero
  — at most one paise per leg). The period start is derived
  as one calendar month before the period end (all catalog
  plans are monthly — ASSUMPTION-032); Stripe's invoice is
  authoritative, not our preview.
- A downgrade charges nothing now (all proration legs zero)
  and makes no provider call: the promise is the
  `scheduled_plan_changes` row, applied by the webhook
  handler when the provider's period rolls. One pending
  change per subscription — a second downgrade while one is
  pending is 409. A downgrade to Free cancels the row at
  the period end (there is no Free subscription row) and the
  user falls back to Free.
- Two honest gaps, both follow-ups (ASSUMPTION-032): the
  provider-side price switch for a downgrade is not built
  yet (Stripe keeps billing the old price past the switch
  until a subscription-schedule lands), and an upgrade
  leaves the old subscription row live-but-stale until the
  webhook supersede lands (the checkout already writes
  `detag_upgrade_from` metadata for it; the newest-live-wins
  rule makes the upgrade effective meanwhile).

## Refund & cancel policy (P1-071)

The ToS policy snippet — one const in
`apps/api/src/services/support_cancel.rs`
(`REFUND_CANCEL_POLICY`), returned as `policy` on every
support-cancel response and quoted verbatim here:

> Cancelling ends the subscription immediately: paid access stops at once. No prorated refund is issued automatically — refunds are at support's discretion and are issued in the Stripe dashboard. This endpoint stops access; it does not cancel the Stripe subscription or move money — cancel it in the dashboard too, so billing stops.

- **Support cancel** — `POST /v1/admin/billing/subscriptions/{id}/cancel` (admin only; support stays read-only, so support files the request and an admin executes it). The row moves to `canceled` NOW (not at period end like a P1-069 downgrade-to-Free); a pending scheduled downgrade is mooted; the `subscription-canceled` mail goes out; the call is audited as `billing.subscription.status_changed` with the admin as actor and the support note as `reason`.
- **Refunds are dashboard-only** — there is no refund endpoint and no refund API call. The cancel response carries `refund_guidance: {dashboard_url, note}`: the Stripe dashboard URL for the subscription (`https://dashboard.stripe.com/subscriptions/{provider_subscription_id}`), where support cancels the Stripe subscription too (so billing stops) and issues any refund on the latest invoice or payment.
- **Two-step cancel, until the provider leg lands** — the endpoint cancels OUR row only (no provider call, ASSUMPTION-034): access stops at once, but Stripe keeps billing until the subscription is canceled in the dashboard. The provider-side cancel leg is a follow-up; until then the dashboard step is mandatory, not optional.
- **Customer self-cancel** stays portal-side (above): the customer cancels in the Stripe portal; the webhook lands the `canceled` move. The portal's cancel timing (immediate vs period end) is the dashboard's default portal configuration, not code.

## Tax (GST) (P1-076)

- **Seller GSTIN only, passthrough only** — `PF_BILLING_GSTIN` carries Detag's own GSTIN to the billing provider at checkout: Stripe subscription metadata `detag_gstin`, Razorpay subscription notes `detag_gstin` (ADR-0041). The provider's invoice carries it; we do no tax math in code — the provider's invoice is authoritative, the same rule as proration (ASSUMPTION-032).
- **Empty means unset** — no GSTIN configured, none is sent, invoices carry none. A set value must be a 15-char GSTIN (shape-validated at boot; a malformed value fails boot, it never reaches an invoice).
- **The buyer's GSTIN is not collected** — checkout takes no customer GSTIN today; collecting one (and passing it to the provider as the customer tax id) is a follow-up, not this ticket (ASSUMPTION-038).
- **ToS tax clause (STUB — owner/counsel review before launch; ToS pages ship in Phase 4, text derived from the compliance doc):**

> Prices are shown exclusive of applicable taxes. Where Detag is registered for Goods and Services Tax (GST) in India, GST is charged at the rate in force on the invoice date and is shown on the invoice; Detag's GSTIN, when configured, appears on the invoice. The customer is responsible for any taxes, duties, or levies imposed by their own jurisdiction on their purchase. No tax advice is given here — the invoice from the payment provider is the tax record.

## Related docs

- Provider parity — `docs/06-DELIVERY/11-provider-parity.md` (P1-077:
  Stripe vs Razorpay, what works vs what is stubbed; ADR-0042 launch
  sequencing)
- Razorpay enable runbook — `docs/06-DELIVERY/12-razorpay-enable-runbook.md` (P1-078:
  test-mode pilot, owner gates, risks, rollback; the executable
  checklist for flipping `PF_FEATURE_RAZORPAY`)
- Open question #2 — `docs/02-REQUIREMENTS/06-open-questions.md`
- Plan editor — `docs/06-DELIVERY/07-admin-guide.md` (plans editor),
  api-spec admin-plans row
- Entitlement suite — `docs/05-QUALITY/01-test-strategy.md` (P1-057)
- Quota/overage — `docs/06-DELIVERY/08-quota-ui-contract.md`,
  `docs/06-DELIVERY/09-overage-policy.md`
- Config — `docs/06-DELIVERY/04-config-reference.md` (Stripe keys)
- Assumptions — ASSUMPTION-022 (editor rules), ASSUMPTION-020
  (`upgrade_url` / `/plans`)
