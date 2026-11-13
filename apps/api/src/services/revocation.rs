//! Token revocation cache (P1-021, FR-A-01): the two mechanisms
//! that kill a live access token before its 15-minute expiry.
//!
//! 1. **The epoch (bulk kill).** `users.revoked_before` is the
//!    instant before which every access token of the user is
//!    dead. The extractor denies a token whose `iat` is earlier
//!    than it. The epoch lives on the user row the extractor
//!    already loads, so the check costs zero extra queries.
//!    The epoch is bumped by a password change — P1-018's
//!    `revoke_sessions_on_password_change`, in the same tx as
//!    the session revocation — and is only ever SET, never
//!    cleared. See `users::set_revoked_before`.
//! 2. **The jti denylist (individual kill).** One Redis key per
//!    revoked token — `pf:revocation:jti:{jti}` — holding `"1"`
//!    until the token would have expired anyway (TTL = the
//!    token's remaining life, capped at 900 s, the access-token
//!    TTL). Short-lived by construction: the entry dies on its
//!    own, so there is no cleanup job and no unbounded growth.
//!    The extractor consults it when the token carries a `jti`
//!    (every post-P1-013 token does; a legacy token without one
//!    skips the check — the epoch still covers it via `iat`).
//!
//! The first writer is `POST /v1/auth/logout-all` (P1-021): it
//! denylists the PRESENTED token's jti, so the token dies
//! immediately instead of living out its ≤ 15-minute residual
//! window. The single logout (`POST /v1/auth/logout`) has no
//! access token in hand (cookie only), so its residual window
//! stands — see the api-spec logout paragraph. Other devices'
//! tokens are untouched by a logout-all (their jtis are
//! unknown — only the epoch, on a password change, kills
//! those). A logout-all that also bumped the epoch would kill
//! every outstanding access token — not wired: the spec names
//! password-change as the bumper, and that change of meaning
//! belongs to P1-022/P1-024 or an owner decision, not to this
//! ticket silently.
//!
//! Fail-open (the P0-103 decision, same as the rate limiter):
//! a Redis error on the denylist READ is a warn + "not
//! revoked" — a cache outage must not take auth down. The
//! epoch check is DB-backed and still runs, so a password
//! change still kills tokens during a Redis outage; only the
//! per-token denylist degrades (a logout-all'd token lives
//! until exp, ≤ 15 min — the same residual window as the
//! single logout). A Redis error on the WRITE (logout-all's
//! denylist) is also warn + continue: the sessions are
//! already revoked, so no NEW token can be minted; only the
//! presented token's immediate death is lost.

use crate::AppState;
use deadpool_redis::redis::cmd;
use std::time::Duration;
use thiserror::Error;
use uuid::Uuid;

/// The Redis key for one revoked jti. The value is `"1"`; the
/// TTL is the token's remaining life (see
/// [`denylist_ttl_secs`]).
pub fn revocation_key(jti: &Uuid) -> String {
    format!("pf:revocation:jti:{jti}")
}

/// The longest a denylist entry can live: the access-token TTL
/// (15 min, P1-003/P1-013). A token cannot outlive its own
/// `exp`, so an entry never needs longer than this.
pub const MAX_DENYLIST_TTL_SECS: u64 = 900;

/// The TTL for a denylist entry, in seconds: the token's
/// remaining life (`exp - now`), clamped to `[1,
/// MAX_DENYLIST_TTL_SECS]`. Pure — unit-tested below. A token
/// that already expired still gets 1 s (a `SETEX` with 0 is a
/// Redis error); a token whose `exp` is further out than the
/// access-token TTL (a clock-skewed or hand-minted token)
/// gets the cap — the entry dies at most 15 minutes after the
/// write, and the token itself is dead at its `exp` anyway.
pub fn denylist_ttl_secs(exp_unix: i64, now_unix: i64) -> u64 {
    let remaining = exp_unix.saturating_sub(now_unix);
    remaining.clamp(1, MAX_DENYLIST_TTL_SECS as i64) as u64
}

/// What the revocation cache can fail with. `Internal` carries
/// no detail by design — callers fail open (see the module
/// docs) and log the error.
#[derive(Debug, Error)]
pub enum RevocationError {
    #[error("revocation cache unavailable")]
    Internal,
}

/// Denylist one token id until its token expires. The write is
/// `SETEX key ttl "1"` — one command, atomic, self-expiring.
/// Returns the TTL that was set (for the caller's log line).
pub async fn revoke_jti(
    state: &AppState,
    jti: &Uuid,
    exp_unix: i64,
) -> Result<u64, RevocationError> {
    let ttl = denylist_ttl_secs(exp_unix, chrono::Utc::now().timestamp());
    let mut conn = state.redis.get().await.map_err(|err| {
        tracing::error!(error = %err, "revocation: redis pool checkout failed");
        RevocationError::Internal
    })?;
    cmd("SETEX")
        .arg(revocation_key(jti))
        .arg(ttl as i64)
        .arg("1")
        .query_async::<()>(&mut conn)
        .await
        .map_err(|err| {
            tracing::error!(error = %err, "revocation: SETEX failed");
            RevocationError::Internal
        })?;
    Ok(ttl)
}

/// Whether a token id is denylisted. One `GET` on a hot key —
/// the key is ABSENT in the common case (the token is live),
/// so the common case is a single round trip returning nil.
/// A Redis error is `Err` (the caller fails open — see the
/// module docs); it is never silently "not revoked" here.
pub async fn is_jti_revoked(state: &AppState, jti: &Uuid) -> Result<bool, RevocationError> {
    let mut conn = state.redis.get().await.map_err(|err| {
        tracing::error!(error = %err, "revocation: redis pool checkout failed");
        RevocationError::Internal
    })?;
    let raw: Option<String> = cmd("GET")
        .arg(revocation_key(jti))
        .query_async(&mut conn)
        .await
        .map_err(|err| {
            tracing::error!(error = %err, "revocation: GET failed");
            RevocationError::Internal
        })?;
    Ok(raw.is_some())
}

/// How long a denylist entry may live, in [`Duration`] form —
/// the same clamp as [`denylist_ttl_secs`], for callers that
/// need a `Duration` (tests, future writers).
pub fn denylist_ttl(exp_unix: i64, now_unix: i64) -> Duration {
    Duration::from_secs(denylist_ttl_secs(exp_unix, now_unix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denylist_ttl_clamps_to_the_token_remaining_life() {
        // 10 minutes left → 600 s.
        assert_eq!(denylist_ttl_secs(1000, 400), 600);
        // 1 second left → 1 s (never 0 — SETEX rejects 0).
        assert_eq!(denylist_ttl_secs(1001, 1000), 1);
        // Already expired → 1 s (the floor, not an error).
        assert_eq!(denylist_ttl_secs(999, 1000), 1);
        assert_eq!(denylist_ttl_secs(0, 1000), 1);
        // Further out than any access token lives → the cap.
        assert_eq!(denylist_ttl_secs(1000 + 10_000, 1000), 900);
        // Exactly the cap → the cap.
        assert_eq!(denylist_ttl_secs(1000 + 900, 1000), 900);
    }

    #[test]
    fn revocation_key_is_namespaced_per_jti() {
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();
        assert_ne!(revocation_key(&a), revocation_key(&b));
        assert!(revocation_key(&a).starts_with("pf:revocation:jti:"));
        assert!(revocation_key(&a).ends_with(&a.to_string()));
    }
}
