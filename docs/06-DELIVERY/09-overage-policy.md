# Overage policy (P1-054, FR-P-06)

Overage SHALL reject with 429 + upgrade hint, never silently
degrade service. This page pins what that means in practice:
the denial contract, what a denial must never do, and the
one sanctioned bypass.

## The denial contract

An over-quota `POST /v1/jobs` is 429 `problem+json`:

| Condition | Code | Detail | `upgrade_url` |
| --- | --- | --- | --- |
| No images left | `PF-3001` | `image quota exceeded; upgrade your plan for more` | `{PF_PUBLIC_BASE_URL}/plans` |
| Not enough video-seconds | `PF-3002` | `video-seconds quota exceeded; upgrade your plan for more` | `{PF_PUBLIC_BASE_URL}/plans` |
| No concurrency slot | `PF-3008` | `concurrency limit reached; upgrade your plan for more` | `{PF_PUBLIC_BASE_URL}/plans` |

`upgrade_url` is the plan-matrix link: the client renders
the 429 detail verbatim and links the plan matrix at that
URL — it never hardcodes a host. The link is derived from
the `PF_PUBLIC_BASE_URL` setting (`quota::plans_url`), so
the owner changes it by changing the setting, not the code.
The console serves the plans page at `/plans` in Phase 4;
until then the URL is the contract, not a live page
(ASSUMPTION-020). No other error carries `upgrade_url` — an
allowed create (the stub's 501) has no link, and neither
does any 4xx/5xx outside the three quota codes.

The denial also carries the `X-Quota-*` headers (P1-052):
the caller sees exactly how much is left. Check order is
quota dimensions first (images, then video-seconds), then
concurrency (P1-051) — a request tripping several limits
answers with the first code, deterministically.

## No silent queueing

A denial does exactly one thing: it answers 429. It never
queues the job for later, never holds it, never partially
creates it. Proven live (`apps/api/tests/overage_policy.rs`,
4 tests — one per code, plus the allowed-create control):

- No `jobs` row is created for the caller.
- No usage is recorded (`usage_counters` unchanged; no row
  appears where none existed).
- No concurrency slot is acquired (`pf:slots:{user}` stays
  empty).
- Nothing is enqueued: the Phase 2 intake can only enqueue
  a job row that exists, and the denial creates none — so
  there is nothing for any queue to pick up later.

The gate is read-only by design (ASSUMPTION-017): the
check records nothing and acquires no slot, because job
creation is still the 501 stub. The atomic check-and-record
lands with real job creation in Phase 2.

## The one bypass: admin quota grants

The ONLY way past an overage rejection is an admin quota
grant (P1-107, E1.13): `POST /v1/admin/users/{id}/quota-grant`
— audited (`quota.granted`), expiring (`ttl_hours`), and
honored by the quota middleware. There is no other bypass:
no header, no flag, no backdoor, no support-role override.
Until P1-107 lands, the bypass does not exist — every
over-limit create is a 429, for every caller including
admins (an admin creating a job for themselves is gated on
their own plan like anyone else).

## What overage is not

- Not a downgrade: the service never answers a degraded
  result (a lower-quality job, a smaller output) in place
  of the rejection.
- Not a retry hint: the 429 carries no `Retry-After` —
  quota resets at 00:00 UTC, not after a window (see the
  quota UI contract for the day rule).
- Not a queue signal: `PF-3005` (`queue_full`) is the
  system-wide queue signal, not a per-user plan cap
  (ADR-0026) — a plan-cap denial is always PF-3001/3002/
  PF-3008 with the upgrade hint.
