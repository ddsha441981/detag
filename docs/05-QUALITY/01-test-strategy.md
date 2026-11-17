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

## API-key lifecycle suite (P1-039)

`apps/api/tests/api_key_lifecycle.rs` walks ONE key through
the whole lifecycle in order, on a frozen clock: create
(201, the full `pf_live_` secret shown once) → use (both
scopes reach the jobs stubs, `last_used_at` stamped) →
rotate (a replacement row, same name + scopes) → old-valid-
in-window (both secrets authenticate, the old one still 501
one second before the deadline) → expired (past the window
the old secret is the generic 401 `PF-1001` while the new
one keeps working) → revoked (the replacement dies
immediately, no grace; the old secret stays dead) — and
asserts the audit trail at the end (exactly one
`api_key.created` / `api_key.rotated` / `api_key.revoked`
row, in order, no secret or hash in any of them). Two gaps
the isolated tests left open are proven here too: a
directly-expired key (its `expires_at` set in the past, not
via rotation) is 401 at the HTTP layer and frees its cap
slot (10 keys + expire one, the 11th create is 201 again);
and scope enforcement in the ticket's phrasing — a
`jobs:read`-only key cannot create (POST /v1/jobs and POST
/v1/api-keys are both 403 `PF-1002`, while GET /v1/jobs is
501), mirrored by a write-only key that writes but cannot
read. Re-login after advancing the frozen clock past the
access token's 15-minute life: the JWT minted at the start
is expired by the time the revoke step runs (the P1-035
precedent — read the user's email back from the users row
if the helper did not return it).

## API-key ACL matrix (P1-040)

`apps/api/tests/api_key_matrix.rs` is the registry the
extractor's scope table answers to: one row per `/v1` route
(65 rows), each naming what an API key gets — `Allowed`
(a key carrying the named scope reaches the handler, 501
on the stubs), `Forbidden` (any key is 403 `PF-1002`),
`Unprotected` (no credential is checked at all — the plain
501 stub answers anonymous and key alike), or `Ignored`
(the handler takes no credential, so the anonymous and key
responses are identical). The completeness test fails if a
`ROUTE_TABLE` entry has no row or a row names no route, so
the matrix cannot drift from the router silently. The
`Unprotected` rows are gaps recorded honestly, not 403s:
the route's handler ticket adds the extractor/guard and
flips the row in the same commit — except the ones that are
public by design (`GET /v1/plans`, the provider webhooks,
abuse intake), which the row's note names. Scope-negative
coverage walks every `Allowed` row with a key lacking the
row's scope (403), and a separate test proves an admin
user's own key is 403 on every live admin route ("no admin
ever"). P1-042's full roles matrix reuses this registry
shape.

## Roles + permission matrix (P1-042)

`apps/api/src/authz.rs` is the single source for who may do
what on the admin surface: one `Capability` per admin
operation (16 code constants, `admin.users.read` …),
each naming the roles that may use it — reads admit admin +
support, writes and impersonation admit admin only — plus
the route→capability table for all 19 `/v1/admin/*`
routes. The guards (`AdminContext`, `AdminReadContext`)
look the route up in the table and enforce the row, so the
matrix, not the extractor type, decides; an API key or an
impersonation token is 403 on every row. The registry
`apps/api/tests/authz_matrix.rs` pins the table three ways:
a completeness test (every admin route has exactly one row,
every row names a real route), a truth-table test (each
row's capability resolves through `for_route` and admits
exactly the promised roles), and a live walk (the 5 live
routes × anonymous / user / support / admin / an admin
user's key — 401 `PF-1001` with no credential, 403
`PF-1002` for a wrong role or a key, 200 for an admitted
role). The 14 stub rows are promises, not proofs — the
stubs take no extractor yet, so their guard lands with
their handler ticket (P1-043 puts the guard on every
`/v1/admin/*` route) — but the row is pinned here first,
so the intended capability is recorded before the guard
lands. The full rendered matrix is the admin guide
(`docs/06-DELIVERY/07-admin-guide.md`, P1-047).

## Admin route guard (P1-043)

