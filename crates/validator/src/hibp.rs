//! Have I Been Pwned k-anonymity helpers (P1-026).
//!
//! The range API (`GET https://api.pwnedpasswords.com/range/{prefix}`)
//! never receives the password or its full hash: the client sends
//! only the first 5 hex characters of the password's SHA-1, and the
//! server returns every suffix (the remaining 35 hex chars) that
//! shares the prefix, with a breach count each. The client checks
//! locally whether its suffix is in the list.
//!
//! Privacy note (this is why the check is behind the
//! `PF_PASSWORD_PWNED_CHECK` flag, default off): the 5-char prefix
//! narrows the password to ~2^20 candidates and tells a third party
//! (Cloudflare, which fronts the API) that a registration or reset
//! is happening — the full hash and the password itself never leave
//! the machine, but the check is still an outbound call per
//! registration/reset, so it is opt-in.
//!
//! This module is the pure half: SHA-1 splitting + range-response
//! parsing, no HTTP. The transport lives in
//! `apps/api/src/services/pwned.rs` (reqwest, 5 s timeout,
//! fail-open on any error).

use sha1::{Digest, Sha1};

/// Split a password's SHA-1 (uppercase hex) into the k-anonymity
/// prefix (first 5 chars, sent to the API) and suffix (the rest,
/// matched locally).
pub fn hibp_sha1_prefix_suffix(password: &str) -> (String, String) {
    let mut hasher = Sha1::new();
    hasher.update(password.as_bytes());
    let hex = hex_encode_upper(&hasher.finalize());
    (hex[..5].to_string(), hex[5..].to_string())
}

/// True when the range response contains the suffix: one
/// `SUFFIX:count` line per match, suffix compared case-insensitively
/// (the API returns uppercase; be liberal). Malformed lines are
/// ignored, never an error — a corrupt body must not reject a
/// password (the caller fails open on transport errors; a body
/// that parses to nothing is "not found").
pub fn hibp_range_contains(body: &str, suffix: &str) -> bool {
    body.lines().any(|line| {
        let Some((candidate, _count)) = line.split_once(':') else {
            return false;
        };
        candidate.trim().eq_ignore_ascii_case(suffix.trim())
    })
}

fn hex_encode_upper(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha1_split_matches_the_known_vector() {
        // "password" SHA-1 is the canonical HIBP example:
        // 5BAA61E4C9B93F3F0682250B6CF8331B7EE68FD8.
        let (prefix, suffix) = hibp_sha1_prefix_suffix("password");
        assert_eq!(prefix, "5BAA6");
        assert_eq!(suffix, "1E4C9B93F3F0682250B6CF8331B7EE68FD8");
        assert_eq!(prefix.len(), 5);
        assert_eq!(suffix.len(), 35);
    }

    #[test]
    fn range_contains_finds_the_suffix_case_insensitively() {
        let body = "0018A45C4D1DEF81644B54AB7F969B88D65:1\n\
                    1E4C9B93F3F0682250B6CF8331B7EE68FD8:9543776\n\
                    01330C689E5DC05884B7E4379AF1E0A7C:2";
        assert!(hibp_range_contains(
            body,
            "1E4C9B93F3F0682250B6CF8331B7EE68FD8"
        ));
        // Lowercase query, uppercase body — the comparison is
        // case-insensitive either way.
        assert!(hibp_range_contains(
            body,
            "1e4c9b93f3f0682250b6cf8331b7ee68fd8"
        ));
        assert!(!hibp_range_contains(
            body,
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
        ));
    }

    #[test]
    fn range_contains_ignores_malformed_lines() {
        let body = "garbage line with no colon\n:3\n1E4C9B93F3F0682250B6CF8331B7EE68FD8:9543776\n";
        assert!(hibp_range_contains(
            body,
            "1E4C9B93F3F0682250B6CF8331B7EE68FD8"
        ));
        assert!(!hibp_range_contains(
            "garbage\nmore garbage",
            "1E4C9B93F3F0682250B6CF8331B7EE68FD8"
        ));
        assert!(!hibp_range_contains(
            "",
            "1E4C9B93F3F0682250B6CF8331B7EE68FD8"
        ));
    }
}
