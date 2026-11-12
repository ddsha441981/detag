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
