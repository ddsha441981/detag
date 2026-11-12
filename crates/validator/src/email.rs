//! Email validation (P0-101).
//!
//! Hand-rolled shape check — no regex crate in the tree, and the
//! rule is small enough to read. This is a *shape* check, not
//! deliverability: `alice@example.com` passes whether or not the
//! mailbox exists. Codes: `required` (empty), `too_long`
//! (> 254 bytes total, or local part > 64), `invalid_email`
//! (everything else).

use detag_core::error::FieldError;

/// Validate an email address for the named field.
pub fn validate_email(field: &str, value: &str) -> Result<(), FieldError> {
    if value.trim().is_empty() {
        return Err(FieldError::new(field, "required", "is required"));
    }
    if value.len() > 254 {
        return Err(FieldError::new(
            field,
            "too_long",
            "must be at most 254 characters",
        ));
    }
    let invalid = || FieldError::new(field, "invalid_email", "must be a valid email address");
    let (local, domain) = value.split_once('@').ok_or_else(invalid)?;
    if value.matches('@').count() != 1 {
        return Err(invalid());
    }
    if local.is_empty() || local.len() > 64 {
        return Err(invalid());
    }
    if local.starts_with('.') || local.ends_with('.') || local.contains("..") {
        return Err(invalid());
    }
    if !local
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "._%+-".contains(c))
    {
        return Err(invalid());
    }
    let labels: Vec<&str> = domain.split('.').collect();
    if labels.len() < 2 {
        return Err(invalid());
    }
    for label in &labels {
        if label.is_empty()
            || label.len() > 63
            || label.starts_with('-')
            || label.ends_with('-')
            || !label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return Err(invalid());
        }
    }
    let tld = labels[labels.len() - 1];
    if tld.len() < 2 || !tld.chars().all(|c| c.is_ascii_alphabetic()) {
        return Err(invalid());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_ordinary_addresses() {
        for good in [
            "alice@example.com",
            "bob.smith+tag@sub.example.co.uk",
            "a@b.co",
            "user_name-1@example-domain.org",
        ] {
            assert!(validate_email("email", good).is_ok(), "{good}");
        }
    }

    #[test]
    fn rejects_bad_shapes() {
        for bad in [
            "no-at-sign",
            "two@@ats.com",
            "@example.com",
            "alice@",
            "alice@example",
            "alice@.example.com",
            "alice@example..com",
            ".alice@example.com",
            "alice.@example.com",
            "al..ice@example.com",
            "alice@exam ple.com",
            "alice@example.c",
            "alice@example.1",
            "alice@-example.com",
            "alice@example-.com",
        ] {
            let err = validate_email("email", bad).expect_err(bad);
            assert_eq!(err.field, "email");
            assert_eq!(err.code, "invalid_email", "{bad}");
        }
    }

    #[test]
    fn empty_is_required_not_invalid() {
        for empty in ["", "   "] {
            let err = validate_email("email", empty).expect_err(empty);
            assert_eq!(err.code, "required");
        }
    }

    #[test]
    fn too_long_is_its_own_code() {
        let long = format!("{}@example.com", "a".repeat(250));
        let err = validate_email("email", &long).expect_err("too long");
        assert_eq!(err.code, "too_long");
        let long_local = format!("{}@example.com", "a".repeat(65));
        let err = validate_email("email", &long_local).expect_err("local too long");
        assert_eq!(err.code, "invalid_email");
    }
}
