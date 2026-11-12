# PHASE 1 — Identity & Billing

> **Tickets: 111** · Types: feat 86 · chore 1 · test 14 · docs 10

**Goal:** Full SaaS core: registration/verification, JWT sessions with rotation, password flows, API keys, RBAC, plans + quotas + entitlements, Stripe billing (test mode) with Razorpay adapter behind a flag, usage metering, audit + abuse workflows, onboarding/consent (ownership attestation).

**Context (read before starting):**
- Stripe stays in TEST MODE; live keys are owner-input (DECISIONS.md queue).
- Security tests in 05-QUALITY/03-security-testing.md (authz matrix, refresh reuse) belong to THIS phase, not Phase 5.
- Ownership attestation is a hard gate (FR-C-01) - it lands here because uploads/jobs begin relying on it in Phase 2.

**How to work:** `08-PROMPTS/02-TICKET-RUNNER.md` governs every ticket. Epic order = build order; per-ticket Deps are sequence hints (auto = previous ticket in the same epic). Tick checkboxes, update `09-STATE/PROGRESS.md`, one commit per ticket. Exit gates below must be green before the next phase starts.

**Exit gates (phase is DONE only when all hold):**
- [ ] Auth lifecycle e2e green: register, verify, login, refresh, rotate, reuse-attack revocation, logout.
- [ ] Authz matrix test with zero gaps across all live /v1 routes (including api-key paths).
- [ ] Stripe test-mode lifecycle green via test-clock: trial, active, past_due, canceled, webhook replay-safe.
- [ ] Quota enforcement race-safe; entitlements surface in /v1/me and headers.
- [ ] Attestation enforced on upload + job creation; audit trail verified append-only.

## Ticket map

| Epic | Area | Tickets |
| --- | --- | --- |
| E1.1 Users & registration | auth/users | 12 |
| E1.2 Sessions & JWT | auth/sessions | 12 |
| E1.3 Password & email security | auth/password | 8 |
| E1.4 API keys | auth/apikeys | 9 |
| E1.5 RBAC & authz | auth/rbac | 7 |
| E1.6 Plans & entitlements | billing/plans | 10 |
| E1.7 Stripe billing | billing/stripe | 14 |
| E1.8 Razorpay adapter | billing/razorpay | 6 |
| E1.9 Usage metering | billing/metering | 8 |
| E1.10 Audit & abuse | audit | 7 |
| E1.11 Onboarding & consent | onboarding | 7 |
| E1.12 Email polish | mailer | 5 |
| E1.13 Support tooling | support | 6 |

---

## E1.1 — Users & registration (area: auth/users)

- [ ] **P1-001** — Registration endpoint
  - **Type:** feat · **Area:** auth/users · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** POST /v1/auth/register per api-spec: validation, disposable-email denylist (config), argon2id hash (params from config), ToS version recorded, verification email queued via outbox; audit events register.attempt/.success.
  - **AC:**
    - [ ] duplicate email generic error (no enumeration)
    - [ ] outbox email queued

- [ ] **P1-002** — Email verification flow
  - **Type:** feat · **Area:** auth/users · **Est:** M · **Deps:** P1-001 · **Prio:** P1
  - **Spec:** Single-use hashed token (24h), verify endpoint, resend rate-limited (3/hour), verified_at set; verification required before job creation (PF-1009).
  - **AC:**
    - [ ] token single-use
    - [ ] unverified cannot create jobs

- [ ] **P1-003** — Login endpoint
  - **Type:** feat · **Area:** auth/users · **Est:** M · **Deps:** P1-002 · **Prio:** P1
  - **Spec:** Constant-time verify, generic error, session create + refresh cookie + access token; audit; per-IP + per-account rate limits wired to E0.11 limiter.
  - **AC:**
    - [ ] no enumeration
    - [ ] rate limit trips

- [ ] **P1-004** — Lockout/backoff
  - **Type:** feat · **Area:** auth/users · **Est:** M · **Deps:** P1-003 · **Prio:** P1
  - **Spec:** Failed-attempt counters per account + per IP, exponential backoff windows, lockout notification email; resets on success and on password reset.
  - **AC:**
    - [ ] backoff curve tested
    - [ ] race-safe counters

- [ ] **P1-005** — Profile GET/PATCH /v1/me
  - **Type:** feat · **Area:** auth/users · **Est:** S · **Deps:** P1-004 · **Prio:** P1
  - **Spec:** Profile + plan + quota snapshot; patch display name and email-change intent; quota snapshot from entitlement service.
  - **AC:**
    - [ ] snapshot matches quota middleware

- [ ] **P1-006** — Email change flow
  - **Type:** feat · **Area:** auth/users · **Est:** M · **Deps:** P1-005 · **Prio:** P1
  - **Spec:** PATCH triggers re-verification to new address (token), old-address notification (takeover alert), swap on verify; pending-state handling tested.
  - **AC:**
    - [ ] old address alerted
    - [ ] pending state safe

- [ ] **P1-007** — Account deletion
  - **Type:** feat · **Area:** auth/users · **Est:** M · **Deps:** P1-006 · **Prio:** P1
  - **Spec:** DELETE /v1/me: status deletion_pending, 30-day grace with cancel-on-login, nightly purge job (cascade per data-model), audit + confirmation email.
  - **AC:**
    - [ ] purge cascades per doc
    - [ ] grace cancel works

