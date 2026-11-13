//! Users repository (P0-041, E0.4). SQL lives here, not in services
//! (coding standards). `query_as!` macros are compile-time checked
//! against the live schema; offline data is checked in under
//! `.sqlx/` (see `just sqlx-prepare`).

use super::error::RepoError;
use crate::pagination::Cursor;
use chrono::{DateTime, NaiveDate, Utc};
use uuid::Uuid;

/// A row of `users` (migration 001). `settings` is the raw jsonb
/// value; services interpret its keys. `display_name` /
/// `pending_email` are the P1-005 profile fields (migration
/// 20261113040605): the display name (nullable until set) and
/// the email-change intent (nullable until a change is
/// requested; P1-006 verifies + swaps it).
#[derive(Debug, Clone, PartialEq)]
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub password_hash: String,
    pub email_verified_at: Option<DateTime<Utc>>,
    pub role: String,
    pub status: String,
    pub tos_version: Option<String>,
    pub deleted_at: Option<DateTime<Utc>>,
    /// The revocation epoch (P1-021): every access token of
    /// this user with `iat` earlier than this instant is dead.
    /// `None` means never revoked. Set (never cleared) by a
    /// password change — `revoke_sessions_on_password_change`
    /// bumps it in the same tx as the session revocation.
    pub revoked_before: Option<DateTime<Utc>>,
    pub display_name: Option<String>,
    pub pending_email: Option<String>,
    pub settings: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Insert a user. Duplicate email (case-insensitive — the column is
/// citext) fails with `RepoError::UniqueViolation` naming
/// `users_email_key`; a bad role/status fails with `CheckViolation`.
pub async fn create(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    email: &str,
    password_hash: &str,
) -> Result<User, RepoError> {
    create_with_tos(pool, email, password_hash, None).await
}

/// Insert a user recording the accepted ToS version (P1-001).
/// `tos_version` is the `accept_tos_version` the client sent —
/// the caller has already checked it equals the current version;
/// this function records, it does not validate.
pub async fn create_with_tos(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    email: &str,
    password_hash: &str,
    tos_version: Option<&str>,
) -> Result<User, RepoError> {
    sqlx::query_as!(
        User,
        "INSERT INTO users (email, password_hash, tos_version) VALUES ($1, $2, $3)
         RETURNING id, email, password_hash, email_verified_at, role, status,
             tos_version, deleted_at, revoked_before, display_name, pending_email, settings, created_at, updated_at",
        email,
        password_hash,
        tos_version
    )
    .fetch_one(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Fetch by id. `None` when the id does not exist.
pub async fn by_id(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    id: Uuid,
) -> Result<Option<User>, RepoError> {
    sqlx::query_as!(
        User,
        "SELECT id, email, password_hash, email_verified_at, role, status,
             tos_version, deleted_at, revoked_before, display_name, pending_email, settings, created_at, updated_at
         FROM users WHERE id = $1",
        id
    )
    .fetch_optional(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Fetch by email. The column is citext, so the match is/// case-insensitive (`Alice@Example.com` finds `alice@example.com`).
///
/// The `$1::citext` cast is load-bearing: a bound `text` parameter
/// makes Postgres resolve `email = $1` as case-sensitive `text =
/// text` (the citext value is cast to text), so a differently-cased
/// lookup would silently miss. Casting the parameter forces the
/// citext comparison. (Caught by the P0-041 live test.)
pub async fn by_email(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    email: &str,
) -> Result<Option<User>, RepoError> {
    sqlx::query_as!(
        User,
        "SELECT id, email, password_hash, email_verified_at, role, status,
             tos_version, deleted_at, revoked_before, display_name, pending_email, settings, created_at, updated_at
         FROM users WHERE email = $1::citext",
        email
    )
    .fetch_optional(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Update the profile fields (P1-005): sets both columns to
/// the given values (`None` clears). The caller loads the user
/// first, applies the patch in Rust, and stores the final
/// values — there is no "leave unchanged" at this layer, so a
/// plain SET is correct (a COALESCE here could never clear a
/// field). The email is bound as `citext` (same load-bearing
/// cast as `by_email`). Returns `NotFound` when the user does
/// not exist. The caller validates (display-name shape, email
/// shape + disposable + duplicate) — this function stores,
/// it does not validate.
pub async fn update_profile(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    id: Uuid,
    display_name: Option<String>,
    pending_email: Option<String>,
) -> Result<User, RepoError> {
    sqlx::query_as!(
        User,
        "UPDATE users SET
             display_name = $2,
             pending_email = $3::citext,
             updated_at = now()
         WHERE id = $1
         RETURNING id, email, password_hash, email_verified_at, role, status,
             tos_version, deleted_at, revoked_before, display_name, pending_email, settings, created_at, updated_at",
        id,
        display_name,
        pending_email,
    )
    .fetch_one(pool)
    .await
    .map_err(|err| match err {
        sqlx::Error::RowNotFound => RepoError::NotFound,
        other => RepoError::from_sqlx(other),
    })
}
pub async fn set_email_verified(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    id: Uuid,
) -> Result<User, RepoError> {
    sqlx::query_as!(
        User,
        "UPDATE users SET email_verified_at = now() WHERE id = $1
         RETURNING id, email, password_hash, email_verified_at, role, status,
             tos_version, deleted_at, revoked_before, display_name, pending_email, settings, created_at, updated_at",
        id
    )
    .fetch_one(pool)
    .await
    .map_err(|err| match err {
        sqlx::Error::RowNotFound => RepoError::NotFound,
        other => RepoError::from_sqlx(other),
    })
}

/// Bump the revocation epoch (P1-021): every access token of
/// this user issued before now dies at the extractor
/// (`iat < revoked_before`). The bump is one UPDATE — the
/// caller (P1-018's `revoke_sessions_on_password_change`)
/// runs it inside the same tx as the session revocation, so
/// the epoch and the revoked sessions commit or roll back
/// together. The epoch is only ever SET, never cleared: there
/// is no "unrevoke". Returns `NotFound` when the id does not
/// exist. The timestamp is `now()` (microseconds) while the
/// token's `iat` is whole seconds — a token minted in the same
/// second as the bump (before OR after it) shares the bump's
/// second, so the comparison is `iat <= revoked_before` (the
/// floored second): a pre-change token in that second must
/// die, and a post-change token in the same second is denied
/// too and self-heals via refresh (its refresh token is still
/// valid — the change-password case keeps the current
/// session). A false denial self-heals; a false admission
/// would be a hole. So the comparison errs toward denial by
/// construction — strict `<` against the floored second has a
/// one-second hole (`iat == floor(bump)` is not `< bump`).
pub async fn set_revoked_before(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    id: Uuid,
) -> Result<(), RepoError> {
    let result = sqlx::query!(
        "UPDATE users SET revoked_before = now(), updated_at = now() WHERE id = $1",
        id
    )
    .execute(pool)
    .await
    .map_err(RepoError::from_sqlx)?;
    if result.rows_affected() == 0 {
        return Err(RepoError::NotFound);
    }
    Ok(())
}

/// Set a user's password hash (P1-025): the forgot/reset flow
/// writes the new Argon2id hash here after consuming the reset
/// token. One UPDATE; returns `NotFound` when the id does not
/// exist. The caller runs it inside the same tx as the token
/// consume + the lockout-counter reset, so the three commit or
/// roll back together (a consumed token with an unchanged
/// password — or a changed password with a live token — cannot
/// exist).
pub async fn set_password_hash(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    id: Uuid,
    password_hash: &str,
) -> Result<(), RepoError> {
    let result = sqlx::query!(
        "UPDATE users SET password_hash = $2, updated_at = now() WHERE id = $1",
        id,
        password_hash
    )
    .execute(pool)
    .await
    .map_err(RepoError::from_sqlx)?;
    if result.rows_affected() == 0 {
        return Err(RepoError::NotFound);
    }
    Ok(())
}

/// Swap the email to the pending address (P1-006): the address
/// moves from `pending_email` to `email`, the pending intent is
/// cleared, and `email_verified_at` is set — the token that
/// authorized this swap was delivered to the new address, so
/// the new address is verified by it. The `pending_email IS
/// NOT NULL` guard is the race guard: if the intent was cleared
/// (or already swapped) concurrently, zero rows update and the
/// caller gets `NotFound` instead of swapping a stale or empty
/// address. A `UniqueViolation` means the pending address was
/// taken by another account while it was pending — the swap is
/// the enforcement point for the P1-005 intent-time duplicate
/// check (which was only a courtesy).
pub async fn swap_email(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    id: Uuid,
) -> Result<User, RepoError> {
    sqlx::query_as!(
        User,
        "UPDATE users SET
             email = pending_email,
             pending_email = NULL,
             email_verified_at = now(),
             updated_at = now()
         WHERE id = $1 AND pending_email IS NOT NULL
         RETURNING id, email, password_hash, email_verified_at, role, status,
             tos_version, deleted_at, revoked_before, display_name, pending_email, settings, created_at, updated_at",
        id
    )
    .fetch_one(pool)
    .await
    .map_err(|err| match err {
        sqlx::Error::RowNotFound => RepoError::NotFound,
        other => RepoError::from_sqlx(other),
    })
}

/// The deletion grace (P1-007): 30 days from the deletion
/// request until the nightly purge hard-deletes the account.
/// A product-policy constant, not config — no doc specified a
/// different value, and the purge, the api-spec, and the
/// data-model doc all cite this one number.
pub const DELETION_GRACE_DAYS: i64 = 30;

/// Request account deletion (P1-007): the account enters the
/// grace (`status = 'deletion_pending'`, `deleted_at = now()`).
/// The caller owns idempotency: a second request for an
/// already-pending account is a no-op at the service layer
/// (this function would refresh `deleted_at` — the grace runs
/// from the FIRST request, never restarted by re-requesting).
pub async fn request_deletion(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    id: Uuid,
) -> Result<User, RepoError> {
    sqlx::query_as!(
        User,
        "UPDATE users SET
             status = 'deletion_pending',
             deleted_at = now(),
             updated_at = now()
         WHERE id = $1
         RETURNING id, email, password_hash, email_verified_at, role, status,
             tos_version, deleted_at, revoked_before, display_name, pending_email, settings, created_at, updated_at",
        id
    )
    .fetch_one(pool)
    .await
    .map_err(|err| match err {
        sqlx::Error::RowNotFound => RepoError::NotFound,
        other => RepoError::from_sqlx(other),
    })
}

/// Cancel a pending deletion (P1-007 — the login hook): the
/// account is active again, `deleted_at` cleared. Returns
/// `None` when the account is not pending (already active, or
/// a concurrent login cancelled it first) — that is not an
/// error, it is the goal state already holding.
pub async fn cancel_deletion(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    id: Uuid,
) -> Result<Option<User>, RepoError> {
    sqlx::query_as!(
        User,
        "UPDATE users SET
             status = 'active',
             deleted_at = NULL,
             updated_at = now()
         WHERE id = $1 AND status = 'deletion_pending'
         RETURNING id, email, password_hash, email_verified_at, role, status,
             tos_version, deleted_at, revoked_before, display_name, pending_email, settings, created_at, updated_at",
        id
    )
    .fetch_optional(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// The users the nightly purge may hard-delete (P1-007):
/// `deletion_pending` with `deleted_at` before `cutoff`
/// (usually now − 30 days). Oldest first, so a bounded purge
/// run takes the longest-waiting accounts first.
pub async fn purgeable(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    cutoff: DateTime<Utc>,
) -> Result<Vec<Uuid>, RepoError> {
    sqlx::query_scalar!(
        "SELECT id FROM users
         WHERE status = 'deletion_pending' AND deleted_at < $1
         ORDER BY deleted_at ASC",
        cutoff,
    )
    .fetch_all(pool)
    .await
    .map_err(RepoError::from_sqlx)
}

/// Hard-delete one user (P1-007, the purge's last step). The
/// database cascades do the work: sessions, jobs (+ artifacts,
/// job events, quality reports via jobs), uploads,
/// subscriptions, usage counters, idempotency keys, and
/// verification tokens are `ON DELETE CASCADE`; audit rows
/// survive with `actor_user_id` intact (no FK on the actor —
/// migration 20261113062352 dropped it: the actor is a
/// historical fact, and the FK's ON DELETE SET NULL was an
/// UPDATE the append-only trigger forbids, so the purge could
/// never delete a user with audit rows); outbox rows survive
/// (they are addressed by hash, not by user). Returns `false`
/// when the user was already gone (a concurrent purge took it
/// first).
pub async fn delete_user(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    id: Uuid,
) -> Result<bool, RepoError> {
    let result = sqlx::query!("DELETE FROM users WHERE id = $1", id)
        .execute(pool)
        .await
        .map_err(RepoError::from_sqlx)?;
    Ok(result.rows_affected() > 0)
}

/// Suspend an account (P1-008): status becomes `suspended`
/// and any pending deletion is cancelled (`deleted_at` is
/// cleared — the deletion request does not survive admin
/// intervention; the audit's `before` records the prior status
/// and `deleted_at`, so the cancellation is visible). The
/// invariant this keeps: `deleted_at` is non-NULL exactly when
/// the status is `deletion_pending`. `NotFound` when the id
/// does not exist. The service decides idempotence (a second
/// suspend is a no-op there, not here).
pub async fn suspend(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    id: Uuid,
) -> Result<User, RepoError> {
    sqlx::query_as!(
        User,
        "UPDATE users SET status = 'suspended', deleted_at = NULL, updated_at = now()
         WHERE id = $1
         RETURNING id, email, password_hash, email_verified_at, role, status,
             tos_version, deleted_at, revoked_before, display_name, pending_email, settings, created_at, updated_at",
        id
    )
    .fetch_one(pool)
    .await
    .map_err(|err| match err {
        sqlx::Error::RowNotFound => RepoError::NotFound,
        other => RepoError::from_sqlx(other),
    })
}

/// Restore a suspended account (P1-008): status becomes
/// `active` (`deleted_at` is cleared too — a suspension
/// cancels any pending deletion, so a restored account never
/// carries a stale grace timestamp). `NotFound` when the id
/// does not exist. Restoring does NOT un-revoke sessions (a
/// suspend revokes them permanently; the user logs in again)
/// and does NOT re-run a deletion — the service owns both.
/// The service decides idempotence (restoring a non-suspended
/// account is a no-op there, not here).
pub async fn restore(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    id: Uuid,
) -> Result<User, RepoError> {
    sqlx::query_as!(
        User,
        "UPDATE users SET status = 'active', deleted_at = NULL, updated_at = now()
         WHERE id = $1
         RETURNING id, email, password_hash, email_verified_at, role, status,
             tos_version, deleted_at, revoked_before, display_name, pending_email, settings, created_at, updated_at",
        id
    )
    .fetch_one(pool)
    .await
    .map_err(|err| match err {
        sqlx::Error::RowNotFound => RepoError::NotFound,
        other => RepoError::from_sqlx(other),
    })
}

/// One row of the admin user search (P1-009): the user, plus
/// the plan code (the live subscription's plan, else `free`)
/// and today's usage (UTC day — zeros when the user has used
/// nothing today; the read never creates the counter row).
/// The three `Option` fields are `None` only in sqlx's eyes:
/// the query wraps each in `COALESCE` (a missing subscription
/// is `free`, a missing usage row is 0), so the service
/// unwraps them with those defaults — the `Option` is sqlx's
/// nullability inference being conservative about the scalar
/// subquery / the LEFT JOIN, not a real null.
#[derive(Debug, Clone, PartialEq)]
pub struct UserSearchRow {
    pub id: Uuid,
    pub email: String,
    pub display_name: Option<String>,
    pub role: String,
    pub status: String,
    pub email_verified_at: Option<DateTime<Utc>>,
    pub tos_version: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub plan_code: Option<String>,
    pub images_used_today: Option<i32>,
    pub video_seconds_used_today: Option<i32>,
}

/// Escape a user-supplied search string for use as a `LIKE`
/// prefix pattern: `\`, `%`, and `_` lose their wildcard
/// meaning (a `%` in the query must match a literal `%`, not
/// everything). The trailing `%` is the prefix match itself.
fn escape_like_prefix(query: &str) -> String {
    let mut out = String::with_capacity(query.len() + 1);
    for ch in query.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('%');
    out
}

/// Search users for the admin list (P1-009): prefix match
/// (case-insensitive) on email, display name, or id — a
/// `None` query lists everything. Keyset pagination on
/// `(created_at, id)`: `cursor` is the last row of the
/// previous page, `limit` is `limit + 1` rows (the caller
/// drops the extra row via `Page::from_window`).
///
/// The keyset truncates `created_at` to milliseconds
/// (`date_trunc`): the `Cursor` stores Unix millis (P0-068)
/// while `created_at` carries microseconds, so comparing the
/// raw timestamp against the cursor would re-include the
/// cursor's own row (its microseconds sit past the truncated
/// millisecond) or skip a row created in the same millisecond
/// with a smaller id. Truncating both sides of the comparison
/// — and ordering by the truncated key — makes the page
/// boundary exact: rows in one millisecond order by `id`,
/// and the cursor's `ts` is `created_at.timestamp_millis()`.
/// `day` is the UTC date whose usage row is joined (today).
pub async fn search_users(
    pool: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    query: Option<&str>,
    cursor: Option<Cursor>,
    limit: u32,
    day: NaiveDate,
) -> Result<Vec<UserSearchRow>, RepoError> {
    let pattern = match query {
        Some(q) => escape_like_prefix(q),
        // No filter: the pattern matches every row.
        None => "%".to_string(),
    };
    match cursor {
        Some(cursor) => {
            let cursor_ts = DateTime::from_timestamp_millis(cursor.ts)
                .expect("a cursor ts is a valid Unix-millis timestamp");
            sqlx::query_as!(
                UserSearchRow,
                "SELECT u.id, u.email, u.display_name, u.role, u.status,
                    u.email_verified_at, u.tos_version, u.created_at, u.updated_at,
                    COALESCE((SELECT p.code FROM subscriptions s JOIN plans p ON p.id = s.plan_id
                              WHERE s.user_id = u.id AND s.status IN ('trialing', 'active')
                              ORDER BY s.created_at DESC LIMIT 1), 'free') AS plan_code,
                    COALESCE(uc.images_used, 0) AS images_used_today,
                    COALESCE(uc.video_seconds_used, 0) AS video_seconds_used_today
                 FROM users u
                 LEFT JOIN usage_counters uc ON uc.user_id = u.id AND uc.day = $2
                 WHERE (u.email ILIKE $1 ESCAPE '\\' OR u.display_name ILIKE $1 ESCAPE '\\'
                        OR u.id::text ILIKE $1 ESCAPE '\\')
                   AND (date_trunc('milliseconds', u.created_at), u.id) > ($3, $4)
                 ORDER BY date_trunc('milliseconds', u.created_at) ASC, u.id ASC
                 LIMIT $5",
                pattern,
                day,
                cursor_ts,
                cursor.id,
                limit as i64,
            )
            .fetch_all(pool)
            .await
            .map_err(RepoError::from_sqlx)
        }
        None => sqlx::query_as!(
            UserSearchRow,
            "SELECT u.id, u.email, u.display_name, u.role, u.status,
                    u.email_verified_at, u.tos_version, u.created_at, u.updated_at,
                    COALESCE((SELECT p.code FROM subscriptions s JOIN plans p ON p.id = s.plan_id
                              WHERE s.user_id = u.id AND s.status IN ('trialing', 'active')
                              ORDER BY s.created_at DESC LIMIT 1), 'free') AS plan_code,
                    COALESCE(uc.images_used, 0) AS images_used_today,
                    COALESCE(uc.video_seconds_used, 0) AS video_seconds_used_today
                 FROM users u
                 LEFT JOIN usage_counters uc ON uc.user_id = u.id AND uc.day = $2
                 WHERE (u.email ILIKE $1 ESCAPE '\\' OR u.display_name ILIKE $1 ESCAPE '\\'
                        OR u.id::text ILIKE $1 ESCAPE '\\')
                 ORDER BY date_trunc('milliseconds', u.created_at) ASC, u.id ASC
                 LIMIT $3",
            pattern,
            day,
            limit as i64,
        )
        .fetch_all(pool)
        .await
        .map_err(RepoError::from_sqlx),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn like_prefix_escapes_wildcards() {
        assert_eq!(escape_like_prefix("alice"), "alice%");
        assert_eq!(escape_like_prefix("100%"), "100\\%%");
        assert_eq!(escape_like_prefix("a_b"), "a\\_b%");
        assert_eq!(escape_like_prefix("a\\b"), "a\\\\b%");
        assert_eq!(escape_like_prefix(""), "%");
    }
}
