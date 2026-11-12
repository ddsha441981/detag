# Contributing to Detag

Detag is the Rust SaaS that removes watermarks from AI-generated media the
owner has rights to, with a verifiable no-quality-loss guarantee. The full
build plan lives in `docs/` (the PureFrame agent kit): start at
`docs/00-AGENT-BOOT.md`, then `docs/01-WORKFLOW.md`.

## Toolchain

- Rust pinned in `rust-toolchain.toml` (currently 1.99.1). Install via rustup;
  make sure `~/.cargo/bin` is on your PATH.
- `just` 1.42+ task runner (`just --list`). `cargo-deny`, `gitleaks`,
  `cocogitto` are used by CI; install them when touching P0-007/P0-008/P0-010.
- `just verify` must be green before any commit: fmt + clippy `-D warnings`
  + tests. Compose smoke is `just smoke` (lands in P0-090).

## Lint policy

- Clippy warnings are errors (`-D warnings`).
- Never add `#[allow(...)]` without a justification comment on the line
  above it explaining why the lint cannot be satisfied.
- MSRV is honoured: do not use features newer than the pinned toolchain.

## Commits

- Conventional commits, one ticket per commit:
  `feat(auth): P1-014 refresh token rotation with reuse detection`
  Ticket id + FR/NFR trace go in the commit body.
- Subject format (enforced by CI, `.github/workflows/commits.yml`):
  `<type>(<scope>): P<phase>-<num> <description>`, subject ≤ 100 chars,
  and the message must reference at least one requirement id
  (`FR-<area>-<n>` or `NFR-<n>`).
- Types: `feat`, `fix`, `chore`, `docs`, `test`, `refactor`, `perf`,
  `ci`, `build`, `revert`.
- Scope vocabulary — use the area the change belongs to:

  | Scope | Meaning |
  | --- | --- |
  | `auth` | Identity, sessions, API keys, RBAC (Phase 1) |
  | `engine` | Removal engine: detect / mask / inpaint / encode / QC |
  | `api` | `apps/api` HTTP layer: routes, services, repos, middleware |
  | `worker` | `apps/worker` job pipeline |
  | `web` | Next.js console + admin UI |
  | `ops` | Compose, CI, deploy, observability, load tests |
  | `core` | `crates/core`: config, errors, domain types, state machine |
  | `storage` | `crates/storage`: storage provider, presign, multipart |
  | `billing` | `crates/billing`: Stripe/Razorpay adapters |
  | `mailer` | `crates/mailer`: SMTP, templates, outbox |
  | `docs` | Kit docs under `docs/` (spec changes, not code) |
  | `repo` | Repo-wide tooling: workspace, justfile, hygiene, CI config |

  A commit touching several areas takes the scope of its primary area.
- Check your message before committing: `just lint-commits`
  (lints HEAD; `just lint-commits "--range main~3..HEAD"` for a range).
  The linter is `scripts/lint_commits.py` (unit tests in
  `scripts/test_lint_commits.py`).
- Every ticket: code + tests land together; tick the checkbox in its phase
  file and update `docs/09-STATE/PROGRESS.md` in the same commit.
- No secrets in the repo (env vars use the `PF_` prefix; see `.env.example`).
  No PII in logs (never emails, tokens, raw IPs, or file bytes).

## Definition of Done

See `docs/04-ENGINEERING/03-definition-of-done.md`. In short: spec
implemented, tests green, `just verify` green, docs/config updated in the
same commit, no silent quality degradation.

## Release flow

Conventional changelog + version bumps via cocogitto (`cog`, P0-010).
Version bumps are gated by `docs/06-DELIVERY/02-release-checklist.md`.