Every `/v1/admin/*` route sits behind the matrix guard now,
live or stub: `apps/api/src/routes/admin.rs` wraps the 14
stubs in `admin_stub` (`AdminContext`, writes) /
`admin_read_stub` (`AdminReadContext`, reads) — the same 501
`PF-5004` as the shared stub, but the guard rejects first
(no credential 401 `PF-1001`, wrong role or key 403
`PF-1002`). The public `POST /v1/abuse` intake keeps the
plain stub — it is not an admin route. The registry
`apps/api/tests/admin_guard.rs` walks all 19 admin routes ×
anonymous / user / support / admin / an admin user's key —
admitted roles answer 200 on the 5 live routes and 501 on
the 14 stubs — and proves the audit trail both ways: an
admin suspend writes exactly one `user.suspended` row
(actor = the admin, target, before `active` → after
`suspended`, reason), while a denied suspend (403), a
suspend of an unknown id (404), and an admitted stub call
(501) each write no row (the trail records actions, not
attempts). Registration/login audit against the same user
target, so the audit assertions filter by action, not just
by target. Side effects of guarding the stubs: the
route-table probe expects 401 (not 501) on every admin
route now, and the API-key matrix flips the 14 admin-stub
rows `Unprotected` → `Forbidden` (counts 13/28/15/9). The
automatic audit middleware is P1-046's scope — this ticket
proves the guard + the audit rows the services already
write. Three doc comments carried a literal `<redacted>`
baked in by the output pipeline (the P1-029 trap: the word
after "Bearer" is redacted in transit) — fixed here in
`routes/admin.rs`, `auth.rs`, and
`api_key_rate_limits.rs`; never write that phrase in a
comment again.

## Support read-only (P1-044)

The support contract pinned from the support role's point
of view (`apps/api/tests/support_readonly.rs`, FR-A-07):
the matrix assert admits support exactly on the `.read`
capabilities (admin on all, plain user and anonymous on
none — every admin route in the route table maps to exactly
one capability); support reads the two live views with real
rows (the users search returns the seeded users with role
and status, the jobs list returns the seeded job with kind
and status and no key material — a web-created job shows no
prefix); support is admitted on all 8 stub reads (501
`PF-5004` — the guard passed, the handler has not landed);
and all 9 writes are 403 `PF-1002` with no state change (a
denied suspend leaves the target active, a denied restore
leaves a suspended target suspended, a denied impersonate
mints no token — each proven by the target's row and the
absence of its audit row). The harness DB persists across
runs, so the list assertions filter by the test's own prefix
/ `user_id`, never an exact unfiltered count.

## IDOR sweep v1 (P1-045)

Cross-user access returns 404, never 200 (a leak) and
never 403 (a leak by another name — it confirms the thing
exists) — `apps/api/tests/idor_sweep.rs` (FR-A-07 / T13,
NFR-31). The two live families are proven end to end:
keys (B revoking / rotating A's key is 404 `PF-2010`, the
key still authenticates afterwards, A revoking it is 204,
and then it is 401) and sessions (B's list never contains
A's session, B deleting A's session is 404, A's session is
still listed afterwards, A deleting it is 204). Jobs,
uploads, webhook-endpoints and the api-keys list have no
handler yet, so the sweep pins their current answer — 501
`PF-5004` to an authenticated cross-user caller (and to an
anonymous caller on the plain upload stubs) — no data, no
200, no 403. The 404 ownership proof for each of those
families lands with its handler ticket, and this test then
gains the live rows and loses the stub rows.

## Privileged action audit middleware (P1-046)

Route metadata drives automatic audit for admin writes, so
no handler can forget its audit row —
`apps/api/tests/privileged_audit.rs` (FR-A-07 / T13,
NFR-31). The middleware (`apps/api/src/audit_middleware.rs`)
sits on the `/v1/admin` nest and reads the action + target
type from `Capability::audit_action_for` /
`audit_target_type_for` in `apps/api/src/authz.rs` (one
action per write route — 9 writes, 10 reads, pinned by the
`audit_actions_cover_every_admin_write_and_no_read` unit
test, and the actions are unique per route). The three live
writes each write exactly one row (suspend `user.suspended`
with before `active` → after `suspended` and the reason
verbatim; restore `user.restored`; impersonate
`user.impersonated` with `after.expires_in` 900) — their
services write the row inside the mutation's transaction
and the handler marks the response `AuditHandled`, so the
middleware stands down; a duplicate row here means the
marker protocol broke. The mechanism proof layers the
middleware over a fake handler with no manual audit call: a
200 write audits one `admin.flags.updated` row (actor the
admin, target type `flags`, no target id), a 501 from the
same route audits nothing, and a 200 read audits nothing.
On the real app a denied write (support suspend, 403), an
admitted stub write (admin quota-grant, 501) and a
successful read (admin user search, 200) each audit nothing
— the trail records actions, not attempts or views. A
handler that wants rich payloads on a middleware-written
row attaches `AuditDetail`; without it the row carries the
action, actor and target, and no before/after. When the
audit insert itself fails the middleware answers 500 (a
privileged action without its audit row is a failed
request, not a silent gap). Wiring note: axum 0.8's
`middleware::from_fn` cannot extract `State` — the nest
uses `from_fn_with_state`, so `admin::router` and
`api_router` take the state as a parameter.

## Role escalation tests (P1-048)

