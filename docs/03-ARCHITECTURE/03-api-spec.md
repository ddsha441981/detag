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
- Rate limits: headers `X-RateLimit-Limit` (burst), `-Remaining`, `-Reset` (unix time when one token is available again). Overage = 429 + problem+json (PF-3007) + `Retry-After` (seconds until one token). The trio on a denial names the DENYING bucket — the middleware stamps its IP numbers on allowed responses only and never overwrites a service denial's trio (P1-020). Scopes (P0-103): per-IP and per-route (`METHOD:/v1/<segment>`) token buckets; per-account login (`login_account`, P1-003) + per-IP login (`login-ip`, P1-020) + per-email register (`register-email`, P1-020) + per-account reset (`reset-account`, P1-020 — the bucket exists, the reset flow P1-025 wires the check) are service buckets with their own key segments (one bucket, one key — two buckets never share a Redis key, P1-020). Per-API-key scope lands with Phase 1 auth (E1.4). Health/meta routes are exempt. Defaults: IP 120 burst / 120 per min, route 600 burst / 600 per min, login-account 10 / 1, login-IP 20 / 10, register-email 5 / 1, reset-account 10 / 1 (`PF_RATE_LIMIT_*`).
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
from `email_outbox` rows for the user + template, not from
token rows — minting deletes token history, so the token table
cannot count it; P1-002); unknown email,
already-verified user, and over-limit resend all answer 204
with no mail (no enumeration, mirroring forgot-password),
audited as `verify-email.resend` only when a mail is actually
queued. Send caps are a second, drain-side backstop (P1-031):
the drain sends at most 3 verification mails (`verify-email`
+ `email-change-verify`) and 3 password-reset mails to one
recipient address per hour, and no marketing mail at all
(cap 0) — counting sent outbox rows for the recipient hash +
class in the last hour; a 4th mail is parked (dropped, not
retried) and the trip is written to the abuse audit log
(`email.rate_limit_tripped`). Transactional alerts
(`lockout-notice`, `new-login-digest`, …) are never capped.
Unverified users cannot create jobs: the job-create
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
row). The access token is an HS256 JWT, 15 min, with the
locked P1-013 claims shape: `sub` = user id, `role` = user
role, `ver` = 1, `jti` = one UUID per issuance,
`iss` = `"detag"`, `aud` = `"detag-api"`, `exp` / `iat` =
unix seconds, `impersonator_id` only on an impersonation
token (P1-010); the JOSE header carries `kid` = the first 16
hex chars of the signing secret's sha256. The verifier
enforces `iss` + `aud` (wrong value → 401 `PF-1010`),
`ver` = 1, and a `kid` that matches this secret (a foreign
`kid` → 401 `PF-1010`); a token issued before P1-013
(`{sub, role, exp, iat}` only, no `kid`) still verifies —
the new fields default on decode. The refresh token is a 32-byte
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
normalized email) + per-IP via the `login_ip` bucket (P1-020:
20 burst / 10 per min, `PF_RATE_LIMIT_LOGIN_IP_*`, keyed by
the sha256 of the client IP — bounds credential-stuffing runs
from one IP against many accounts), checked in that order
AFTER the lockout check (P1-020: the lockout is checked
first, so a locked login answers 423 and never spends a
bucket token — a 429 must not mask the 423) and BEFORE the
password verify — a denial is 429 `PF-3007` with
`Retry-After` and the denying bucket's trio. Redis down fails
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

