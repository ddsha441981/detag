# Security Testing Plan

Executed as tickets in Phase 1 (authz), Phase 2/3 (upload abuse), Phase 5 (full sweep). Each item = a named test that lives in CI, not a one-off manual check.

Threat model: `docs/threat-model.md` — the living threat model this plan tests against. If a test here covers a threat, the threat's ID (T1–T16) is the cross-reference.

## Authorization matrix sweep (IDOR)

For every `/v1` endpoint × role {anonymous, user-A, user-B, support, admin, api-key-scoped}: assert allowed matrix. Auto-generated table test in `apps/api/tests/authz_matrix.rs`; adding an endpoint without a matrix row fails the test (registry pattern: routes self-register their matrix entry).

## Session & token tests

- Refresh rotation: reuse of rotated refresh ⇒ whole family revoked (PF-1007), event logged.
- JWT: alg-confusion attempt (alg:none, HS/RS swap) rejected; exp/nbf enforced; kid confusion rejected.
- Password flows: reset token single-use; old password invalid after reset; email-verification enforced before jobs.
- Cookie flags: Secure, HttpOnly, SameSite=Lax asserted in integration tests.

## Upload abuse suite

| Attack | Expected |
| --- | --- |
| Zip bomb inside batch ZIP | rejected PF-2006, resource caps on extraction |
| PNG decompression bomb (huge logical dims) | dimension/byte budget rejected at sniff (FR-U-04) |
| Polyglot (JPEG+HTML/JS) | magic-byte sniff + re-decode verify rejects |
| SVG | rejected outright (PF-2001) |
| EXIF exploit strings (oversized tags) | parser hardened; fuzz nightly (cargo-fuzz on EXIF/metadata parsers) |
| Mismatched extension vs magic | magic wins (FR-U-03) |
| Path traversal in filename | sanitized to opaque storage key (ADR-0011) |

## API & webhook hardening

- Body caps per route class (P0-104): auth/meta 64 KiB, jobs/admin 1 MiB, billing (webhooks) 256 KiB; over-cap is 413 PF-2003, tested per class. Uploads are exempt (bytes never transit the api).
- Dimension caps (P0-104, FR-U-04): 8000x8000 per side, 64 MP total, enforced at job-create by `validate_dimensions` (PF-2005, 413, fail-closed on missing dimensions); the sniff-time dimension check in the abuse table above lands with intake (Phase 2/3).
- Webhook SSRF: endpoint URL must resolve to public HTTPS; private/link-local CIDRs denied; redirect-following disabled; tested with a listener on 169.254.169.254-style address.
- Outbound webhook HMAC: constant-time compare; replay window enforced; tampered payload rejected.
- Idempotency: replayed Idempotency-Key returns original response; concurrent duplicates create one job (integration test with racing requests).
- Rate limits: per-IP + per-route token buckets enforced by the middleware (P0-103; race-safe — no over-admission under concurrency, proven by test); per-account + per-key tiers land with Phase 1 auth; lockout backoff curve asserted.
- Injection: sqlx parameterized everywhere (CI grep forbids `format!`-built SQL); no string-built shell calls (ffmpeg args as argv array, never shell string).

## Headers & transport

CSP (web), HSTS (prod only — dev is plain http, so dev responses carry the rest of the table but no HSTS), X-Content-Type-Options, Referrer-Policy, frame-ancestors none for app routes; the Swagger UI at `/docs` gets its own same-origin CSP (the api's `default-src 'none'` would block its JS) — asserted by a route test against a header table. TLS terminated at caddy in prod; internal services never exposed (compose networks: `internal` vs `edge`).

## Secrets & supply chain

gitleaks CI; `cargo deny` advisories+licenses gate; Dependabot-style weekly bump window (Phase 5 cadence); no secrets in fixtures, logs, or error messages (test asserts error bodies contain no stack traces in prod profile).

## Reporting

Findings land in `09-STATE/ISSUES.md` with severity per `06-DELIVERY/01-bug-triage.md`. S1 security = stop feature work immediately. A security summary table (test → result) ships in the Phase 5 release checklist.
