# API Spec v1 (canonical)

Conventions first; endpoint tables are the contract. OpenAPI is generated from code (utoipa) — this file is the source of truth the code must match.

## Conventions

- Base: `/v1`. JSON bodies. Timestamps ISO-8601 UTC. IDs: UUIDv7 strings.
- Auth: `Authorization: Bearer <jwt>` (web + API keys: `Bearer pf_live_...`), or web session cookie (httpOnly refresh + in-memory access).
- Errors: RFC 7807 `application/problem+json`:
  ```json
  { "type": "https://docs.pureframe.dev/errors/PF-2001", "title": "unsupported_media_type",
    "status": 415, "code": "PF-2001", "detail": "magic bytes indicate SVG; SVG is not accepted",
    "request_id": "01J..." }
  ```
- Validation failures are 422 `PF-2002` (`validation_failed`) with
  `field_errors` — one entry per failed field, in request order:
  ```json
  { "type": "https://docs.pureframe.dev/errors/PF-2002", "title": "validation_failed",
    "status": 422, "code": "PF-2002", "detail": "the request failed validation",
    "field_errors": [{ "field": "email", "code": "invalid_email",
      "message": "must be a valid email address" }] }
  ```
  `field_errors` is present only on validation rejections; every
  other error body carries exactly the RFC 7807 fields above.
  Messages are generic (they never echo the offending value), so
  the list is safe in production. Rules live in `detag-validator`
  (email shape, filename sanitize, public-only URL); handlers take
  `ValidatedJson<T>` (see `04-ENGINEERING/04-adding-an-endpoint.md`).
- Error code ranges: PF-1xxx auth/session, PF-2xxx upload/media, PF-3xxx quota/billing, PF-4xxx engine/quality/compliance, PF-5xxx system.
- Rate limits: headers `X-RateLimit-Limit` (burst), `-Remaining`, `-Reset` (unix time when one token is available again). Overage = 429 + problem+json (PF-3007) + `Retry-After` (seconds until one token). Scopes (P0-103): per-IP and per-route (`METHOD:/v1/<segment>`) token buckets; per-user / per-API-key scopes land with Phase 1 auth. Health/meta routes are exempt. Defaults: IP 120 burst / 120 per min, route 600 burst / 600 per min (`PF_RATE_LIMIT_*`).
- Body caps per route class (P0-104): auth + meta 64 KiB, jobs + admin 1 MiB, billing (webhooks) 256 KiB; over-cap = 413 + problem+json (PF-2003). Uploads are exempt — bytes never transit the api (presigned S3 PUTs).
- Dimension caps at job-create (P0-104, FR-U-04): each side at most 8000 px, total at most 64 MP; over-cap = 413 + problem+json (PF-2005). The job-create handler checks the upload row's probed dimensions before queuing any work (Phase 2 hook; the guard lives in `detag-core::media::validate_dimensions`).
- Idempotency: `Idempotency-Key` header honored on `POST /v1/jobs`, `POST /v1/uploads`, payment endpoints (24 h window).
- Pagination: cursor-based — `?limit=&cursor=`, response `{"items": [...], "next_cursor": "..."}`.
- Quota headers on job-creating routes: `X-Quota-Images-Remaining`, `X-Quota-Video-Seconds-Remaining`.

## Endpoints

### Health & meta
| Method | Path | Auth | Notes |
| --- | --- | --- | --- |
| GET | `/healthz` | none | liveness |
| GET | `/readyz` | none | readiness: db, redis, storage, models |
| GET | `/metrics` | internal network | Prometheus (not exposed publicly) |

### Auth
| Method | Path | Auth | Body → Response |
| --- | --- | --- | --- |
| POST | `/v1/auth/register` | none | email, password, accept_tos_version → 201 user + verification email sent |
| POST | `/v1/auth/verify-email` | none | token → 204 |
| POST | `/v1/auth/resend-verification` | none | email → always 204 (no enumeration) |
| POST | `/v1/auth/confirm-email-change` | none | token → 204 (swap to pending address; P1-006 — not in the original spec, same as resend) |