- [ ] **P1-008** — Admin suspend/restore
  - **Type:** feat · **Area:** auth/users · **Est:** M · **Deps:** P1-007 · **Prio:** P1
  - **Spec:** POST /v1/admin/users/{id}/suspend|restore with reason; suspended sessions revoked immediately; queued jobs held; audited.
  - **AC:**
    - [ ] sessions revoked
    - [ ] jobs held

- [ ] **P1-009** — Admin user search
  - **Type:** feat · **Area:** auth/users · **Est:** M · **Deps:** P1-008 · **Prio:** P1
  - **Spec:** GET /v1/admin/users?query= (email/name/id prefix), cursor pagination, fields incl. plan, status, usage today; support role read-only.
  - **AC:**
    - [ ] pagination + filter tests

- [ ] **P1-010** — Impersonation
  - **Type:** feat · **Area:** auth/users · **Est:** M · **Deps:** P1-009 · **Prio:** P1
  - **Spec:** POST /v1/admin/users/{id}/impersonate (reason required): 15-minute scoped token with impersonator_id claim, banner requirement documented, full audit, auto-expiry enforced.
  - **AC:**
    - [ ] token expires 15m
    - [ ] banner contract documented

- [ ] **P1-011** — Impersonation audit tests
  - **Type:** test · **Area:** auth/users · **Est:** M · **Deps:** P1-010 · **Prio:** P1
  - **Spec:** Every impersonated action logged with both ids; impersonator cannot reach admin endpoints while impersonating; expiry enforced.
  - **AC:**
    - [ ] audit rows complete
    - [ ] admin blocked during impersonation

- [ ] **P1-012** — User lifecycle docs
  - **Type:** docs · **Area:** auth/users · **Est:** S · **Deps:** P1-011 · **Prio:** P1
  - **Spec:** State diagram (active/suspended/deletion_pending), retention per NFR-40, admin procedure notes.
  - **AC:**
    - [ ] diagram + notes committed

---

## E1.2 — Sessions & JWT (area: auth/sessions)

- [ ] **P1-013** — JWT claims + issuance
  - **Type:** feat · **Area:** auth/sessions · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** jsonwebtoken: claims {sub, role, ver, impersonator?, jti, exp 15m, iat, iss, aud, kid}; key from PF_JWT_SECRET; errors map to PF-1010/1011.
  - **AC:**
    - [ ] claims shape locked by test

- [ ] **P1-014** — Refresh rotation
  - **Type:** feat · **Area:** auth/sessions · **Est:** M · **Deps:** P1-013 · **Prio:** P1
  - **Spec:** 30d opaque refresh (hashed), family_id lineage; rotation replaces token; REUSE of a rotated token revokes the whole family (PF-1007) + security email.
  - **AC:**
    - [ ] reuse-attack test revokes family

- [ ] **P1-015** — Sessions table + repo
  - **Type:** feat · **Area:** auth/sessions · **Est:** S · **Deps:** P1-014 · **Prio:** P1
  - **Spec:** Per data-model; cleanup worker purges expired rows nightly; indexes per doc.
  - **AC:**
    - [ ] cleanup job green

- [ ] **P1-016** — Cookie + bearer dual auth
  - **Type:** feat · **Area:** auth/sessions · **Est:** M · **Deps:** P1-015 · **Prio:** P1
  - **Spec:** Web: httpOnly Secure SameSite=Lax refresh cookie + in-memory access; API: bearer only; precedence documented; CSRF posture documented (same-site + no state-changing GET).
  - **AC:**
    - [ ] both paths tested
    - [ ] posture documented

- [ ] **P1-017** — Logout endpoints
  - **Type:** feat · **Area:** auth/sessions · **Est:** S · **Deps:** P1-016 · **Prio:** P1
  - **Spec:** Revoke current session; logout-all revokes every session; audited; revoked refresh cannot rotate.
  - **AC:**
    - [ ] revocation enforced

- [ ] **P1-018** — Revoke on password change
  - **Type:** feat · **Area:** auth/sessions · **Est:** M · **Deps:** P1-017 · **Prio:** P1
  - **Spec:** Reset/change-password revokes all sessions except current; new-login digest email; tested.
  - **AC:**
    - [ ] sessions revoked
    - [ ] digest emailed

- [ ] **P1-019** — Session listing
  - **Type:** feat · **Area:** auth/sessions · **Est:** S · **Deps:** P1-018 · **Prio:** P1
  - **Spec:** GET /v1/me/sessions with device (UA parsed, ip-hashed), created, last_seen (sliding, throttled update); DELETE one session.
  - **AC:**
    - [ ] throttled last_seen verified

- [ ] **P1-020** — Brute-force limiter wiring
  - **Type:** feat · **Area:** auth/sessions · **Est:** S · **Deps:** P1-019 · **Prio:** P1
  - **Spec:** Redis token buckets per account+ip on login/register/reset; standard headers; lockout integrates backoff; concurrency tests.
  - **AC:**
    - [ ] headers present
    - [ ] concurrency safe

- [ ] **P1-021** — Token revocation cache
  - **Type:** feat · **Area:** auth/sessions · **Est:** S · **Deps:** P1-020 · **Prio:** P1
  - **Spec:** Short-lived jti denylist in redis consulted by extractor; password-change bumps denylist epoch; p99 unaffected (cache hit path).
  - **AC:**
    - [ ] denylist consulted
    - [ ] perf note recorded