Every way up the ladder is closed, and the closed door
leaves no trace in the audit trail —
`apps/api/tests/role_escalation.rs` (FR-A-07 / T13,
NFR-31). A plain user tries all 19 admin routes (403
`PF-1002` everywhere), support tries all 9 writes (403),
and an admin user's own full-scope key tries all 19 (403
— "no admin ever"); each denied write asserts no audit row
for its action with the caller as actor (scoped by actor,
not absolute count — the harness DB persists across runs,
the P1-046 lesson), and the live-write targets are proven
untouched (a denied suspend leaves the target `active`, a
denied restore leaves a suspended target `suspended`, a
denied impersonate mints no token row). The spec's "all 403
+ audited" is read under the rule P1-043/P1-046 settled
("the trail records actions, not attempts"): the audit
behaviour asserted is that a denial writes nothing — a
denial that wrote a row would contradict the admin guide.
The fourth test forges the token itself: a plain user's
token with its `role` claim rewritten to `admin` (payload
re-encoded with a hand-rolled base64url pair, original
signature kept) is 401 `PF-1010` on a read and on a write,
and the same for a role forged to `support` — the signature
fails, so the forged role never reaches a guard. The role
is never trusted from the token at all: it is re-read from
the user row on every request (P1-022).

## Plans service (P1-049)

The catalog is the seeded `plans` table read through a
cache, and the effective plan is the newest live
subscription's plan, else Free —
`crates/core/src/plans.rs` (FR-P-01) +
`apps/api/tests/plans.rs` (7 tests). `Entitlements`
parses the seven jsonb keys in exactly one place, and the
parse is fail-closed: a missing, mistyped, or negative
numeric key reads as 0, an unknown lane reads as
`standard`, a non-bool feature value is ignored — a plan
that does not grant a quota grants nothing. The seed
values are pinned per plan (Free 10/0/25 MB/6 h/1,
Pro 500/3600/1 GB/24 h/3 + api_keys + webhooks,
Business 5000/18000/4 GB/72 h/10 + priority + batch).
The catalog caches for 5 minutes (`PLAN_CACHE_TTL`) and
invalidates explicitly on admin edit; an edit made behind
its back is visible once the TTL expires (proven by a
short-TTL test). Resolution is proven live: no subscription
→ Free (`FreeDefault`); `trialing`/`active` → that plan;
`past_due`/`canceled` → Free; two live subscriptions →
the newest wins (not the dearest). A catalog with no
`free` row errors `MissingFreePlan` instead of inventing
a plan — proven with an empty store, never by deleting
the seeded row (the harness DB is shared). The profile
now resolves through `effective_plan` (the quota snapshot
parses through the same `Entitlements::from_value`), so
the profile, the middleware (P1-051), and the snapshot
can never disagree about what a plan grants.

## Quota counters (P1-050)

Usage is counted twice, on purpose, and the two counts
must agree — `crates/core/src/usage.rs` (FR-P-02 /
FR-P-07) + `crates/core/tests/usage_live.rs` (4 live
tests) + 3 in-crate unit tests. `UsageMeter::record`
adds to the Redis day-hash (`pf:usage:{user}:{YYYY-MM-DD}`,
fields `images` / `video_seconds`, 48 h TTL) with one
atomic Lua script (HINCRBY both fields + EXPIRE), then
upserts the durable `usage_counters` row
(`repos::usage_counters::increment`, ON CONFLICT add).
The day is always UTC, from the injected clock — the
AC test freezes the clock at 23:59:59 UTC, records into
the 15th, advances two seconds past midnight, and proves
the 16th's counters start at zero while the 15th's are
untouched, in Redis AND in the DB. Ten concurrent
records land as exactly 10 in both stores (no lost
update). A dead Redis fails the record closed (no DB row
either — the Redis write is first, so a Redis failure
never reaches the DB). `reconcile_day` repairs drift
with the DB winning, compare-and-set (a concurrent
record moves the hash and the CAS fails rather than
wiping the record); the nightly schedule that runs it is
P1-082's. The video-seconds estimator is the upload's
known duration rounded up; an unknown duration reserves
nothing (ASSUMPTION-016 — no bitrate data exists to
invent an estimate from; the completion true-up in
P1-079 charges the real seconds).

## Quota middleware (P1-051)

