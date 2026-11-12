# Slow-Query Log + Index Checklist (P0-044)

The slow-query bar is NFR-12: the Postgres log must stay clean
(< 50 ms per statement) at 100k jobs/day. This ticket only enables
the log and records the checklist — the load validation itself is a
later revisit at NFR-12.

## Slow-query log — how it is enabled (dev)

Threshold: `log_min_duration_statement = 50ms` (the NFR-12 bar).
Every statement slower than 50 ms is logged with its duration;
fast statements are not logged.

| Where | Mechanism |
| --- | --- |
| Compose (canonical, `ops/compose.db.yml`) | `command: ["postgres", "-c", "log_min_duration_statement=50ms"]` on the postgres service |
| Native dev DB on this VM (ASSUMPTION-007 / DOCKER-001) | Cluster-wide in `/etc/postgresql/16/main/postgresql.conf`: `log_min_duration_statement = '50ms'`, applied with `SELECT pg_reload_conf()` as the `postgres` superuser. Cluster-wide (not `ALTER DATABASE`) on purpose: the `pf` role is not a superuser, so `ALTER DATABASE ... SET` is refused for this parameter, and a cluster setting survives `just db::reset` (a per-database setting would be dropped with the database). |

Verify it (dev DB):

```sql
SHOW log_min_duration_statement;  -- 50ms
SELECT pg_sleep(0.06);            -- appears in the log:
```

```
/var/log/postgresql/postgresql-16-main.log:
... pf@pf LOG:  duration: 60.710 ms  statement: SELECT pg_sleep(0.06);
```

A fast statement (`SELECT 1`) adds no `duration:` line. The
log prefix is already `%m [%p] %q%u@%d` (timestamp, pid,
user@db) from the Debian cluster defaults — no extra
`log_line_prefix` needed.

Verified live 2026-11-12: `SHOW` returns `50ms`; `pg_sleep(0.06)`
logged at 60.710 ms; a fast query added no line.

## Index checklist — from `docs/03-ARCHITECTURE/04-data-model.md`

"Indexes (minimum)". Each row: the representative query, the
index it must use, and what was actually observed when the
checklist was verified (2026-11-12, scratch DB
`detag_p044_explain`, migrated + `ANALYZE`, 200 users / 20k jobs /
5k uploads, sessions, audit rows, deliveries / 6k counters).

A plain `Index Scan` is the ideal; a `Bitmap Index Scan` is the
same index used in bitmap form (multi-row match) and is a pass;
a `Seq Scan` on a tiny table is the planner's cost choice, not a
missing index — prove the index works with
`SET enable_seqscan = off` (see row 5).

| # | Query (representative) | Must use | Observed 2026-11-12 |
| --- | --- | --- | --- |
| 1 | `SELECT id FROM jobs WHERE user_id = $1 ORDER BY created_at DESC LIMIT 20` | `jobs_user_created_idx` | Index Scan, 0.185 ms |
| 2 | `SELECT id FROM jobs WHERE status = 'queued' AND lane = 'standard' ORDER BY priority DESC LIMIT 20` | `jobs_status_lane_priority_idx` | Index Scan Backward, 0.078 ms |
| 3 | `SELECT id FROM uploads WHERE user_id = $1 AND status = 'ready'` | `uploads_user_status_idx` | Bitmap Index Scan, 0.134 ms |
| 4 | `SELECT id FROM sessions WHERE refresh_hash = $1` | `sessions_refresh_hash_idx` (unique) | Index Scan, 0.088 ms |
| 5 | `SELECT id FROM api_keys WHERE prefix = $1` | `api_keys_prefix_idx` | Seq Scan at 200 rows (planner cost); with `enable_seqscan = off` → Index Scan, 0.019 ms — index proven usable |
| 6 | `SELECT images_used FROM usage_counters WHERE user_id = $1 AND day = CURRENT_DATE` | `usage_counters_pkey` | Index Scan, 0.064 ms |
| 7 | `SELECT id FROM audit_logs WHERE actor_user_id = $1 ORDER BY created_at DESC LIMIT 20` | `audit_logs_actor_created_idx` | Bitmap Index Scan + quicksort (26 kB), 0.19 ms — index-backed; the sort is the planner's choice for 25 rows |
| 8 | `SELECT id FROM audit_logs WHERE target_type = 'job' AND target_id = $1` | `audit_logs_target_idx` | Index Scan, 0.082 ms |
| 9 | `SELECT id FROM webhook_deliveries WHERE endpoint_id = $1 ORDER BY created_at DESC LIMIT 20` | `webhook_deliveries_endpoint_created_idx` | Bitmap Index Scan + quicksort (26 kB), 0.19 ms — same as row 7 |
| 10 | `SELECT key FROM idempotency_keys WHERE expires_at < now()` (GC query) | `idempotency_keys_expires_at_idx` | Bitmap Index Scan, 0.952 ms (2,500 expired rows) |

All 10 indexes exist in the migrated schema (verified:
`pg_indexes`, 39 indexes incl. the doc's 10 plus the
migration-added `jobs_upload_id_idx`, `job_events_job_at_idx`,
`artifacts_job_id_idx`, `sessions_user_id_idx`,
`subscriptions_user_id_idx`, `email_outbox_due_idx`).

## Reproducing the EXPLAIN run

Against any migrated dev/scratch DB, generate rows first (empty
tables always seq-scan — the planner is right to). The
verification run used `generate_series` inserts (200 users,
25 sessions/uploads per user, 100 jobs per user, 25 audit rows
and deliveries per endpoint, 5,000 idempotency keys with half
expired, 30 days of usage counters), then `ANALYZE`, then
`EXPLAIN (ANALYZE)` per row above with real ids from the data.

## Revisit at NFR-12

Re-run this checklist at realistic load (100k jobs/day) and
confirm the log stays clean (< 50 ms) — the per-query times
above are small-table numbers, not a load result. If a query
goes over 50 ms at load, it shows up in the slow-query log
first; add the missing index (additive migration, per the
migration policy) and re-check.
