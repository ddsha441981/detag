//! Access-token JWTs (P1-003, FR-A-01): HS256, short-lived.
//!
//! The login service issues one access token per login; the
//! `AuthContext` extractor (P1-019) verifies it. Both sides use
//! this module so the algorithm and claims shape have exactly
//! one definition.
//!
//! Claims (minimal, nothing invented): `sub` = the user id,
//! `role` = the user's role string, `exp` / `iat` = unix
//! seconds, `impersonator_id` = the admin's id when the token
//! is an impersonation token (P1-010 — absent on a normal
//! login token). No `iss`/`aud` — no issuer or audience is
//! defined anywhere in the docs, and inventing one would be a
//! lie the verifier would then have to enforce.
//!
//! Impersonation (P1-010): an impersonation token is a normal
//! access token whose claims carry `impersonator_id`. It acts
//! as the target user (its `sub` + `role` are the target's),
//! lives 15 minutes ([`ACCESS_TOKEN_TTL_SECS`]), and has no
//! refresh — there is no session row behind it, so P1-014's
//! refresh must reject it (there is nothing to rotate). The
//! `AuthContext` surfaces the claim; the admin guards reject
//! any context that carries it (an impersonator cannot reach
//! admin endpoints — P1-011 tests that).
//!
//! Lifetimes are product-policy constants, not config (the
//! P1-001 precedent: the password minimum is a constant too,
//! because no doc specified it):
//! - [`ACCESS_TOKEN_TTL_SECS`] = 900 (15 minutes — the
//!   industry-standard short access token; the refresh token
//!   carries the session).
//! - [`REFRESH_TOKEN_TTL_SECS`] = 30 days (P1-014's "30d opaque
//!   refresh" — the session row's `expires_at`).
//!
//! Crypto backend: `jsonwebtoken` with the `aws_lc_rs`
//! feature — NOT `rust_crypto`. The `rust_crypto` feature
//! pulls `rsa 0.9.x` (RUSTSEC-2023-0071, Marvin — patched in
//! 0.10, which jsonwebtoken 11 cannot use) and
//! `crypto-bigint 0.5.5` (RUSTSEC-2026-0387, patched in
//! 0.7.6, which rsa 0.9.x cannot use); both advisories have no
//! upgrade path under that feature, and `just deny` failed on
//! them. The `aws_lc_rs` backend removes both crates from the
//! tree entirely (only the patched `crypto-bigint 0.7.x`, via
//! the AWS SDK, remains) and `just deny` is green. It needs
//! cmake + a C compiler at build time — both exist on this VM
//! and in the CI image (verify before switching builders).
//!
//! The secret is `Settings::jwt_secret` (`PF_JWT_SECRET`).
//! Verification enforces HS256 only (`Validation::new(HS256)`)
//! — an `alg: none` or RS256 token can never verify against an
//! HMAC key, and the `exp` check is on by default.

use chrono::{Duration, Utc};
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// Access-token lifetime: 15 minutes.
pub const ACCESS_TOKEN_TTL_SECS: i64 = 15 * 60;
/// Refresh-token / session lifetime: 30 days (P1-014).
pub const REFRESH_TOKEN_TTL_SECS: i64 = 30 * 24 * 60 * 60;

/// The access-token claims. `sub` is the user id (UUID
/// string); `role` is the user's role string (`user`,
/// `support`, `admin`). `impersonator_id` is `Some` only on
/// an impersonation token (P1-010) — it is omitted from the
/// JSON on a normal token, and a token issued before P1-010
/// (without the field) still verifies, because the field
/// defaults to `None`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessTokenClaims {
    /// The user id.
    pub sub: String,
    /// The user's role.
    pub role: String,
    /// The impersonating admin's id, when this token is an
    /// impersonation token (P1-010).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impersonator_id: Option<Uuid>,
    /// Expiry (unix seconds).
    pub exp: i64,
    /// Issued at (unix seconds).
    pub iat: i64,
}

/// What issuing / verifying a token can fail with. The two
/// failure modes map to the taxonomy's PF-1010 (invalid) and
/// PF-1011 (expired) — the same codes the verification-token
/// flow uses, because to the caller a bad access token and a
/// bad verification token are the same shape of failure.
#[derive(Debug, Error)]
pub enum JwtError {
    #[error("invalid access token")]
    Invalid,
    #[error("access token expired")]
    Expired,
    #[error("failed to issue access token")]
    IssueFailed,
}

impl From<jsonwebtoken::errors::Error> for JwtError {
    fn from(err: jsonwebtoken::errors::Error) -> Self {
        use jsonwebtoken::errors::ErrorKind;
        match err.kind() {
            ErrorKind::ExpiredSignature => JwtError::Expired,
            _ => JwtError::Invalid,
        }
    }
}

/// Issue an access token for `user_id` + `role`, valid
/// `ttl_secs` seconds from now. The caller passes
/// [`ACCESS_TOKEN_TTL_SECS`] in production; tests pass short
/// (or negative) TTLs to prove expiry without a clock.
pub fn issue_access_token(
    secret: &str,
    user_id: Uuid,
    role: &str,
    ttl_secs: i64,
) -> Result<String, JwtError> {
    let now = Utc::now();
    let claims = AccessTokenClaims {
        sub: user_id.to_string(),
        role: role.to_string(),
        impersonator_id: None,
        exp: (now + Duration::seconds(ttl_secs)).timestamp(),
        iat: now.timestamp(),
    };
    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .map_err(|_| JwtError::IssueFailed)
}