Verify behavior (P1-002, FR-A-01): the token is a 32-byte
random string, base64url-encoded (43 characters), valid 24
hours, single-use, stored as its sha256 only — the raw token
exists only in the outbox payload (`{user_id, token}`) and
in the email itself. Consuming is one atomic
`UPDATE ... SET used_at = now() WHERE ... AND used_at IS NULL
AND expires_at > now()` (the database decides single-use,
not the app): unknown or already-used token → 401 `PF-1010`
`invalid_token`; expired token → 401 `PF-1011`
`expired_token`. Success sets `email_verified_at` and audits
`verify-email.success` in the same transaction. Minting a
token deletes the user's previous unused tokens — one live
token per user, so a resend invalidates the earlier link.
Resend: at most 3 tokens minted per user per hour (counted
from the token table, not from emails sent); unknown email,
already-verified user, and over-limit resend all answer 204
with no mail (no enumeration, mirroring forgot-password),
audited as `verify-email.resend` only when a mail is actually
queued. Unverified users cannot create jobs: the job-create
handler calls the `require_verified` guard first — `PF-1009`
`email_not_verified`, 403. (Note: `POST
/v1/auth/resend-verification` is not in the original spec —
the spec says "resend rate-limited (3/hour)" but names no
route; P1-002 added it.)

Login behavior (P1-003, FR-A-01): `POST /v1/auth/login`
takes email + password and returns 200 `{access_token,
token_type: "Bearer", expires_in: 900}` plus the
`detag_refresh` cookie (`HttpOnly; Secure; SameSite=Lax;
Path=/v1/auth; Max-Age=2592000` — 30d, matching the session
row). The access token is an HS256 JWT (`sub` = user id,
`role` = user role, 15 min); the refresh token is a 32-byte
random string, stored as its sha256 only, one session row
per login with a fresh `family_id` and device `{ua,
ip_hash}` (the user-agent + the sha256 of the socket IP).
No enumeration: unknown email and wrong password answer the
same 401 `PF-1001` with the taxonomy's static detail — the
bodies are byte-identical. Constant-time verify: an unknown
email still costs one Argon2id verify, against the state's
precomputed dummy hash (one hash at boot, same params). A
malformed stored hash is a 500, never "wrong password".
Login does NOT require a verified email — verification gates
job creation (PF-1009), not authentication. Login DOES block
a suspended account (P1-008): after a correct password the
service returns 403 `PF-1003` (`account_suspended`) and
audits `login.failure` with reason `suspended` (internal
only, no lockout counter recorded — the account is already
out). A wrong password is still the generic 401, so probing
an address learns nothing about whether it is suspended. A
suspended user who is ALSO locked gets the 423 first (the
lockout check runs before the password verify). A login
cancels a pending deletion (P1-007) — the grace ends in the
same transaction as the new session. Rate limits:
per-IP via the middleware bucket (P0-103) + per-account via
the `login_account` bucket (10 burst / 1 per min,
`PF_RATE_LIMIT_LOGIN_ACCOUNT_*`, keyed by the sha256 of the
normalized email), checked BEFORE the password verify — a
denial is 429 `PF-3007` with `Retry-After`. Redis down fails
open (warn logged, P0-103); the durable lockout is P1-004.
Lockout (P1-004, FR-A-01): `login_attempt_counters` is the
durable lockout of record — one row per (scope, key), scope
`account` (sha256 of the normalized email) or `ip` (sha256
of the client IP, the same XFF-first identity the rate
limiter uses). Five failures inside a 15-minute window lock
the account AND the IP: the first lock is 60 s, then the
duration doubles per further failure (120, 240, 480, 960,
1920), capped at 3600 s. The lock is checked BEFORE the
password verify — a locked login is 423 `PF-1008`
`account_locked` with `Retry-After` (the later of the
account and IP locks) and is NOT recorded, so hammering a
locked account cannot extend the lock (the race window is
real: attempts that passed the check before the lock was set
still record, and extend it — they were genuine attempts).
Failures record in one transaction: both counters (atomic
`SELECT ... FOR UPDATE`, so concurrent failures serialize
and no increment is lost) + the `login.failure` audit. On
the first lock the notification mail is queued in the outbox
(template `lockout-notice`, payload `{user_id}`, recipient
as sha256 only) + `login.locked` is audited — once per lock
episode; extensions do not re-notify. A success resets both
counters (the reset is in the session transaction); a
password reset clears the account counter (the hook exists —
the reset-password ticket wires the call); registering an
email clears any counter the address accumulated before it
existed (an attacker cannot pre-lock an address the owner
registers later). The counters live in Postgres, not Redis —
a Redis flush never silently unlocks an account under
attack. The IP counter is skipped when the client is
`"unknown"` (no IP identity to lock — oneshot tests).
Audit: `login.attempt` (anonymous, email hash) +
`login.success` (in the session transaction) +
`login.failure` (anonymous, email hash + `reason`:
`unknown_email` / `wrong_password` — internal only) +
`login.locked` (on the first lock, with `locked_until`).

