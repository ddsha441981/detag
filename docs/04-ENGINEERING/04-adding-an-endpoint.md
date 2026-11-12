# Adding an endpoint — checklist

Derived from the ownership map (`02-repo-layout.md`: a new endpoint
lives in `apps/api/src/routes/` + service + repo + OpenAPI derive +
integration test). Work the boxes in order; every box names the
file or convention it comes from, so a reviewer can check it
without hunting.

## Before you write code

- [ ] Read the layer table in `apps/api/src/lib.rs` (top of file).
  A new layer goes through that table — a layer can only see
  state/extensions added outside it, the timer must wrap
  everything it measures, and the request id must be assigned
  before anything logs.
- [ ] Pick the PF code from the taxonomy
  (`crates/core/src/error.rs`): PF-1xxx auth, PF-2xxx
  validation/media, PF-3xxx quota/rate-limit, PF-4xxx engine,
  PF-5xxx system. No new code without recording it there —
  the response-table test enforces the list.
- [ ] Check the phase file for the ticket that owns this
  endpoint (`docs/07-PHASES/`). A route with no ticket is a
  stub with no owner — do not add one.

## Route + handler

- [ ] Add the handler to `apps/api/src/routes/{area}.rs`
  (`auth`, `uploads`, `jobs`, `billing`, `admin`, `meta`) and
  register the nest in `api_router` (`apps/api/src/routes/mod.rs`).
- [ ] Add the `(METHOD, PATH)` entry to `ROUTE_TABLE` in the
  same file, in its area section. The route-table probe test
  fails if the entry is missing from the router.
- [ ] Authed routes extract `AuthContext`
  (`apps/api/src/auth.rs`, shared via `routes/extractors.rs`)
  and enforce the role; unauthed routes say so in the module
  docs. The extractor never rejects in Phase 0 — Phase 1
  verification replaces it.
- [ ] Handlers return `Result<..., AppError>` rendered through
  `ProblemResponse` (`apps/api/src/problem.rs`): every error
  body is problem+json with a PF code. Never return a bare
  status or a plain-text body.
- [ ] JSON bodies use typed extractors. The framework maps
  rejections for you (error mapper, P0-065/P0-073): bad JSON or
  bad query → 400/422 PF-2002, wrong content type → 415
  PF-2001, oversize → 413 PF-2003. The per-module 1 MiB body
  limit applies to every JSON route (P0-069); uploads are
  exempt structurally — upload bytes never transit the api
  (presigned S3 PUTs).
- [ ] Business logic goes in a service
  (`apps/api/src/services/` — target layout; create the
  directory when the first real handler lands), SQL goes in a
  repo (`crates/core/src/repos/`) — never SQL in a handler.
  A new table means a migration + the data-model doc in the
  same commit (`03-ARCHITECTURE/04-data-model.md`).

## OpenAPI + docs

- [ ] Add `#[utoipa::path]` on the handler and register the
  path in `ApiDoc` (`apps/api/src/openapi.rs`). Only annotated
  handlers appear in the spec — an unannotated route is
  invisible to the console viewer.
- [ ] Regenerate and commit: `just openapi`
  (`docs/api/openapi.json`) and `just route-table`
  (`docs/api/route-table.md`). Both have `-check` recipes;
  CI fails on drift.

## Tests

- [ ] Integration test through the harness
  (`apps/api/tests/common/mod.rs`): `spawn_app()`,
  `seed_user` for rows the test needs, `post_json`/`get`,
  `TestResponse::json()`. The first endpoint tests to use it
  are the OpenAPI tests (P0-067/P0-071).
- [ ] Golden error shapes for the new error paths
  (`apps/api/tests/error_contract.rs` pattern): exact body,
  problem+json content type, PF-code range, request id
  echoed. A 401/429/500 with no live path yet is locked at
  the responder level and the live leg is added when the path
  lands.
- [ ] `just verify` green (fmt + clippy `-D warnings` +
  tests). A red shutdown-drain or pubsub test is the known
  flake (FLAKY-001/FLAKY-002) — re-run, do not chase it.

## Observability + hygiene

- [ ] Nothing new to wire: the access log (P0-072) and the
  HTTP metrics (P0-028) cover the route automatically, with
  the route template as the label — raw ids never reach a
  label. Slow requests (> 250 ms, NFR-1) always log.
- [ ] No PII in logs or error bodies (P0-032): no emails,
  passwords, tokens, or raw IPs. Caller detail goes to
  `tracing` only, in the production profile.
- [ ] Rate limiting / quotas (when the endpoint needs them):
  the core ratelimit module, PF-3007 for overage, PF-3001/
  PF-3002 for plan quotas. A 429 without a `Retry-After`
  is a bug.

## Close-out

- [ ] Tick the ticket in its phase file
  (`docs/07-PHASES/PHASE-*.md`), update `docs/09-STATE/PROGRESS.md`
  (count, current ticket, next-up queue), one conventional
  commit per ticket. Never `git push` from a coordinator
  run — the parent pushes via the snapshot script.
