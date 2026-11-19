# Razorpay enable runbook (P1-078)

> Owner: the person who flips the flag. Status: the flag exists and is
> OFF by default; this runbook is the executable checklist for turning
> it on. Nothing in this document flips anything by itself.
>
> Related: `docs/06-DELIVERY/11-provider-parity.md` (what works vs
> Stripe), ADR-0042 in `docs/09-STATE/DECISIONS.md` (launch sequencing
> — this runbook is step 3 of that order), ADR-0038 (adapter design),
> `docs/06-DELIVERY/03-deploy-runbook.md` (deploy + key rotation),
> `docs/06-DELIVERY/10-pricing-config.md` (plans, GSTIN).

## 0. Read this first — three facts that decide everything else

1. **The flag flip alone can only ever enable test mode.** `allow_live`
   is hardcoded `false` in `apps/api/src/lib.rs` (owner decision #3,
   still pending): a `rzp_live_…` key is refused at adapter
   construction (`BillingError::Configuration`). Going live needs a
   code change that passes `allow_live = true` **plus** the owner's
   explicit answer to decision #3. There is no env var that turns
   live keys on.
2. **Enabling Razorpay disables Stripe.** One active provider only
   (ADR-0038 §7): flag on + Razorpay configured wins over Stripe.
   While Razorpay is active, Stripe prices are not sold and the
   Stripe webhook fails signature verification (400). The flip is a
   deliberate switch, not an addition, until dual-provider lands.
3. **Rollback does not un-bill anyone.** Turning the flag back off
   restores Stripe sales, but subscriptions created under Razorpay
   stay live at Razorpay until canceled in the Razorpay dashboard
   (support cancel, P1-071, moves only our row). A pilot that creates
   real subscriptions must cancel them at the provider too.

## 1. Preconditions (do these before touching the flag)

| # | Precondition | How to check |
| --- | --- | --- |
| 1 | Razorpay dashboard: one plan per Detag plan (Free / Pro / Business), monthly, INR | Dashboard → Subscriptions → Plans; note each `plan_…` id |
| 2 | `plans.razorpay_plan_id` set for every sellable plan | `SELECT slug, razorpay_plan_id FROM plans;` — NULL means that plan cannot be sold under Razorpay (checkout maps the id to the provider; a bad id is `InvalidPrice`) |
| 3 | Webhook endpoint registered in the Razorpay dashboard | Dashboard → Settings → Webhooks → `https://<PF_PUBLIC_BASE_URL>/v1/webhooks/razorpay`, subscribed to `subscription.authenticated`, `subscription.activated`, `subscription.updated`, `subscription.pending`, `subscription.halted`, `subscription.cancelled`, `subscription.completed`, `subscription.expired`, `subscription.resumed`, `subscription.charged` |
| 4 | Webhook secret recorded | The secret from step 3 goes in `PF_RAZORPAY_WEBHOOK_SECRET` — it is the THIRD credential, never the API key secret (P1-073) |
| 5 | Test-mode key pair at hand | `PF_RAZORPAY_KEY=rzp_test_…`, `PF_RAZORPAY_SECRET=…` (test secret, from the dashboard) |
| 6 | The api can reach `https://api.razorpay.com` | From the api host: `curl -s -o /dev/null -w '%{http_code}' https://api.razorpay.com/v1/plans` returns 401 (auth required = reachable), not a connection error |
| 7 | Stripe still configured | `PF_STRIPE_SECRET_KEY` + `PF_STRIPE_WEBHOOK_SECRET` stay in `.env` — rollback (step 5) needs Stripe to fall back to |

## 2. Sequence

### Step A — Test-mode pilot (the first live run, ADR-0042 §2)

Do this on the production host with test-mode keys, or on a staging
host with the same `.env` shape. The pilot proves the real wire;
nothing in it touches real money.

1. Set in `.env`:
   `PF_FEATURE_RAZORPAY=true`,
   `PF_RAZORPAY_KEY=rzp_test_…`,
   `PF_RAZORPAY_SECRET=…`,
   `PF_RAZORPAY_WEBHOOK_SECRET=…`
   (the webhook secret from precondition 4).
2. Restart the api (flags are read at boot — a running api does not
   pick up a flag change). Verify `/readyz` is 200 and the boot log
   has no `billing provider not built` warning — that warning means
   the key was refused and billing endpoints will answer 503.
3. `POST /v1/billing/checkout` as a test user for the Pro plan.
   Expect 200 `{id: sub_…, url: <short_url>}`. Open the URL and
   authorize the mandate with a Razorpay test card/UPI.
