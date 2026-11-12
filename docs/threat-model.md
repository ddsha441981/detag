# Threat model — Detag (living doc)

> Status: **STUB** (P0-106, 2026-11-13). Phase 0 scope: the entry
> points and mitigations that exist today, plus the planned
> Phase 1/2 entry points marked PLANNED so nothing is claimed
> before it lands. Phase 1 updates it for token theft / replay /
> family revocation (P1 ticket); Phase 2 ties engine security
> notes to it. This is a living doc — update it in the same
> ticket that adds an entry point, an actor, or a new
> dependency class; do not let it drift from the code.
>
> Out of scope here: the LaMa inpainting model internals and
> the video pipeline (Phase 2/3 tickets); payment processing
> itself (provider-side, Phase 1); the operator's host OS.
>
> See also: `docs/05-QUALITY/03-security-testing.md` (the
> test plan that tests against this model),
> `docs/06-DELIVERY/03-deploy-runbook.md` (secret rotation),
> `docs/09-STATE/DECISIONS.md` (owner decisions).

## Assets

| Asset | Value / sensitivity | Where it lives |
| --- | --- | --- |
| User uploads (owned media) | High — the user's own images/videos; the product promise is that only the owner can process them | S3 (MinIO bundled) / Postgres rows |
| Processed results | High — same ownership as the input | S3, presigned URLs (TTL-bound) |
| Ownership attestation + AUP record | High — the legal basis for processing; must not be forgeable by the requester | Postgres (Phase 2 job flow) |
| Session / refresh tokens | High — bearer-equivalent; theft = account takeover | Phase 1 (planned) |
| API keys (per-user, Phase 1) | High — programmatic account access | Phase 1 (planned) |
| Billing webhook payloads | Medium — drive plan activation; forged payloads = free Pro | Phase 1 (planned) |
| Job / session metadata | Medium — who processed what, when | Postgres |
| Templates (badge/text packs) | Low — synthesized test assets, public by design | `templates/` |
| Fixtures / golden media | Low — synthesized, no user data | `fixtures/` |
| Secrets (JWT, S3, DB, payment) | High — see the rotation runbook | Deployment `.env` only, never in repo |

## Actors

| Actor | Trust | Notes |
| --- | --- | --- |
| Anonymous internet user | Untrusted | Can reach every public route; rate-limited (P0-103), body-capped (P0-104), validated (P0-101) |
| Signed-in user (Phase 1) | Semi-trusted | Owns their uploads only; IDOR matrix enforced (security-testing plan) |
| Admin / support (Phase 1) | Trusted, least-privilege | Admin routes + coupon engine; invite-only CLI owner is an open owner decision (DECISIONS.md item 8) |
| api / worker services | Trusted within the cluster | Talk to Postgres/Redis/S3 over the private network; no public exposure |
| Operator (deploy host) | Trusted | Holds `.env`; rotates secrets per the runbook |
| Payment provider (Stripe / Razorpay) | Trusted via webhook signature | Webhook verification is Phase 1; a mismatched signature rejects the event (safe default) |
| Compromised dependency | Untrusted by design | `cargo-deny` gates advisories/bans/licenses in CI (P0-100); weekly audit workflow |
| Malicious uploader | Untrusted | Upload abuse suite (security-testing plan): zip bombs, decompression bombs, polyglots, traversal |

## Entry points

| Entry point | Status | What guards it today |
| --- | --- | --- |
| `POST /v1/jobs` + upload intake | Stub (Phase 0); real intake Phase 2 | Body cap 1 MiB (P0-104); dimension guard `validate_dimensions` (P0-104) — not yet wired to a live handler (no upload row in Phase 0); magic-byte sniff + re-decode planned (Phase 2) |
| `GET /healthz`, `/readyz`, `/metrics` | Live | Exempt from rate limiting (P0-103); `/metrics` is operator-scoped by deployment, not by code — see open question below |
| Auth routes (`/v1/auth/*`) | Planned (Phase 1) | Body cap 64 KiB (P0-104) is already wired; token/cookie rules are Phase 1 |
| Billing webhooks (`/v1/billing/webhook`) | Planned (Phase 1) | Body cap 256 KiB (P0-104) is already wired; signature verification is Phase 1 |
| Admin routes (`/v1/admin/*`) | Planned (Phase 1) | Body cap 1 MiB (P0-104) is already wired; authz matrix is Phase 1 |
| `/docs` (Swagger UI) | Live in dev only | Docs CSP (P0-102); disabled in prod via `docs_enabled` |
| Redis queue / pubsub | Internal | Queue keys are namespaced per tenant; pubsub slow-consumer drops safely (FLAKY-002 is a test flake, not a product gap) |
| Postgres / S3 / MinIO | Internal | Private network only; credentials from `.env`, no literals in compose (P0-105) |

## Trust boundaries

1. **Internet ↔ api** — TLS terminates at the edge (caddy in
   prod); HSTS is stamped only outside dev (P0-102). Everything
   past this boundary is authenticated in Phase 1; in Phase 0
   the api is unauthenticated by design (no user data yet).
2. **api ↔ worker** — the queue (Redis) and the database are
   the shared state; neither service trusts the other's
   in-memory state, only rows + queue messages.
3. **api/worker ↔ storage** — S3/MinIO and Postgres are
   reachable only from the private network; presigned result
   URLs are TTL-bound and die with the TTL.
4. **Browser ↔ api** — the secure-headers table (P0-102) +
   the docs CSP; cookies are HttpOnly/Secure/SameSite=Lax
   (Phase 1).
5. **Operator ↔ infra** — `.env` on the host; the runbook is
   the only place a secret is ever written down.