Refresh rotation (P1-014, FR-A-01): `POST /v1/auth/refresh`
takes no body — the refresh token comes from the
`detag_refresh` cookie only (a client that lost the cookie
logs in again). Presenting a live refresh token mints a new
access token (15 min, the same `LoginResponse` shape) + a
new refresh token in the SAME family, sets the new cookie
(same attrs as login), and marks the old row replaced
(`revoked_at` + `replaced_by` = the new row's id) — one
transaction (new session row + replacement mark +
`session.refreshed` audit). The new session row carries
this request's device (`{ua, ip_hash}`) and a fresh 30-day
expiry. **Reuse:** presenting an already-replaced token is
401 `PF-1007` (`RefreshReuse`) — the whole family is
revoked, a `session.refresh_reused` audit is written
(`after` carries `{family_id, sessions_revoked}`), and a
security email is queued (`refresh-reuse-alert`, payload
`{user_id}` — the mailer does not exist yet, the outbox row
is the queue). A rotated row carries BOTH marks; a merely
revoked row (suspend, logout, family revocation) carries
`revoked_at` only — that difference is what distinguishes
reuse (PF-1007) from a dead token. A dead token — unknown
hash, revoked row, expired session — is the generic 401
`PF-1001` (the hash lookup must not distinguish "never
existed" from "dead"). Two concurrent presents of the same
token race: the replacement mark is conditional
(`replaced_by IS NULL`), so the loser runs the reuse path
instead of minting a second live session in the family. An
impersonation token can never refresh (no session row
behind it — the hash lookup fails with the generic 401). A
suspended user's token cannot refresh either (the suspend
revoked every session — no separate suspended check is
needed). The refresh token itself is never stored — only
its sha256 lives in the session row.

Logout (P1-017, FR-A-01): `POST /v1/auth/logout` revokes
the session behind the `detag_refresh` cookie — the
presented session — and clears the cookie (`Set-Cookie:
detag_refresh=; ...; Max-Age=0`). No cookie, or an unknown
hash → the generic 401 `PF-1001` (the lookup must not say
whether the session existed). A second logout of the same
session is a no-op 204 (no second audit — P1-008's
idempotent-no-op convention). A Bearer token alone cannot
log one session out: the access token carries no session
identity (its `jti` is per-issuance, not linked to a session
row) — that is why the logout row says "cookie", not "any".
`POST /v1/auth/logout-all` revokes EVERY session of the
user: the caller is the Bearer `AuthContext` (the access
token identifies the user, not the session, so the only safe
scope is every session), one transaction (revoke-all + the
`session.logged_out_all` audit with `after:
{sessions_revoked}` — the count is the sessions THIS call
revoked; a session the rotation already revoked is not
counted twice). A cookie-only caller gets 401 `PF-1001`
(the extractor rejects a bare cookie — P1-016): the web
client sends both, an API client sends the Bearer, and a
caller with only the cookie uses `/logout` for the single
session. Both logouts audit (`session.logged_out` with
`after: {session_id}`; `session.logged_out_all` with the
count) — no audit when nothing was revoked. A revoked
refresh token cannot rotate (the refresh service answers
the generic 401 `PF-1001` for a revoked row); a replaced
token presented after logout-all is still a reuse detection
(401 `PF-1007` — the row still carries `replaced_by`, and
logout-all does not erase the rotation history). The access
token already issued stays valid until it expires (≤ 15
min — the documented residual window: logout revokes the
SESSION, not the stateless token) — EXCEPT the token
presented to logout-all itself: logout-all denylists its
`jti` (P1-021), so that one token dies immediately; the
single logout (cookie-only, no token in hand) cannot do
the same, and its residual window stands.

Token revocation (P1-021, FR-A-01): a live access token can
die before its 15-minute expiry through two mechanisms, and
the extractor consults both on every authenticated request:
- the epoch (`users.revoked_before`, NULL = never revoked):
a token whose `iat` is at or before the epoch's second is
dead (`iat <= revoked_before.timestamp()`). The comparison
is `<=`, not `<`: `iat` is whole seconds while the epoch is
microsecond-precise, so a token minted in the same second
as the bump (before OR after it) shares the bump's second
— strict `<` against the floored second has a one-second
hole there (a pre-change token in that second survives;
caught by the revocation tests). A post-change token
minted in the same second is denied too and self-heals via
refresh (its refresh token is still valid; the next mint
lands in a later second). The epoch costs zero extra
queries (the column rides the user row the extractor
already loads). Only a password change bumps it — the
bump is in the same transaction as the session revocation
(P1-018's service), so the epoch and the revoked sessions
commit or roll back together and can never diverge.
- the jti denylist (Redis `pf:revocation:jti:{jti}` →
`"1"`, TTL = the token's remaining life, clamped to
[1, 900] s): a listed `jti` is dead. The only writer today
is `POST /v1/auth/logout-all`, which denylists the
PRESENTED token's `jti` (re-verified in the handler to read
its `jti` + `exp`); other devices' tokens live until
expiry — their jtis are unknown, and logout-all does NOT
bump the epoch (the spec names password-change as the
bumper; a logout-all epoch bump would kill every
outstanding access token and belongs to P1-022/P1-024 or
an owner decision). A token without a `jti` (pre-P1-013
legacy) skips the denylist check — the epoch still covers
it via `iat`. A Redis error on the READ fails OPEN (warn
logged — the P0-103 rule): the epoch is the durable
backstop, the denylist is the fast path; a Redis error on
the WRITE at logout-all is the same (warn + continue —
the sessions are already revoked, so no NEW token can be
minted; only the presented token's immediate death is
lost). Both rejections are 401 `PF-1010` — a revoked token
is indistinguishable from a forged one.
Perf (measured 13 Nov 2026, local Redis on this VM, 200
sequential calls): the denylist GET is mean ~127 µs, p50
~114 µs, p99 ~378 µs (miss: p50 ~111 µs, p99 ~500 µs) —
one GET adds ~0.1–0.5 ms to an authenticated request,
negligible next to the Postgres user lookup the extractor
already does; the p99 is unaffected in any meaningful
sense. The epoch check adds no query at all.

Revoke on password change (P1-018, FR-A-01): when the
password changes, every live session except the current
one is revoked (`sessions::revoke_all_except` — one
statement, `revoked_at` where NULL and `id <> keep`, the
count is the sessions THIS call revoked; a session the
rotation already revoked is not counted twice), audited
once as `session.revoked_on_password_change` (`after:
{sessions_revoked, kept_session_id}` — no audit when
nothing was revoked), and the digest mail is queued
(template `new-login-digest`, payload `{user_id,
sessions_revoked}` — the mailer renders "your password
changed; N other sessions were signed out" + the
recent-login list from the session rows at render time;
the rows are revoked, not deleted, so they are still
there to read). The mail is queued on EVERY password
change, even when nothing was revoked — it notifies about
the CHANGE, while the audit records the REVOCATION (two
different events). The "current" session is the one
behind the presented refresh cookie (read the way the
logout handler reads it), so the user is not signed out
of the device they are holding. The reset case passes no
current session — a reset happens logged-out, so every
session is revoked. The same call ALSO bumps the
revocation epoch (`users.revoked_before = now()`, P1-021)
in the same transaction — a password change kills the
access tokens, not just the sessions: every token issued
before the change dies at the extractor (`iat <= epoch`),
and the bump happens even when nothing was revoked (an
orphaned access token — its session already revoked, the
token still inside its 15 minutes — dies with it). The
mechanism is the shared service
`services::password::revoke_sessions_on_password_change`;
the reset flow (P1-025) calls it when it lands — the
forgot-password ticket landed the call (below) — and no
change-password route exists yet (when it lands it calls
the same service with the current session from the
presented cookie) — the same land-early pattern as
P1-004's lockout `reset`.

Forgot / reset password (P1-025, FR-A-01):
`POST /v1/auth/forgot-password` (`{email}`) is ALWAYS 204
— a known email, an unknown email, and an already-… all
answer 204 with an empty body; only the rate-limit 429
and a 500 vary the response. No enumeration, by
construction: the per-account reset bucket (`reset-account`,
10 burst / 1 per min, P1-020) is spent BEFORE the lookup,
keyed by the sha256 of the normalized email, so it trips
identically for known and unknown addresses, and a denied
request does no work at all; a Redis error fails OPEN
(P0-103). A known email mints a reset token (below) +
queues the reset mail (template `password-reset`, payload
`{user_id, token}`) + audits `password.reset_requested`
(actor None — the request is anonymous — target the
user, `after: {email_hash}`); an unknown email audits
the same action with only the email hash and mints
nothing. `POST /v1/auth/reset-password`
(`{token, new_password}`) consumes the token and sets
the new password: unknown / used / wrong-purpose token →
401 `PF-1010`; expired token → 401 `PF-1011`; success is
204. The token is single-use (the consume is atomic:
`used_at IS NULL AND expires_at > now()`), 30 minutes,
hashed (sha256 — the raw token lives only in the outbox
payload + the email), purpose `password_reset` (the
migration CHECK admits it; a registration or email-change
token is `InvalidToken` here and is NOT consumed, so it
still works for its own flow afterwards). One live token
per user (the P1-006 rule, applied across purposes):
minting a reset token deletes the user's previous UNUSED
tokens — a live verification or email-change token dies
with it; the pending intent survives (it lives on the
user row). On success the password is re-hashed with the
current Argon2 params, the lockout counter for the
account is cleared (P1-004's hook — a reset un-locks the
account), and EVERY session is revoked via the shared
service (the reset case: no current session — a reset
happens logged-out, so every session is revoked) — which
also bumps the revocation epoch, so pre-reset access
tokens die at the extractor, and queues the
`new-login-digest` mail. Two transactions, on purpose:
the token consume + password write + counter reset
commit together FIRST (a consumed token with an unchanged
password — or a changed password with a live token —
cannot exist); the session revocation runs second, in its
own transaction. If the revocation fails, the password is
already changed and the handler answers 500 — the caller
requests a fresh reset token (the forgot flow still
works; the account is not locked). The new password is
min 12 chars (the P1-001 rule; strength + blocklist land
with P1-026). The reset works for an unverified user (the
login gate is not the verification gate).

Session listing (P1-019, FR-A-01): `GET /v1/me/sessions`
lists the user's LIVE sessions (not revoked, not expired),
newest first — each item is `{id, device, ip_hash,
created_at, last_seen, expires_at}`. The `device` is the
parsed User-Agent label (`"Chrome on macOS"` — a small
heuristic table in `detag_core::user_agent`, never the raw
UA string); `ip_hash` is the sha256 of the client IP at
login (a hash, not PII); `last_seen` is the last time the
session was presented (NULL for rows created before the
P1-019 migration — no activity was recorded for them).
There is NO "current" flag: the access token carries no
session identity (P1-017's logout has the same
limitation), so the handler cannot know which row the
caller is — the user tells sessions apart by device +
created + last_seen. `last_seen` advances on refresh
presentation, throttled to one write per session per 5
minutes (`LAST_SEEN_THROTTLE_SECS`, a product-policy
constant — a normal refresh cycle is every 15 minutes, so
every normal refresh still records while a burst of
presents cannot cause a write per present). The touch runs
inside the rotation transaction, on the PRESENTED row
(the new row is created with `last_seen = now()` anyway);
a throttled-out touch is not an error. `DELETE
/v1/me/sessions/{id}` revokes one session (audited
`session.revoked`, `after: {session_id}`); unknown id, or
an id belonging to another user → 404 `PF-2010` (no
enumeration); already revoked → no-op 204.

Register behavior (P1-001, FR-A-01): the email is trimmed +
lowercased before storage (the column is citext, so the match
is case-insensitive either way); the password is Argon2id-hashed
(params from `PF_ARGON2_*`, defaults m=64 MiB/t=3/p=4 — P1-027) and
never stored, logged, or echoed — the 201 body is the user
without any hash field. Password rule (P1-001, extended P1-026):
minimum 12 characters (NIST SP 800-63B), never trimmed; the
whole password must not be one of the 10,000 most commonly used
passwords (422 `common_password` — the blocklist is zxcvbn's
embedded ranked passwords dictionary, top 10k by rank; no
external list is downloaded); zxcvbn score >= 3 (422
`too_weak` — the crate's own "too weak" line). The same three
rules gate the reset flow's new password (P1-025's
`POST /v1/auth/reset-password` — a weak new password is 422
before the token is consumed). Optional HIBP k-anonymity check
(P1-026, `PF_PASSWORD_PWNED_CHECK`, default off): when on, the
register + reset services send only the first 5 hex chars of the
password's SHA-1 to `PF_PASSWORD_PWNED_URL` (default
`https://api.pwnedpasswords.com`) and reject a password whose
suffix is in the returned range (422 `pwned_password`); any
transport error or non-2xx answer fails open (warn logged, the
registration/reset continues). Opt-in because it contacts a
third party per registration/reset — the full hash and the
password never leave the machine, but the 5-char prefix narrows
the password to ~2^20 candidates and reveals that a check
happened. `accept_tos_version` must equal
the current version (`PF_TOS_CURRENT_VERSION`, default "v1") and
is recorded on the user row. A disposable-email domain (config
`PF_DISPOSABLE_EMAIL_DOMAINS`) is a 422 field error
(`disposable_email`); a wrong ToS version is a 422 field error
(`tos_version_mismatch`). Rate limit (P1-020): the
`register_email` bucket (5 burst / 1 per min,
`PF_RATE_LIMIT_REGISTER_EMAIL_*`, keyed by the sha256 of the
normalized email), checked FIRST — before the policy checks,
the hash, and the audit, so a denied request does no work at
all; a denial is 429 `PF-3007` with `Retry-After` and the
bucket's trio, and duplicate attempts spend the bucket too. A duplicate email is 409 `PF-2011`
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

