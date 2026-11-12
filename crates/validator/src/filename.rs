//! Filename sanitization (P0-101).
//!
//! Upload filenames come from the client and end up as storage
//! keys — a `../` or a NUL byte in one is a traversal bug, not a
//! cosmetic issue. `sanitize_filename` cleans instead of merely
//! rejecting: path separators, NUL and control characters become
//! `_`, leading dots are stripped (so `.`, `..` and hidden files
//! cannot smuggle through), trailing dots/spaces are stripped.
//! Codes: `required` (empty after trim), `invalid_filename`
//! (nothing usable left after cleaning), `filename_too_long`
//! (> 255 bytes — a filesystem component limit; we error rather
//! than silently truncate, because truncation can collide names).

use detag_core::error::FieldError;

/// Maximum filename length in bytes (a filesystem component limit).
pub const MAX_FILENAME_BYTES: usize = 255;

/// Clean an upload filename for the named field. Returns the cleaned
/// name (never the raw input — callers must store what this returns).
pub fn sanitize_filename(field: &str, value: &str) -> Result<String, FieldError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(FieldError::new(field, "required", "is required"));
    }
    // Strip leading path characters first (`.`, `/`, `\`): a
    // traversal prefix must disappear, not become an underscore —
    // `../secret.png` is `secret.png`, and `/` alone leaves nothing.
    // (Replacing separators first would turn `../x` into `.._x`.)
    let stripped = trimmed.trim_start_matches(['.', '/', '\\']);
    let mut cleaned: String = stripped
        .chars()
        .map(|c| {
            if c == '/' || c == '\\' || c == '\0' || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    // Strip trailing dots / spaces (Windows-hostile, and a trailing
    // dot is never meaningful).
    while cleaned.ends_with('.') || cleaned.ends_with(' ') {
        cleaned.pop();
    }
    if cleaned.is_empty() {
        return Err(FieldError::new(
            field,
            "invalid_filename",
            "must contain a usable name",
        ));
    }
    if cleaned.len() > MAX_FILENAME_BYTES {
        return Err(FieldError::new(
            field,
            "filename_too_long",
            "must be at most 255 bytes",
        ));
    }
    Ok(cleaned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_clean_names_through() {
        for good in [
            "photo.png",
            "my file (1).jpg",
            "a-b_c.d-e.webp",
            "ünicode-ß.mp4",
        ] {
            assert_eq!(sanitize_filename("filename", good).unwrap(), good, "{good}");
        }
    }

    #[test]
    fn cleans_traversal_and_separators() {
        assert_eq!(
            sanitize_filename("filename", "../secret.png").unwrap(),
            "secret.png"
        );
        assert_eq!(
            sanitize_filename("filename", "..\\..\\win.png").unwrap(),
            "win.png"
        );
        assert_eq!(
            sanitize_filename("filename", "a/b\\c.png").unwrap(),
            "a_b_c.png"
        );
        assert_eq!(
            sanitize_filename("filename", ".hidden.png").unwrap(),
            "hidden.png"
        );
        assert_eq!(
            sanitize_filename("filename", "...dots.png").unwrap(),
            "dots.png"
        );
        assert_eq!(sanitize_filename("filename", "name.").unwrap(), "name");
        assert_eq!(
            sanitize_filename("filename", "  spaced.png  ").unwrap(),
            "spaced.png"
        );
        assert_eq!(
            sanitize_filename("filename", "nul\0byte.png").unwrap(),
            "nul_byte.png"
        );
    }

    #[test]
    fn rejects_empty_and_unusable() {
        for bad in ["", "   ", ".", "..", "...", "/", "\\"] {
            let err = sanitize_filename("filename", bad).expect_err(bad);
            assert!(
                err.code == "required" || err.code == "invalid_filename",
                "{bad}: {}",
                err.code
            );
        }
    }

    #[test]
    fn too_long_is_its_own_code() {
        let long = format!("{}.png", "a".repeat(252));
        let err = sanitize_filename("filename", &long).expect_err("too long");
        assert_eq!(err.code, "filename_too_long");
    }
}
