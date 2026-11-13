# Security baseline — Phase 0 (summary)

> Status: **Phase 0 baseline, 2026-11-13** (P0-108). This is the
> honest inventory of what Phase 0 enforces and what it does
> not — read it before Phase 1 work, not after. Anything marked
> PLANNED is a claim about a future ticket, not a fact about
> the code.
>
> See also: `docs/threat-model.md` (assets, actors, STRIDE-lite
> T1–T16), `docs/05-QUALITY/03-security-testing.md` (the test
> plan this summary maps to), `docs/06-DELIVERY/03-deploy-
> runbook.md` (secret rotation), `docs/09-STATE/DECISIONS.md`
> (owner decisions), `docs/09-STATE/ISSUES.md` (open blockers).

## 1. What is enforced where

| Area | CI | Runtime | Ops |
| --- | --- | --- | --- |
| Secrets in code | gitleaks full scan on every push/PR + pre-commit hook on staged changes (P0-008); `just secrets` — 107 commits, no leaks | `PF_` env vars only; no secret is ever read from code or logs | Rotation runbook (P0-105): JWT dual-verify (drafted), S3, Postgres, payment keys; `.env` never committed |
| Secrets in compose | `just compose-secrets-check` in the verify job (P0-105): base/prod allow only bare `${VAR}` refs; dev-only files allow the documented placeholders (`pf`, `minioadmin`, `dev-fixed`) | — | Dev values come from the env file (`--env-file .env`, copied from `.env.example`); prod must set real values — a missing secret fails the deploy fast |
| Headers & transport | Header-table test (`middleware_chain.rs`, P0-102) + smoke suite (P0-107) | Secure-header table on every response; HSTS prod-only; docs CSP for `/docs` (P0-102) | TLS terminates at caddy in prod; internal services on the private network only |
| Rate limiting | Race test: 20 concurrent vs burst 5 admits exactly 5 (P0-103) + smoke suite (P0-107) | Per-IP + per-route token buckets (P0-103); 429 PF-3007 with `Retry-After`; health/meta exempt; **fails OPEN on Redis down** (warn logged) | Defaults 120/120 IP, 600/600 route per min (`.env.example`); per-plan tiers are open question #17, tuned Phase 5 |
| Body / dimension caps | Per-class tests (P0-104) + smoke suite (P0-107) | `BodyLimitClass`: auth/meta 64 KiB, jobs/admin 1 MiB, billing 256 KiB; uploads exempt; `validate_dimensions` 8000 px / 64 MP, fail-closed (P0-104) | Numbers documented in the api-spec; the dimension guard is not yet wired to a live handler (no upload row in Phase 0 — Phase 2 hooks it in) |
| Input validation | Validator unit tests + live `validation.rs` (P0-101) | `ValidatedJson<T>`: malformed body → 422 PF-2002; field errors in `field_errors`, never echoing the offending value | — |
| Error envelope | Error-contract tests; insta snapshots of `Problem` (P0-095) | problem+json mapper rewrites 400/404/413/415/422; production profile hides caller detail (P0-065, P0-101) | — |
| Supply chain | `cargo-deny` advisories/bans/licenses/sources in CI (P0-100); weekly audit workflow (Mondays 03:00 UTC) | — | `deny.toml`: 9 no-upgrade-path advisories ignored with per-ID reasons + removal conditions; a new advisory on any crate still fails the build |
| Quality gates | `just verify` (fmt + clippy `-D warnings` + 396 tests); coverage report-only until Phase 2 (P0-098); insta `INSTA_UPDATE=no`; fixtures/templates checks; nightly proptest/bench/quarantined jobs (P0-096/097/099) | — | — |
| Threat model | — | — | `docs/threat-model.md` (P0-106), updated in the ticket that changes it |

## 2. Checklist mapping to the security-testing plan

`docs/05-QUALITY/03-security-testing.md` section by section.
Status: **Enforced** (in code + tested), **Partial** (part
live, part planned), **Planned** (a Phase 1+ ticket).

| Plan section | Status | Notes |
| --- | --- | --- |
| Authorization matrix sweep (IDOR) | Planned (Phase 1) | No auth exists yet; the matrix test lands with the first authed endpoint. T13 |
| Session & token tests | Planned (Phase 1) | JWT dual-verify is drafted, not implemented (P0-105); refresh rotation + family revocation land with P1-016. T1 |
| Upload abuse suite | Partial | Live: dimension guard + body caps + filename sanitizer. Planned (Phase 2/3): magic-byte sniff + re-decode, zip/decompression bombs, polyglots, SVG rejection. T3, T5 |
| Security smoke suite | Enforced (P0-107) | 5 gates: headers, rate-limit trip, oversized body, method-not-allowed, no swagger in prod. `just security-smoke` |
| API & webhook hardening | Partial | Live: body caps, dimension caps, rate limits, parameterized SQL (sqlx). Planned: webhook SSRF + HMAC + idempotency (Phase 1/3), per-account/per-key tiers (Phase 1). T4, T11, T12 |
| Headers & transport | Enforced (P0-102) | Table asserted verbatim; HSTS prod-only; docs CSP. T9 |
| Secrets & supply chain | Enforced (P0-100, P0-105) | Gitleaks + compose gate + cargo-deny. T8, T15 |
| Reporting | Enforced | Findings in `09-STATE/ISSUES.md`; S1 security = stop feature work immediately (bug-triage doc) |

