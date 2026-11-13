//! detag-core::tokens — single-use token primitives (P1-002).
//!
//! Email-verification tokens (and, later, password-reset
//! tokens — same shape, different table): 32 random bytes,
//! base64url-encoded without padding (43 characters, safe in a
//! URL query string with no escaping). Only the sha256 of a
//! token is ever stored (`token_hash`); the raw token exists in
//! the outbox payload (so the mail drain can build the link)
//! and in the email itself, and nowhere else — a leaked
//! database reveals no usable token. Verification is a
//! constant-time hash compare ([`hash_matches`]); the lookup is
//! by hash, so a wrong token costs one indexed lookup, not a
//! scan.
//!
//! Lifetimes are spec-fixed constants, not config: verification
//! tokens live 24h (P1-002), password-reset tokens 30 min
//! (P1-003's ticket, FR-A-03).

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rand_core::{OsRng, RngCore};

/// Verification-token lifetime: 24 hours (P1-002 spec).
pub const VERIFICATION_TOKEN_TTL_SECS: i64 = 24 * 60 * 60;

/// Raw-token length in bytes (32 → 43 base64url characters).
pub const TOKEN_BYTES: usize = 32;

/// Generate a fresh raw token. OS RNG; every call differs.
pub fn generate_token() -> String {
    let mut bytes = [0u8; TOKEN_BYTES];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// The stored form of a raw token: lowercase sha256 hex.
/// Deterministic — the same raw token always hashes the same
/// way, so verification is a lookup by this value.
pub fn hash_token(raw: &str) -> String {
    crate::checksum::sha256_hex(raw.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_tokens_are_url_safe_and_unique() {
        for _ in 0..100 {
            let token = generate_token();
            assert_eq!(token.len(), 43, "{token}");
            assert!(
                token
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
                "base64url has no +/=/: {token}"
            );
        }
        assert_ne!(generate_token(), generate_token());
    }

    #[test]
    fn hash_is_deterministic_and_hides_the_token() {
        let token = generate_token();
        let hash = hash_token(&token);
        assert_eq!(hash, hash_token(&token));
        assert_eq!(hash.len(), 64);
        assert!(
            !hash.contains(&token[..8]),
            "the hash must not embed the token"
        );
        assert_ne!(hash, hash_token(&generate_token()));
    }

    #[test]
    fn compare_is_constant_time() {
        // The compare itself: equal hashes match, any difference
        // does not — including a one-character token difference
        // (which hashes to a wholly different value).
        let token = generate_token();
        let hash = hash_token(&token);
        assert!(crate::checksum::hash_matches(&hash, &hash_token(&token)));
        let mut other = token.clone();
        other.replace_range(0..1, "A");
        if other == token {
            other.replace_range(0..1, "B");
        }
        assert!(!crate::checksum::hash_matches(&hash, &hash_token(&other)));
    }

    #[test]
    fn ttl_is_24_hours() {
        assert_eq!(VERIFICATION_TOKEN_TTL_SECS, 86_400);
    }
}