/// Issue an impersonation token (P1-010): an access token
/// for the target user (`sub` + `role` are the target's)
/// that carries the impersonating admin's id. The caller
/// passes [`ACCESS_TOKEN_TTL_SECS`] in production (15
/// minutes — the impersonation token's whole lifetime); tests
/// pass short (or negative) TTLs to prove expiry without a
/// clock. There is no refresh for this token — no session row
/// is created behind it, so it dies at `exp` and cannot be
/// renewed (P1-014's refresh must reject it).
pub fn issue_impersonation_token(
    secret: &str,
    user_id: Uuid,
    role: &str,
    impersonator_id: Uuid,
    ttl_secs: i64,
) -> Result<String, JwtError> {
    let now = Utc::now();
    let claims = AccessTokenClaims {
        sub: user_id.to_string(),
        role: role.to_string(),
        impersonator_id: Some(impersonator_id),
        exp: (now + Duration::seconds(ttl_secs)).timestamp(),
        iat: now.timestamp(),
    };
    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .map_err(|_| JwtError::IssueFailed)
}

/// Verify an access token: signature (HS256 only) + expiry.
/// Returns the claims on success.
///
/// The `exp` check has NO leeway (`validation.leeway = 0`):
/// issuer and verifier are the same service, so there is no
/// clock skew to tolerate — a token is dead the second it
/// expires, and tests can prove expiry with a 1-second-old
/// token instead of waiting out jsonwebtoken's 60 s default.
pub fn verify_access_token(secret: &str, token: &str) -> Result<AccessTokenClaims, JwtError> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.leeway = 0;
    let data = decode::<AccessTokenClaims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )?;
    Ok(data.claims)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "test-secret-that-is-long-enough";
    const OTHER_SECRET: &str = "a-different-test-secret";

    fn user_id() -> Uuid {
        Uuid::parse_str("019aa1b2-c3d4-7e5f-8000-000000000001").unwrap()
    }

    #[test]
    fn roundtrip_issues_a_verifiable_token() {
        let token = issue_access_token(SECRET, user_id(), "user", ACCESS_TOKEN_TTL_SECS)
            .expect("issue succeeds");
        // Three base64url segments, HS256 header.
        assert_eq!(token.split('.').count(), 3);
        let claims = verify_access_token(SECRET, &token).expect("verify succeeds");
        assert_eq!(claims.sub, user_id().to_string());
        assert_eq!(claims.role, "user");
        assert_eq!(claims.exp - claims.iat, ACCESS_TOKEN_TTL_SECS);
    }

    #[test]
    fn expired_token_is_expired_not_invalid() {
        let token = issue_access_token(SECRET, user_id(), "user", -60).expect("issue succeeds");
        let err = verify_access_token(SECRET, &token).expect_err("expired must fail");
        assert!(matches!(err, JwtError::Expired));
    }

    #[test]
    fn tampered_payload_is_invalid() {
        let token = issue_access_token(SECRET, user_id(), "admin", ACCESS_TOKEN_TTL_SECS)
            .expect("issue succeeds");
        let parts: Vec<&str> = token.split('.').collect();
        // Flip one character of the payload (the middle segment).
        let mut payload: Vec<char> = parts[1].chars().collect();
        payload[0] = if payload[0] == 'A' { 'B' } else { 'A' };
        let tampered = format!(
            "{}.{}.{}",
            parts[0],
            payload.into_iter().collect::<String>(),
            parts[2]
        );
        let err = verify_access_token(SECRET, &tampered).expect_err("tampered must fail");
        assert!(matches!(err, JwtError::Invalid));
    }

    #[test]
    fn wrong_secret_is_invalid() {
        let token = issue_access_token(SECRET, user_id(), "user", ACCESS_TOKEN_TTL_SECS)
            .expect("issue succeeds");
        let err = verify_access_token(OTHER_SECRET, &token).expect_err("wrong key must fail");
        assert!(matches!(err, JwtError::Invalid));
    }

    #[test]
    fn garbage_is_invalid_not_expired() {
        let err = verify_access_token(SECRET, "not.a.jwt").expect_err("garbage must fail");
        assert!(matches!(err, JwtError::Invalid));
        let err = verify_access_token(SECRET, "").expect_err("empty must fail");
        assert!(matches!(err, JwtError::Invalid));
    }

    #[test]
    fn impersonation_token_carries_the_impersonator() {
        let admin = Uuid::parse_str("019aa1b2-c3d4-7e5f-8000-000000000002").unwrap();
        let token =
            issue_impersonation_token(SECRET, user_id(), "user", admin, ACCESS_TOKEN_TTL_SECS)
                .expect("issue succeeds");
        let claims = verify_access_token(SECRET, &token).expect("verify succeeds");
        assert_eq!(claims.sub, user_id().to_string());
        assert_eq!(claims.role, "user");
        assert_eq!(claims.impersonator_id, Some(admin));
        assert_eq!(claims.exp - claims.iat, ACCESS_TOKEN_TTL_SECS);
    }

    #[test]
    fn normal_token_has_no_impersonator() {
        let token = issue_access_token(SECRET, user_id(), "user", ACCESS_TOKEN_TTL_SECS)
            .expect("issue succeeds");
        let claims = verify_access_token(SECRET, &token).expect("verify succeeds");
        assert_eq!(claims.impersonator_id, None);
    }

    #[test]
    fn expired_impersonation_token_is_expired() {
        let admin = Uuid::parse_str("019aa1b2-c3d4-7e5f-8000-000000000002").unwrap();
        let token = issue_impersonation_token(SECRET, user_id(), "user", admin, -60)
            .expect("issue succeeds");
        let err = verify_access_token(SECRET, &token).expect_err("expired must fail");
        assert!(matches!(err, JwtError::Expired));
    }
}
