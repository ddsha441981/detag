//! Display-name validation (P1-005).
//!
//! A display name is profile text, not an identifier: it is
//! trimmed, must be 1–100 characters after trimming, and must
//! not contain control characters. It is never required — the
//! caller decides whether the field was sent at all (PATCH
//! semantics); this rule only judges a value that IS present.

use detag_core::error::FieldError;

/// The longest a display name may be, in characters.
pub const MAX_DISPLAY_NAME_LEN: usize = 100;

/// Validate a display name that was sent: trim it, then it
/// must be non-empty and at most [`MAX_DISPLAY_NAME_LEN`]
/// characters, with no control characters. Returns the
/// TRIMMED value on success (the caller stores the trimmed
/// form — leading/trailing spaces are never stored).
pub fn validate_display_name(field: &str, value: &str) -> Result<String, FieldError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(FieldError::new(field, "required", "is required"));
    }
    if trimmed.chars().count() > MAX_DISPLAY_NAME_LEN {
        return Err(FieldError::new(
            field,
            "too_long",
            "must be at most 100 characters",
        ));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(FieldError::new(
            field,
            "invalid_display_name",
            "must not contain control characters",
        ));
    }
    Ok(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_a_plain_name_and_trims() {
        assert_eq!(
            validate_display_name("display_name", "  Deendayal  ").unwrap(),
            "Deendayal"
        );
    }

    #[test]
    fn accepts_exactly_100_characters() {
        let name = "a".repeat(100);
        assert_eq!(validate_display_name("display_name", &name).unwrap(), name);
    }

    #[test]
    fn rejects_empty_and_blank() {
        assert_eq!(
            validate_display_name("display_name", "").unwrap_err().code,
            "required"
        );
        assert_eq!(
            validate_display_name("display_name", "   ")
                .unwrap_err()
                .code,
            "required"
        );
    }

    #[test]
    fn rejects_over_100_characters() {
        let err = validate_display_name("display_name", &"a".repeat(101)).unwrap_err();
        assert_eq!(err.code, "too_long");
    }

    #[test]
    fn rejects_control_characters() {
        let err = validate_display_name("display_name", "Deen\tdayal").unwrap_err();
        assert_eq!(err.code, "invalid_display_name");
    }

    #[test]
    fn counts_characters_not_bytes() {
        // 100 multi-byte characters is fine (100 chars, 200 bytes).
        let name = "é".repeat(100);
        assert_eq!(validate_display_name("display_name", &name).unwrap(), name);
        let err = validate_display_name("display_name", &"é".repeat(101)).unwrap_err();
        assert_eq!(err.code, "too_long");
    }
}