Auth extractor, Bearer token (P1-005, FR-A-02): the extractor
verifies a `Bearer <jwt>` (HS256 signature + expiry +
not-before, leeway 0) against `PF_JWT_SECRET`, loads the
`sub` as a user, and yields a verified context carrying that
user + the role from the user row. The role is re-read from
the database on EVERY request (P1-022): a role change takes
effect immediately on the same token — a demoted admin loses
admin access at once (403 `PF-1002`) while their user routes
still work, a promoted user gains admin access at once, and
a token whose embedded `role` claim says "admin" for a plain
user is still treated as that user (the claim is never
trusted — proven by test). A tampered token (flipped payload
character, or the same claims signed with another secret) is
401 `PF-1010`; a token whose `nbf` is in the future is
immature — 401 `PF-1010`, never `PF-1011` (expired). The
locked claims shape is now nine keys (P1-022 adds `nbf` =
`iat` on every minted token; a pre-P1-022 token without
`nbf` still verifies — it defaults to 0, long past). A Bearer
API key is 401 `PF-1001` (no key table is read yet — the
API-key leg is the E1.4 integration point and its
verification lands with P1-034, "Key auth extractor"). A
session cookie, alone, is 401 `PF-1001` — FINAL (P1-016):
the `detag_refresh` cookie holds a refresh token, which is
not an access credential. User status is NOT checked here —
suspension revokes sessions at suspend time (P1-008); a token
issued before suspension staying valid until expiry (≤ 15
min) is that ticket's design space, not this extractor's.

