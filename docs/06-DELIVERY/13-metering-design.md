# Metering design (P1-086, FR-P-07)

The consolidated statement of how Detag counts usage: counter
semantics, the edge cases (clock skew, retry double-count guard),
and the reconciliation policy. It consolidates what P1-050,
P1-079, P1-080, P1-081, P1-082, P1-084 and P1-085 built
separately — the ADRs and assumptions those tickets recorded
stay the decision record; this doc is the one place that says
how the pieces fit and what they do not do. (ADR-0050,
ASSUMPTION-047.)

FR-P-07: "Usage metering SHALL count images and video-seconds
per day per user, reconciled nightly."

## 1. The counters

Two counters per (user, UTC day) — images (a count) and
video_seconds (a sum of actual seconds) — in the
`usage_counters` row for that (user, day) (PK on both columns,
`i32` legs, `CHECK >= 0`).

- **The day is UTC, from the injected clock.** `UsageMeter`
  takes the day from its injected `Clock` (`SystemClock` in
  production — the host system time in UTC; `ManualClock` in
  tests), never `Utc::now()` in the service and never a local
  timezone or `created_at::date`. A record at exactly 00:00:00
  UTC belongs to the new day (proven by the suite). The reset
  is a new key/row, not a decrement.
- **Two stores, one truth.** The Redis day-hash
  `pf:usage:{user}:{YYYY-MM-DD}` (fields `images` /
  `video_seconds`, 48 h TTL — `TTL_USAGE_COUNTER`) is the hot
  mirror; the Postgres `usage_counters` row is the durable
  truth. When the two disagree, the DB wins — always, in every
  reader and every repair.
- **Record** (`UsageMeter::record`, P1-050): Redis first — one
  atomic Lua script (`HINCRBY` both fields + `EXPIRE`), so
  concurrent job-creates can never lose a count — then the
  durable row upserted (`usage_counters::increment`,
  `ON CONFLICT` add; the row lock serializes concurrent
  upserts). Negative inputs are clamped to 0: `record` never
  decrements a counter (a correction goes through
  reconciliation, which overwrites).
- **Estimate at create** (`estimate_video_seconds`, P1-050):
  the upload's known duration, rounded up to whole seconds. An
  unknown, zero, negative, or non-finite duration reserves
  nothing (ASSUMPTION-016) — there is no bitrate data in the
  kit to invent an estimate from, and a made-up number would be
  fake precision. The completion true-up charges the real
  seconds.
- **True-up at completion** (`UsageMeter::true_up`,
  `true_up_delta`, P1-079): the delta is `actual - estimated`,
  saturating, negative inputs clamped to 0 — a difference, not
  a new total. It lands on the job's CREATE day, passed
  explicitly — never the clock's today — so a job that finishes
  after UTC midnight still corrects the day its estimate was
  reserved on (ASSUMPTION-040). Order is DB-first, the reverse
  of `record`: the DB decides how much of the delta actually
  applies (`usage_counters::adjust` floors the day total at 0
  in SQL on both legs — the `EXCLUDED` trap, where the clamped
  insert row would have zeroed a negative delta on the conflict
  leg, is fixed by using the raw `$3`/`$4` parameters in the
  `ON CONFLICT SET` clause), and Redis receives exactly the
  applied delta, additionally clamped against the hash's own
  value so it never goes negative. The applied delta is capped
  at the headroom to `i32::MAX`, so the DB add can never
  overflow.

## 2. Who reads what

The Redis hash is never a decision source. Every reader that
decides anything reads the durable rows:

