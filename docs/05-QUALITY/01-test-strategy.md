# Test Strategy

## Pyramid

| Layer | Scope | Where | Gate |
| --- | --- | --- | --- |
| Unit | pure logic: mask math, SSIM/PSNR impls, state machine, quota math, JWT claims, pricing calc | colocated `#[cfg(test)]` | every commit |
| Integration | HTTP endpoints + repos + redis + storage via **testcontainers** (pg, redis, minio) | `apps/api/tests/`, `apps/worker/tests/` | every commit |
| Engine golden | full detect→inpaint→encode→QC on fixture media with asserted metrics | `crates/engine/tests/golden/` | every engine commit + nightly |
| Property/fuzz | mask codec roundtrips, idempotency, state-machine invariants, parsers (proptest) | `crates/core/tests/prop/` | nightly |
| E2E | Playwright: register → upload → job → result; admin flows | `web/e2e/` | nightly + pre-release |
| Load/soak | k6: auth, job create, status poll, SSE; 24h soak pre-release | `ops/loadtest/` | Phase 5 |
| Smoke | compose up, healthz/readyz, model registry verified, one tiny image job end-to-end | `just smoke` | every commit (fast path) |

## Coverage floors (llvm-cov, CI-reported)

crates/core >= 85%, crates/engine >= 80%, apps/api >= 75%, apps/worker >= 70%. Web: critical flows covered by e2e, no unit-floor.

- **Report:** `just coverage` — runs the whole suite under llvm-cov
  (~12 min on the dev VM; the instrumented rebuild dominates) and
  prints the per-crate table via `scripts/coverage_gate.py`, which
  reads `target/llvm-cov/lcov.info` and counts production sources
  only (`crates/<name>/src/`, `apps/<name>/src/` — test/bench/example
  files are instrumented too but are not what the floors measure).
  The CI verify job runs the same script in report mode after its
  own llvm-cov step, so the per-crate table lands in the CI log and
  `lcov.info` is uploaded as an artifact.
- **Gate (OFF until Phase 2):** `just coverage-gate` — same report,
  but exits 1 if any crate is below its floor. The Phase 2 ticket
  flips CI to the gate; until then a below-floor crate is printed as
  `BELOW FLOOR` and the run still exits 0 ("report-only"). Do not
  wire the gate into CI or `just verify` before that ticket.
