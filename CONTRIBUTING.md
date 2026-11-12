# Contributing to Detag

Detag is the Rust SaaS that removes watermarks from AI-generated media the
owner has rights to, with a verifiable no-quality-loss guarantee. The full
build plan lives in `docs/` (the PureFrame agent kit): start at
`docs/00-AGENT-BOOT.md`, then `docs/01-WORKFLOW.md`.

## Toolchain

- Rust pinned in `rust-toolchain.toml` (currently **1.99.1**). Install via
  rustup; make sure `~/.cargo/bin` and `~/.local/bin` are on your PATH.
  `rust-toolchain.toml` also pins the `rustfmt` and `clippy` components.
- `llvm-tools-preview` is **required** too (`just coverage` and
  `cargo llvm-cov` need its `llvm-cov` / `llvm-profdata` binaries), but it
  is not in `rust-toolchain.toml` — add it once per toolchain:
  `rustup component add llvm-tools-preview`. CI installs it explicitly
  (`.github/workflows/ci.yml`). Without it, `cargo llvm-cov` hangs
  silently trying to auto-install the component — do not wait it out,
  install the component.
- `just` 1.42+ task runner (`just --list`). `CARGO_BUILD_JOBS=2` is set in
  `.cargo/config.toml` (the VM is small; more jobs OOM the build).
- Tools CI uses (install locally when touching the matching ticket):

  | Tool | Version | Install |
  | --- | --- | --- |
  | `just` | 1.42.0 | package manager / `cargo install just` |
  | `cargo-deny` | 0.20.2 | `cargo install cargo-deny --locked` |
  | `gitleaks` | 8.30.1 | release tarball, checksum-verified, to `~/.local/bin` |
  | `cocogitto` (`cog`) | 7.0.0 | `cargo install cocogitto --locked` |
  | `cargo-nextest` | 0.9.151 | `cargo install cargo-nextest --locked` |
  | `cargo-llvm-cov` | 0.9.1 | `cargo install cargo-llvm-cov --locked` |

- `cargo install` builds in `/tmp`, which is a 512 MB tmpfs on this VM
  and fills up on big crates (nextest failed exactly this way). Run
  installs with `TMPDIR=$HOME/workspace/detag/.scratch/tmp`.
- `just verify` must be green before any commit: fmt + clippy `-D warnings`
  + tests. Compose smoke is `just smoke` (lands in P0-090).
- `just coverage` writes `target/llvm-cov/lcov.info` (llvm-cov). Coverage
  floors per crate live in `docs/05-QUALITY/01-test-strategy.md`.

## MSRV

- The **MSRV is the pinned toolchain: 1.99.1**. There is no separate
  MSRV number to track — the pin in `rust-toolchain.toml` is the floor.
- Do not use language or std features newer than the pinned toolchain
  ("MSRV is honoured" in the lint policy below means exactly this).
- CI builds with the pinned toolchain, so a too-new feature fails CI
  before it can land. `cargo +stable` / `cargo +nightly` builds are not
  a substitute for the pinned one.
- **Bumping the pin is bumping the MSRV.** It needs an ADR in
  `docs/09-STATE/DECISIONS.md` first (why, what breaks, what the new
  floor enables), then the pin change, then a full `just verify` from a
  clean clone. A toolchain bump is never snuck into an unrelated ticket.

## Supply-chain policy (cargo-deny)

Policy lives in `deny.toml`; `just deny` (or `cargo deny check`) runs it
locally and CI runs it on every push/PR. The rules:

- **Advisories:** RUSTSEC advisories are errors. Yanked crates are denied;
  `unmaintained` fires for every crate in the graph; `unsound` fires for
  workspace crates. The `ignore` list is empty by policy.
  - A new advisory on a dependency: upgrade the crate first. If no fixed
    version exists and the crate cannot be dropped, add its id to `ignore`
    with a reason comment, file an issue, and record the exception in
    `docs/09-STATE/DECISIONS.md`. Never weaken `yanked` / `unmaintained` /
    `unsound` to make CI pass.
- **Licenses:** only this allow-list is accepted (anything else, including
  AGPL, fails the build by default-deny): `Apache-2.0`, `MIT`,
  `BSD-2-Clause`, `BSD-3-Clause`, `ISC`, `Unicode-DFS-2016`, `Unicode-3.0`,
  `MPL-2.0`, `Zlib`. A dual-licensed `X OR Y` crate passes when any one of
  its licenses is allowed. Allow-listed licenses no crate uses warn
  (`unused-allowed-license`), so the list stays honest.
  - A dependency under a new license needs an ADR before it is added to
    `allow` — licenses are a legal decision, not a build fix.
