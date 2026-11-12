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
- Error code ranges: PF-1xxx auth/session, PF-2xxx upload/media, PF-3xxx quota/billing, PF-4xxx engine/quality/compliance, PF-5xxx system.
- Rate limits: headers `X-RateLimit-Limit`, `-Remaining`, `-Reset`. Overage = 429 + problem+json.
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