- **First report (P0-098, 13 Nov 2026):** core 96.7% (4438/4589),
  engine 99.1% (111/112), api 92.7% (793/855), worker 83.8%
  (1212/1447) — all above their floors. Note the engine number is
  nearly vacuous: the engine is a placeholder with 112 production
  lines; the denominator (and the floor's meaning) arrives with the
  Phase 2 engine. Re-measure after every engine ticket, not just at
  the gate flip.

## Golden media suite (source of truth for "quality did not drop")

Fixtures in `fixtures/` (all synthesized in-house or permissively licensed):
- **Images (24):** {png, jpeg, webp} × {512², 1920×1080, 3840×2160, 8000×8000-tile} × {badge mark, text mark, clean control}.
- **Videos (3):** 30 s 1080p h264 with badge + audio; 10 s 720p; 4 s vertical 1080×1920.
- **Templates:** synthesized badge pack + manifest (name, sha256, source = "synthesized").

Every fixture has an expected-metrics envelope (`fixtures/expected/*.json`). Engine changes must keep all envelopes green (floors from `02-REQUIREMENTS/04-quality-policy.md`). Determinism: run twice, outputs must be bit-identical (FR-D-06).

## Snapshot testing (P0-095)

JSON the api serves is pinned with [insta](https://insta.rs/)
(`crates/core/tests/snapshots.rs`, snapshots in
`crates/core/tests/snapshots/*.snap`): `problem+json` bodies in both
profiles and the `Page<T>` list envelope today; engine quality
reports join with the Phase 2 golden suite (same harness, same gate
— no report type is faked before it exists). Transactional email
copy is pinned the same way (P1-029:
`crates/mailer/tests/snapshots.rs`, snapshots in
`crates/mailer/tests/snapshots/*.snap`): subject + text + HTML per
template, rendered with a fixed context.

Snapshots are code: a changed snapshot is reviewed like a code
change, and the `.snap` files are committed with the code that
produced them. Never accept a snapshot you have not read.

- **Review:** `cargo insta review` (install once:
  `cargo install cargo-insta`) — shows the diff per snapshot,
  accept/reject one by one. Without the CLI:
  `INSTA_UPDATE=always cargo test -p detag-core --test snapshots`
  overwrites the `.snap` files — only after reading the `git diff`.
  Same for the mailer snapshots:
  `INSTA_UPDATE=always cargo test -p detag-mailer --test snapshots`.
- **Check locally:** `INSTA_UPDATE=no cargo test -p detag-core --test snapshots`
  (also the CI mode — see below). `just snapshots-check` runs both
  the core and the mailer snapshot suites in check mode.
- **CI gate:** the verify job runs with `INSTA_UPDATE=no`; any new
  or changed snapshot fails the build until it is reviewed and
  committed. A test that produces a snapshot with a nondeterministic
  value (timestamp, random id, port) is a bug in the test, not in
  insta — pin the value (fixed request ids, fixed cursors).

## Email flow tests (P1-030)

`apps/api/tests/email_flow.rs` proves the whole mail path, not
just the endpoints: the outbox contract per mail (template,
recipient hash, token shape, stored token row under the right
purpose, payload carries ids only — never a raw address) and the
delivery path end to end (register/forgot → drain → delivered
mail → the token in the delivered mail verifies / resets).
The "MailHog" in these tests is a fake SMTP relay on loopback
(the same minimal protocol subset the mailer and worker suites
use): no docker on this VM (DOCKER-001) and no real SMTP
anywhere in CI — the relay is always 127.0.0.1, so no test mail
ever leaves the test process. Real MailHog exists only in the
compose dev stack (`ops/compose.dev.yml`). The e2e tests drain
the outbox, which claims every due row, so each runs on its own
scratch database (testkit `TestCtx`) — the shared harness DB
would let one test's drain mark another test's mail sent.

## Email send caps (P1-031)

`crates/mailer/tests/drain.rs` proves the per-recipient hourly
caps at the drain, the only choke point: 3 verification mails
send and the 4th is parked (`rate_limited: 1`, nothing sent,
abuse audit row `email.rate_limit_tripped` with hash/template/
class/cap/count and no raw address); the same for reset; a
marketing-class row parks on sight (cap 0, even the first one);
the 3rd mail under the cap still sends; backdating `sent_at`
past the hour re-opens the window; the cap is per recipient
(3 sends to A do not spend B's budget); `email-change-verify`
shares the verify class with `verify-email` (2 + 1 fill it, the
next trips it); and four `lockout-notice` alerts to one address
all send — transactional alerts are never capped. The count
itself is pinned in `crates/core/tests/repos_live.rs`
(`count_sent_recent_for_recipient`: only sent rows, only the
named templates, only the named hash, only inside the window).
A test that samples `now` before enqueueing will see nothing
due — `claim_due` needs `scheduled_at <= now`, and the rows'
`scheduled_at` is their enqueue time, so pass a fresh
`Utc::now()` at drain time.

## API keys (P1-033)

`apps/api/tests/api_keys.rs` proves key creation through the
real router: 201 with the full `pf_live_` secret (40 chars —
`pf_live_` + 32 base64url) shown once, the stored row
carrying only its sha256 (`secret_hash`, never the secret),
`prefix` = `pf_live_` + first 4 secret chars, the creation
audited as `api_key.created` with no secret or hash in the
audit row; validation 422s (unknown / empty / duplicate
scopes, blank or >100-char name — `field_errors` on
`scopes`/`name`); 401 with no credential; the 10-live-keys
cap (the 11th sequential create is 409 `PF-2011`, and the
failed create stores nothing); and the cap under concurrency
(12 concurrent creates admit exactly 10, 2 conflict — the
row lock in the service transaction serialises them; the
loop mints its token once up front, so no Argon2id sits in
it, unlike the login race in `lockout.rs`). The repo layer
is pinned in `crates/core/tests/repos_live.rs` (create
roundtrip, live count excludes revoked rows and other
users' keys, duplicate `secret_hash` is a unique
violation). Test targets are not covered by `cargo sqlx
prepare`, so tests must not use `query!` macros — they use
untyped `sqlx::query` + `try_get`, or repo functions.

## Property testing (P0-096)

Property tests live in `crates/core/tests/prop/` (cursor codec,
Redis key scheme, upload state machine; the mask codec's roundtrips
join with the Phase 2 codec). Each `proptest!` block runs
**1000 cases** (`ProptestConfig::with_cases(1000)` in the test
file — not an env var, so the count is pinned in code). Strategies
use proptest's standard generators, so a failure shrinks to a
minimal case automatically; turn a shrunk failure into a regression
test. Run: `just prop`. Also runs on every push (part of
`cargo test --workspace`) and on the nightly job
(`.github/workflows/nightly.yml`, 02:00 UTC).

## Flaky-test policy

A test that fails twice without a product change is quarantined with
an ISSUES.md entry; fix or delete within the week. Nightly run
surfaces quarantined tests on the report.

**Quarantine convention (P0-099):**

- Tag the test: `#[ignore = "flaky:<issue-id>"]` (e.g.
  `#[ignore = "flaky:FLAKY-001"]`). Plain `#[ignore]` without the
  tag does not count — the tag is what the tooling finds.
- Add (or update) the `FLAKY-<nnn>` entry in `docs/09-STATE/ISSUES.md`
  with the failure signature, the suspected cause, and the strike
  count. `just list-quarantined` flags any tagged test whose entry
  is missing.
- A quarantined test is skipped by `cargo test` (and therefore by
  `just verify`) by default; run it on purpose with
  `cargo test -- --ignored` or clear it with
  `just test-repeat <name> 5`.
- **Strikes:** each failure without a product change is one strike;
  two strikes = quarantine. A fix (or a hardening ticket) clears the
  tag; the ISSUES.md entry records the fix commit.
- **Nightly:** the nightly job runs `just list-quarantined` so the
  quarantined set is visible every day instead of rotting silently.
  Quarantined tests are listed, not run, in nightly.
- **Reproduce first:** before quarantining, run
  `just test-repeat <name> 5` — a test that passes 5/5 in a row is
  load-sensitive, not broken; quarantine it anyway (the policy is
  about the suite, not the test) and let the hardening ticket own
  the race.

**Not quarantined yet (deliberate):** FLAKY-001 and FLAKY-002 meet
the letter of the two-strike rule, but earlier runs kept them in the
suite on purpose — re-running `just verify` until green, rather than
silently dropping shutdown-drain and pubsub coverage from every
commit. Quarantining either one is an owner decision (it changes
what `just verify` covers); the tooling to do it is here, the call
is not made in P0-099. FLAKY-003 has one strike (P0-093).

## What "green" means per run

- Ticket commits: unit + integration + smoke green.
- Nightly: + property/fuzz, engine golden, e2e (if web changed), quarantined list refresh.
- Phase boundary: everything from clean clone + 30-min k6 burst + soak summary.

## Test data hygiene

No real user data in tests. Emails use `example.com` variants. Storage fixtures regenerated by `just fixtures:gen` (deterministic seeds). Test templates are clearly labeled `SYNTHETIC-TEST-ONLY` to never collide with the production template pack.