The pre-check on `POST /v1/jobs` (`apps/api/src/services/quota.rs` + `apps/api/tests/quota_middleware.rs`, 9 live tests, FR-P-02 / FR-P-06 / FR-P-07): the gate reads the durable snapshot (`effective_plan` + `quota_snapshot_for_plan` — the same two sources as `/v1/me`, so the decision agrees with the profile snapshot by construction; the agreement is proven, not assumed, by `quota_headers_agree_with_profile_snapshot`). Image uploads need 1 image remaining (`PF-3001`), video uploads need the P1-050 estimate (at least 1 video-second, even for an unknown duration — a 0-limit plan grants no video at all) against the remaining video-seconds (`PF-3002`), and `running >= plan.concurrency` is `PF-3008` (ADR-0026). Check order is quota dimensions first, then concurrency, so the code is deterministic when several limits trip. Every denial is 429 problem+json with the FR-P-06 upgrade hint in the detail and the `X-Quota-*` headers; an allowed request reaches the stub's 501 with the same headers. A `queued` job occupies a slot, a `held` job does not (proven by the two-user concurrency test). A missing or foreign `upload_id` is 404 `PF-2010` (the P1-045 IDOR rule); no body, or no `upload_id`, reaches the stub — with the caller's quota headers since P1-052 (the P1-051 no-headers carve-out is gone). The check records nothing and acquires no slot — job creation is still the 501 stub, so recording would burn quota for a job that never exists (ASSUMPTION-017); the atomic check-and-record lands with real job creation in Phase 2, and the crash-safe slot acquire/release is P1-053's.

## Quota headers everywhere (P1-052)

