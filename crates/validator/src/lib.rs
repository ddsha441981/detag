//! Input validation rules (P0-101).
//!
//! Pure functions, no I/O, no network: every rule takes the field
//! name (so one rule serves many request fields) and returns a
//! [`FieldError`] on failure. The api consumes them through the
//! [`Validate`] trait + the `ValidatedJson` extractor (see
//! `apps/api/src/routes/extractors.rs`), which renders failures as
//! a 422 problem+json body with `field_errors`.
//!
//! Rules:
//! - [`validate_email`] — RFC-ish email shape (hand-rolled, no regex
//!   crate in the tree).
//! - [`validate_display_name`] — a profile display name:
//!   trimmed, 1–100 characters, no control characters (P1-005).
//! - [`validate_reason`] — an admin-action reason: trimmed,
//!   1–500 characters, no control characters (P1-008).
//! - [`sanitize_filename`] — cleans an upload filename (separators,
//!   control chars, leading dots) or rejects it when nothing usable
//!   remains.
//! - [`validate_password`] — minimum 12 characters (NIST
//!   SP 800-63B); never trimmed, counted in characters.
//! - [`validate_public_url`] — http(s) URL whose host is public:
//!   no localhost, no private/loopback/link-local IP literal, no
//!   single-label hostname. DNS names are NOT resolved here — a
//!   hostname that resolves to a private IP passes this check; the
//!   fetch layer must re-validate after resolution (SSRF
//!   defense-in-depth, Phase 3).

///   (P1-005).
pub mod display_name;
pub mod email;
pub mod filename;
pub mod password;
pub mod public_url;
pub mod reason;

pub use display_name::{validate_display_name, MAX_DISPLAY_NAME_LEN};
pub use email::validate_email;
pub use filename::sanitize_filename;
pub use password::{validate_password, MIN_PASSWORD_LEN};
pub use public_url::validate_public_url;
pub use reason::{validate_reason, MAX_REASON_LEN};

use detag_core::error::{AppError, FieldError, Problem, Profile};

/// The collected failures of one request. Empty = valid.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ValidationErrors(pub Vec<FieldError>);

impl ValidationErrors {
    pub fn new() -> Self {
        Self(Vec::new())
    }

    pub fn push(&mut self, error: FieldError) {
        self.0.push(error);
    }

    pub fn extend(&mut self, errors: impl IntoIterator<Item = FieldError>) {
        self.0.extend(errors);
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Render the 422 problem+json body: `ValidationFailed`
    /// (PF-2002) with `field_errors`. The `detail` stays the
    /// variant's static safe text in production; the field list
    /// carries the per-field reasons (generic messages, no echoed
    /// values — safe in both profiles).
    pub fn into_problem(self, profile: Profile, request_id: Option<&str>) -> Problem {
        AppError::ValidationFailed
            .to_problem(profile, request_id, None)
            .with_field_errors(self.0)
    }
}

/// Implemented by request DTOs. `validate` collects every failure
/// (not just the first) so the client can fix the whole request in
/// one round trip.
pub trait Validate {
    fn validate(&self) -> Result<(), ValidationErrors>;
}