Register behavior (P1-001, FR-A-01): the email is trimmed +
lowercased before storage (the column is citext, so the match
is case-insensitive either way); the password is Argon2id-hashed
(params from `PF_ARGON2_*`, OWASP defaults m=19 MiB/t=2/p=1) and
never stored, logged, or echoed — the 201 body is the user
without any hash field. Password rule: minimum 12 characters
(NIST SP 800-63B), never trimmed. `accept_tos_version` must equal
the current version (`PF_TOS_CURRENT_VERSION`, default "v1") and
is recorded on the user row. A disposable-email domain (config
`PF_DISPOSABLE_EMAIL_DOMAINS`) is a 422 field error
(`disposable_email`); a wrong ToS version is a 422 field error
(`tos_version_mismatch`). A duplicate email is 409 `PF-2011`
Conflict with the generic `conflict` body — the body never names
the email (no enumeration detail), though the status itself
distinguishes duplicate (409) from new (201). The verification
mail is *queued* in the outbox (template `verify-email`, payload
`{user_id, token}` — the token is minted by P1-002's
`issue_verification`, recipient as sha256 only) in the same
transaction as the user row. Audit: `register.attempt` (anonymous,
email as sha256) + `register.success`, in the same transaction as
the user row and the outbox enqueue.

Profile behavior (P1-005, FR-A-01): `GET /v1/me` returns the
profile (id, email, display name, verification state, role,
status, created at) + the current plan (the live subscription's
plan, else Free) + today's quota snapshot (the two daily
buckets: limit / used / remaining). `PATCH /v1/me` sets the
display name and/or records an email-change intent. Both routes
take the auth extractor's Bearer leg (below) — no credential,
a malformed JWT, or a JWT for an unknown user is 401 `PF-1001`;
an expired JWT is 401 `PF-1011`. Display name rule: trim, 1–100
characters, no control characters (422 `required` / `too_long` /
`invalid_display_name`). An `email` value stores `pending_email`
only — it is validated (shape + disposable denylist, the same
rule as registration), must differ from the current email (422
`email_unchanged`), and is duplicate-checked against other
accounts (409 `PF-2011`, the same enumeration trade-off as
registration: the check is a courtesy, the swap in P1-006
enforces uniqueness via the citext unique constraint on
`email`). No mail is sent and no swap happens in P1-005 —
P1-006 mints the verification token, notifies the old address,
and swaps on verify. `email: null` clears a pending intent;
`display_name: null` clears the name; absent fields leave the
value unchanged; a PATCH with no fields is a no-op 200 (no
write, no audit). The update + the `profile.update` audit are
one transaction; the audit's `after` carries only what changed
— the display name plainly, the pending email as sha256 (the
P1-001 rule: no raw email in audits). GET is not audited (reads
aren't audited in this codebase). The quota snapshot is computed
by the shared entitlements service from the plan's entitlements
jsonb + today's `usage_counters` row (no row = zero usage) —
the quota middleware (P1-051) MUST reuse that service, so the
snapshot and the middleware agree by construction; the AC
"snapshot matches quota middleware" is verified by P1-051 when
the middleware lands (there is no middleware to compare against
yet — the same honest-scope pattern as P0-104). Serde trap,
found by the failing test: a double `Option<Option<String>>`
PATCH field does NOT distinguish JSON `null` from a missing
field with plain derive (`null` deserializes as the outer
`None`) — the fields carry an explicit `deserialize_with`
that maps `null` to `Some(None)` (clear) and leaves missing
as `None` (unchanged).

Auth extractor, Bearer leg (P1-005, FR-A-02): the extractor
verifies a `Bearer <jwt>` (HS256 signature + expiry, leeway 0)
against `PF_JWT_SECRET`, loads the `sub` as a user, and yields
a verified context carrying that user + the role from the user
row. A Bearer API key or a session cookie is 401 `PF-1001`
for now (no key table is read yet; the refresh cookie is not
an access credential — the refresh rotation is P1-014, the
cookie leg is P1-016, which also owns precedence + CSRF posture
docs). User status is NOT checked here — suspension revokes
sessions at suspend time (P1-008); a token issued before
suspension staying valid until expiry (≤ 15 min) is that
ticket's design space, not this extractor's.

Email change (P1-006, FR-A-01): setting a NEW `pending_email`
via `PATCH /v1/me` triggers, in the same transaction as the
profile update + audit, a change token (single-use, 24h,
hash-only, purpose `email_change` — the token table's
`purpose` column, migration 20261113050515) and TWO mails:
the verification mail to the NEW address (template
`email-change-verify`, payload `{user_id, token}`) and the
takeover alert to the OLD address (template
`email-change-alert`, payload `{user_id}` — no token, and the
new address never appears in it). `POST
/v1/auth/confirm-email-change` consumes the token and swaps
the email to the pending address (clearing the intent,
setting `email_verified_at` — the token was delivered to the
new address, so the new address is verified by it) in one
transaction (consume + swap + `email.change` audit, the audit
carrying both addresses as sha256 only). Unknown / used /
wrong-purpose token → 401 `PF-1010` (a registration token
presented here is invalid — the purposes are never
interchangeable); expired → 401 `PF-1011`; an address taken
by another account while it was pending → 409 `PF-2011` (the
swap is the enforcement point for the P1-005 intent-time
duplicate check, which was only a courtesy — the failed swap
rolls the whole transaction back, so the token stays unused
and the intent stays pending). A token whose intent was
cleared meanwhile → 401 `PF-1010` (the swap's
`pending_email IS NOT NULL` guard finds no row). Clearing a
pending intent deletes the live change token (purpose-scoped
— a registration token survives, so clearing must not force
the user to re-verify their current address). Re-PATCHing the
SAME pending address is a no-op (no new token, no new mails)
— re-trigger by clearing + re-setting. One-live-token
consequences: a resend-verification mints a `verification`
token and kills a live change token (the pending intent
survives on the user row); an email-change PATCH kills a
live registration token (after the swap the user is
verified, so the registration token is moot — if the change
is abandoned instead, the user must resend verification).
The outbox accumulates one mail per mint — a superseded
mail stays queued with a dead token (its link 401s), the
same as the resend flow; only the token row is the live
credential.

Account deletion (P1-007, FR-A-01): `DELETE /v1/me` requests
deletion — it does NOT delete the account. The account enters
the 30-day grace (`status = 'deletion_pending'`, `deleted_at`
= now; `GET /v1/me` shows both, so the client can display the
grace), and the nightly purge hard-deletes it 30 days later
unless a login cancels it. The request + the
`account.delete_requested` audit + the confirmation mail
(template `account-deletion`, payload `{user_id}`, to the
address on record) are one transaction. A second DELETE while
already pending is a no-op 204 — the grace runs from the FIRST
request (no refreshed `deleted_at`, no second audit row, no
second mail). A login during the grace cancels the deletion
(`status` back to `active`, `deleted_at` cleared,
`account.delete_cancelled` audited) in the same transaction as
the new session — a second concurrent login that finds the
account already active is not an error. The purge itself is
the gc binary (`detag-gc --purge-users`, dry-run by default):
one transaction per user (the `account.purged` audit row +
the hard delete), the run continuing past per-user failures.
The cascade is the data-model doc's: sessions, jobs (+
artifacts, job events, quality reports via jobs), uploads,
subscriptions, usage counters, idempotency keys and
verification tokens are `ON DELETE CASCADE`; audit rows
survive with `actor_user_id` intact (no FK on the actor —
migration 20261113062352 dropped it: the actor is a
historical fact, and the FK's `ON DELETE SET NULL` was an
UPDATE the append-only trigger forbids, so the purge could
never delete a user with audit rows); outbox rows survive
(they are addressed by hash — a queued mail to a purged
account is undeliverable, and its link 401s because the
token row is gone with the user). The purge does NOT delete
S3 objects itself: the row delete makes them orphans and the
next gc sweep deletes them (rows first, objects after, via
the sweep, is the safe order). The cascade needed two more
corrections in the same migration: the `job_events` /
`attestations` append-only triggers are UPDATE-only now
(history can never be rewritten; DELETE is the purge's
erasure path — keeping those rows would retain the user's
data after erasure, and no app code deletes them directly),
while the `audit_logs` trigger keeps its DELETE branch (the
audit trail is never deleted). The purge test proved the
conflict first (purge_one failed with "table audit_logs is
append-only") — the migration is the fix, with its own live
test (`migration_purge_cascade_triggers.rs`).
| POST | `/v1/auth/login` | none | email, password → access_token, expires_in (+ sets refresh cookie) |
| POST | `/v1/auth/refresh` | cookie | → new access token, rotated refresh (reuse ⇒ revoke all sessions PF-1007) |
| POST | `/v1/auth/logout` | any | → 204, revoke presented session |
| POST | `/v1/auth/forgot-password` | none | email → always 204 (no enumeration) |
| POST | `/v1/auth/reset-password` | none | token, new_password → 204, revoke all sessions |
| GET | `/v1/me` | user | profile + plan + quota snapshot |
| PATCH | `/v1/me` | user | email change / profile fields (email change re-verifies) |
| DELETE | `/v1/me` | user | request deletion → 204 (30 d grace, purge deletes) |
| GET | `/v1/me/sessions` | user | list sessions (device, created, last_seen) |
| DELETE | `/v1/me/sessions/{id}` | user | revoke one |
| POST | `/v1/me/export` | user | request data export (async, emailed link) |

### API keys
| Method | Path | Notes |
| --- | --- | --- |
| GET | `/v1/api-keys` | list (prefix + last4 only) |
| POST | `/v1/api-keys` | create; full secret shown once; scope: `jobs:write`, `jobs:read`, `webhooks:manage` |
| DELETE | `/v1/api-keys/{id}` | revoke |

### Uploads
| Method | Path | Notes |
| --- | --- | --- |
| POST | `/v1/uploads` | body: filename, bytes, kind(image/video) → upload id, part size, presigned part URLs (multipart), attestation required in body |
| POST | `/v1/uploads/{id}/complete` | parts + sha256 verify → `ready`; server re-sniffs magic bytes + extracts metadata |
| POST | `/v1/uploads/{id}/abort` | cancel multipart, cleanup |
| GET | `/v1/uploads/{id}` | status, metadata summary, detected kind |

### Jobs
| Method | Path | Notes |
| --- | --- | --- |
| POST | `/v1/jobs` | body: upload_id, mode(auto/manual/detect_only/metadata_only), hints[], format_policy, strip_metadata, provenance(preserve/strip-gated), webhook_url? → 202 job; quota pre-check |
| GET | `/v1/jobs` | cursor page; filters: status, kind, created_after |
| GET | `/v1/jobs/{id}` | full job incl. stage, progress, quality report when done |
| POST | `/v1/jobs/{id}/cancel` | queued/running |
| POST | `/v1/jobs/{id}/retry` | failed/quality_failed (counts against quota) |
| GET | `/v1/jobs/{id}/events` | SSE: `progress`, `stage`, `done`, `failed`; heartbeat 15 s |
| GET | `/v1/jobs/{id}/result` | signed output URL + report + artifacts list (detect_only: mask URL + detections) |
| GET | `/v1/jobs/{id}/report` | quality report JSON standalone |

### Billing
| Method | Path | Notes |
| --- | --- | --- |
| GET | `/v1/plans` | public plan matrix + quotas |
| GET | `/v1/billing/subscription` | current state |
| POST | `/v1/billing/checkout` | → Stripe Checkout URL (price_id from plan config) |
| POST | `/v1/billing/portal` | → Stripe portal URL |
| GET | `/v1/billing/invoices` | list |
| GET | `/v1/usage` | today + 30-day daily buckets (images, video-seconds) |
| POST | `/v1/webhooks/stripe` | provider-signed; idempotent (event table) |
| POST | `/v1/webhooks/razorpay` | flagged; HMAC verify |

### User webhooks (entitled plans)
| Method | Path | Notes |
| --- | --- | --- |
| GET/POST | `/v1/webhook-endpoints` | HTTPS-only URL, secret returned once, events subset |
| DELETE | `/v1/webhook-endpoints/{id}` | — |
| GET | `/v1/webhook-endpoints/{id}/deliveries` | status, response code, attempts |

Delivery: `PF-Signature: sha256=<hmac(secret, body+timestamp)>`, `PF-Timestamp` header, replay window 5 min, retries 1/5/60 min, then dead.

### Admin (role: admin; support role gets read-only subset)
| Method | Path | Notes |
| --- | --- | --- |
| GET | `/v1/admin/metrics/dashboard` | jobs today, success rate, p95, queue depth, MRR, DAU |
| GET | `/v1/admin/users?query=` | search, plan, status |
| POST | `/v1/admin/users/{id}/suspend` | body: reason; audited |
| POST | `/v1/admin/users/{id}/restore` | audited |
| POST | `/v1/admin/users/{id}/quota-grant` | temporary quota bump; audited |

Suspend/restore (P1-008, FR-A-01): `POST
/v1/admin/users/{id}/suspend` takes `{"reason": "..."}` (1–500
characters after trimming, no control characters — 422
`PF-2002` otherwise) and returns 200 with the updated user
(the `UserResponse` shape). The suspend is one transaction:
status `suspended` + every live session revoked
(`sessions.revoked_at` set — the refresh tokens die, so no
new access token can be minted) + every `queued` job held
(status `held`) + a `user.suspended` audit (actor = the
admin, `after` carries `{status, sessions_revoked,
jobs_held}`, the reason in the audit's `reason`). `POST
/v1/admin/users/{id}/restore` takes no body and returns 200
with the updated user: status `active`, held jobs released
back to `queued` (original `created_at` order — the hold is
a status flip, not a re-queue), `user.restored` audited
(`after` carries `{status: active, jobs_released}`).
Sessions stay revoked on restore — the user logs in again.
Both are idempotent no-ops when the account is already in
the target state (no write, no audit, no second mail).
A suspend cancels any pending deletion: `deleted_at` is
cleared, and the audit's `before` records the prior status +
`deleted_at` — the invariant is `deleted_at` is non-NULL
exactly when status is `deletion_pending`. Only `queued`
jobs are held — a job already running finishes; the Phase 2
intake/claim must treat `held` as "do not start" (a held job
is never claimed, and a restore releases it, so no job is
lost to the hold). No suspension mail is sent (the spec does
not ask for one). Access: both routes take the `AdminContext`
extractor — a verified non-admin gets 403 `PF-1002`, no
credential gets 401 `PF-1001`, an unknown target gets 404
`PF-2010`. Residual window: the access token already issued
is stateless and stays valid until it expires (≤ 15 min);
the extractor does NOT check status (that check is the
login's, plus the revocation) — the window closes at expiry
and cannot be reopened, because the refresh sessions are
dead and the next login is blocked.
| POST | `/v1/admin/users/{id}/impersonate` | reason; audited; returns scoped token |
| GET | `/v1/admin/jobs?status=&kind=` | browse + inspect raw params/report |
| POST | `/v1/admin/jobs/{id}/reprocess` · `/cancel` | audited |
| GET | `/v1/admin/dlq?queue=&limit=` | dead-letter queue: quarantined jobs with payload snapshot (P0-078 stub — Phase 4 wires UI) |
| GET/PUT | `/v1/admin/flags` | feature-flag editor |
| GET/PUT | `/v1/admin/plans` | plan/entitlement editor |
| GET | `/v1/admin/audit?actor=&action=` | audit query |
| GET | `/v1/admin/abuse-reports` | queue; POST resolve with action |
| GET | `/v1/admin/attestations?flagged=` | review queue |
| GET | `/v1/admin/system` | storage usage, backup status, model registry state |

### Abuse & compliance
| Method | Path | Notes |
| --- | --- | --- |
| POST | `/v1/abuse` | public intake (rate-limited): job id, reason → 201 ticket |
| POST | `/v1/jobs/{id}/attestation` | second explicit consent step for gated provenance operations |

## SSE event payloads

```
event: stage   data: {"job_id":"...","stage":"detect","progress":0.34}
event: done    data: {"job_id":"...","status":"succeeded","result_url":"/v1/jobs/{id}/result","gate":"pass"}
event: failed  data: {"job_id":"...","status":"quality_failed","code":"PF-4002","report_url":"..."}
```

## Outbound webhook payloads

`job.completed`, `job.failed`, `quota.exceeded`, `subscription.updated` — same JSON shape as the corresponding API resources plus `event_id` (uuid) for idempotent consumer handling.
