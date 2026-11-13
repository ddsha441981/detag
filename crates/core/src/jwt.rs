//! Access-token JWTs (P1-003, FR-A-01): HS256, short-lived.
//!
//! The login service issues one access token per login; the
//! `AuthContext` extractor (P1-019) verifies it. Both sides use
//! this module so the algorithm and claims shape have exactly
//! one definition.
//!
//! Claims (P1-013 — the shape is locked by test, see the
//! `claims_shape_is_locked` test): `sub` = the user id,
//! `role` = the user's role string, `ver` = the token
//! version (1), `impersonator_id` = the admin's id when the
//! token is an impersonation token (P1-010 — absent on a
//! normal login token), `jti` = the token's unique id (one
//! UUID per issuance), `exp` / `iat` = unix seconds,
//! `iss` = `"detag"`, `aud` = `"detag-api"`. The JOSE header
//! carries `kid` = the signing key's fingerprint (the first
//! 16 hex chars of the secret's sha256) + `alg: HS256`.
//!
//! The values are product-policy constants, not config (the
//! P1-001 precedent): `iss` is the service name, not a URL —
//! Detag has no domain yet, and a URL issuer would be a lie
//! today; `aud` is the API the token is for. P1-003's "no
//! `iss`/`aud`" decision is superseded by this ticket: the
//! spec (P1-013) defines them, so they are defined here and
//! enforced by the verifier.
//!
//! Backward compatibility: a token issued before P1-013
//! (claims `{sub, role, exp, iat}` only, no header `kid`)
//! still verifies — the new fields default on decode
//! (`ver` → 1, `iss` → `"detag"`, `aud` → `"detag-api"`,
//! `jti` / `impersonator_id` → `None`, missing `kid` →
//! accepted). A token that CARRIES a wrong value is
//! rejected: wrong `iss`/`aud` → Invalid (jsonwebtoken's
//! issuer/audience validation), `ver` ≠ 1 → Invalid, a
//! `kid` that is not this secret's fingerprint → Invalid.
//! `jti` is never required by the verifier (legacy tokens
//! have none) — it exists for future revocation/debugging
//! (e.g. P1-018, revoke on password change).
//!
//! Key rotation (P0-105's dual-verify draft, Phase 5): the
//! `kid` fingerprint lets a future verifier pick the key
//! from a key set instead of trying every secret. Today
//! there is one key, so the verifier knows one fingerprint.
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

use crate::checksum::sha256_hex;
use chrono::{Duration, Utc};
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// Access-token lifetime: 15 minutes.
pub const ACCESS_TOKEN_TTL_SECS: i64 = 15 * 60;
/// Refresh-token / session lifetime: 30 days (P1-014).
pub const REFRESH_TOKEN_TTL_SECS: i64 = 30 * 24 * 60 * 60;
/// The token issuer (P1-013): the service name, not a URL
/// (Detag has no domain yet).
pub const TOKEN_ISSUER: &str = "detag";
/// The token audience (P1-013): the API the token is for.
pub const TOKEN_AUDIENCE: &str = "detag-api";
/// The token version (P1-013): 1. The verifier rejects any
/// other version — a future shape change bumps this and the
/// verifier learns to accept both during the migration.
pub const TOKEN_VERSION: u32 = 1;

/// The signing key's fingerprint (P1-013): the first 16 hex
/// chars of the secret's sha256. This is the JOSE header
/// `kid` on every issued token. It identifies the key without
/// revealing it (a truncated hash is not reversible); the
/// verifier accepts a missing `kid` (a pre-P1-013 token) and
/// rejects a `kid` that is not this secret's fingerprint.
pub fn key_fingerprint(secret: &str) -> String {
    sha256_hex(secret.as_bytes())[..16].to_string()
}

fn default_token_version() -> u32 {
    TOKEN_VERSION
}

fn default_issuer() -> String {
    TOKEN_ISSUER.to_string()
}

fn default_audience() -> String {
    TOKEN_AUDIENCE.to_string()
}

