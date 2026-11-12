# Coding Standards (Rust + TS)

## Rust

- Toolchain pinned via `rust-toolchain.toml`; MSRV honored. `cargo fmt` and `clippy --all-targets -- -D warnings` are CI gates — no allow attributes without a justification comment.
- Errors: crates define typed errors with `thiserror`; the API layer maps them to problem+json with PF-codes. `anyhow` only inside binaries for top-level reporting. No `unwrap()`/`expect()` outside tests and true invariants (comment why).
- Module pattern: one feature per module; handlers thin (parse → service call → map response); business logic in services; SQL in repositories (sqlx, offline mode `sqlx prepare` checked in CI).
- Public items documented (`///`), including invariants; internal complexity documented when non-obvious.
- Secrets/PII: never log emails, tokens, raw IPs (hash), or file bytes. A `redact` helper exists for structured logs.
- Time: `chrono` with explicit `Utc`; day boundaries for quotas in UTC; never `SystemTime::now()` scattered — inject a `Clock` in services for testability.
- Concurrency: all shared state through tokio primitives; any `select!` race gets a dedicated test (Phase 5 race audit).
- Feature flags in code: `#[cfg(feature = "gpu")]` style; flags documented in Cargo.toml with defaults.

## Testing rules (details in `05-QUALITY/01-test-strategy.md`)

- New code lands with tests: unit for logic, integration for endpoints (testcontainers), golden tests for engine.
- Test names: `what_is_expected`; each test asserts one behavior.
- Fixtures: small binary media fixtures in `fixtures/` (checked in, sizes < 2 MB each).

## TypeScript / Next.js (`web/`)

- Strict TS; no `any` (use unknown + narrowing); eslint passes as CI gate.
- Server components by default; client components only for interactivity (uploader, compare slider, toasts).
- API client generated from OpenAPI; no hand-written fetch paths (typo-proof).
- Component a11y: labels, focus states, keyboard paths; axe check in CI.
- No secrets in client env; only `NEXT_PUBLIC_*` config that is truly public.

## Commits & PRs

- Conventional commits with ticket ID: `feat(engine): P2-034 tiled LaMa inference with overlap blend (FR-D-05)`.
- One logical change per commit; engine changes include bench numbers in the body.
- PR checklist (self-review prompt `08-PROMPTS/03-BUGFIX-REVIEW.md`): tests, gates, docs touched, migration additive, no PII logs, quality-gate impact stated.

## Definition of engine quality

Any change under `crates/engine` must state in the commit: golden-suite result (metrics before → after), benchmark delta, and whether thresholds moved (threshold moves require ADR).