4. Wait for the webhook: `subscription.authenticated` must create
   the subscription row (status `trialing`, plan Pro) — check the
   `subscriptions` table and the audit row
   `billing.subscription.created`. If no row appears within 2
   minutes, check the Razorpay dashboard webhook deliveries (a 400
   means the secret mismatches; a 404 means the flag is off; a 503
   `PF-5005` means the adapter did not build).
5. Confirm the negative legs, all on the pilot host:
   - `POST /v1/billing/portal` → 500 (no Razorpay portal — expected;
     see risk R2).
   - `GET /v1/billing/invoices` → 200, only this customer's invoices.
   - Replay the `subscription.authenticated` delivery from the
     dashboard → still one subscription row (ledger idempotency).
   - A tampered delivery (wrong signature) → 400.
6. Cancel through support (`POST
   /v1/admin/billing/subscriptions/{id}/cancel`) and **also** cancel
   the subscription in the Razorpay dashboard (risk R3: support
   cancel moves only our row).
7. Turn the flag back off (`PF_FEATURE_RAZORPAY=false`), restart,
   and confirm a Stripe test checkout still works. The pilot is
   done only when the Stripe leg works again after the flag-off.

**Stop rule:** a pilot that cannot complete a test-mode checkout
stops here. Nothing else in this sequence is attempted until the
pilot passes end to end.

### Step B — Owner gates (before any live key exists)

All four must be answered in `docs/09-STATE/DECISIONS.md`
(owner-input-needed queue). The checklist in §3 is the sign-off
sheet; the queue is the record.

1. Pricing confirmed (queue #2: Free ₹0 / Pro ₹499 / Business
   ₹1,999 is the safe default until the owner says otherwise).
2. Live keys approved (queue #3: owner decision #3 — test mode is
   the ceiling until the owner explicitly approves live keys, for
   Stripe and Razorpay alike).
