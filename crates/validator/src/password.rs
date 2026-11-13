//! Password validation (P1-001, extended P1-026).
//!
//! Three rules, in order (the first failure is the error — the
//! most specific code wins):
//!
//! 1. Length: a minimum of 12 characters (NIST SP 800-63B —
//!    length is the control; no complexity rules, no composition
//!    checks). The minimum is a constant, not config: it is a
//!    product policy, and the owner changes it by changing this
//!    file. Codes: `required` (empty), `too_short` (under 12).
//! 2. Blocklist: the whole password must not be one of the 10,000
//!    most commonly used passwords (code `common_password`).
//!    The list is zxcvbn's embedded ranked passwords dictionary
//!    (30,000 entries, RockYou lineage — the same lineage as the
//!    SecLists top-10k list the ticket names). No external list
//!    is downloaded: the SecLists file would have to come from
//!    raw.githubusercontent.com, and that download needs the
//!    owner's approval — so the blocklist reads the dictionary
//!    the `zxcvbn` crate already ships, via its public match
//!    sequence (`is_blocklisted`). If the owner approves the
//!    SecLists import later, the swap is one function + a data
//!    file; the error code and the callers do not change.
//! 3. Strength: zxcvbn score >= [`MIN_PASSWORD_SCORE`] (3 — the
//!    crate's own "too weak" line: "Any score less than 3 should
//!    be considered too weak"). Code `too_weak`. The score is the
//!    backstop for everything the blocklist misses: a 12-digit
//!    sequence (`123456789012` is not in the dictionary at all)
//!    scores 1, a 12-letter keyboard walk scores 0.
//!
//! Passwords are never trimmed — spaces are valid password
//! characters, and trimming would silently change what the
//! user typed. Length is counted in characters, not bytes, so
//! a 12-character passphrase with multi-byte characters is 12.

use detag_core::error::FieldError;
use zxcvbn::{feedback::Warning, matching::patterns::MatchPattern, zxcvbn};

/// Minimum password length in characters.
pub const MIN_PASSWORD_LEN: usize = 12;

/// Minimum zxcvbn score (0–4). 3 is the crate's own line:
/// "Any score less than 3 should be considered too weak".
pub const MIN_PASSWORD_SCORE: u8 = 3;

/// Blocklist cutoff: the most common this many passwords, by
/// rank in zxcvbn's passwords dictionary (1 = most common).
pub const BLOCKLIST_MAX_RANK: usize = 10_000;

/// Validate a password for the named field.
pub fn validate_password(field: &str, value: &str) -> Result<(), FieldError> {
    if value.is_empty() {
        return Err(FieldError::new(field, "required", "is required"));
    }
    if value.chars().count() < MIN_PASSWORD_LEN {
        return Err(FieldError::new(
            field,
            "too_short",
            "must be at least 12 characters",
        ));
    }
    if is_blocklisted(value) {
        return Err(FieldError::new(
            field,
            "common_password",
            "is one of the most commonly used passwords",
        ));
    }
    let score = u8::from(zxcvbn(value, &[]).score());
    if score < MIN_PASSWORD_SCORE {
        return Err(FieldError::new(
            field,
            "too_weak",
            "is too easy to guess — use a longer passphrase or more random characters",
        ));
    }
    Ok(())
}