Every job-creating response carries the `X-Quota-*` headers (`apps/api/tests/quota_headers.rs`, 8 live tests, FR-P-02): `POST /v1/jobs` with no body or no `upload_id` reaches the stub WITH the caller's headers (the P1-051 test that pinned their absence is updated to the new contract); `POST /v1/jobs/{id}/retry` carries the caller's headers on its 501 stub — the handler reads no job row, so an unknown id still 501s, and the 404 ownership proof lands with the real retry handler in Phase 2 (ASSUMPTION-018). The stub-path headers agree with the `/v1/me` snapshot for the same user (`stub_headers_agree_with_profile_snapshot`; the gated path's agreement was proven in P1-051). An API key with `jobs:write` is a creator too — its no-body create carries the owner's headers (the extractor loads the key's owner into the context, P1-034). `POST /v1/admin/jobs/{id}/reprocess` carries no headers: the quota subject is the job owner, not the admin (ASSUMPTION-018). The UI contract is `docs/06-DELIVERY/08-quota-ui-contract.md`.

## Concurrency slots (P1-053)

The crash-safe slot acquire/release (`crates/core/src/slots.rs` + `crates/core/tests/slots_live.rs`, 9 live tests + 2 unit tests, FR-P-02): one Redis sorted set per user (`pf:slots:{user}`, member = job id, score = lease-expiry epoch-ms), one atomic Lua script per acquire (prune expired, admit under the plan's `concurrency` limit, re-acquire idempotent), `release` on completion, `renew` by the worker heartbeat, `recover` as the explicit leak sweep. The AC is `expired_leases_are_reclaimed`: acquire a slot, advance the frozen clock 61 min past the 1 h lease, `recover` reclaims exactly 1, and a fresh acquire succeeds — the same acquire also succeeds with no sweep at all (lazy prune). The race test proves exactly 3 admits under a 10-way concurrent acquire at limit 3; `renew_extends_the_lease` proves a heartbeat extends a live lease past its original expiry and a released slot cannot be renewed back to life; a 0 limit admits nothing without touching Redis, and a dead Redis fails closed (Pool/Redis error, never a silent grant). No production caller yet — Phase 2 intake wires acquire at job create and release at completion; the P1-051 pre-check stays DB-count-based until then (ASSUMPTION-019).

## Overage policy (P1-054)

Every quota denial is a 429 with the plan-matrix link, and it leaves nothing behind (`apps/api/tests/overage_policy.rs`, 4 live tests, FR-P-06): `PF-3001` (images), `PF-3002` (video-seconds) and `PF-3008` (concurrency) each carry `upgrade_url` = `{PF_PUBLIC_BASE_URL}/plans` in the problem body, next to the detail's upgrade hint and the `X-Quota-*` headers. The tests prove the no-silent-queueing half: after a denial the caller has no new `jobs` row, the `usage_counters` row is unchanged (or still absent), and `SlotPool::held` reads 0 — so the Phase 2 intake, which can only enqueue a job row that exists, can never queue the denied job later. The allowed-create control proves the link appears only on denials: the stub's 501 carries the headers and no `upgrade_url`. The policy (denial contract, grant bypass, what overage is not) is `docs/06-DELIVERY/09-overage-policy.md` (ASSUMPTION-020).

## Plan feature flags (P1-055)

The plan's feature flags are checked by the services (`apps/api/tests/feature_flags.rs`, 7 live tests, FR-P-01): `Entitlements::grants` in core maps the four gated names — `api_keys`, `webhooks`, `batch` from the `features` map, `priority_lane` from the `lane` (a `priority_lane: true` map entry can never grant it) — and `require_feature` in `services/entitlements.rs` resolves the caller's effective plan and denies with 403 `PF-3003` (detail "your plan does not include this feature; upgrade to use it") when it is not granted. Gated today: `POST /v1/api-keys` (Free 403 + no row minted, Pro/Business 201; validation still runs first, so a malformed Free request is 422) and `POST /v1/webhook-endpoints` (Free 403, entitled caller 501 stub; a key caller passes the gate to the stub). `batch` and `priority_lane` have no HTTP surface yet — their mapping is proven by the `grants()` unit tests in `crates/core/src/plans.rs` (Free grants none, Pro `api_keys` + `webhooks`, Business all four) and their call sites land with the batch endpoint and the Phase 2 intake (ASSUMPTION-021). The gate rippled through 10 existing suites: every test that mints a key now grants its user Pro first via the new `TestApp::grant_plan` harness helper (one suite, `quota_headers`, had to re-pin its expected header values to the Pro limits 500/3600); `idor_sweep`'s stub-rows test grants its probing user Pro so the webhook-endpoints row still reaches the 501 stub. ASSUMPTION-015 clause (1) (no plan gate on key creation) is closed by this ticket — its clauses (2) (no verified-email requirement) and (3) (409 cap) still stand.

## Admin plan editor (P1-056)

The plan editor is `GET` / `PUT /v1/admin/plans` (`apps/api/tests/admin_plans.rs`, 6 live tests, FR-P-01): GET lists the three seeded plans cheapest first with the FR-P-01 values; PUT batch-edits them (per-plan updates + a `remove` list, one transaction, cache invalidated after the commit) and is audited once as `admin.plans.updated` (actor, target type `plans`, changed codes in `after`). The PUT's validation table is proven case by case — negative or missing entitlement keys, unknown lane, unknown feature, mistyped feature value (plain 422, no `field_errors`), bad Stripe price id, Free priced or given a price, bad currency, negative amount, a code in both lists, a duplicated code, Free in `remove` (422) — each followed by a GET proving nothing was applied, and the failures write no audit row. Removal is 409 `PF-2012` (new code, the next free PF-2xxx slot) while the plan has any subscription — live or historical, since `plan_id` is `ON DELETE RESTRICT` — and removing Business with no subscriptions leaves a 2-plan catalog. Each test runs on its OWN scratch database (`detag_api_harness_plans_<tag>`, the `job_events_retry.rs` pattern: static DROP/CREATE literals, drop-stale-first, drop at the end): the editor mutates the shared catalog, and the other suites pin the seeded values (`plans.rs` pins all three plans exactly; the quota suites pin Pro's limits), so editing the shared harness DB would race them. The edit rippled through 5 suites, all updated in the same commit: `admin_guard` + `authz_matrix` (plans rows `live: true`, the walks send the no-op `{"plans": []}` body for the PUT — 7 live routes, 12 stubs now), `support_readonly` (plans leaves the stub-read list for the live-views test), `api_key_matrix` (notes, outcome unchanged — keys are still 403), and the route-table generator (plans live; it also corrects two older omissions — `POST /v1/api-keys/{id}/rotate` and `DELETE /v1/api-keys/{id}` were live since P1-035/P1-036 but never marked, so the table said 23 live / 44 stubs when the truth was 25 / 42; it now says 27 live / 40 stubs). The editor's spec gaps are ASSUMPTION-022 (batch PUT, no plan creation, Free protections, historical-subscriptions block).

## Entitlement suite (P1-057)

Free vs Pro, end to end through the HTTP layer (`apps/api/tests/entitlements.rs`, 6 live tests, FR-P-01 / FR-P-02 / FR-P-06 / FR-P-07): the image limit trips at exactly the plan's number — Free 9/10 reaches the stub and 10/10 is 429 `PF-3001`, Pro 499/500 reaches the stub and 500/500 is 429 `PF-3001`, both with the upgrade hint and the plan's remaining in the `X-Quota-*` headers (Free 0/0, Pro 0/3600). The UTC reset is proven through the gate AND the profile on a frozen clock: a Free user at 10/10 is denied at 23:59 on the 15th, and two minutes past midnight the same user reads 10 remaining on `/v1/me` and reaches the stub, while the 15th's usage row is untouched. Pro's concurrency is 3 slots: 2 queued jobs reach the stub, the 3rd makes the next create 429 `PF-3008` (Free's 1-slot boundary is already proven in `quota_middleware.rs`). The three plans differ in exactly the FR-P-01 values, read through the app's own catalog: result TTL 6 / 24 / 72 h, concurrency 1 / 3 / 10, daily images 10 / 500 / 5000, video-seconds 0 / 3600 / 18000, max bytes 25 MiB / 1 GiB / 4 GiB — plus the usage hash's 48 h Redis TTL after a meter record. Result TTL has no enforcement surface yet (no real jobs/results exist); the catalog value is the proof until retention lands with the Phase 2 intake. Reconciliation catches drift without changing enforcement: the meter records 3 images, Redis is corrupted to 99 behind its back, the gate still enforces the DB truth (create reaches the stub, 7 remaining), and `reconcile_day` repairs Redis (corrected, `redis_before` 99) with a second reconcile a no-op. The meter-level halves (UTC boundary, reconcile repair, concurrent records) are already proven in `crates/core/tests/usage_live.rs`; this suite proves the HTTP layer never sees the drift. The suite forced a small fix: `quota_snapshot_for_plan` used to read `Utc::now()` directly, so a frozen clock could not move its day — it now takes the UTC day as a parameter, and the state-level wrappers pass the app clock's day (ADR-0027).

## BillingProvider trait (P1-059)

The trait is `crates/billing/src/provider.rs` (`detag-billing`, FR-P-05): `create_checkout` / `create_portal` / `fetch_subscription` / `list_invoices` / `create_customer` / `fetch_customer` (async, provider calls) plus `verify_webhook_signature` and `map_webhook` (sync, pure). All types are domain types — `SubscriptionState`, `CheckoutSession` / `PortalSession`, `Customer`, `Invoice`, `BillingEvent` → `DomainEvent` (`CheckoutCompleted` / `SubscriptionStatusChanged` / `SubscriptionDeleted` / `InvoicePaid`) — no Stripe or Razorpay type crosses the trait. `SubscriptionStatus` round-trips exactly the four `subscriptions.status` CHECK values (`trialing` / `active` / `past_due` / `canceled`); anything else is `BillingError::UnknownStatus`, never coerced. The typed error (`crates/billing/src/error.rs`) carries no payloads or secrets in its messages. The contract is proven by a `FakeProvider` double (8 unit tests in the crate, all green): the same caller code written once against `&dyn BillingProvider` works unchanged for a Stripe double and a Razorpay double (the FR-P-05 swap proof); a known webhook type maps to a `DomainEvent`, an unknown-but-well-formed type maps to `None` (ignored, not an error), a malformed payload is `InvalidPayload`, an unknown status fails closed, and a bad signature is `SignatureInvalid`. The trait is `dyn`-compatible via `#[async_trait]` so `AppState` can hold `Arc<dyn BillingProvider>` (ADR-0028 — the `PlanStore` native-`async`-fn + generics pattern was rejected because it would infect `AppState` and every handler with a type parameter). No adapter exists yet: the Stripe adapter lands in P1-060, Razorpay behind its flag after it.

## Stripe adapter (P1-060)

The adapter is `crates/billing/src/stripe.rs` (`detag-billing`, FR-P-05): `StripeAdapter` over `async-stripe` 0.41 (`runtime-tokio-hyper-rustls-webpki` — tokio + hyper + rustls, no OpenSSL). `StripeAdapter::new(secret_key, webhook_secret, allow_live)` enforces test mode at construction: only `sk_test_...` keys, plus `sk_live_...` when the owner flag is set (owner decision #3, still pending — defaults false); anything else is `BillingError::Configuration` (new variant — the constructor's failure mode needed a typed home; its messages name the problem, never the key value). The adapter's `Debug` is hand-written and prints nothing (the client holds the secret key). The trait's async legs map to the real client: checkout in subscription mode with the catalog price (`client_reference_id` = our user id, so the webhook can link the session), portal, subscription fetch, invoice list, customer create/fetch — customer create stores our user id as `detag_user_id` metadata. A Stripe 404 maps to the typed `UnknownSubscription` / `UnknownCustomer`; `resource_missing` on checkout maps to `InvalidPrice`. Signature verification is `stripe::Webhook::construct_event` (real HMAC check + 5-minute timestamp tolerance). `map_webhook` takes the event's `data.object` (ASSUMPTION-023) and maps the real Stripe types (`checkout.session.completed`, `customer.subscription.created/updated/deleted`, `invoice.paid`); a checkout without a subscription id is ignored, not an error; a subscription status outside the four DB values fails closed (`UnknownStatus`).

Proven by 8 unit tests in the crate (16 total with P1-059's): the live-key guard (rejected without the flag, accepted with it, malformed keys rejected, errors carry no key value), a real HMAC-SHA256 header built in the test (accepts; tampered payload, wrong secret, garbage, empty, non-UTF8 all `SignatureInvalid`), and the full webhook mapping (known/ignored/malformed, fail-closed on `incomplete`). The HTTP legs have NO live test here: no Stripe test-mode key exists on this VM and no network path to `api.stripe.com` was exercised — their first live run is the P1-062 checkout endpoint's integration test, with a test-mode key.

## Checkout endpoint (P1-062)

`POST /v1/billing/checkout` (`apps/api/tests/checkout.rs`, 7 live tests, FR-P-03): the endpoint is proven against a `FakeProvider` double injected into `AppState` (`with_billing_provider`, the `with_clock` pattern) — the provider's HTTP legs are still untested live (no test-mode key on this VM; ASSUMPTION-023's "first live run is P1-062" is corrected here: the first live run is the endpoint with a real test-mode key, whenever one exists — the fake proves the endpoint, the idempotency, and the customer reuse, not the Stripe call). The guard: anonymous 401 `PF-1001`, an API key 403 `PF-1002` (the route is not in the extractor's scope table — no billing for keys), a JWT user 200 with the session. No provider in the state is 503 `PF-5005`. Validation: missing/empty `plan_code` is 422, an unknown code is 404 `PF-2010`, Free is 422 (`not_purchasable`), and a plan with no Stripe price is 422 too — none of them calls the provider. Success: the first checkout sends no customer; a user with a (canceled) subscription row reuses its `provider_customer_id`; the success URL carries the `{CHECKOUT_SESSION_ID}` placeholder and both URLs come from `PF_PUBLIC_BASE_URL`. Idempotency (the AC): the same key + the same request replays the stored session with no second provider call (call count asserted on the fake); the same key + a different plan is 409 `PF-2011`; a fresh key is a fresh checkout; the stored rows are asserted in `idempotency_keys`. Expiry is proven on a frozen clock: live at 23 h, dead at 25 h (the token is re-minted after each advance — a 15-minute access token does not survive a 23-hour jump). A configured price Stripe does not know is 500 `PF-5000` (config drift, not a client error). Two harness lessons: the plans table is shared by every binary (one scratch DB), so the price-touching tests serialize on a static mutex and each sets the exact price it needs immediately before the probe that reads it — the catalog caches on first read (5 min TTL), so a price change after that read is invisible to the test's own app — and the idempotency keys are unique per run (a stale row for a fixed key would 409 the next run's first probe, because the fingerprint carries the user id).

## Webhook receiver (P1-063)

`POST /v1/webhooks/stripe` (`apps/api/tests/webhook.rs`, 8 live tests, FR-P-04): the endpoint is proven against the REAL `StripeAdapter` injected into `AppState` (a fake `sk_test_` key + test webhook secret — construction and signature verification need no network), so the suite proves the real HMAC-SHA256 path end to end through the real HTTP layer, not a fake whose verify always says yes. The provider's HTTP legs are still untested live (no test-mode key on this VM — ASSUMPTION-023). No provider in the state is 503 `PF-5005`. A missing signature header, a header signed for another body (tamper), and a header built with a different secret are each 400 `PF-2013` with no ledger row stored (the ACs). A verified ignored event (`invoiceitem.created`, the P1-060 shape — `construct_event` parses the payload into a `stripe::Event`, so the object must be a full invoice item) is 200 with a receipt, one `webhook_events` row, and a `dispatched_at` stamp; a verified `checkout.session.completed` (full session object: `automatic_tax`, `custom_fields`, `custom_text`, `mode: subscription`, `payment_status: paid`, customer + subscription ids) is 200, stored, and dispatched. The replay AC: the same event id posted twice is 200 both times, exactly one row (count asserted), and the `dispatched_at` stamp does not move. No credential is needed: the same delivery with a junk Bearer token is still 200 (the matrix row is `Ignored` — the key changes nothing). A verified-but-unparseable envelope is 200 with no row (the "4xx only on bad signature" rule) — proven against an always-accepting test double, because the real adapter rejects unparseable envelopes as a bad signature while verifying. Two harness lessons: the signature timestamp is enforced against the REAL clock (`construct_event`'s 5-minute tolerance), so the header is built at "now" even though the app clock is frozen — and Postgres stores microseconds, so the frozen nanosecond clock truncates on write and stamp assertions compare at microsecond precision.

## Subscription state machine (P1-064)

The pure machine (`crates/billing/src/state_machine.rs`, 5 unit tests incl. the full 4×4 matrix) plus the service P1-065 will call (`apps/api/src/services/subscription_state.rs`, `apps/api/tests/subscription_state.rs`, 7 live tests, FR-P-04): `trialing → active | past_due | canceled`, `active → past_due | canceled`, `past_due → active | canceled`; `canceled` is terminal and no move returns to `trialing` — every other move is `BillingError::InvalidTransition`, surfaced as 409 `PF-5002` (the AC). A same-state ask is a no-op (`changed: false`, no write, no audit, no mail) so a replayed event has a single effect. The service applies a move as a guarded `UPDATE ... WHERE status = $from` in one transaction with the `billing.subscription.status_changed` audit row and the lifecycle mail (`trialing → active` welcomes, any move to `past_due` duns, any move to `canceled` says goodbye; recovery mails nothing), then invalidates the plan catalog after commit (there is no per-user entitlement cache — `effective_plan` reads the row live). `apply_domain_event` maps a `DomainEvent` to its target (`SubscriptionStatusChanged` → its status, `SubscriptionDeleted` → `canceled`; checkout creation and invoice payment carry no state change and are no-ops). Proven live: conversion grants Pro with one welcome mail + one audit row, past-due duns and falls back to Free, recovery is silent and grants Pro again, `canceled → active` is 409 with the row untouched and nothing written, and an unknown row is 404 `PF-2010`. Two harness lessons: the DB is shared by every test in the binary, so mail assertions filter by the test user's id in the payload (the P1-046 precedent), and the seeder uses runtime `sqlx::query`, not `query!`, so it needs no offline metadata. No new route, no migration, no Stripe key: the machine proves itself against seeded rows. The Stripe HTTP legs stay untested live (no test-mode key on this VM — ASSUMPTION-023).

## Webhook event handlers (P1-065)

The mapping table plus the handler (`apps/api/tests/webhook_handlers.rs`, 17 live tests — 13 P1-065 + 4 P1-066 reordered-delivery — FR-P-04): the endpoint is proven against a DELEGATING provider ([`DelegatingProvider`](apps/api/tests/common/mod.rs)) — the real `StripeAdapter` for signature verification + mapping (so the HMAC path and the mapping table stay real), canned `fetch_subscription` answers (the adapter's HTTP legs are still untested live — no test-mode key on this VM, ASSUMPTION-023). The AC ("mapping table tested") is proven twice: the adapter's unit tests cover all six mapped types (`checkout.session.completed`, `customer.subscription.created`/`updated`, `customer.subscription.deleted`, `invoice.paid`, `invoice.payment_failed`) + ignored + malformed, and this suite proves each mapping through the real HTTP layer to the row. A checkout with our user (`client_reference_id`) creates the row at the provider-reported status with the plan resolved from the fetched price, plus one `billing.subscription.created` audit row, and the user resolves to Pro; a checkout with no user, an unknown subscription at the provider, or a price no plan carries is 200 with no row. `subscription.updated` moves `trialing → active` (welcome mail, Pro granted, audit with the event id as reason); `subscription.deleted` cancels (goodbye mail, falls back to Free). `invoice.paid` moves nothing (past-due stays past-due — recovery arrives as a subscription update); `invoice.payment_failed` duns (`active → past_due`, dunning mail, falls back to Free); a one-off failed invoice is 200 and touches nothing; an update for a row that does not exist is 200 and invents no row; a failed payment for a `canceled` row is 200 with the row untouched (the machine rejects `canceled → past_due`). The replay rules: the same checkout delivered twice creates exactly one row (and one audit row) with the stamp unmoved; a first delivery whose fetch fails transiently is 500 `PF-5000` with `dispatched_at` NULL, and its redelivery applies the event (the P1-065 re-dispatch rule). Two harness lessons: the plans table is shared by every binary, so the price is set immediately before the probe that reads it (the checkout tests' pattern), and the delegating double's canned fetch is added after spawn (its `with_subscription` takes `&self`). The P1-063 checkout tests in `webhook.rs` now run against the delegating provider too (the real adapter would call Stripe live from the handler); the signature / ignore / replay-stamp tests keep the real adapter.

## Idempotency + replay safety (P1-066)

The reordered-delivery proofs (`apps/api/tests/webhook_handlers.rs`, 4 new live tests, 17 total, FR-P-04): no new code — the ticket proves the mechanisms P1-063/064/065 already built, through the real HTTP layer against the delegating provider. An update arriving before its checkout is a logged 200 with no row, and the checkout that follows still creates the row at the provider-fresh state (one `billing.subscription.created` audit row) — the early event is not lost work, it was simply early. A delayed update carrying an old state is rejected by the machine: `past_due` stays `past_due` when a stale `trialing` update arrives (no return to `trialing`, no audit row), and `canceled` stays `canceled` when a delayed `active` update arrives (`canceled` is terminal). A replayed status change has a single effect: same event id twice moves the row once, writes one audit row, sends one welcome mail, and the replay moves no stamp. Honesty: the guard is machine-validity, not event sequencing — a stale event carrying a *valid forward* move still applies (ASSUMPTION-029); closing that needs a per-row last-event timestamp, a follow-up.

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
