# API keys — getting started (P1-041)

For API clients: how to mint a key, call the API with it, rotate it, and
revoke it. Every claim here is per the API spec
(`docs/03-ARCHITECTURE/03-api-spec.md`, § API keys) and proven by the
suites in `apps/api/tests/api_keys.rs`, `extractor.rs`,
`api_key_lifecycle.rs`, and `api_key_matrix.rs` — if this page and the
spec ever disagree, the spec wins and this page is the bug.

Base URL in dev: `http://127.0.0.1:8080` (`PF_API_HOST` /
`PF_API_PORT`). All examples use it as `$DETAG_API`.

## What works today

- Creating, rotating, and revoking keys: **live**.
- Calling with a key: the extractor is **live** — a key with the right
  scope reaches the handler; a key without it is 403. The jobs and
  webhook-endpoints handlers themselves are still 501 stubs (their
  Phase 2 tickets land the real handlers), so today a successful key
  call answers 501 `PF-5004` — that 501 *is* the proof the key
  authenticated and passed the scope check.
- Listing keys (`GET /v1/api-keys`): **not yet** — still a 501 stub;
  its handler ticket lands the list (prefix + last4 only, never the
  secret).

## 1. Log in (get a user token)

Keys are minted by a user, with the user's access token — a key can
never mint, rotate, or revoke a key. Log in first:

```bash
curl -s -X POST "$DETAG_API/v1/auth/login" \
  -H 'content-type: application/json' \
  -d '{"email": "you@example.com", "password": "correct horse battery staple"}'
```

```json
{"access_token": "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.example-key", "token_type": "Bearer", "expires_in": 900}
```

The access token lives 15 minutes (900 s). Save it as `$TOKEN` for the
next step; if it expires, log in again.

## 2. Create a key

```bash
curl -s -X POST "$DETAG_API/v1/api-keys" \
  -H "authorization: Bearer $TOKEN" \
  -H 'content-type: application/json' \
  -d '{"name": "CI deploy", "scopes": ["jobs:write", "jobs:read"]}'
```

```json
{
  "id": "019aa1b2-c3d4-7e5f-8000-000000000001",
  "name": "CI deploy",
  "prefix": "pf_live_a1B2",
  "secret": "pf_live_a1B2-example-key-c3D4e5F6g7",
  "scopes": ["jobs:write", "jobs:read"],
  "created_at": "2026-11-15T03:00:00Z"
}
```

- The `secret` is shown **once, here, and never again** — there is no
  endpoint that returns it. Save it now (see § Security notes).
  The `prefix` (`pf_live_` + the first 4 secret chars) is the display
  identifier you use to recognise the key afterwards. The secrets in
  this guide are placeholders — `example-key` marks them fake; never
  paste a real secret into docs.
- `name`: 1–100 chars, no control characters. `scopes`: at least one,
  each a known scope, no duplicates — otherwise 422 `PF-2002`.
- Max **10 live keys** per user (revoked and expired keys do not
  count). The 11th create is 409 `PF-2011`. The count + insert run
  under the user's row lock, so concurrent creates cannot overshoot.
- No plan gate and no verified-email requirement on creation today
  (ASSUMPTION-015 in `docs/09-STATE/ISSUES.md`): a Free user can mint
  keys, and the key is unusable for jobs until the owner's email is
  verified anyway (job creation calls the verified guard).

## 3. Call with the key

Send the secret as a Bearer token. The scope the route needs decides
whether the call lands (§ Scopes):

```bash
curl -s "$DETAG_API/v1/jobs" \
  -H "authorization: Bearer $DETAG_API_KEY"
# today: 501 PF-5004 (the stub) — auth passed; the handler lands in Phase 2
```

```bash
curl -s -X POST "$DETAG_API/v1/jobs" \
  -H "authorization: Bearer $DETAG_API_KEY" \
  -H 'content-type: application/json' -d '{}'
# with a jobs:read-only key: 403 PF-1002 (a read-only key cannot create)
```

