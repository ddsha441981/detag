//! Nightly user purge (P1-007): hard-delete accounts whose
//! 30-day deletion grace has passed. Plus the nightly
//! session purge (P1-015): hard-delete session rows past
//! their `expires_at`.
//!
//! The user purge is one function over the database —
//! [`purge_expired_users`] — so the gc binary (`main.rs`,
//! `--purge-users`) and the tests below share it. Dry-run is
//! the default (list only); `--execute` deletes. The run
//! continues past a per-user failure (recorded in the report,
//! like `run_gc`'s failures) — a wedged row must not block
//! every other purge.
//!
//! One transaction per user: the `account.purged` audit row +
//! the hard delete commit or roll back together. The audit is
//! written BEFORE the delete, with no actor (`actor_client:
//! "purge"` — the system did it, not a user): the audit row
//! survives the delete (no FK on `target_id`), so the purge
//! leaves a durable record per account. The cascades do the
//! rest: sessions, jobs (+ artifacts, job events, quality
//! reports via jobs), uploads, subscriptions, usage counters,
//! idempotency keys, and verification tokens are `ON DELETE
//! CASCADE`; audit rows survive with `actor_user_id` intact
//! (no FK on the actor — migration 20261113062352 dropped it:
//! the actor is a historical fact, and the FK's ON DELETE SET
//! NULL was an UPDATE the append-only trigger forbids, so the
//! purge could never delete a user with audit rows); outbox
//! rows survive (they are addressed by hash, not by user — a
//! queued mail to a purged account is undeliverable anyway, and
//! its link 401s: the token row is gone with the user).
//!
//! S3 objects are NOT deleted by the purge: the row delete
//! makes them orphans, and the next gc sweep deletes them
//! (that is what the orphan sweep is for — deleting objects
//! here would need the keys before the row delete and a
//! half-deleted state if the transaction then failed; rows
//! first, objects after, via the sweep, is the safe order).

use chrono::{DateTime, Duration, Utc};
use detag_core::{
    db::{DbPool, PoolConfig},
    repos::{
        audit_logs::{self, NewAuditLog},
        sessions,
        users::{self, DELETION_GRACE_DAYS},
        RepoError,
    },
    tx::{self, TxError},
};
use uuid::Uuid;

/// What a session-purge run did (P1-015).
#[derive(Debug, Default)]
pub struct SessionsPurgeReport {
    /// Sessions past `expires_at` (dry-run counts them;
    /// execute deletes them).
    pub candidates: i64,
    /// Sessions actually deleted (0 on dry-run).
    pub purged: u64,
}

/// Purge sessions past their expiry (P1-015). `now` is the
/// run's timestamp (one value for the whole run, so the
/// cutoff is stable). Dry-run counts the candidates and
/// deletes nothing; execute deletes them in one statement
/// and returns the count. No audit row per session — an
/// expired refresh token is routine churn (every login's
/// token dies after 30 days), and the audit trail for a
/// session lives in `audit_logs` (`session.refreshed` /
/// `session.refresh_reused`), not the session row. The gc
/// run's stdout line (candidates/purged) is the record of
/// the run.
pub async fn purge_expired_sessions(
    pool: &DbPool,
    now: DateTime<Utc>,
    dry_run: bool,
) -> Result<SessionsPurgeReport, RepoError> {
    let candidates = sessions::count_expired(pool, now).await?;
    if dry_run {
        return Ok(SessionsPurgeReport {
            candidates,
            purged: 0,
        });
    }
    let purged = sessions::purge_expired(pool, now).await?;
    Ok(SessionsPurgeReport { candidates, purged })
}

/// What a purge run did.
#[derive(Debug, Default)]
pub struct PurgeReport {
    /// Accounts past grace (dry-run lists them; execute
    /// deletes them).
    pub candidates: Vec<Uuid>,
    /// Accounts actually deleted (empty on dry-run).
    pub purged: Vec<Uuid>,
    /// Per-user failures, as (user id, error) — the run
    /// continues past them.
    pub failures: Vec<(Uuid, String)>,
}