| Reader | Source | Why |
| --- | --- | --- |
| Quota pre-check (P1-051) | Durable snapshot (`quota_snapshot_for_plan`: plan entitlements + today's `usage_counters` row) | The gate must agree with `/v1/me` by construction |
| `GET /v1/me` quota | Same durable snapshot | Same two sources as the gate |
| `maybe_warn_quota` (P1-085) | Same durable snapshot | The warning agrees with the gate by construction |
| `GET /v1/usage` (P1-080) | `usage_counters` rows (`for_user_range`, one query for the 30-day window, oldest first; missing days zero-fill in the service) | The DB is the truth; the hash may drift until the nightly run |
| Admin usage view (P1-081) | Same window read + all-time totals (`totals_for_user`, one `COALESCE(SUM(...), 0)` query over every row) | A dispute answer comes from durable rows |
| Per-key breakdown (P1-038/P1-080) | Today's `jobs` rows counted by `key_id` | The counters carry no key; the per-key `video_seconds` reads 0 until Phase 2 records durations (jobs carry no duration) |
| Nightly reconciliation (P1-082) | DB rows UNION the Redis SCAN leg, DB wins | Repair only — it writes the hash, never a decision |
| Nothing | The Redis hash, for a decision | The hash may lag the DB until the next reconcile (§3) |

## 3. Edge cases

### Clock skew

There is one clock source: the host system clock
(`SystemClock.now()` is `Utc::now()`). The nightly loop reads
`Utc::now()` directly for its sleep math — the same host clock
in production, so there is no second source to disagree with;
the pure `next_run_at` math is what the tests pin. The meter
has no skew detection and no NTP discipline in code — time
sync is an ops concern. What a skew does, by direction:

- **Forward jump (clock ahead):** records land on the next
  day early — today's quota effectively resets early
  (under-count today, over-grant). A true-up still lands on
  the caller-passed create day, so it is immune to a jump at
  completion time — but the create-day value itself came from
  the skewed clock at create time.
- **Backward jump:** records land back on the previous day —
  they can push a settled day over its plan limit after the
  gate already ran. That is accounting only: the gate ran
  against the day the clock reported at create, and a
  backward jump cannot un-run it. A backward jump across
  midnight re-opens yesterday's counters for new records.
- **Invisible to reconciliation:** both legs were written
  with the same clock, so a wrong-day row agrees across the
  stores — the nightly job repairs drift, not wrong days. A
  skewed day's rows are visible only to a human comparing the
  DB day against the job's `created_at`.
- **Nightly schedule:** wall-clock 02:00 UTC. A jump can skip
  a night (the next-run math moves forward) or double-fire
  one (the clock set back across 02:00 re-triggers the sleep
  math) — the job is idempotent, so a double run is a no-op
  repair, not a double charge, and a missed night is repaired
  the night after (the window covers today + yesterday). A
  multi-day outage leaves days older than yesterday
  unreconciled — but their hashes have expired by TTL and no
  reader reads the hash, so nothing is lost, only unrepaired
  hash state that no longer matters.

Precedents in the kit: JWT `exp` has leeway 0 (no skew
tolerance for tokens); dunning treats a future entry time as
Skip (a clock-skewed entry mails nothing). The ±5 min skew
tests and handling are P5-043's follow-up — this ticket
documents the metering behaviour; it does not change it.

### Retry double-count guard

- **`record` has no idempotency key: a duplicate call adds
  twice, in both stores.** The guard is at the caller: the
  Phase 2 intake must call `record` once per job — the create
  path is the single call site, and a retried create must
  reuse the job row / idempotency key, not re-record. Today no
  production caller exists, so the hazard is latent, not live.
- **`true_up` is NOT idempotent.** A retried completion
  re-applies the delta — see the correction in §6: the floor
  bounds the damage (the day total never goes below 0) but
  does not make the retry a no-op. The real guard is
  completion-processed-once at the caller: Phase 2 must check
  the job's terminal state (or key the completion on the job's
  completion event) before calling `true_up`, so a duplicate
  delivery cannot re-apply. Until that guard lands, a retried
  completion is a known meter-level hazard, documented here,
  not hidden.
- **The retry guards that do exist, and are proven:**
  - The quota-warning cap is idempotent by construction: the
    partial unique index + `ON CONFLICT DO NOTHING` makes the
    insert itself the cap check, and two concurrent calls
    racing the same threshold land exactly one row (proven by
    `concurrent_calls_enqueue_once`).
  - Reconciliation is idempotent: a second run over agreed
    stores is a no-op (proven by the suite).
  - Refunds floor at 0 in SQL on both legs of `adjust` (the
    `EXCLUDED` trap is fixed — ADR-0043).

### Concurrency: record vs reconcile

Convergence, not atomicity (ADR-0048): a record's Redis
increment can land between a reconcile's DB read and its CAS,
and the CAS then sets the hash back to the DB value it read —
transiently wiping that increment from Redis. The DB upsert
still lands, so the durable row is never wiped, and the next
reconcile repairs the hash. After the records settle, one
reconcile makes the stores agree — proven by the suite, whose
first version asserted Redis == 20 immediately after the join
and failed (Redis 12, DB 20): that failure is the proof, not a
flake.

`reconcile_day` retries the CAS 3 times (`RECONCILE_ATTEMPTS`)
and then returns `Conflict` — transient contention, not a lost
count; the nightly sweep counts it and continues, and the next
night retries. The lag is never an enforcement error: the
quota gate and `/v1/me` read the DB, never the hash.

### Failure modes, per leg

- **Redis down at `record`:** the error is returned and NO DB
  row is written — dead-Redis fails closed (P1-050 test).
  Nothing is recorded; the job create fails rather than
  creating unmetered usage.
- **DB down at `record` (after the Redis write):** the error
  is returned and a Redis-only hash is left behind; the
  nightly job zeroes it (DB wins — a row the DB never got is
  not usage the quota decisions can see, ASSUMPTION-043). The
  hash is never a decision source, so the zeroing is safe.
- **Redis down at `true_up` (after the DB write):** the error
  is returned and drift is left behind; the nightly repairs
  it. Same failure shape as `record`, in the other direction.
- **DB down at `true_up`:** the error comes from the pre-read
  before any write; nothing moves.
- **A poisoned day in the nightly sweep** (Redis WRONGTYPE,
  CAS conflict after 3 retries): logged + counted, the sweep
  continues (the dunning precedent). Only a failure to LIST
  the candidates (the DB query or the SCAN) fails the whole
  job — without the list there is no run to report on — and the
  error propagates so apalis retries.
- **Kill-switches:** `PF_BILLING_RECONCILE_ENABLED=false`
  reports `ran: false` with every count 0; 
...[truncated 6551 chars]