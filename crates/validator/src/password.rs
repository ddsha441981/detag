//! Password validation (P1-001).
//!
//! One rule, on purpose: a minimum length of 12 characters
//! (NIST SP 800-63B — length is the control; no complexity
//! rules, no composition checks, no blocklists at this layer).
//! The minimum is a constant, not config: it is a product
//! policy, and the owner changes it by changing this file.
//! Codes: `required` (empty), `too_short` (under 12 chars).
//! Passwords are never trimmed — spaces are valid password
//! characters, and trimming would silently change what the
//! user typed. Length is counted in characters, not bytes, so
//! a 12-character passphrase with multi-byte characters is 12.

use detag_core::error::FieldError;

/// Minimum password length in characters.
pub const MIN_PASSWORD_LEN: usize = 12;

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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_twelve_or_more() {
        for good in [
            "123456789012",
            "correct horse battery staple",
            "pässwörd-1234",
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
    fn spaces_are_password_characters() {
        // Twelve spaces is a (terrible) 12-character password —
        // the rule is length, and trimming would change the
        // credential the user chose.
        assert!(validate_password("password", "            ").is_ok());
        // But eleven spaces plus nothing is short.
        assert!(validate_password("password", "           ").is_err());
    }

    #[test]
    fn length_counts_characters_not_bytes() {
        // 11 multi-byte chars + 1 ascii = 12 characters (but 23
        // bytes) — must pass.
        assert!(validate_password("password", "äääääääääää1").is_ok());
    }
}