Cookie + bearer dual auth (P1-016, FR-A-02): the web client
holds TWO things — the access token in memory (sent as
`Authorization: Bearer <jwt>` on API calls, exactly like an
API client) and the refresh token in the httpOnly cookie
(`detag_refresh`: `HttpOnly; Secure; SameSite=Lax;
Path=/v1/auth; Max-Age=2592000`). The cookie exists only for
`POST /v1/auth/refresh` (P1-014). Precedence is Bearer wins:
a request carrying both is authenticated by the Bearer token
and the cookie is ignored, never validated as an access
credential (a forged cookie alongside a valid Bearer token
does not fail the request; a valid refresh token alongside a
Bearer token adds nothing). The cookie's `Path=/v1/auth`
means a browser sends it only to `/v1/auth/*` — never to a
protected route (`/v1/me`, `/v1/admin/*`) — so the extractor
never sees it there from a real browser at all. CSRF
posture: `SameSite=Lax` means a cross-site `fetch`/XHR POST
does not carry the cookie, so `POST /v1/auth/refresh` from
another origin has no token to rotate; a top-level GET
navigation DOES carry it — but there is no state-changing
GET (every mutation is POST/PATCH/DELETE, every GET is a
read, and `GET /v1/auth/refresh` is 405), so a navigated GET
cannot rotate a session or change anything. `HttpOnly` keeps
the refresh token out of JavaScript (defense-in-depth, not a
cure for XSS — same-origin XSS can still call `/refresh`
itself); `Secure` keeps it off plain HTTP in production
(`http://localhost` is a secure context, so dev still works).
The access token is never a cookie: a stolen one is worth at
most 15 minutes, and the refresh token that mints the next
one never appears in a request body or a log line.

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
| POST | `/v1/auth/logout` | cookie | → 204, revoke presented session; clears the refresh cookie |
| POST | `/v1/auth/logout-all` | bearer | → 204, revoke every session; clears the refresh cookie |
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
| POST | `/v1/api-keys` | create; full secret shown once; scope: `jobs:write`, `jobs:read`, `webhooks:manage`. Secret is `pf_live_` + 24 random bytes base64url (32 chars); only its sha256 is stored (`secret_hash`, unique). Response 201 carries the full secret once (id, name, prefix, secret, scopes, created_at); listings show prefix + last4 only. Name: display-name rules (1–100 chars, no control chars); scopes: ≥1, each known, no duplicates — else 422 `PF-2002`. Max 10 live keys per user (revoked do not count) — the 11th is 409 `PF-2011`; the count + insert run under the user's row lock so concurrent creates cannot overshoot. Creation is audited (`api_key.created`, no secret/hash). No plan gate and no verified-email requirement in P1-033 (see ASSUMPTION-015 in ISSUES.md). |
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
| GET | `/v1/admin/users?query=` | search (email/name/id prefix), cursor pagination, plan + status + usage today; admin + support |
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

