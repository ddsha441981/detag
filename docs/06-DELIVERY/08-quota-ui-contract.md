# Quota UI contract (P1-052)

How the console (and any API client) reads remaining quota: the
`X-Quota-*` response headers on job-creating routes, and the
quota snapshot in `GET /v1/me`. FR-P-02 — remaining quota
surfaces in headers AND UI.

## The headers

| Header | Meaning |
| --- | --- |
| `X-Quota-Images-Remaining` | Today's image quota left (`limit - used`, floored at 0, never negative) |
| `X-Quota-Video-Seconds-Remaining` | Today's video-seconds quota left (same rule) |

Both headers are present together, on every response from a
job-creating route — allowed and denied alike. They are
integers (plain decimal, no units suffix). They are NOT
present on any other route.

## Which routes carry them

| Route | Headers | Notes |
| --- | --- | --- |
| `POST /v1/jobs` (names an `upload_id`) | yes | Gated (P1-051): inside quota → 501 stub with headers; over quota → 429 with headers |
| `POST /v1/jobs` (no body, or no `upload_id`) | yes | The stub with the caller's headers (P1-052) — no gate runs, there is no job to check |
| `POST /v1/jobs/{id}/retry` | yes | The caller's headers on the 501 stub (P1-052) — the handler reads no job row, so an unknown id still 501s; the 404 ownership proof lands with the real retry handler in Phase 2 (ASSUMPTION-018) |
| `POST /v1/admin/jobs/{id}/reprocess` | no | The quota subject is the job owner, not the admin (ASSUMPTION-018); the owner's remaining quota is in the admin jobs browse view (P1-038 usage breakdown) |

A snapshot failure (database error, missing Free plan) is a
500 with no headers — the same failure arm as the gate.

## The `/v1/me` snapshot

`GET /v1/me` returns the same numbers as `quota`:

```json
"quota": {
  "daily_images": { "limit": 10, "used": 3, "remaining": 7 },
  "daily_video_seconds": { "limit": 0, "used": 0, "remaining": 0 }
}
```

The headers and the snapshot agree by construction: both
read the same durable snapshot (plan entitlements + today's
`usage_counters` row) — proven by the
`quota_headers_agree_with_profile_snapshot` test (gated
path, P1-051) and `stub_headers_agree_with_profile_snapshot`
(stub path, P1-052). A client MUST NOT compute `remaining`
itself from `limit - used` when it can read the header or
the snapshot — both already floor at 0.

## The day, and what "remaining" means

- The day is UTC (`usage_counters.day`), reset at 00:00 UTC.
  `remaining` is today's quota left, not a rolling window.
- The numbers are a pre-check, not a reservation: the
  check is read-only (ASSUMPTION-017) — no usage is
  recorded and no slot is acquired until a real job is
  created in Phase 2. Two concurrent creates can both pass
  the check today; the atomic check-and-record lands with
  real job creation.
- A video job always needs at least 1 video-second, even
  when its duration is unknown — a plan with a 0
  video-seconds limit (Free) grants no video at all.

## Denials (what the UI renders)

An over-quota create is 429 `problem+json`:

| Condition | Code | Detail (ends with the upgrade hint) | `upgrade_url` |
| --- | --- | --- | --- |
| No images left | `PF-3001` | `image quota exceeded; upgrade your plan for more` | `{PF_PUBLIC_BASE_URL}/plans` |
| Not enough video-seconds | `PF-3002` | `video-seconds quota exceeded; upgrade your plan for more` | `{PF_PUBLIC_BASE_URL}/plans` |
| No concurrency slot | `PF-3008` | `concurrency limit reached; upgrade your plan for more` | `{PF_PUBLIC_BASE_URL}/plans` |

Render rule: on 429, show the detail verbatim and link the
plan matrix at the `upgrade_url` from the body — never
silently degrade, never queue the job (FR-P-06; the full
policy is `docs/06-DELIVERY/09-overage-policy.md`). A
missing or foreign `upload_id` is 404 `PF-2010`, not a
quota denial. A non-empty body that is not valid JSON is
422 `PF-2002`.

## Current plan limits (seeded)

| Plan | Daily images | Daily video-seconds | Concurrency |
| --- | --- | --- | --- |
| Free | 10 | 0 | 1 |
| Pro | 500 | 3600 | 3 |
| Business | 5000 | 18000 | 10 |

(The Pro/Business concurrency and Business video-seconds
values are the seeded entitlements; the console MUST read
them from `/v1/me` at runtime, never hardcode this table.)