Verification, in order: the secret's sha256 is looked up (live keys
only — a revoked key reads exactly like an unknown one, 401
`PF-1001`); the owner must be `active` (a suspended user's keys stop
working at the next request, ADR-0024 — suspend does not revoke the
rows, so restoring the user restores the keys); then the scope check
runs by route. A verified use re-stamps `last_used_at`, throttled to
once per 300 s; a denied request is not a use.

## 4. Rotate a key

Rotation mints a replacement (same name + scopes, new secret) and
keeps the old secret valid for the overlap window —
`PF_API_KEY_ROTATION_OVERLAP_SECS`, default 86400 s (24 h):

```bash
curl -s -X POST "$DETAG_API/v1/api-keys/$KEY_ID/rotate" \
  -H "authorization: Bearer $TOKEN"
```

```json
{
  "id": "019aa1b2-c3d4-7e5f-8000-000000000002",
  "name": "CI deploy",
  "prefix": "pf_live_z9Y8",
  "secret": "pf_live_z9Y8-example-key-x7W6v5U4t3S2",
  "scopes": ["jobs:write", "jobs:read"],
  "created_at": "2026-11-15T04:00:00Z"
}
```

The response carries the NEW secret once, same shape as creation.
Inside the window both secrets work; past it the old secret is the
generic 401 `PF-1001` and stops counting toward the 10-key cap.
Rotation is exempt from the cap (it replaces, it does not add). A key
cannot rotate (403 `PF-1002`); an unknown id, another user's id, or a
revoked key are all 404 `PF-2010` (no enumeration).

Rotate *before* the old secret's deadline when you can: mint the
replacement, deploy it everywhere, then let the old one die on its
own — there is no step that kills the old secret early except
revoking it (below).

## 5. Revoke a key

```bash
curl -s -o /dev/null -w '%{http_code}\n' -X DELETE \
  "$DETAG_API/v1/api-keys/$KEY_ID" \
  -H "authorization: Bearer $TOKEN"
# 204, no body
```

Revocation is immediate: the row's `revoked_at` is stamped and the
key's sha256 is denylisted in Redis (24 h TTL), so the next use is
401 `PF-1001` — no grace, no overlap — and the key stops counting
toward the cap. A key cannot revoke (403 `PF-1002`); an unknown or
another user's id is 404 `PF-2010`. Re-revoking your own dead key is
a no-op 204 with no second audit row (DELETE is idempotent).

## TypeScript

```ts
// Node 18+ (global fetch). The secret comes from the environment —
// never hard-code it, never commit it (see § Security notes).
const api = process.env.DETAG_API ?? "http://127.0.0.1:8080";
const key = process.env.DETAG_API_KEY!;

async function detag(path: string, init: RequestInit = {}) {
  const res = await fetch(`${api}${path}`, {
    ...init,
    headers: { authorization: `Bearer ${key}`, ...init.headers },
  });
  if (!res.ok) {
    // The body is problem+json: { code, title, detail, ... }.
    const problem = await res.json();
    throw new Error(`${res.status} ${problem.code}: ${problem.title}`);
  }
  return res.json();
}

// List jobs (501 today — the stub — until the Phase 2 handler lands;
// the 501 proves the key authenticated and passed the scope check).
const jobs = await detag("/v1/jobs");
```

## Python

```python
# Standard library only. The secret comes from the environment —
# never hard-code it, never commit it (see § Security notes).
import json
import os
import urllib.request

API = os.environ.get("DETAG_API", "http://127.0.0.1:8080")
KEY = os.environ["DETAG_API_KEY"]

def detag(path, method="GET", body=None):
    req = urllib.request.Request(
        API + path,
        method=method,
        data=json.dumps(body).encode() if body is not None else None,
        headers={
            "Authorization": f"Bearer {KEY}",
            "Content-Type": "application/json",
        },
    )
    try:
        with urllib.request.urlopen(req) as res:
            return json.loads(res.read())
    except urllib.error.HTTPError as e:
        # The body is problem+json: { code, title, detail, ... }.
        problem = json.loads(e.read())
        raise SystemExit(f"{e.code} {problem['code']}: {problem['title']}")

# List jobs (501 today — the stub — until the Phase 2 handler lands;
# the 501 proves the key authenticated and passed the scope check).
jobs = detag("/v1/jobs")
```