User search (P1-009, FR-A-01): `GET /v1/admin/users` lists
users in `created_at` order, oldest first (the list is a
keyset, not an offset). `?query=` is a prefix filter,
case-insensitive, on email, display name, or id (a `%`, `_`,
or `\` in the query matches literally, never as a wildcard);
blank or absent lists everything. `?limit=` clamps to
1..=100 (default 20; 0 is a limit, not an error — it clamps
to 1). `?cursor=` is the opaque keyset from the previous
page's `next_cursor`; a tampered cursor is 422 `PF-2002`
(never a silent restart from the first page). Response is
the pagination shape (`{"items": [...], "next_cursor":
"..."}` — `next_cursor` omitted on the last page). Every row
carries `id, email, display_name, role, status,
email_verified_at, tos_version, created_at, updated_at`
plus `plan` (the plan code — the live subscription's plan,
else `free`) and `images_used_today` /
`video_seconds_used_today` (the UTC day's counters, zeros
when the user has used nothing today; the read never creates
the counter row). One query, no N+1 — the plan and the usage
come back in the same rows. Access: the `AdminReadContext`
extractor — admin + support (support is read-only: it may
use the readable admin routes, never a write); a verified
plain user gets 403 `PF-1002`, no credential gets 401
`PF-1001`. Keyset detail: the cursor stores Unix millis
(P0-068) while `created_at` carries microseconds, so the
repo truncates `created_at` to milliseconds on both sides of
the comparison and orders by the truncated key — otherwise
the cursor's own row would be re-included (its microseconds
sit past the truncated millisecond) or a row created in the
same millisecond with a smaller id would be skipped.

Impersonation (P1-010, FR-A-01): `POST
/v1/admin/users/{id}/impersonate` takes `{"reason": "..."}`
(1–500 characters after trimming, no control characters —
422 `PF-2002` otherwise) and returns 200
`{access_token, token_type: "Bearer", expires_in: 900,
impersonator_id, user_id}`. The token is a normal HS256
access token whose claims carry `impersonator_id` (the
admin's id); its `sub` + `role` are the TARGET user's, so it
acts as them on user routes (`GET /v1/me` returns the
target's profile). It lives 15 minutes and has no refresh:
no session row is created behind it, no cookie is set, and
P1-014's refresh must reject it (there is nothing to
rotate). Auto-expiry is the JWT `exp`, enforced by the
extractor with leeway 0 — an expired impersonation token is
401 `PF-1011`, exactly like an expired login token. The
impersonation is audited as `user.impersonated` (actor = the
admin, target = the user, the reason in the audit's `reason`,
`after` carries `{expires_in: 900}`); the token is issued
before the audit insert, so the audit records a token that
was actually handed out (an audit failure returns 500 and
the client never learns the token string). Access: admin
only — the `AdminContext` extractor; support gets 403
`PF-1002` (impersonation is a write), no credential gets
401 `PF-1001`, an unknown target gets 404 `PF-2010`. An
impersonator cannot reach admin endpoints: both admin
guards (`AdminContext` + `AdminReadContext`) reject any
context carrying `impersonator_id` with 403 `PF-1002` —
writes AND reads — so the token cannot be used to suspend
someone, read the admin list, or impersonate someone else.
Any user may be impersonated, including another admin: the
audit is the control, not a restriction the spec does not
name (the guards make it safe for admin endpoints either
way). Impersonation does not bypass the target's suspension
for login (that check is the login's, and this is not a
login).
**Banner contract (the AC "banner requirement
documented"):** any console that uses an impersonation
token MUST render a persistent banner for the whole
session naming both parties ("You are impersonating
<user> as <admin>") and MUST NOT offer admin actions
(the API rejects them anyway). The signal is the token
itself: the console requested it, so it knows — and the
response carries both ids for the banner text. The banner
is a Phase 4 console requirement; this paragraph is the
contract it implements.
**Audit convention for impersonated actions (P1-011's
scope):** when an action is performed under impersonation,
the audit row's `actor_user_id` is the impersonator (the
human who acted) and the `target_id` is the user acted-as —
both ids are already in the standard columns, so no new
convention is needed beyond "the actor is whoever the
`impersonator_id` names". P1-011 proves it on the
audited actions that exist today (profile update) and on
the admin block.
| POST | `/v1/admin/users/{id}/impersonate` | reason; audited; returns 15-min scoped token with `impersonator_id` claim; admin only (support forbidden) |
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