- [ ] **P1-022** — JWT verification extractor
  - **Type:** feat · **Area:** auth/sessions · **Est:** M · **Deps:** P1-021 · **Prio:** P1
  - **Spec:** Full AuthContext: verify sig+exp+nbf, role refreshed from db for admin routes, api-key path integration (E1.4).
  - **AC:**
    - [ ] tampered token rejected
    - [ ] role refresh tested

- [ ] **P1-023** — Auth integration suite
  - **Type:** test · **Area:** auth/sessions · **Est:** M · **Deps:** P1-022 · **Prio:** P1
  - **Spec:** Lifecycle: register, verify, login, refresh, rotate, reuse-attack, logout; clock-injected expiry tests; token tampering rejected.
  - **AC:**
    - [ ] full suite green

- [ ] **P1-024** — Auth design notes
  - **Type:** docs · **Area:** auth/sessions · **Est:** S · **Deps:** P1-023 · **Prio:** P1
  - **Spec:** Threat-model update: token theft, replay, family revocation rationale; cookie vs bearer trade-offs.
  - **AC:**
    - [ ] notes committed

---

## E1.3 — Password & email security (area: auth/password)

- [ ] **P1-025** — Forgot/reset flow
  - **Type:** feat · **Area:** auth/password · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** Request always 204 (no enumeration), hashed single-use 30-min token, reset revokes sessions, audit + email.
  - **AC:**
    - [ ] no enumeration
    - [ ] token single-use

- [ ] **P1-026** — Password policy + pwned check
  - **Type:** feat · **Area:** auth/password · **Est:** S · **Deps:** P1-025 · **Prio:** P1
  - **Spec:** Min length 12, strength score (crate), optional HIBP k-anonymity range check behind flag (privacy note), top-10k blocklist.
  - **AC:**
    - [ ] policy tested
    - [ ] flag default off

- [ ] **P1-027** — Argon2 params config
  - **Type:** feat · **Area:** auth/password · **Est:** S · **Deps:** P1-026 · **Prio:** P1
  - **Spec:** m=64MB t=3 p=4 defaults, configurable, rehash-on-login upgrade path; benchmark note per target VPS.
  - **AC:**
    - [ ] rehash path tested

- [ ] **P1-028** — Mailer abstraction
  - **Type:** feat · **Area:** auth/password · **Est:** M · **Deps:** P1-027 · **Prio:** P1
  - **Spec:** crates/mailer: lettre SMTP + outbox drain worker; retries with backoff; MailHog dev; template engine (tera).
  - **AC:**
    - [ ] drain worker green
    - [ ] retry tested

- [ ] **P1-029** — Email templates v1
  - **Type:** feat · **Area:** auth/password · **Est:** M · **Deps:** P1-028 · **Prio:** P1
  - **Spec:** Verify-email, reset-password, password-changed, new-login, lockout, verification-thanks: HTML + plain text; copy passes honesty rule.
  - **AC:**
    - [ ] templates snapshot-tested

- [ ] **P1-030** — Email flow tests
  - **Type:** test · **Area:** auth/password · **Est:** M · **Deps:** P1-029 · **Prio:** P1
  - **Spec:** Outbox assertions (template, recipient, token shape); mailhog e2e for verify + reset; no real SMTP in CI.
  - **AC:**
    - [ ] flows e2e green

- [ ] **P1-031** — Email send rate limits
  - **Type:** feat · **Area:** auth/password · **Est:** S · **Deps:** P1-030 · **Prio:** P1
  - **Spec:** Per-recipient hourly caps per template class (verify 3, reset 3, marketing 0); abuse log entries on trips.
  - **AC:**
    - [ ] caps enforced

- [ ] **P1-032** — Email ops docs
  - **Type:** docs · **Area:** auth/password · **Est:** S · **Deps:** P1-031 · **Prio:** P1
  - **Spec:** Deliverability checklist (SPF/DKIM/DMARC placeholders), sender config, test addresses, transactional-only stance v1.
  - **AC:**
    - [ ] docs committed

---

## E1.4 — API keys (area: auth/apikeys)

- [ ] **P1-033** — Key generation
  - **Type:** feat · **Area:** auth/apikeys · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** POST /v1/api-keys: pf_live_ prefix + 24B url-safe secret shown once; sha256 stored; scopes (jobs:write, jobs:read, webhooks:manage); max 10 keys/user.
  - **AC:**
    - [ ] secret shown once
    - [ ] cap enforced

- [ ] **P1-034** — Key auth extractor
  - **Type:** feat · **Area:** auth/apikeys · **Est:** S · **Deps:** P1-033 · **Prio:** P1
  - **Spec:** Bearer pf_live_* path: hash lookup, scope check, throttled last_used_at, per-key rate tier.
  - **AC:**
    - [ ] scope check enforced

- [ ] **P1-035** — Key rotation
  - **Type:** feat · **Area:** auth/apikeys · **Est:** S · **Deps:** P1-034 · **Prio:** P1
  - **Spec:** POST /v1/api-keys/{id}/rotate: new secret; old valid for 24h overlap (config); audited.
  - **AC:**
    - [ ] overlap window tested