/// Purge accounts past the deletion grace. `now` is the run's
/// timestamp (one value for the whole run, so the cutoff is
/// stable across users); the cutoff is `now` − 30 days
/// (`DELETION_GRACE_DAYS`). Dry-run lists the candidates and
/// deletes nothing.
pub async fn purge_expired_users(
    pool: &DbPool,
    now: DateTime<Utc>,
    dry_run: bool,
) -> Result<PurgeReport, RepoError> {
    let cutoff = now - Duration::days(DELETION_GRACE_DAYS);
    let candidates = users::purgeable(pool, cutoff).await?;
    let mut report = PurgeReport {
        candidates: candidates.clone(),
        purged: Vec::new(),
        failures: Vec::new(),
    };
    if dry_run {
        return Ok(report);
    }
    for id in candidates {
        match purge_one(pool, id, now).await {
            Ok(true) => report.purged.push(id),
            // Already gone (a concurrent purge took it first).
            Ok(false) => {}
            Err(err) => report.failures.push((id, err)),
        }
    }
    Ok(report)
}

/// The transaction closure's error: small on purpose.
#[derive(Debug)]
enum PurgeTxError {
    Db(RepoError),
}

/// Delete one user: the `account.purged` audit row + the hard
/// delete, one transaction. Returns `false` when the user was
/// already gone.
async fn purge_one(pool: &DbPool, user_id: Uuid, now: DateTime<Utc>) -> Result<bool, String> {
    let timeout = PoolConfig::default().transaction_timeout;
    tx::with_tx(pool, timeout, |conn| {
        Box::pin(async move {
            audit_logs::insert(
                &mut *conn,
                &NewAuditLog {
                    actor_user_id: None,
                    actor_client: Some("purge".to_string()),
                    action: "account.purged".to_string(),
                    target_type: "user".to_string(),
                    target_id: Some(user_id.to_string()),
                    before: Some(serde_json::json!({ "status": "deletion_pending" })),
                    after: Some(serde_json::json!({ "purged_at": now })),
                    reason: Some("deletion grace expired (30 days)".to_string()),
                },
            )
            .await
            .map_err(PurgeTxError::Db)?;
            let deleted = users::delete_user(&mut *conn, user_id)
                .await
                .map_err(PurgeTxError::Db)?;
            Ok(deleted)
        })
    })
    .await
    .map_err(|err| match err {
        TxError::Inner(PurgeTxError::Db(db_err)) => db_err.to_string(),
        other => other.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use detag_core::repos::{
        audit_logs, email_verification_tokens,
        jobs::{self, NewJob},
        sessions::{self, NewSession},
        users as users_repo,
    };
    use detag_core::tokens::{generate_token, hash_token};
    use detag_testkit::TestCtx;

    /// Seed a user + dependents (session, job, token, audit
    /// row, subscription, usage counter, idempotency key), then
    /// backdate `deleted_at` by `days` (the repo sets it to
    /// now; the test moves it into the past, like time passing).
    async fn seed_user_with_dependents(ctx: &TestCtx, email: &str, days_ago: i64) -> Uuid {
        let user =
            users_repo::create_with_tos(&ctx.pool, email, "argon2id$placeholder", Some("v1"))
                .await
                .expect("create user");
        users_repo::request_deletion(&ctx.pool, user.id)
            .await
            .expect("request deletion");
        sqlx::query(
            "UPDATE users SET deleted_at = now() - ($1 || ' days')::interval WHERE id = $2",
        )
        .bind(days_ago.to_string())
        .bind(user.id)
        .execute(&ctx.pool)
        .await
        .expect("backdate deleted_at");

        sessions::create(
            &ctx.pool,
            &NewSession {
                user_id: user.id,
                refresh_hash: sha256_hex_for_test(&user.id),
                family_id: Uuid::now_v7(),
                device: serde_json::json!({ "ua": "test" }),
                expires_at: Utc::now() + Duration::days(30),
            },
        )
        .await
        .expect("create session");
        let upload_id: Uuid = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO uploads (user_id, kind, filename, bytes, sha256, mime, storage_key) \
             VALUES ($1, 'image', 'a.png', 1, 'h', 'image/png', 'k') RETURNING id",
        )
        .bind(user.id)
        .fetch_one(&ctx.pool)
        .await
        .expect("create upload");
        let job = jobs::create(
            &ctx.pool,
            &NewJob {
                user_id: user.id,
                upload_id: Some(upload_id),
                kind: "image",
                mode: "auto",
                status: "queued",
                lane: "standard",
                params: serde_json::json!({}),
                idempotency_key: None,
            },
        )
        .await
        .expect("create job");
        // A job event + an attestation: both are append-only
        // tables whose rows the cascade must delete (the
        // migration 20261113062352 made their triggers
        // UPDATE-only for exactly this — the purge is the
        // erasure path).
        sqlx::query("INSERT INTO job_events (job_id, type) VALUES ($1, 'stage')")
            .bind(job.id)
            .execute(&ctx.pool)
            .await
            .expect("create job event");
        sqlx::query(
            "INSERT INTO attestations (user_id, upload_id, text_version, ip_hash, client) \
             VALUES ($1, $2, 'v1', 'hash123', 'web')",
        )
        .bind(user.id)
        .bind(upload_id)
        .execute(&ctx.pool)
        .await
        .expect("create attestation");
        let raw = generate_token();
        email_verification_tokens::mint(
            &ctx.pool,
            user.id,
            &hash_token(&raw),
            email_verification_tokens::PURPOSE_VERIFICATION,
            Utc::now() + Duration::hours(24),
        )
        .await
        .expect("mint token");
        audit_logs::insert(
            &ctx.pool,
            &NewAuditLog {
                actor_user_id: Some(user.id),
                actor_client: None,
                action: "test.action".to_string(),
                target_type: "user".to_string(),
                target_id: Some(user.id.to_string()),
                before: None,
                after: None,
                reason: None,
            },
        )
        .await
        .expect("insert audit");
        let plan = detag_core::repos::plans::by_code(&ctx.pool, "free")
            .await
            .expect("plan lookup")
            .expect("free plan exists");
        sqlx::query(
            "INSERT INTO subscriptions (user_id, plan_id, provider, status) VALUES ($1, $2, 'razorpay', 'active')",
        )
        .bind(user.id)
        .bind(plan.id)
        .execute(&ctx.pool)
        .await
        .expect("insert subscription");
        sqlx::query(
            "INSERT INTO usage_counters (user_id, day, images_used, video_seconds_used)
             VALUES ($1, CURRENT_DATE, 3, 0)",
        )
        .bind(user.id)
        .execute(&ctx.pool)
        .await
        .expect("insert usage");
        sqlx::query(
            "INSERT INTO idempotency_keys (key, user_id, endpoint, expires_at) VALUES ($1, $2, '/v1/jobs', now() + interval '1 day')",
        )
            .bind(format!("key-{}", user.id))
            .bind(user.id)
            .execute(&ctx.pool)
            .await
            .expect("insert idempotency key");
        user.id
    }

    fn sha256_hex_for_test(id: &Uuid) -> String {
        detag_core::checksum::sha256_hex(id.as_bytes())
    }

    /// Count a user's rows in a dependent table. Explicit
    /// per-table queries with literal SQL (the function form,
    /// not the macro — `just sqlx-prepare` does not compile
    /// `#[cfg(test)]` code, so macro queries here would have no
    /// offline data; and sqlx 0.9 rejects dynamically built SQL
    /// strings, so the table name is never interpolated).
    async fn count(pool: &DbPool, table: &str, user_id: Uuid) -> i64 {
        let sql = match table {
            "sessions" => "SELECT COUNT(*) FROM sessions WHERE user_id = $1",
            "jobs" => "SELECT COUNT(*) FROM jobs WHERE user_id = $1",
            "uploads" => "SELECT COUNT(*) FROM uploads WHERE user_id = $1",
            "subscriptions" => "SELECT COUNT(*) FROM subscriptions WHERE user_id = $1",
            "usage_counters" => "SELECT COUNT(*) FROM usage_counters WHERE user_id = $1",
            "idempotency_keys" => "SELECT COUNT(*) FROM idempotency_keys WHERE user_id = $1",
            "email_verification_tokens" => {
                "SELECT COUNT(*) FROM email_verification_tokens WHERE user_id = $1"
            }
            "attestations" => "SELECT COUNT(*) FROM attestations WHERE user_id = $1",
            // job_events carries no user_id — count through the
            // job it belongs to.
            "job_events" => {
                "SELECT COUNT(*) FROM job_events je JOIN jobs j ON j.id = je.job_id WHERE j.user_id = $1"
            }
            other => panic!("count: unknown table {other}"),
        };
        sqlx::query_scalar::<_, i64>(sql)
            .bind(user_id)
            .fetch_one(pool)
            .await
            .expect("count")
    }

    #[tokio::test]
    async fn purge_deletes_expired_user_and_cascades() {
        let ctx = TestCtx::new().await;
        let user_id = seed_user_with_dependents(&ctx, "purge-me@example.com", 31).await;

        let report = purge_expired_users(&ctx.pool, Utc::now(), false)
            .await
            .expect("purge");
        assert_eq!(report.purged, vec![user_id]);
        assert!(report.failures.is_empty());

        // The user row is gone, and every CASCADE dependent
        // with it.
        assert!(users_repo::by_id(&ctx.pool, user_id)
            .await
            .expect("lookup")
            .is_none());
        for table in [
            "sessions",
            "jobs",
            "uploads",
            "job_events",
            "attestations",
            "subscriptions",
            "usage_counters",
            "idempotency_keys",
            "email_verification_tokens",
        ] {
            assert_eq!(count(&ctx.pool, table, user_id).await, 0, "{table}");
        }
        // The audit rows SURVIVE the purge (the audit trail is
        // never deleted): the test.action row keeps its actor
        // (the actor is a historical fact — migration
        // 20261113062352 dropped the FK so the purge no longer
        // NULLs it; the trail is stronger than a NULL), plus
        // the purge's own `account.purged` row (actor None —
        // the system did it).
        let audits = audit_logs::list_for_target(&ctx.pool, "user", &user_id.to_string())
            .await
            .expect("list audits");
        assert_eq!(audits.len(), 2, "{audits:?}");
        let acted = audits
            .iter()
            .find(|a| a.action == "test.action")
            .expect("test.action row");
        assert_eq!(acted.actor_user_id, Some(user_id), "{audits:?}");
        assert!(
            audits
                .iter()
                .any(|a| a.action == "account.purged" && a.actor_user_id.is_none()),
            "{audits:?}"
        );
        ctx.close().await;
    }

    #[tokio::test]
    async fn purge_keeps_grace_and_active_users() {
        let ctx = TestCtx::new().await;
        let expired = seed_user_with_dependents(&ctx, "expired@example.com", 31).await;
        let in_grace = seed_user_with_dependents(&ctx, "in-grace@example.com", 29).await;
        let fresh = seed_user_with_dependents(&ctx, "fresh@example.com", 0).await;
        let active = users_repo::create_with_tos(&ctx.pool, "active@example.com", "h", Some("v1"))
            .await
            .expect("create active user")
            .id;

        let report = purge_expired_users(&ctx.pool, Utc::now(), false)
            .await
            .expect("purge");
        assert_eq!(report.candidates, vec![expired]);
        assert_eq!(report.purged, vec![expired]);

        // Only the 31-day user is gone.
        assert!(users_repo::by_id(&ctx.pool, expired)
            .await
            .expect("lookup")
            .is_none());
        for id in [in_grace, fresh, active] {
            assert!(
                users_repo::by_id(&ctx.pool, id)
                    .await
                    .expect("lookup")
                    .is_some(),
                "{id}"
            );
        }
        ctx.close().await;
    }

    #[tokio::test]
    async fn dry_run_deletes_nothing() {
        let ctx = TestCtx::new().await;
        let user_id = seed_user_with_dependents(&ctx, "dry-run@example.com", 31).await;

        let report = purge_expired_users(&ctx.pool, Utc::now(), true)
            .await
            .expect("purge");
        assert_eq!(report.candidates, vec![user_id]);
        assert!(report.purged.is_empty());
        assert!(users_repo::by_id(&ctx.pool, user_id)
            .await
            .expect("lookup")
            .is_some());
        ctx.close().await;
    }

    /// Seed one session for a user with the given expiry;
    /// optionally mark it revoked and/or replaced (by
    /// `replaced_by`, which must be another session row's id
    /// — the caller creates the replacement first). Returns
    /// the session id.
    async fn seed_session(
        ctx: &TestCtx,
        user_id: Uuid,
        expires_at: DateTime<Utc>,
        revoked: bool,
        replaced_by: Option<Uuid>,
    ) -> Uuid {
        let session = sessions::create(
            &ctx.pool,
            &NewSession {
                user_id,
                refresh_hash: sha256_hex_for_test(&Uuid::now_v7()),
                family_id: Uuid::now_v7(),
                device: serde_json::json!({ "ua": "test" }),
                expires_at,
            },
        )
        .await
        .expect("create session");
        if revoked {
            sqlx::query("UPDATE sessions SET revoked_at = now() WHERE id = $1")
                .bind(session.id)
                .execute(&ctx.pool)
                .await
                .expect("revoke session");
        }
        if let Some(replacement) = replaced_by {
            sqlx::query("UPDATE sessions SET replaced_by = $1 WHERE id = $2")
                .bind(replacement)
                .bind(session.id)
                .execute(&ctx.pool)
                .await
                .expect("mark replaced");
        }
        session.id
    }

    #[tokio::test]
    async fn purge_sessions_deletes_only_expired() {
        let ctx = TestCtx::new().await;
        let now = Utc::now();
        let user = users_repo::create_with_tos(&ctx.pool, "sess-a@example.com", "h", Some("v1"))
            .await
            .expect("create user")
            .id;
        let other = users_repo::create_with_tos(&ctx.pool, "sess-b@example.com", "h", Some("v1"))
            .await
            .expect("create user")
            .id;

        // The live replacement row first (unexpired): the
        // expired replaced row points at it.
        let replacement = seed_session(&ctx, user, now + Duration::days(30), false, None).await;
        let _expired_live = seed_session(&ctx, user, now - Duration::hours(1), false, None).await;
        let _expired_revoked = seed_session(&ctx, user, now - Duration::days(1), true, None).await;
        let _expired_replaced = seed_session(
            &ctx,
            user,
            now - Duration::hours(2),
            true,
            Some(replacement),
        )
        .await;
        let live = seed_session(&ctx, user, now + Duration::days(30), false, None).await;
        let live_revoked = seed_session(&ctx, user, now + Duration::days(30), true, None).await;
        let _other_expired = seed_session(&ctx, other, now - Duration::hours(1), false, None).await;

        let report = purge_expired_sessions(&ctx.pool, now, false)
            .await
            .expect("purge");
        assert_eq!(
            report.candidates, 4,
            "expired live + revoked + replaced + other user's"
        );
        assert_eq!(report.purged, 4);

        // The expired rows are gone; the unexpired rows
        // remain — including the live replacement the expired
        // row pointed at.
        let remaining = sessions::list_for_user(&ctx.pool, user)
            .await
            .expect("list sessions");
        let remaining_ids: Vec<Uuid> = remaining.iter().map(|s| s.id).collect();
        assert_eq!(remaining.len(), 3, "{remaining:?}");
        assert!(remaining_ids.contains(&replacement));
        assert!(remaining_ids.contains(&live));
        assert!(remaining_ids.contains(&live_revoked));
        assert!(sessions::list_for_user(&ctx.pool, other)
            .await
            .expect("list sessions")
            .is_empty());

        // The purge writes no audit rows (routine expiry is
        // not audited per session — the run's stdout line is
        // the record).
        let audits = audit_logs::list_for_target(&ctx.pool, "user", &user.to_string())
            .await
            .expect("list audits");
        assert!(audits.is_empty(), "{audits:?}");
        ctx.close().await;
    }

    #[tokio::test]
    async fn purge_sessions_dry_run_deletes_nothing() {
        let ctx = TestCtx::new().await;
        let now = Utc::now();
        let user = users_repo::create_with_tos(&ctx.pool, "sess-dry@example.com", "h", Some("v1"))
            .await
            .expect("create user")
            .id;
        let _expired = seed_session(&ctx, user, now - Duration::hours(1), false, None).await;
        let _live = seed_session(&ctx, user, now + Duration::days(30), false, None).await;

        let report = purge_expired_sessions(&ctx.pool, now, true)
            .await
            .expect("purge");
        assert_eq!(report.candidates, 1);
        assert_eq!(report.purged, 0);
        assert_eq!(
            sessions::list_for_user(&ctx.pool, user)
                .await
                .expect("list sessions")
                .len(),
            2
        );
        ctx.close().await;
    }
}