## 3. Known gaps (accepted risks — see the threat model)

1. **Rate limiter fails OPEN on Redis down** (T11). Availability
   first; the residual flood risk during a Redis outage is
   accepted until Phase 5 revisits it.
2. **Public-URL validation does not resolve DNS** (T4). A
   hostname resolving to a private IP passes today; the Phase 3
   fetch layer must re-validate after resolution. No URL fetch
   exists in the product yet.
3. **The api does not fail fast in prod on the placeholder
   `dev-fixed` JWT secret** (P0-105). The runbook checklist is
   the only guard; a boot-time guard is a Phase 5 hardening
   candidate.
4. **`/metrics` has no in-code auth.** Operator-scoped by
   deployment; if it is ever exposed publicly, add auth in the
   same ticket. Phase 5 sweep confirms the prod edge rule.
5. **JWT dual-verify is drafted, not implemented** (T1). The
   Phase 5 rehearsal is the proof.
6. **405 is bare, not problem+json.** The mapper rewrites
   400/404/413/415/422 only; whether 405 belongs in the
   envelope is a Phase 1 question (P0-107).
7. **FLAKY-001/002 are not quarantined** (owner decision, P0-099).
   Quarantining would silently drop shutdown-drain/pubsub
   coverage from every commit; they fail intermittently and are
   re-run until green instead.
8. **The CI gates in this repo have never run remotely.**
   The workflow files cannot be pushed via the GitHub App (403,
   CI-WORKFLOWS-001) — remote CI runs the stale 12 Nov workflow.
   The fixtures/templates/snapshots/nightly/audit/compose-
   secrets gates exist locally only until the owner pushes the
   workflows another way. Do not claim "CI is green" for them.
9. **Three Phase 0 tickets are implemented but unverified:**
   P0-034 (Postgres in compose), P0-046 (Redis in compose),
   P0-083 (Dockerfile.api size gate) — live ACs blocked by
   DOCKER-001 (no container can start on this VM). Their
   compose files / Dockerfile / gates are committed and
   statically verified; live verification waits for CI/E0.9
   smoke. Same reason the Phase 0 exit gates for `just dev`
   (full stack) and `just smoke` (end-to-end ping job) cannot
   hold on this VM.

## 4. Phase 1+ handover

Phase 1 inherits, and must extend rather than redo:

- **Threat model** (`docs/threat-model.md`): the P1 ticket
  updates it for token theft, replay, family revocation,
  cookie vs bearer. T1/T2/T13 move from Planned to Enforced
  there, in the same tickets that land the code.
- **Smoke suite** (`apps/api/tests/security_smoke.rs`):
  auth'd-endpoint smoke (401/403 shapes, matrix spot-check)
  joins the five Phase 0 gates; do not fork a second suite.
- **Gates to keep green**: `just verify` (396 tests at the
  Phase 0 close), `just secrets`, `just compose-secrets-check`,
  `just security-smoke`, `just coverage` (report-only until
  Phase 2 flips the gate), the Python script suite (143 tests).
- **Owner decisions still open** (DECISIONS.md): final pricing,
  live payment keys, domain/email sender, legal-review gate
  for provenance_strip, SynthID scope, CleanUpload relation,
  admin-invite CLI owner (item 8).
- **Phase 0 exit gates**: `just verify` green locally (this
  run, 396 tests) — the CI half, `just dev` full-stack health,
  and `just smoke` end-to-end cannot hold until docker/CI
  work (see gap 9 above). The phase closes with those three
  tickets + gates explicitly deferred, not silently dropped.

## 5. How to verify this baseline

```sh
just verify            # fmt + clippy -D warnings + tests (396 at Phase 0 close)
just secrets           # gitleaks full scan — 107 commits, no leaks
just compose-secrets-check   # no literal secrets in compose
just security-smoke    # the 5-gate smoke suite on its own
just coverage          # per-crate table vs NFR-50 floors (report-only)
python3 -m unittest discover -s scripts -p "test_*.py"   # 143 script tests
```

All of the above were green at the Phase 0 close (2026-11-13,
local HEAD `4392a8b` + this commit), except where §3 says
otherwise. `just dev` / `just smoke` are excluded on this VM
per ADR-0017 (no docker daemon); they are the CI/E0.9 gates.