- [ ] **P1-036** — Key revoke
  - **Type:** feat · **Area:** auth/apikeys · **Est:** S · **Deps:** P1-035 · **Prio:** P1
  - **Spec:** DELETE immediate; cached key entries invalidated via redis denylist; tested.
  - **AC:**
    - [ ] revocation immediate

- [ ] **P1-037** — Per-key rate limits
  - **Type:** feat · **Area:** auth/apikeys · **Est:** S · **Deps:** P1-036 · **Prio:** P1
  - **Spec:** Tier from config (default lower than user tier); 429 headers; abuse log on repeated trips.
  - **AC:**
    - [ ] tier enforced

- [ ] **P1-038** — Key-scoped usage attribution
  - **Type:** feat · **Area:** auth/apikeys · **Est:** M · **Deps:** P1-037 · **Prio:** P1
  - **Spec:** Jobs record key_id; usage endpoints filter by key; admin view shows prefix.
  - **AC:**
    - [ ] attribution verified

- [ ] **P1-039** — API-key suite
  - **Type:** test · **Area:** auth/apikeys · **Est:** M · **Deps:** P1-038 · **Prio:** P1
  - **Spec:** Lifecycle: create, use, rotate, old-valid-in-window, expired, revoked; scope enforcement (jobs:read cannot create).
  - **AC:**
    - [ ] suite green

- [ ] **P1-040** — Key ACL matrix rows
  - **Type:** feat · **Area:** auth/apikeys · **Est:** S · **Deps:** P1-039 · **Prio:** P1
  - **Spec:** Matrix rows for api-key on all /v1 routes (no admin ever, no billing); checked by matrix test registry.
  - **AC:**
    - [ ] matrix complete

- [ ] **P1-041** — API-key docs page
  - **Type:** docs · **Area:** auth/apikeys · **Est:** S · **Deps:** P1-040 · **Prio:** P1
  - **Spec:** Getting-started curl/TS/py examples per api-spec; security notes (storage, rotation).
  - **AC:**
    - [ ] docs committed

---

## E1.5 — RBAC & authz (area: auth/rbac)

- [ ] **P1-042** — Roles + permission matrix
  - **Type:** feat · **Area:** auth/rbac · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** user/support/admin capability table (code constants), role-guard middleware, 403 problem+json; matrix is the single source consumed by tests.
  - **AC:**
    - [ ] matrix test registry live