## Scopes

| Scope | Gates |
| --- | --- |
| `jobs:read` | `GET /v1/jobs`, `GET /v1/jobs/{id}`, `GET /v1/jobs/{id}/events`, `GET /v1/jobs/{id}/result`, `GET /v1/jobs/{id}/report` |
| `jobs:write` | `POST /v1/jobs`, `POST /v1/jobs/{id}/cancel`, `POST /v1/jobs/{id}/retry`, `POST /v1/jobs/{id}/attestation` |
| `webhooks:manage` | `GET` + `POST /v1/webhook-endpoints`, `DELETE /v1/webhook-endpoints/{id}`, `GET /v1/webhook-endpoints/{id}/deliveries` |

Least privilege: give a key only the scopes its job needs — a
`jobs:read`-only key cannot create a job or mint a key, and a
write-only key cannot read. Anything the scope table does not name
rejects every key with 403 `PF-1002` (fail closed). Keys never reach
admin routes (`/v1/admin/*`) or billing (`/v1/billing/*`,
`/v1/plans`, `/v1/usage`) — the matrix in
`apps/api/tests/api_key_matrix.rs` pins all 65 `/v1` rows, and an
admin user's own key is 403 on the live admin routes too.

## Errors

| Status | Code | Meaning |
| --- | --- | --- |
| 401 | `PF-1001` | No credential, or an unknown / expired / revoked key. Deliberately generic — the response never says which. |
| 403 | `PF-1002` | The key is live but lacks the route's scope (or the route is not key-callable at all). |
| 409 | `PF-2011` | The user already has 10 live keys. Revoke or let one expire, then retry. |
| 422 | `PF-2002` | Invalid name or scopes on create. |
| 429 | `PF-3007` | The key's rate-limit bucket is empty. The response carries `Retry-After` (seconds) and the key bucket's `X-RateLimit-*` trio. |
| 404 | `PF-2010` | Unknown id (rotate/revoke) — or another user's / a revoked key's id. No enumeration. |

Rate limits (P1-037): after the scope check the extractor spends one
token from the key's tier bucket — `default` 60 burst / 60 per min
(`PF_API_KEY_RATE_LIMIT_*`), or `high` 600 / 600 for keys whose row
says so (set out of band; NULL or an unknown name means default).
An allowed key response still carries the IP bucket's trio — the key
bucket's remaining is visible on the denial, not on every 200.
Repeated trips write one `api_key.rate_limit_tripped` audit row per
key per hour. A denied request (401/403) spends nothing.

## Security notes

- **Storage — treat the secret like a password.** It is shown once
  at creation/rotation and is never retrievable afterwards: if you
  lose it, revoke the key and mint a new one. Keep it in an
  environment variable or a secret manager (`DETAG_API_KEY` in the
  examples above), never in source control, never in a log line, never
  pasted into chat or a ticket. Only the sha256 is stored server-side
  (unique index), and audit rows (`api_key.created` / `.rotated` /
  `.revoked`) carry ids and prefixes only — no secret, no hash.
- **Rotation — rotate before you have to.** The overlap window is 24 h
  by default: mint the replacement, deploy it, and let the old secret
  die on its own. Do not wait for the old secret to expire mid-deploy
  — past the window it is dead with no grace.
- **Revocation — the kill switch.** Revoking a key kills it
  immediately (next use 401, no overlap). Revoke on any suspicion of
  exposure; minting a replacement is cheap. Suspending the owner also
  stops every key at the next request (ADR-0024).
- **Least privilege, one key per use.** Mint separate keys per
  environment / integration (CI, staging, a single customer) with only
  the scopes that job needs, and name them so the prefix identifies
  the owner in the audit trail. A leaked narrow key can do far less
  damage than a leaked full-scope one — and revoking it breaks nothing
  else.
- **What a key cannot do.** A key cannot mint/rotate/revoke keys, reach
  admin routes, touch billing, or act as the user on profile/session
  routes — those need the user's access token. If a key ever could,
  that is a bug: the matrix test fails first.
