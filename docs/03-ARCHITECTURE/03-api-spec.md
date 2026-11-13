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
| POST | `/v1/auth/login` | none | email, password → access_token, expires_in (+ sets refresh cookie) |
| POST | `/v1/auth/refresh` | cookie | → new access token, rotated refresh (reuse ⇒ revoke all sessions PF-1007) |
| POST | `/v1/auth/logout` | any | → 204, revoke presented session |
| POST | `/v1/auth/forgot-password` | none | email → always 204 (no enumeration) |
| POST | `/v1/auth/reset-password` | none | token, new_password → 204, revoke all sessions |
| GET | `/v1/me` | user | profile + plan + quota snapshot |
| PATCH | `/v1/me` | user | email change / profile fields (email change re-verifies) |
| DELETE | `/v1/me` | user | schedule deletion (30 d grace) |
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