/// The access-token claims (P1-013 — the shape is locked by
/// the `claims_shape_is_locked` test). `sub` is the user id
/// (UUID string); `role` is the user's role string (`user`,
/// `support`, `admin`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessTokenClaims {
    /// The user id.
    pub sub: String,
    /// The user's role.
    pub role: String,
    /// The token version (1). Defaults to 1 on decode, so a
    /// pre-P1-013 token (no `ver`) still verifies.
    #[serde(default = "default_token_version")]
    pub ver: u32,
    /// The impersonating admin's id, when this token is an
    /// impersonation token (P1-010).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impersonator_id: Option<Uuid>,
    /// The token's unique id (one UUID per issuance). Defaults
    /// to `None` on decode — pre-P1-013 tokens have none, and
    /// the verifier never requires it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jti: Option<Uuid>,
    /// Expiry (unix seconds).
    pub exp: i64,
    /// Issued at (unix seconds).
    pub iat: i64,
    /// The issuer (`"detag"`). Defaults on decode, so a
    /// pre-P1-013 token (no `iss`) still verifies.
    #[serde(default = "default_issuer")]
    pub iss: String,
    /// The audience (`"detag-api"`). Defaults on decode, so a
    /// pre-P1-013 token (no `aud`) still verifies.
    #[serde(default = "default_audience")]
    pub aud: String,
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
/// (or negative) TTLs to prove expiry without a clock. The
/// header carries `kid` = this secret's fingerprint; the
/// claims carry the locked P1-013 shape (`ver`, `jti`, `iss`,
/// `aud` — no `impersonator_id` on a normal token).
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
        ver: TOKEN_VERSION,
        impersonator_id: None,
        jti: Some(Uuid::now_v7()),
        exp: (now + Duration::seconds(ttl_secs)).timestamp(),
        iat: now.timestamp(),
        iss: TOKEN_ISSUER.to_string(),
        aud: TOKEN_AUDIENCE.to_string(),
    };
    let mut header = Header::new(Algorithm::HS256);
    header.kid = Some(key_fingerprint(secret));
    encode(
        &header,
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
        ver: TOKEN_VERSION,
        impersonator_id: Some(impersonator_id),
        jti: Some(Uuid::now_v7()),
        exp: (now + Duration::seconds(ttl_secs)).timestamp(),
        iat: now.timestamp(),
        iss: TOKEN_ISSUER.to_string(),
        aud: TOKEN_AUDIENCE.to_string(),
    };
    let mut header = Header::new(Algorithm::HS256);
    header.kid = Some(key_fingerprint(secret));
    encode(
        &header,
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
///
/// P1-013 enforcement, in order: the signature + `exp`
/// (jsonwebtoken); `iss` + `aud` (jsonwebtoken's issuer /
/// audience validation — a wrong value is `Invalid`, i.e.
/// PF-1010); `ver` must be [`TOKEN_VERSION`] (a token from
/// a future shape is not one this verifier understands);
/// a header `kid`, when present, must be this secret's
/// fingerprint (a `kid` for another key is not this key's
/// token — the Phase 5 dual-verify extends this to a key
/// set). A missing `kid`, `ver`, `iss`, `aud`, or `jti` is a
/// pre-P1-013 token and still verifies (the claims default
/// on decode).
pub fn verify_access_token(secret: &str, token: &str) -> Result<AccessTokenClaims, JwtError> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.leeway = 0;
    validation.set_issuer(&[TOKEN_ISSUER]);
    validation.set_audience(&[TOKEN_AUDIENCE]);
    let data = decode::<AccessTokenClaims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )?;
    if data.claims.ver != TOKEN_VERSION {
        return Err(JwtError::Invalid);
    }
    if let Some(kid) = &data.header.kid {
        if kid != &key_fingerprint(secret) {
            return Err(JwtError::Invalid);
        }
    }
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

    /// Decode a token's raw JSON payload (no verification —
    /// the shape test reads what was issued, not what verifies).
    fn payload_of(token: &str) -> serde_json::Value {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3, "a JWT has three segments");
        let bytes = URL_SAFE_NO_PAD
            .decode(parts[1])
            .expect("payload is base64url");
        serde_json::from_slice(&bytes).expect("payload is JSON")
    }

    /// Decode a token's raw JOSE header.
    fn header_of(token: &str) -> serde_json::Value {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        let parts: Vec<&str> = token.split('.').collect();
        let bytes = URL_SAFE_NO_PAD
            .decode(parts[0])
            .expect("header is base64url");
        serde_json::from_slice(&bytes).expect("header is JSON")
    }

    /// A valid claims value, for tests that mutate one field
    /// and prove the verifier rejects it.
    fn base_claims() -> AccessTokenClaims {
        let now = Utc::now();
        AccessTokenClaims {
            sub: user_id().to_string(),
            role: "user".to_string(),
            ver: TOKEN_VERSION,
            impersonator_id: None,
            jti: Some(Uuid::now_v7()),
            exp: (now + Duration::seconds(ACCESS_TOKEN_TTL_SECS)).timestamp(),
            iat: now.timestamp(),
            iss: TOKEN_ISSUER.to_string(),
            aud: TOKEN_AUDIENCE.to_string(),
        }
    }

    fn encode_with(secret: &str, claims: &AccessTokenClaims, kid: Option<&str>) -> String {
        let mut header = Header::new(Algorithm::HS256);
        header.kid = kid.map(str::to_string);
        encode(
            &header,
            claims,
            &EncodingKey::from_secret(secret.as_bytes()),
        )
        .expect("encode succeeds")
    }

    #[test]
    fn claims_shape_is_locked() {
        let token = issue_access_token(SECRET, user_id(), "user", ACCESS_TOKEN_TTL_SECS)
            .expect("issue succeeds");
        // The header: HS256, typ JWT, kid = this secret's
        // fingerprint.
        let header = header_of(&token);
        assert_eq!(header["alg"], "HS256");
        assert_eq!(header["typ"], "JWT");
        assert_eq!(header["kid"], key_fingerprint(SECRET));
        // The payload: exactly the locked key set — no more,
        // no fewer (a stray claim would be a shape change).
        // Sorted: serde_json's map order is an implementation
        // detail, not part of the shape.
        let payload = payload_of(&token);
        let mut keys: Vec<&str> = payload
            .as_object()
            .expect("payload is an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["aud", "exp", "iat", "iss", "jti", "role", "sub", "ver"],
            "the claims shape is locked: {payload}"
        );
        assert_eq!(payload["sub"], user_id().to_string());
        assert_eq!(payload["role"], "user");
        assert_eq!(payload["ver"], TOKEN_VERSION);
        assert_eq!(payload["iss"], TOKEN_ISSUER);
        assert_eq!(payload["aud"], TOKEN_AUDIENCE);
        assert_eq!(
            payload["exp"].as_i64().unwrap() - payload["iat"].as_i64().unwrap(),
            900
        );
        assert!(
            payload["jti"].as_str().unwrap().parse::<Uuid>().is_ok(),
            "jti is a UUID: {payload}"
        );
        assert!(
            payload.get("impersonator_id").is_none(),
            "a normal token has no impersonator"
        );
        // And it verifies, with the same values in the claims.
        let claims = verify_access_token(SECRET, &token).expect("verify succeeds");
        assert_eq!(claims.ver, TOKEN_VERSION);
        assert_eq!(claims.iss, TOKEN_ISSUER);
        assert_eq!(claims.aud, TOKEN_AUDIENCE);
        assert!(claims.jti.is_some());
        assert_eq!(claims.impersonator_id, None);
    }

    #[test]
    fn impersonation_token_shape_includes_impersonator() {
        let admin = Uuid::parse_str("019aa1b2-c3d4-7e5f-8000-000000000002").unwrap();
        let token =
            issue_impersonation_token(SECRET, user_id(), "user", admin, ACCESS_TOKEN_TTL_SECS)
                .expect("issue succeeds");
        let payload = payload_of(&token);
        assert_eq!(payload["impersonator_id"], admin.to_string());
        assert_eq!(header_of(&token)["kid"], key_fingerprint(SECRET));
    }

    #[test]
    fn verify_rejects_wrong_issuer_audience_version_kid() {
        // Wrong issuer.
        let mut claims = base_claims();
        claims.iss = "evil".to_string();
        let token = encode_with(SECRET, &claims, Some(&key_fingerprint(SECRET)));
        assert!(matches!(
            verify_access_token(SECRET, &token).expect_err("wrong iss must fail"),
            JwtError::Invalid
        ));
        // Wrong audience.
        let mut claims = base_claims();
        claims.aud = "other-api".to_string();
        let token = encode_with(SECRET, &claims, Some(&key_fingerprint(SECRET)));
        assert!(matches!(
            verify_access_token(SECRET, &token).expect_err("wrong aud must fail"),
            JwtError::Invalid
        ));
        // Future version.
        let mut claims = base_claims();
        claims.ver = TOKEN_VERSION + 1;
        let token = encode_with(SECRET, &claims, Some(&key_fingerprint(SECRET)));
        assert!(matches!(
            verify_access_token(SECRET, &token).expect_err("future ver must fail"),
            JwtError::Invalid
        ));
        // A kid for another key.
        let claims = base_claims();
        let token = encode_with(SECRET, &claims, Some("0000000000000000"));
        assert!(matches!(
            verify_access_token(SECRET, &token).expect_err("foreign kid must fail"),
            JwtError::Invalid
        ));
    }

    #[test]
    fn verify_accepts_legacy_token_without_new_fields() {
        // A pre-P1-013 token: claims {sub, role, exp, iat}
        // only, no header kid. It still verifies — the new
        // fields default on decode.
        let now = Utc::now();
        let legacy = serde_json::json!({
            "sub": user_id().to_string(),
            "role": "user",
            "exp": (now + Duration::seconds(ACCESS_TOKEN_TTL_SECS)).timestamp(),
            "iat": now.timestamp(),
        });
        let token = encode(
            &Header::new(Algorithm::HS256),
            &legacy,
            &EncodingKey::from_secret(SECRET.as_bytes()),
        )
        .expect("encode succeeds");
        let claims = verify_access_token(SECRET, &token).expect("legacy verifies");
        assert_eq!(claims.ver, TOKEN_VERSION);
        assert_eq!(claims.iss, TOKEN_ISSUER);
        assert_eq!(claims.aud, TOKEN_AUDIENCE);
        assert_eq!(claims.jti, None);
        assert_eq!(claims.impersonator_id, None);
    }
}