- [ ] **P1-043** — Admin route guard
  - **Type:** feat · **Area:** auth/rbac · **Est:** M · **Deps:** P1-042 · **Prio:** P1
  - **Spec:** All /v1/admin/* behind admin role + audit middleware (actor, action, target, before/after).
  - **AC:**
    - [ ] guard + audit verified

- [ ] **P1-044** — Support read-only
  - **Type:** feat · **Area:** auth/rbac · **Est:** M · **Deps:** P1-043 · **Prio:** P1
  - **Spec:** Support can read user/job/admin views only; writes 403; matrix asserts.
  - **AC:**
    - [ ] write attempts 403

- [ ] **P1-045** — IDOR sweep v1
  - **Type:** test · **Area:** auth/rbac · **Est:** M · **Deps:** P1-044 · **Prio:** P1
  - **Spec:** For each owned resource (jobs, uploads, keys, sessions, webhooks): cross-user access returns 404 (not 200/403 leak) per test plan.
  - **AC:**
    - [ ] sweep green

- [ ] **P1-046** — Privileged action audit middleware
  - **Type:** feat · **Area:** auth/rbac · **Est:** S · **Deps:** P1-045 · **Prio:** P1
  - **Spec:** Route metadata drives automatic audit for admin routes (no forgotten manual calls); rows asserted.
  - **AC:**
    - [ ] middleware audits all admin writes

- [ ] **P1-047** — Authz docs
  - **Type:** docs · **Area:** auth/rbac · **Est:** S · **Deps:** P1-046 · **Prio:** P1
  - **Spec:** Matrix rendered into admin guide; change procedure (matrix + tests together).
  - **AC:**
    - [ ] docs committed

- [ ] **P1-048** — Role escalation tests
  - **Type:** test · **Area:** auth/rbac · **Est:** S · **Deps:** P1-047 · **Prio:** P1
  - **Spec:** Attempts: user to admin endpoints, support to writes, api-key to admin: all 403 + audited; token role tamper rejected (signature).
  - **AC:**
    - [ ] all attempts blocked + audited

---

## E1.6 — Plans & entitlements (area: billing/plans)

- [ ] **P1-049** — Plans service
  - **Type:** feat · **Area:** billing/plans · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** Catalog from db (seeded) + cache; entitlement struct {daily_images, daily_video_seconds, max_bytes, result_ttl_hours, concurrency, lane, features}; effective-plan resolution (subscription override).
  - **AC:**
    - [ ] resolution tested

- [ ] **P1-050** — Quota counters
  - **Type:** feat · **Area:** billing/plans · **Est:** M · **Deps:** P1-049 · **Prio:** P1
  - **Spec:** Redis atomic daily counters (UTC reset) + usage_counters write-through; video-seconds estimator at job create; nightly reconciliation fixes drift.
  - **AC:**
    - [ ] UTC boundary clock-injected test

- [ ] **P1-051** — Quota middleware
  - **Type:** feat · **Area:** billing/plans · **Est:** M · **Deps:** P1-050 · **Prio:** P1
  - **Spec:** Pre-check on job create (images + video seconds + concurrency slot); 429 problem+json with upgrade hint + PF-3001/3002; X-Quota-* headers.
  - **AC:**
    - [ ] rejection shape tested
    - [ ] headers present

- [ ] **P1-052** — Quota headers everywhere
  - **Type:** feat · **Area:** billing/plans · **Est:** S · **Deps:** P1-051 · **Prio:** P1
  - **Spec:** Job-creating routes attach remaining quota; snapshot in /v1/me; UI contract documented.
  - **AC:**
    - [ ] headers on all create routes

- [ ] **P1-053** — Concurrency slots
  - **Type:** feat · **Area:** billing/plans · **Est:** S · **Deps:** P1-052 · **Prio:** P1
  - **Spec:** Per-user running-jobs cap per plan; crash-safe slot acquire/release with TTL; slot-leak recovery tested.
  - **AC:**
    - [ ] leak recovery green

- [ ] **P1-054** — Overage policy
  - **Type:** feat · **Area:** billing/plans · **Est:** S · **Deps:** P1-053 · **Prio:** P1
  - **Spec:** Reject with clear message + plan matrix link; no silent queueing; admin quota-grant bypass audited (E1.13).
  - **AC:**
    - [ ] no silent overage

- [ ] **P1-055** — Plan feature flags
  - **Type:** feat · **Area:** billing/plans · **Est:** S · **Deps:** P1-054 · **Prio:** P1
  - **Spec:** features jsonb (api_keys, webhooks, batch, priority_lane) checked by services; 403 PF-3003 with upgrade hint.
  - **AC:**
    - [ ] gated features tested

- [ ] **P1-056** — Admin plan editor
  - **Type:** feat · **Area:** billing/plans · **Est:** M · **Deps:** P1-055 · **Prio:** P1
  - **Spec:** GET/PUT /v1/admin/plans (validated entitlements + stripe price mapping), cache invalidation, audited; plan removal requires no-active-subs check.
  - **AC:**
    - [ ] editor + guard tests

- [ ] **P1-057** — Entitlement suite
  - **Type:** test · **Area:** billing/plans · **Est:** M · **Deps:** P1-056 · **Prio:** P1
  - **Spec:** Free vs Pro behaviors: limits trip, UTC reset, concurrency slots, TTL differences; reconciliation catches drift.
  - **AC:**
    - [ ] suite green

- [ ] **P1-058** — Pricing config docs
  - **Type:** docs · **Area:** billing/plans · **Est:** S · **Deps:** P1-057 · **Prio:** P1
  - **Spec:** Where defaults live (open-questions #2), safe change procedure (config + stripe dashboard + seed), rollout notes.
  - **AC:**
    - [ ] docs committed

---

## E1.7 — Stripe billing (area: billing/stripe)

- [ ] **P1-059** — BillingProvider trait
  - **Type:** feat · **Area:** billing/stripe · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** Trait: create_checkout, create_portal, fetch_subscription, map_webhook(event) to DomainEvent, customer ops; typed error; rationale documented (FR-P-05).
  - **AC:**
    - [ ] trait + docs committed

- [ ] **P1-060** — Stripe adapter
  - **Type:** feat · **Area:** billing/stripe · **Est:** M · **Deps:** P1-059 · **Prio:** P1
  - **Spec:** async-stripe: checkout session (price from plan config), portal session, subscription fetch; test-mode enforced (live keys rejected without owner flag).
  - **AC:**
    - [ ] live-key guard tested

- [ ] **P1-061** — Products/prices sync job
  - **Type:** feat · **Area:** billing/stripe · **Est:** M · **Deps:** P1-060 · **Prio:** P1
  - **Spec:** Nightly sync plan catalog to Stripe products/prices (test mode) with drift report; manual trigger admin endpoint.
  - **AC:**
    - [ ] drift report produced

- [ ] **P1-062** — Checkout endpoint
  - **Type:** feat · **Area:** billing/stripe · **Est:** M · **Deps:** P1-061 · **Prio:** P1
  - **Spec:** POST /v1/billing/checkout: auth, plan from body, customer reuse, success/cancel URLs from config; idempotency honored.
  - **AC:**
    - [ ] idempotent checkout

- [ ] **P1-063** — Webhook receiver
  - **Type:** feat · **Area:** billing/stripe · **Est:** M · **Deps:** P1-062 · **Prio:** P1
  - **Spec:** POST /v1/webhooks/stripe: constant-time signature verify, raw-body capture, event table insert (unique event id) for idempotency, then dispatch; 4xx only on bad signature.
  - **AC:**
    - [ ] tamper rejected
    - [ ] replay no-op

- [ ] **P1-064** — Subscription state machine
  - **Type:** feat · **Area:** billing/stripe · **Est:** M · **Deps:** P1-063 · **Prio:** P1
  - **Spec:** trialing/active/past_due/canceled transitions; side effects: entitlement cache flush, emails (welcome, dunning, canceled), audit.
  - **AC:**
    - [ ] invalid transitions rejected

- [ ] **P1-065** — Webhook event handlers
  - **Type:** feat · **Area:** billing/stripe · **Est:** M · **Deps:** P1-064 · **Prio:** P1
  - **Spec:** checkout.session.completed, invoice.paid/payment_failed, customer.subscription.updated/deleted mapped; unknown event logged + 200 (no retry storm).
  - **AC:**
    - [ ] mapping table tested

- [ ] **P1-066** — Idempotency + replay safety
  - **Type:** feat · **Area:** billing/stripe · **Est:** S · **Deps:** P1-065 · **Prio:** P1
  - **Spec:** Replayed event: single effect (event-table unique); out-of-order events guarded by state machine; reordered-delivery tests.
  - **AC:**
    - [ ] replay/order tests green

- [ ] **P1-067** — Billing portal endpoint
  - **Type:** feat · **Area:** billing/stripe · **Est:** S · **Deps:** P1-066 · **Prio:** P1
  - **Spec:** POST /v1/billing/portal returns portal URL (test mode); portal configuration note (invoice display, cancel behavior).
  - **AC:**
    - [ ] portal URL returned

- [ ] **P1-068** — Invoices list
  - **Type:** feat · **Area:** billing/stripe · **Est:** S · **Deps:** P1-067 · **Prio:** P1
  - **Spec:** GET /v1/billing/invoices from stripe (cached 5m) mapped to our shape; stripe-hosted download URLs.
  - **AC:**
    - [ ] shape contract tested

- [ ] **P1-069** — Upgrade/downgrade proration
  - **Type:** feat · **Area:** billing/stripe · **Est:** M · **Deps:** P1-068 · **Prio:** P1
  - **Spec:** Upgrade via checkout with immediate proration; downgrade schedules at period end; entitlement switch timing documented + tested.
  - **AC:**
    - [ ] timing verified

- [ ] **P1-070** — Dunning emails
  - **Type:** feat · **Area:** billing/stripe · **Est:** S · **Deps:** P1-069 · **Prio:** P1
  - **Spec:** past_due sequence (day 0/3/7), retry window, suspend-at-day-14 policy job (configurable), audited.
  - **AC:**
    - [ ] sequence simulated via test-clock

- [ ] **P1-071** — Refund/cancel policy endpoints
  - **Type:** feat · **Area:** billing/stripe · **Est:** S · **Deps:** P1-070 · **Prio:** P1
  - **Spec:** Support-facing: immediate cancel (no proration) + refund guidance (Stripe dashboard) - audited note; ToS policy snippet.
  - **AC:**
    - [ ] flow audited

- [ ] **P1-072** — Stripe suite
  - **Type:** test · **Area:** billing/stripe · **Est:** M · **Deps:** P1-071 · **Prio:** P1
  - **Spec:** Test-clock lifecycle (trial to active to past_due to canceled), signature tamper, replay, idempotency, proration math units.
  - **AC:**
    - [ ] suite green

---

## E1.8 — Razorpay adapter (area: billing/razorpay)

- [ ] **P1-073** — Razorpay adapter
  - **Type:** feat · **Area:** billing/razorpay · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** BillingProvider impl: checkout link, subscription mapping (best-effort to our states), PF_RAZORPAY_* config; flag OFF default; India-market intent documented.
  - **AC:**
    - [ ] flag-off endpoints 404

- [ ] **P1-074** — Razorpay webhook verify
  - **Type:** feat · **Area:** billing/razorpay · **Est:** M · **Deps:** P1-073 · **Prio:** P1
  - **Spec:** HMAC sha256 (x-razorpay-signature), event mapping to DomainEvents, idempotent via event table.
  - **AC:**
    - [ ] tamper rejected
    - [ ] idempotent

- [ ] **P1-075** — Razorpay adapter tests
  - **Type:** test · **Area:** billing/razorpay · **Est:** M · **Deps:** P1-074 · **Prio:** P1
  - **Spec:** Mock server: signature pass/fail, mapping table, idempotency; flag-off behavior tested.
  - **AC:**
    - [ ] mock suite green

- [ ] **P1-076** — GST/invoice note
  - **Type:** feat · **Area:** billing/razorpay · **Est:** S · **Deps:** P1-075 · **Prio:** P1
  - **Spec:** Config fields for GSTIN passthrough (provider metadata), ToS tax clause stub, docs.
  - **AC:**
    - [ ] fields flow through

- [ ] **P1-077** — Provider parity check
  - **Type:** feat · **Area:** billing/razorpay · **Est:** S · **Deps:** P1-076 · **Prio:** P1
  - **Spec:** Parity doc: what works vs Stripe, what is stubbed; ADR for launch sequencing.
  - **AC:**
    - [ ] parity doc committed

- [ ] **P1-078** — Flag enable runbook
  - **Type:** chore · **Area:** billing/razorpay · **Est:** S · **Deps:** P1-077 · **Prio:** P1
  - **Spec:** Steps + risks to enable razorpay in prod; owner checklist in DECISIONS.md template.
  - **AC:**
    - [ ] checklist committed

---

## E1.9 — Usage metering (area: billing/metering)

- [ ] **P1-079** — Usage counters service
  - **Type:** feat · **Area:** billing/metering · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** Write-through usage_counters (user, day) + redis hot counters; UTC day boundary; video-seconds accumulate on completion (estimate at create, true-up later).
  - **AC:**
    - [ ] true-up math tested

- [ ] **P1-080** — Usage API
  - **Type:** feat · **Area:** billing/metering · **Est:** S · **Deps:** P1-079 · **Prio:** P1
  - **Spec:** GET /v1/usage: today + 30-day buckets; per-key breakdown when api key used; shape documented.
  - **AC:**
    - [ ] shape contract tested

- [ ] **P1-081** — Admin usage view API
  - **Type:** feat · **Area:** billing/metering · **Est:** S · **Deps:** P1-080 · **Prio:** P1
  - **Spec:** Per-user 30d + totals; CSV export endpoint (admin) for billing disputes.
  - **AC:**
    - [ ] export parses

- [ ] **P1-082** — Reconciliation job
  - **Type:** feat · **Area:** billing/metering · **Est:** M · **Deps:** P1-081 · **Prio:** P1
  - **Spec:** Nightly: redis vs db drift, db wins (source of truth), anomaly report; audit on corrections beyond threshold.
  - **AC:**
    - [ ] drift corrected + audited

- [ ] **P1-083** — Abuse spike detection
  - **Type:** feat · **Area:** billing/metering · **Est:** S · **Deps:** P1-082 · **Prio:** P1
  - **Spec:** Velocity rules (N jobs/minute threshold) create abuse_reports entries + alert; auto-suspend flag OFF default.
  - **AC:**
    - [ ] spike detected
    - [ ] flag off

- [ ] **P1-084** — Metering suite
  - **Type:** test · **Area:** billing/metering · **Est:** M · **Deps:** P1-083 · **Prio:** P1
  - **Spec:** Races (parallel creates under limit), midnight rollover (clock), true-up math, drift reconciliation.
  - **AC:**
    - [ ] suite green

- [ ] **P1-085** — Quota warning emails
  - **Type:** feat · **Area:** billing/metering · **Est:** S · **Deps:** P1-084 · **Prio:** P1
  - **Spec:** 80%/100% thresholds (config) warning emails with once/day cap; outbox tests cover.
  - **AC:**
    - [ ] capped correctly

- [ ] **P1-086** — Metering design docs
  - **Type:** docs · **Area:** billing/metering · **Est:** S · **Deps:** P1-085 · **Prio:** P1
  - **Spec:** Counter semantics, edge cases (clock skew, retry double-count guard), reconciliation policy.
  - **AC:**
    - [ ] docs committed

---

## E1.10 — Audit & abuse (area: audit)

- [ ] **P1-087** — Audit service
  - **Type:** feat · **Area:** audit · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** Append-only writes via service only (no direct repo writes elsewhere); actor/action/target/before/after/reason; db grant test proves UPDATE/DELETE denied.
  - **AC:**
    - [ ] append-only enforced

- [ ] **P1-088** — Audit query API
  - **Type:** feat · **Area:** audit · **Est:** S · **Deps:** P1-087 · **Prio:** P1
  - **Spec:** GET /v1/admin/audit?actor=&action=&target=&from=&to= cursor-paginated; indexes per data-model.
  - **AC:**
    - [ ] filters + pagination tested

- [ ] **P1-089** — Abuse intake
  - **Type:** feat · **Area:** audit · **Est:** M · **Deps:** P1-088 · **Prio:** P1
  - **Spec:** POST /v1/abuse (public, rate-limited, honeypot field): job id/reporter contact/reason creates row; ack email template.
  - **AC:**
    - [ ] honeypot drops bots

- [ ] **P1-090** — Abuse queue + actions
  - **Type:** feat · **Area:** audit · **Est:** M · **Deps:** P1-089 · **Prio:** P1
  - **Spec:** Admin queue (open/reviewing/resolved) with actions: reject job, suspend user, dismiss - all audited; SLA timestamps (72h target).
  - **AC:**
    - [ ] actions audited

- [ ] **P1-091** — Enforcement workflow
  - **Type:** feat · **Area:** audit · **Est:** S · **Deps:** P1-090 · **Prio:** P1
  - **Spec:** Warning to suspension to termination ladder helpers + template emails; suspension effects (sessions killed, jobs held) tested.
  - **AC:**
    - [ ] ladder e2e green

- [ ] **P1-092** — Audit/abuse suite
  - **Type:** test · **Area:** audit · **Est:** M · **Deps:** P1-091 · **Prio:** P1
  - **Spec:** Matrix-driven: every privileged action audited; abuse lifecycle; retention prunes per NFR-40.
  - **AC:**
    - [ ] suite green

- [ ] **P1-093** — Abuse ops playbook
  - **Type:** docs · **Area:** audit · **Est:** S · **Deps:** P1-092 · **Prio:** P1
  - **Spec:** Triage checklist, evidence preservation (artifacts within TTL), DMCA-style takedown response stub.
  - **AC:**
    - [ ] playbook committed

---

## E1.11 — Onboarding & consent (area: onboarding)

- [ ] **P1-094** — Attestation capture
  - **Type:** feat · **Area:** onboarding · **Est:** M · **Deps:** — · **Prio:** P1
  - **Spec:** Attestations table + service: required at upload-create and referenced at job-create; text versioned; ip_hash; client web/api-key; immutable rows.
  - **AC:**
    - [ ] job without attestation rejected (PF-4001)

- [ ] **P1-095** — Consent versioning
  - **Type:** feat · **Area:** onboarding · **Est:** M · **Deps:** P1-094 · **Prio:** P1
  - **Spec:** tos_version + aup_version on users; accept at register; bump flow re-accept banner (config-driven); audited.
  - **AC:**
    - [ ] bump forces re-accept

- [ ] **P1-096** — Onboarding checklist API
  - **Type:** feat · **Area:** onboarding · **Est:** S · **Deps:** P1-095 · **Prio:** P1
  - **Spec:** GET /v1/me/onboarding: steps (verify email, accept ToS, first upload, first job, optional api key); consumed by web; completion events.
  - **AC:**
    - [ ] steps transition correctly

- [ ] **P1-097** — Welcome email
  - **Type:** feat · **Area:** onboarding · **Est:** S · **Deps:** P1-096 · **Prio:** P1
  - **Spec:** Template + send on verification; quickstart link + honest quality-promise one-liner.
  - **AC:**
    - [ ] copy passes honesty rule

- [ ] **P1-098** — Attestation admin review
  - **Type:** feat · **Area:** onboarding · **Est:** M · **Deps:** P1-097 · **Prio:** P1
  - **Spec:** Admin list + flag (velocity anomalies) + link to abuse queue; read-model only v1.
  - **AC:**
    - [ ] anomalies flagged

- [ ] **P1-099** — Consent/attestation tests
  - **Type:** test · **Area:** onboarding · **Est:** M · **Deps:** P1-098 · **Prio:** P1
  - **Spec:** Job without attestation impossible; version bump forces re-accept; immutability enforced.
  - **AC:**
    - [ ] suite green

- [ ] **P1-100** — Onboarding spec
  - **Type:** docs · **Area:** onboarding · **Est:** S · **Deps:** P1-099 · **Prio:** P1
  - **Spec:** UI flow contract for Phase 4 (screens, states), copy points, drop-off metric events list.
  - **AC:**
    - [ ] spec committed

---

## E1.12 — Email polish (area: mailer)

- [ ] **P1-101** — Weekly digest email
  - **Type:** feat · **Area:** mailer · **Est:** S · **Deps:** — · **Prio:** P1
  - **Spec:** Digest template (usage, plan, tips) behind flag OFF default; opt-out in user settings.
  - **AC:**
    - [ ] flag respected

- [ ] **P1-102** — Quota/billing email set
  - **Type:** feat · **Area:** mailer · **Est:** S · **Deps:** P1-101 · **Prio:** P1
  - **Spec:** quota-80, quota-100, payment-failed, subscription-activated templates wired to events.
  - **AC:**
    - [ ] triggers verified

- [ ] **P1-103** — Unsubscribe/preferences
  - **Type:** feat · **Area:** mailer · **Est:** S · **Deps:** P1-102 · **Prio:** P1
  - **Spec:** Settings jsonb prefs; transactional exempt; List-Unsubscribe header for future marketing.
  - **AC:**
    - [ ] prefs honored

- [ ] **P1-104** — Email regression suite
  - **Type:** test · **Area:** mailer · **Est:** S · **Deps:** P1-103 · **Prio:** P1
  - **Spec:** All templates snapshot-tested; link tokens single-use; no PII in subject lines.
  - **AC:**
    - [ ] suite green

- [ ] **P1-105** — Email matrix docs
  - **Type:** docs · **Area:** mailer · **Est:** S · **Deps:** P1-104 · **Prio:** P1
  - **Spec:** Table: trigger, template, cap, audience; ops notes.
  - **AC:**
    - [ ] matrix committed

---

## E1.13 — Support tooling (area: support)

- [ ] **P1-106** — Support user view
  - **Type:** feat · **Area:** support · **Est:** S · **Deps:** — · **Prio:** P1
  - **Spec:** Read-only user detail endpoint (support role): profile, plan, usage, recent jobs, session meta; no secrets.
  - **AC:**
    - [ ] role-gated + no secrets

- [ ] **P1-107** — Manual quota grant
  - **Type:** feat · **Area:** support · **Est:** S · **Deps:** P1-106 · **Prio:** P1
  - **Spec:** POST /v1/admin/users/{id}/quota-grant {images?, video_seconds?, ttl_hours, reason}: audited, expiring, honored by quota middleware.
  - **AC:**
    - [ ] expiry honored

- [ ] **P1-108** — Refund tool stub
  - **Type:** feat · **Area:** support · **Est:** S · **Deps:** P1-107 · **Prio:** P1
  - **Spec:** Guided checklist endpoint returning Stripe dashboard deep-link + required fields; actual refund stays in Stripe UI; audited note.
  - **AC:**
    - [ ] checklist complete

- [ ] **P1-109** — User lookup helper
  - **Type:** feat · **Area:** support · **Est:** S · **Deps:** P1-108 · **Prio:** P1
  - **Spec:** Resolve by email/id/job-id to user (support role); audited access log for privacy.
  - **AC:**
    - [ ] lookups audited

- [ ] **P1-110** — Ticket notes
  - **Type:** feat · **Area:** support · **Est:** S · **Deps:** P1-109 · **Prio:** P1
  - **Spec:** Admin-only append-only notes on users (jsonb list), audit-tracked; support flow uses them.
  - **AC:**
    - [ ] append-only + audited

- [ ] **P1-111** — Support suite
  - **Type:** test · **Area:** support · **Est:** S · **Deps:** P1-110 · **Prio:** P1
  - **Spec:** Role enforcement, audit rows, quota-grant expiry honored.
  - **AC:**
    - [ ] suite green