3. Portal gap accepted (queue #9): Razorpay subscribers have no
   self-serve cancel / payment-method flow; they cancel through
   support + the dashboard. The owner accepts that, or a
   portal-side flow lands first.
4. Shared money gaps accepted or closed (queue #10): provider-side
   cancel, provider-side price switch on downgrade, and upgrade
   supersede are not built for either provider. They must be
   explicitly accepted or closed before Razorpay takes real
   subscribers.

### Step C — Live enable (NOT executable today)

Live enable is blocked on a code change, not on this runbook: pass
`allow_live = true` at the adapter construction in
`apps/api/src/lib.rs`, behind owner decision #3, and only after
steps A and B are complete. When that change lands, the live
sequence is:

1. Create the live plans in the Razorpay dashboard (separate
   `plan_…` ids from test mode) and record them in
   `plans.razorpay_plan_id` (same SQL as precondition 2).
2. Register the live webhook endpoint + secret (same as
   precondition 3–4, live dashboard) and set the live key pair in
   `.env` (`rzp_live_…`).
3. Set `PF_FEATURE_RAZORPAY=true`, restart the api, verify
   `/readyz` 200 and no `billing provider not built` warning.
4. Run one live checkout for ₹1 worth of test (a real Pro
   subscription on a test account, then cancel it at the provider
   and refund via the dashboard) to prove the live wire before
   announcing the switch.
5. Announce the switch: Stripe sales stop the moment the flag is
   on. Existing Stripe subscribers keep billing on Stripe until
   their subscriptions end — the flag does not migrate them.

## 3. Owner checklist (sign-off sheet)

Copy this into the release note when the switch is announced. Each
line needs a named owner + date; a blank line blocks the step that
needs it.

- [ ] Test-mode pilot passed end to end, including the flag-off
      Stripe leg (§2 step A) — pilot date: _______, run by: _______
- [ ] Pricing confirmed (DECISIONS.md queue #2) — answer: _______
- [ ] Live keys approved (DECISIONS.md queue #3, owner decision #3)
      — answer: _______
- [ ] Portal gap accepted (DECISIONS.md queue #9) — answer: _______
- [ ] Shared money gaps accepted or closed (DECISIONS.md queue #10)
      — answer: _______
- [ ] `allow_live = true` code change landed + reviewed (blocks §2
      step C; no env var substitutes) — commit: _______
- [ ] Live ₹1 checkout + cancel + refund proven (§2 step C.4) —
      date: _______, run by: _______

## 4. Risks

| # | Risk | If it happens | Mitigation / gate |
| --- | --- | --- | --- |
| R1 | Live key used before owner approval | Real charges on a test setup; a `rzp_live_…` key in `.env` is silently refused today, but only because `allow_live` is hardcoded false | §0 fact 1: no env var enables live; decision #3 + code change gate step C |
| R2 | Subscriber cannot cancel or change payment method | Support tickets; a Razorpay subscriber has no self-serve portal (`POST /v1/billing/portal` 500s) | Queue #9 owner acceptance; support cancel (P1-071) + dashboard cancel is the documented path |
| R3 | Cancel in Detag but billing continues at Razorpay | Customer charged after canceling — the support-cancel path moves only our row | Pilot step A.6 cancels at the provider too; queue #10 acceptance; dashboard cancel is part of every cancel SOP |
| R4 | Downgrade flips entitlement but the provider keeps the old price | Customer pays Pro price on a Free plan past the switch | Queue #10 acceptance; until a subscription schedule lands, downgrades are dashboard-adjusted by hand |
| R5 | Upgrade leaves the old subscription live at the provider | Double billing: the old row stays live-but-stale, newest-live-wins carries the entitlement | Queue #10 acceptance; the old provider subscription must be canceled in the dashboard at upgrade time |
| R6 | Stripe webhook breaks during the switch | Stripe events 400 while Razorpay is active (single provider, §0 fact 2) — Stripe renewals/cancels stop flowing | Announce the switch; existing Stripe subscribers are billed by Stripe but their lifecycle events are not processed until the flag goes back off |
| R7 | Webhook secret mismatch after rotation | All Razorpay webhooks 400; renewals/cancels never land | Rotate on BOTH sides (deploy runbook §Payment keys); a mismatch is safe (rejected) but silent — check dashboard delivery logs after any rotation |
| R8 | `razorpay_plan_id` missing for a plan | Checkout for that plan fails `InvalidPrice` at the provider | Precondition 2: the SQL check before any enable; a NULL id is a launch blocker for that plan, not a runtime surprise |
| R9 | Flag flipped without a restart | Nothing changes (flags read at boot) and the operator thinks Razorpay is on | Step A.2 / C.3: restart + `/readyz` + boot-log check are part of the sequence, not optional |
| R10 | Razorpay invoice has no PDF | `GET /v1/billing/invoices` returns `download_url: None`; the invoice opens on Razorpay's hosted page (`hosted_url`) instead | Console must link `hosted_url`, not assume a PDF (parity doc §invoices) |

## 5. Rollback

1. Set `PF_FEATURE_RAZORPAY=false` in `.env` and restart the api.
   Stripe resumes as the provider immediately (it was never
   unconfigured — precondition 7). Verify: a Stripe test checkout
   works, `/readyz` is 200.
2. **Then** cancel any Razorpay subscriptions created during the
   window in the Razorpay dashboard — rollback alone does not stop
   their billing (§0 fact 3).
3. Tell support: Razorpay subscribers created during the window
   cancel through support + dashboard until their provider
   subscriptions are canceled; their Detag rows stay readable for
   the dispute trail.
4. Record the rollback in the night report: time, reason, whether
   any Razorpay subscription was live at rollback time.

## 6. Verification commands (run on the host, after any flag change)

```sh
# api up and billing built (no 'billing provider not built' warning)
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8080/readyz
docker compose -f ops/compose.base.yml -f ops/compose.prod.yml \
  --profile prod logs api --tail 50 | grep -i billing

# flag state (what the running api believes)
# PF_FEATURE_RAZORPAY=true  -> Razorpay webhook answers (not 404)
# PF_FEATURE_RAZORPAY=false -> POST /v1/webhooks/razorpay answers 404 PF-2010
curl -s -o /dev/null -w '%{http_code}\n' -X POST \
  https://<PF_PUBLIC_BASE_URL>/v1/webhooks/razorpay

# provider the api is actually using (Stripe vs Razorpay vs none)
# There is no endpoint that names the active provider — prove it with
# the pilot checkout (§2 step A.3): a Razorpay checkout returns a
# `sub_…` id + `short_url`; a Stripe checkout returns a Checkout
# Session URL. Never infer the provider from an error message alone.
```

## 7. What NOT to do

- Do not put a live key in `.env` before owner decision #3 — the
  adapter refuses it today, and the refusal is the only thing
  standing between a test setup and real charges.
- Do not enable the flag on the strength of ADR-0042 alone — the
  ADR owns the order, this runbook owns the checklist, and the
  pilot (§2 step A) owns the proof.
- Do not treat the webhook secret as the API secret — they are
  different credentials, and a mismatch fails every delivery.
- Do not cancel a Razorpay subscriber in Detag only — cancel at
  the provider too (risk R3).
- Do not promise subscribers a self-serve portal while Razorpay is
  active — there is not one.