## STRIDE-lite

Mitigation status: **LIVE** (in code + tested), **WIRED**
(configuration exists, handler lands later), **PLANNED**
(Phase 1/2 ticket), **OPEN** (accepted risk / owner decision).

| # | Threat (STRIDE) | Entry point / asset | Mitigation | Status |
| --- | --- | --- | --- | --- |
| T1 | Spoofing — fake identity, forged session | Auth routes, API keys | JWT dual-verify window (drafted, P0-105); refresh rotation + family revocation | PLANNED (P1-016) |
| T2 | Spoofing — forged billing webhook | Billing webhook | Provider signature verification; mismatched signature rejects the event | PLANNED (Phase 1) |
| T3 | Tampering — path traversal in filename | Upload intake | `sanitize_filename` cleans traversal/separators/control chars (P0-101); opaque storage key (ADR-0011) | LIVE (validator) / WIRED (intake Phase 2) |
| T4 | Tampering — SSRF via user-supplied URL | Public-URL fetch (Phase 3) | `validate_public_url` rejects private/loopback/link-local/CGNAT/reserved IPs (P0-101) | LIVE — with a known gap: DNS names are NOT resolved; the fetch layer must re-validate after resolution (Phase 3) |
| T5 | Tampering — oversized / malicious media | Upload intake | Body caps per route class (P0-104); dimension guard 8000 px / 64 MP (P0-104); magic-byte sniff + re-decode (Phase 2); zip/decompression bomb suite | WIRED / PLANNED |
| T6 | Repudiation — "I never processed that" / "I never attested" | Job flow | Ownership attestation recorded per job; audit events (Phase 2); admin actions logged | PLANNED (Phase 2) |
| T7 | Information disclosure — error bodies leak internals | Every route | Problem+json mapper: production bodies carry the caller-safe detail only; `dev_body_shows_caller_detail` is dev-only | LIVE |
| T8 | Information disclosure — secrets in code/logs/compose | Repo, CI, deploy | Gitleaks full scan + pre-commit hook (P0-105); compose gate `just compose-secrets-check` (P0-105); `PF_` env vars only | LIVE |
| T9 | Information disclosure — Swagger UI in prod | `/docs` | `docs_enabled` false in prod; docs CSP same-origin (P0-102) | LIVE |
| T10 | Information disclosure — presigned result URL shared | Results | TTL-bound presigned URLs; rotation kills old keys only on leak (runbook) | WIRED (Phase 2) |
| T11 | Denial of service — flood / slowloris | Every route | Per-IP + per-route token buckets (P0-103); body caps (P0-104); `/healthz` exempt so probes stay cheap | LIVE — with the fail-open trade-off below |
| T12 | Denial of service — expensive job abuse | Job intake | Per-plan quotas + dimension/pixel budgets (P0-104); engine memory budgets (Phase 2) | WIRED / PLANNED |
| T13 | Elevation of privilege — IDOR across users | Every `/v1` route | Authz matrix sweep per endpoint × role (security-testing plan) | PLANNED (Phase 1) |
| T14 | Elevation of privilege — admin invite abuse | Admin CLI | Invite-only admin creation; owner decision item 8 (who owns the CLI) | OPEN (owner) |
| T15 | Supply chain — compromised crate | Build | `cargo-deny` advisories/bans/licenses in CI + weekly audit (P0-100); 9 no-upgrade-path advisories ignored with per-ID reasons + removal conditions in `deny.toml` | LIVE |
| T16 | Provenance strip abused to launder AI media | Engine (Phase 2) | C2PA strip + SynthID removal are gated OFF (research only); ownership attestation + AUP enforced | LIVE (policy) |

## Recorded trade-offs (accepted risks)

- **Rate limiter fails OPEN on Redis down (P0-103).**
  Availability first: if the limiter's store is unreachable
  the request is admitted and a warning is logged. A fail-
  closed limiter would turn a Redis outage into a total
  outage. The residual risk is a flood during a Redis outage;
  the body caps and the edge rate limits (if any) are the
  remaining layers. Revisit in Phase 5.
- **Public-URL validation does not resolve DNS (P0-101).** A
  hostname that resolves to a private IP passes validation
  today. The fetch layer (Phase 3) must re-validate after
  resolution. Until then, no URL fetch exists in the product.
- **The api does not fail fast in prod on the placeholder
  `dev-fixed` JWT secret (P0-105).** The runbook checklist is
  the only guard today; a boot-time guard is a Phase 5
  hardening candidate.
- **`/metrics` has no in-code auth.** It is operator-scoped by
  deployment (private network / edge rule), not by the api.
  If the deployment ever exposes it publicly, add auth in the
  same ticket that changes the exposure — do not leave it
  open. (Open question: confirm the prod edge rule blocks it;
  Phase 5 sweep.)
- **JWT dual-verify is drafted, not implemented (P0-105).**
  Rotation is a procedure on paper until P1-016 lands the
  verifier; the Phase 5 rehearsal is the proof.

## Compliance boundaries (non-negotiable)

- Ownership attestation + AUP are enforced on every job
  (Phase 2 flow); stock-photo watermark removal is an AUP
  violation, not a feature.
- Provenance-strip (C2PA) is OFF; SynthID removal is OFF
  (research only). Neither may be enabled by a code change
  alone — both need the legal-review gate (DECISIONS.md).
- No secrets in repo or logs; no live payment keys in any
  environment below prod; no spending without owner approval.

## Update log

| Date | Change | Ticket |
| --- | --- | --- |
| 2026-11-13 | Stub created: assets, actors, entry points, trust boundaries, STRIDE-lite (T1–T16), recorded trade-offs | P0-106 |
