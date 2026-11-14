//! API key primitives (P1-033, FR-A-06).
//!
//! An API key is a long-lived bearer credential for machine
//! clients: `pf_live_` + 24 random bytes, base64url-encoded
//! without padding (32 characters, so the full presented string
//! is 40 characters). Only the sha256 of the full string is
//! ever stored (`secret_hash`); the raw secret exists in the
//! creation response and nowhere else — a leaked database
//! reveals no usable key, and the secret is shown exactly once
//! (there is no endpoint that returns it again). Verification
//! is a hash lookup, so it lands with P1-034's extractor; this
//! module owns the shape both sides share.
//!
//! The `prefix` column is the display identifier shown in
//! listings (`GET /v1/api-keys` shows prefix + last4 only):
//! `pf_live_` + the first 4 characters of the secret part.
//! Together with the last 4 characters it lets the user tell
//! keys apart without ever exposing the middle of the secret.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rand_core::{OsRng, RngCore};

/// The literal prefix of every presented API key.
pub const API_KEY_PREFIX: &str = "pf_live_";

/// Raw-secret length in bytes (24 → 32 base64url characters).
pub const API_KEY_SECRET_BYTES: usize = 24;

/// Max live (unrevoked) keys per user (P1-033 spec).
pub const API_KEY_MAX_PER_USER: u32 = 10;

/// Throttle for `last_used_at` writes (P1-034): the
/// extractor re-stamps a key's `last_used_at` at most once
/// per 300 s. The value is a freshness signal for security
/// review ("when did this key last authenticate"), not an
/// audit log — minute precision is enough, and a write per
/// request would double the DB traffic of every key-authenticated
/// call for no benefit. A first use (NULL) always stamps.
pub const API_KEY_LAST_USED_THROTTLE_SECS: i64 = 300;

/// Default overlap window for key rotation (P1-035): after
/// `POST /v1/api-keys/{id}/rotate` the old secret stays
/// valid for this many seconds (24 h). The operator tunes it
/// via `PF_API_KEY_ROTATION_OVERLAP_SECS` (`Settings::
/// api_key_rotation_overlap_secs`); the value is a freshness
/// signal for rotation, not a security boundary — the old
/// secret is already replaced, the window only buys clients
/// time to deploy the new one.
pub const API_KEY_ROTATION_OVERLAP_DEFAULT_SECS: u64 = 86_400;

/// The scopes a key may carry (P1-033 spec). `jobs:write`
/// implies nothing — each scope is checked independently by
/// the P1-034 extractor.
pub const KNOWN_SCOPES: &[&str] = &["jobs:write", "jobs:read", "webhooks:manage"];

/// Generate a fresh presented secret: `pf_live_` + 32
/// base64url characters. OS RNG; every call differs.
pub fn generate_secret() -> String {
    let mut bytes = [0u8; API_KEY_SECRET_BYTES];
    OsRng.fill_bytes(&mut bytes);
    format!("{API_KEY_PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes))
}

/// The stored form of a presented secret: lowercase sha256
/// hex of the FULL presented string (prefix included — the
/// extractor hashes exactly what the client presented).
/// Deterministic, so verification is a lookup by this value.
pub fn hash_secret(secret: &str) -> String {
    crate::checksum::sha256_hex(secret.as_bytes())
}

/// The display prefix for a presented secret: `pf_live_` +
/// the first 4 characters of the secret part. This is what
/// the `prefix` column stores and what listings show.
pub fn display_prefix(secret: &str) -> String {
    let part = secret.strip_prefix(API_KEY_PREFIX).unwrap_or(secret);
    let head: String = part.chars().take(4).collect();
    format!("{API_KEY_PREFIX}{head}")
}

/// The last 4 characters of the secret part — the second half
/// of the listing identifier (prefix + last4).
pub fn last4(secret: &str) -> String {
    let part = secret.strip_prefix(API_KEY_PREFIX).unwrap_or(secret);
    let skip = part.chars().count().saturating_sub(4);
    part.chars().skip(skip).collect()
}

/// Is this one of the scopes a key may carry?
pub fn is_known_scope(scope: &str) -> bool {
    KNOWN_SCOPES.contains(&scope)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_secrets_have_the_spec_shape() {
        for _ in 0..100 {
            let secret = generate_secret();
            assert!(secret.starts_with(API_KEY_PREFIX), "{secret}");
            let part = &secret[API_KEY_PREFIX.len()..];
            assert_eq!(part.len(), 32, "{secret}");
            assert!(
                part.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
                "base64url has no +/=/: {secret}"
            );
        }
        assert_ne!(generate_secret(), generate_secret());
    }

    #[test]
    fn hash_is_deterministic_and_hides_the_secret() {
        let secret = generate_secret();
        let hash = hash_secret(&secret);
        assert_eq!(hash, hash_secret(&secret));
        assert_eq!(hash.len(), 64);
        assert!(
            !hash.contains(&secret[API_KEY_PREFIX.len()..API_KEY_PREFIX.len() + 8]),
            "the hash must not embed the secret"
        );
        assert_ne!(hash, hash_secret(&generate_secret()));
    }

    #[test]
    fn prefix_and_last4_derive_from_the_secret() {
        let secret = generate_secret();
        let part = &secret[API_KEY_PREFIX.len()..];
        assert_eq!(
            display_prefix(&secret),
            format!("{}{}", API_KEY_PREFIX, &part[..4])
        );
        assert_eq!(last4(&secret), &part[part.len() - 4..]);
    }

    #[test]
    fn known_scopes_are_exactly_the_spec_three() {
        assert_eq!(KNOWN_SCOPES, ["jobs:write", "jobs:read", "webhooks:manage"]);
        for scope in KNOWN_SCOPES {
            assert!(is_known_scope(scope));
        }
        assert!(!is_known_scope("jobs:delete"));
        assert!(!is_known_scope("admin"));
        assert!(!is_known_scope(""));
    }
}
