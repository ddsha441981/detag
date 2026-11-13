//! Admin-action reason validation (P1-008).
//!
//! A suspend reason is audit text, not an identifier: it is
//! trimmed, must be 1–500 characters after trimming, and must
//! not contain control characters. The suspend endpoint
//! REQUIRES a reason (an admin action without one is not
//! auditable) — the handler rejects a missing/blank reason
//! with 422 `required` before the service runs.

use detag_core::error::FieldError;

/// The longest a suspend reason may be, in characters.
pub const MAX_REASON_LEN: usize = 500;

/// Validate a suspend reason: trim it, then it must be
/// non-empty and at most [`MAX_REASON_LEN`] characters, with
/// no control characters. Returns the TRIMMED value on
/// success (the caller stores the trimmed form — leading /
/// trailing spaces are never stored).
pub fn validate_reason(field: &str, value: &str) -> Result<String, FieldError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(FieldError::new(field, "required", "is required"));
    }
    if trimmed.chars().count() > MAX_REASON_LEN {
        return Err(FieldError::new(
            field,
            "too_long",
            "must be at most 500 characters",
        ));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(FieldError::new(
            field,
            "invalid_reason",
            "must not contain control characters",
        ));
    }
    Ok(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_a_plain_reason_and_trims() {
        assert_eq!(
            validate_reason("reason", "  Spam account  ").unwrap(),
            "Spam account"
        );
    }

    #[test]
    fn accepts_exactly_500_characters() {
        let reason = "a".repeat(500);
        assert_eq!(validate_reason("reason", &reason).unwrap(), reason);
    }

    #[test]
    fn rejects_empty_and_blank() {
        assert_eq!(validate_reason("reason", "").unwrap_err().code, "required");
        assert_eq!(
            validate_reason("reason", "   ").unwrap_err().code,
            "required"
        );
    }

    #[test]
    fn rejects_over_500_characters() {
        let err = validate_reason("reason", &"a".repeat(501)).unwrap_err();
        assert_eq!(err.code, "too_long");
    }

    #[test]
    fn rejects_control_characters() {
        let err = validate_reason("reason", "Spam\taccount").unwrap_err();
        assert_eq!(err.code, "invalid_reason");
    }

    #[test]
    fn counts_characters_not_bytes() {
        // 500 multi-byte characters is fine (500 chars, 1000 bytes).
        let reason = "é".repeat(500);
        assert_eq!(validate_reason("reason", &reason).unwrap(), reason);
        let err = validate_reason("reason", &"é".repeat(501)).unwrap_err();
        assert_eq!(err.code, "too_long");
    }
}
