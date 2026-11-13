# Data Model (Postgres 16, sqlx migrations)

Naming: snake_case, plural tables, UUIDv7 PKs, `created_at/updated_at` everywhere, soft-delete only where stated. Migrations numbered, additive-first policy (see runbook).

## ERD (textual)

```
users 1─* sessions        users 1─* api_keys       users 1─1 subscriptions
users 1─* uploads 1─1 jobs 1─* job_events          plans 1─* subscriptions
jobs 1─* artifacts        jobs 1─1 quality_reports  users 1─* usage_counters(daily)
users 1─* attestations    users 1─* webhook_endpoints 1─* webhook_deliveries
users 1─* email_verification_tokens (single-use, 24h)
users 1─* login_attempt_counters (lockout, P1-004)
audit_logs (actor: users, action, target)           abuse_reports ─ jobs
feature_flags   idempotency_keys   email_outbox     plans (catalog)
```

## Tables

**users** — id, email (citext, unique), password_hash (argon2id), email_verified_at, role (user|support|admin), status (active|suspended|deletion_pending), tos_version, deleted_at (grace), display_name (nullable, P1-005), pending_email (citext, nullable, no unique constraint — the swap in P1-006 enforces uniqueness via `email`; P1-005), settings jsonb.
**email_verification_tokens** (P1-002; purpose P1-006) — id, user_id, token_hash (sha256, unique — the raw token lives only in the outbox payload + the email), purpose (`verification` | `email_change` — the two are never interchangeable; backfilled `verification` for pre-P1-006 rows), expires_at (24h), used_at (set on consume; single-use), created_at. Minting deletes the user's previous unused tokens (one live token per user). The resend rate limit (3/hour) counts `email_outbox` rows for the user + template, not token rows — minting deletes token history, but every mint enqueues exactly one outbox row in the same transaction.
**login_attempt_counters** (P1-004) — scope (`account`|`ip`), key (sha256 of the email / of the client IP), attempts (since the last success or window reset), first_failed_at, last_failed_at, locked_until, notified_at (when the lockout mail was queued), created_at, updated_at. PK (scope, key). The lockout of record: 5 failures in a 15-min window lock (60 s, doubling per further failure, capped at 3600 s — the curve is `detag_core::lockout`); a success deletes the row; registering the email deletes the account row. While locked, attempts are not recorded (a locked account cannot be kept locked by hammering); the lock is set with GREATEST against the existing value, so concurrent failures can only extend it. `notified_at` is set once per lock episode, in the same transaction as the `lockout-notice` outbox enqueue.
**sessions** — id, user_id, refresh_hash (sha256), family_id (rotation lineage), device (ua, ip-hashed), expires_at, revoked_at, replaced_by.
**api_keys** — id, user_id, name, prefix, secret_hash, scopes text[], last_used_at, revoked_at, rate_limit_tier.
**plans** — id, code (free|pro|business), stripe_price_id, currency, amount, entitlements jsonb (daily_images, daily_video_seconds, max_bytes, result_ttl_hours, concurrency, lane, features jsonb).
**subscriptions** — id, user_id, plan_id, provider (stripe|razorpay), provider_customer_id, provider_subscription_id, status (trialing|active|past_due|canceled), current_period_end, cancel_at_period_end.
**usage_counters** — user_id, day (date), images_used, video_seconds_used, unique(user_id, day); Redis mirrors for hot path.
**uploads** — id, user_id, kind, filename, bytes, sha256, phash, mime, width, height, duration_s?, metadata jsonb (exif summary, icc present, container atoms), status (pending_parts|ready|aborted), storage_key, parts int.
**jobs** — id, user_id, upload_id, kind (image|video|batch), mode, params jsonb, status, stage, progress real, priority/lane, attempts int, idempotency_key unique, error jsonb, attestation_id, webhook_url?, started_at, finished_at.
**job_events** — id, job_id, at, type (stage|progress|note|state_change), payload jsonb (append-only: UPDATE-only trigger since P1-007 — DELETE is the purge cascade's erasure path).
**artifacts** — id, job_id, type (output|mask|preview_before|preview_after|report|batch_zip), storage_key, bytes, sha256, expires_at.
**quality_reports** — id, job_id, engine_version, models jsonb, metrics jsonb (ssim/psnr/vmaf…), encode jsonb, regions jsonb, gate (pass|fail), retried bool, seed text.
**attestations** — id, user_id, upload_id, text_version, ip_hash, client (web|api_key), created_at (immutable: UPDATE-only trigger since P1-007 — DELETE is the purge cascade's erasure path).
**audit_logs** — id, actor_user_id?, actor_client, action, target_type, target_id, before jsonb, after jsonb, reason, created_at. Append-only: no UPDATE/DELETE grants (the trigger keeps its DELETE branch — the audit trail is never deleted). `actor_user_id` is a plain UUID with NO foreign key (migration 20261113062352, P1-007): the actor is a historical fact, not a live reference — the FK's `ON DELETE SET NULL` was an UPDATE the trigger forbids, so the purge could never delete a user with audit rows; without it the purge leaves the actor id intact, a stronger trail than NULL.
**webhook_endpoints** — id, user_id, url, secret_hash, events text[], disabled_at.
**webhook_deliveries** — id, endpoint_id, event_id, event_type, payload jsonb, attempts int, last_status, last_response_code, next_retry_at.
**abuse_reports** — id, reporter_contact?, job_id?, reason, status (open|reviewing|resolved), resolution, notes.
**feature_flags** — key PK, enabled bool, rollout jsonb, updated_by, updated_at (cached 30 s).
**idempotency_keys** — key PK, user_id, endpoint, response jsonb, expires_at.
**email_outbox** — id, to_hash?, template, payload jsonb, scheduled_at, sent_at, attempts, last_error (worker drains; supports retry + test assertions).
**worker_heartbeats** — worker_id PK (`{hostname}:{pid}`), pid, queues text[], in_flight int (>= 0), started_at, last_heartbeat_at (upserted every heartbeat tick, P0-080; staleness = dead worker; pruned after 1 h).

## Retention & cleanup jobs

- artifacts: `expires_at` = now + plan TTL → hard delete object + row (daily worker).
- uploads: pending_parts > 24 h → abort + delete.
- users (P1-007): `status = 'deletion_pending'` and `deleted_at` older than 30 d → hard delete via `detag-gc --purge-users` (nightly, dry-run by default). The cascade: sessions, jobs (+ artifacts, job events, quality reports via jobs), uploads, subscriptions, usage counters, idempotency keys, verification tokens and attestations are `ON DELETE CASCADE`; audit rows survive with `actor_user_id` intact (no FK); outbox rows survive (addressed by hash). S3 objects are NOT deleted by the purge — the row delete makes them orphans and the next gc sweep deletes them.
- webhook_deliveries: 30 d. email_outbox sent rows: 30 d. audit_logs: 365 d. app logs: 90 d (log shipper/rotate).
- usage_counters: keep 400 d.

## Indexes (minimum)

jobs(user_id, created_at desc), jobs(status, lane, priority), uploads(user_id, status), sessions(refresh_hash) unique, api_keys(prefix), usage_counters PK(user_id, day), audit_logs(actor_user_id, created_at desc), audit_logs(target_type, target_id), webhook_deliveries(endpoint_id, created_at desc), idempotency_keys(expires_at) for GC, worker_heartbeats(last_heartbeat_at) for staleness scans.

## Migration policy

1. Migrations in `migrations/`, `sqlx migrate`; CI checks a clean migrate + `sqlx prepare` freshness.
2. Night runs: additive only (add table/column/index with defaults). Contract steps (drop/rename) wait for owner-ok, tagged in ISSUES.md.
3. Every schema change updates this file in the same commit.