- **Sources:** crates.io only. A git dependency needs an explicit exception
  in `deny.toml` (comment + ADR); none exist today.
- cargo-deny 0.20 schema note: there are no `vulnerability` / `notice`
  severity keys, `unmaintained` / `unsound` are scopes, and `deny` /
  `copyleft` / `default` under `[licenses]` are deprecated — see the
  P0-007 commit message if the config looks odd.

## Lint policy

- Clippy warnings are errors (`-D warnings`).
- Never add `#[allow(...)]` without a justification comment on the line
  above it explaining why the lint cannot be satisfied.
- MSRV is honoured: do not use features newer than the pinned toolchain.

## Secrets policy

- All secrets are env vars with the `PF_` prefix (see `.env.example`,
  P0-011 — placeholders only, never real values). Never commit `.env`
  (it is gitignored); never paste a real key into code, docs, or logs.
- Two gates enforce this: CI runs `gitleaks detect` on every push/PR
  (`.github/workflows/ci.yml`), and the pre-commit hook runs
  `gitleaks protect --staged`. Install the hook once per checkout:
  `./scripts/install-hooks.sh`. Local full scan: `just secrets`.
- If a real secret ever lands in history: rotate the credential
  immediately, file an S1 issue, and purge it from history only with
  owner approval (history rewrite is destructive).

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
- Config changes: a new or renamed `PF_*` var goes in `Settings`
  (`crates/core/src/config.rs`), `.env.example`, and the runbook env
  table (`docs/06-DELIVERY/03-deploy-runbook.md`) in the same commit —
  `just env-check` fails on drift. Then regenerate the settings
  reference table with `just gen-config-table` and commit
  `docs/06-DELIVERY/04-config-reference.md` with it;
  `just config-table-check` (and CI) fail when the table is stale.
  Script tests run with `just test-lint-commits` (discovers all
  `scripts/test_*.py`, despite the name).
- Every ticket: code + tests land together; tick the checkbox in its phase
  file and update `docs/09-STATE/PROGRESS.md` in the same commit.
- No secrets in the repo — see "Secrets policy" above.
  No PII in logs (never emails, tokens, raw IPs, or file bytes).

## Definition of Done

See `docs/04-ENGINEERING/03-definition-of-done.md`. In short: spec
implemented, tests green, `just verify` green, docs/config updated in the
same commit, no silent quality degradation.

## Release flow

Conventional changelog + SemVer tags via cocogitto (`cog`, P0-010,
config in `cog.toml`, tags prefixed `v`). The full flow lives in
`docs/06-DELIVERY/03-deploy-runbook.md` ("Release flow") and the gate in
`docs/06-DELIVERY/02-release-checklist.md`; the short version:

1. **Gate:** the release checklist top-to-bottom. Anything red blocks the
   release (owner-input items get listed, not skipped).
2. **Dry run first:** `just release-dry-run` (`cog bump --auto --dry-run`
   prints the next version the commits imply — `feat` → minor,
   `fix`/`perf` → patch, `BREAKING CHANGE` → major — and writes nothing).
   `just changelog` regenerates `CHANGELOG.md` from history; review the
   diff. Never hand-edit changelog entries or the version in
   `Cargo.toml` — `cog` owns both.
3. **Cut:** `cog bump --auto` (bumps `Cargo.toml` versions, writes
   `CHANGELOG.md`, creates the `vX.Y.Z` tag), then push the tag.
   `cog bump` requires a clean tree.
4. **Deploy the tag:** build images from the tag (not from `main`) and
   verify with the post-deploy smoke. The tag is the rollback point
   (previous image tags are kept).
5. **Pre-1.0 rule:** while the workspace version is `0.x`, `feat` bumps
   the minor and `fix` bumps the patch; breaking changes bump the minor,
   never the major, until v1.0.0 is declared.

Push/tag policy (ADR-0016): the build coordinator commits locally only
and never pushes or tags; the parent pushes and tags after the run report.

## Version updates

- `Cargo.lock` is committed. Dependency updates go through it, never
  around it.
- **Patch / minor updates** (including `cargo update` runs): update, run
  the full `just verify`, and commit the lockfile change with a short note
  on what changed and why. Routine; no ADR.
- **Major updates** (any dependency's major version, a Rust edition
  change, or a toolchain major bump): an **ADR is required first** in
  `docs/09-STATE/DECISIONS.md` — what changes, what breaks, migration
  plan, rollback plan, and why now. The update lands only after the ADR
  is recorded. This is the "ADR on majors" rule from P0-014.
- **Toolchain updates:** see "MSRV" above — bumping the pin is bumping
  the MSRV and needs an ADR, even for a minor toolchain release, because
  it moves the floor for every contributor and for CI.