/// True when the whole password is one of the 10,000 most
/// commonly used passwords.
///
/// The check reads zxcvbn's match sequence, not a list file:
/// the password is blocklisted when zxcvbn matches it as a
/// SINGLE dictionary word covering the whole password, the
/// match's rank is <= [`BLOCKLIST_MAX_RANK`], and the feedback
/// warning names a passwords-dictionary match
/// (`ThisIsATop10Password` / `ThisIsATop100Password` /
/// `ThisIsACommonPassword` / `ThisIsSimilarToACommonlyUsedPassword`
/// — those four warnings are produced only for the passwords
/// dictionary, so the warning identifies the dictionary without
/// naming zxcvbn's private `DictionaryType`). A l33t or reversed
/// variant of a top-10k word is blocklisted too (its warning is
/// `ThisIsSimilarToACommonlyUsedPassword`); a longer password
/// that merely CONTAINS a common word is not — the sequence
/// then has more than one match, and the score gate judges it.
///
/// The feedback is only set when the score is <= 2, so a
/// password that scores >= 3 never reaches this check as
/// blocklisted — that is fine: a score-3 password is, by the
/// crate's definition, not trivially guessable, and the score
/// gate is the backstop for the whole rule.
pub fn is_blocklisted(password: &str) -> bool {
    let entropy = zxcvbn(password, &[]);
    let Some(warning) = entropy.feedback().and_then(|f| f.warning()) else {
        return false;
    };
    if !matches!(
        warning,
        Warning::ThisIsATop10Password
            | Warning::ThisIsATop100Password
            | Warning::ThisIsACommonPassword
            | Warning::ThisIsSimilarToACommonlyUsedPassword
    ) {
        return false;
    }
    let sequence = entropy.sequence();
    if sequence.len() != 1 {
        return false;
    }
    let sole = &sequence[0];
    let MatchPattern::Dictionary(pattern) = &sole.pattern else {
        return false;
    };
    pattern.rank <= BLOCKLIST_MAX_RANK && sole.token == password
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_twelve_or_more() {
        for good in [
            "correct horse battery staple",
            "pässwörd-1234",
            "Tr7$kQ9!wX2z",
            // Contains a top-20 word ("letmein", rank 16) plus a
            // year and symbols — zxcvbn scores it 3, the crate's
            // own pass line. The blocklist does not fire: the
            // sequence has two matches, so the whole password is
            // not one dictionary word.
            "letmein2024!!",
        ] {
            assert!(validate_password("password", good).is_ok(), "{good}");
        }
    }

    #[test]
    fn rejects_short() {
        for bad in ["", "short", "12345678901"] {
            let err = validate_password("password", bad).expect_err(bad);
            assert_eq!(
                err.code,
                if bad.is_empty() {
                    "required"
                } else {
                    "too_short"
                },
                "{bad}"
            );
        }
    }

    #[test]
    fn spaces_are_password_characters_but_twelve_spaces_is_too_weak() {
        // Twelve spaces is 12 characters, so it passes the length
        // rule — but zxcvbn scores a single repeated character 0,
        // so the strength rule rejects it. (P1-001's test asserted
        // this was accepted; P1-026's strength rule changes that,
        // deliberately: a 12-space password is terrible.)
        let err = validate_password("password", "            ").expect_err("12 spaces");
        assert_eq!(err.code, "too_weak");
        // But eleven spaces plus nothing is short.
        let err = validate_password("password", "           ").expect_err("11 spaces");
        assert_eq!(err.code, "too_short");
    }

    #[test]
    fn length_counts_characters_not_bytes() {
        // "pässwörd-1234" is 12 characters but 13 bytes (ä is two
        // bytes) — it must pass the length rule, and it scores 4.
        // (An 11-multi-byte-char + 1-ascii password like
        // "äääääääääää1" is also 12 characters, but zxcvbn scores
        // a single repeated character 1 — the strength rule, not
        // the length rule, rejects it.)
        assert!(validate_password("password", "pässwörd-1234").is_ok());
    }

    #[test]
    fn rejects_top_10k_passwords() {
        // "qwerty123456" IS one dictionary word (rank 1850, well
        // inside the top 10k) — the blocklist fires before the
        // strength rule, so the code is common_password, not
        // too_weak.
        let err = validate_password("password", "qwerty123456").expect_err("qwerty123456"); // gitleaks:allow — "qwerty123456" is a public top-10k test password, not a credential
        assert_eq!(err.code, "common_password");
        // "password" is rank 2 — but it fails the length rule
        // first (8 chars), so the code is too_short, not
        // common_password: the most specific rule wins, in order.
        let err = validate_password("password", "password").expect_err("password");
        assert_eq!(err.code, "too_short");
        assert!(is_blocklisted("password"));
    }

    #[test]
    fn blocklist_stops_at_rank_10000() {
        // "qazxswedcvfr" is a dictionary word at rank 10023 —
        // just outside the top 10k — so the blocklist does NOT
        // fire. It is still rejected: a 12-character keyboard
        // walk scores 0, so the code is too_weak. (This pins the
        // cutoff: the blocklist is top-10k, the score gate is the
        // backstop for ranks 10001+.)
        assert!(!is_blocklisted("qazxswedcvfr"));
        let err = validate_password("password", "qazxswedcvfr").expect_err("qazxswedcvfr");
        assert_eq!(err.code, "too_weak");
    }

    #[test]
    fn rejects_weak_passwords() {
        // Each of these is 12+ characters (so length passes) and
        // not one top-10k dictionary word (so the blocklist does
        // not fire) — but zxcvbn scores them under 3.
        for (bad, why) in [
            ("123456789012", "a digit sequence, score 1"),
            ("abcdefghijkl", "a letter sequence, score 0"),
            ("Password1234", "a top-2 word + a sequence, score 1"),
            ("dragonballz99", "a dictionary word + repeat, score 1"),
        ] {
            let err = validate_password("password", bad).expect_err(bad);
            assert_eq!(err.code, "too_weak", "{bad}: {why}");
        }
    }

    #[test]
    fn blocklist_is_whole_password_only() {
        // A longer password that merely CONTAINS a common word is
        // not blocklisted — the sequence has more than one match.
        // ("Sunshine-42-rain" contains "sunshine", rank 46, but
        // scores 3 and is accepted.)
        assert!(!is_blocklisted("Sunshine-42-rain"));
        assert!(validate_password("password", "Sunshine-42-rain").is_ok());
    }
}
