//! detag-core::error — error taxonomy (P0-017, NFR-52).
//!
//! `AppError` is the single error type the api and worker map to HTTP
//! responses. Every variant carries a stable `PF-` code in the ranges
//! from the api spec (`docs/03-ARCHITECTURE/03-api-spec.md`):
//!
//! | Range | Area |
//! | --- | --- |
//! | PF-1xxx | auth / session |
//! | PF-2xxx | upload / media (+ request validation, see below) |
//! | PF-3xxx | quota / billing (+ rate limiting) |
//! | PF-4xxx | engine / quality / compliance |
//! | PF-5xxx | system |
//!
//! Codes marked **kit** are defined by the kit (phase docs / api spec /
//! compliance doc) and must never be renumbered. Codes marked
//! **taxonomy** fill gaps the kit leaves (generic 401/403/404/409/400/
//! 429/500 cases every API needs); they use free numbers inside the
//! documented ranges and are recorded here so the phase that first uses
//! one can ratify it without a renumber.
//!
//! Responses are RFC 7807 `application/problem+json`:
//! `{ "type": "https://docs.pureframe.dev/errors/PF-2001",
//!    "title": "unsupported_media_type", "status": 415,
//!    "code": "PF-2001", "detail": "...", "request_id": "01J..." }`
//!
//! **Profiles and leaking.** `Profile::Production` never puts
//! caller-supplied detail into the body: the body gets the variant's
//! static, safe `default_detail`, and the caller detail is emitted to
//! `tracing` only (trace-only). `Profile::Development` shows the caller
//! detail. There is no third option — a new profile that leaks must be
//! a deliberate code change, not a config slip.

use serde::Serialize;
use thiserror::Error;

/// Which error profile a response is rendered in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// Caller detail is shown in the body (local dev only).
    Development,
    /// Caller detail goes to `tracing` only; the body gets the variant's
    /// static safe detail. This is the only profile production may use.
    Production,
}

/// RFC 7807 problem body, as served by the api (`application/problem+json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Problem {
    /// `https://docs.pureframe.dev/errors/<code>` (kit URL, verbatim).
    #[serde(rename = "type")]
    pub problem_type: String,
    /// Snake-case machine title, e.g. `unsupported_media_type`.
    pub title: String,
    /// HTTP status code.
    pub status: u16,
    /// Stable `PF-` code, e.g. `PF-2001`.
    pub code: String,
    /// Human-readable detail. In production this is always the variant's
    /// static safe detail — never caller-supplied internals.
    pub detail: String,
    /// Echoed request id (UUIDv7); omitted when the caller has none yet
    /// (request-id middleware lands in P0-019).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

impl Problem {
    /// Serialize the body the way the api serves it.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("Problem is always serializable")
    }
}

/// The application's error taxonomy. Unit variants on purpose: the
/// caller supplies diagnostic detail at the mapping site, and the
/// profile decides whether the body may show it.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AppError {
    // -- PF-1xxx auth / session -------------------------------------------
    /// Taxonomy: no usable credentials (missing/invalid token). PF-1001, 401.
    #[error("unauthenticated")]
    Unauthenticated,
    /// Taxonomy: authenticated but not allowed (wrong role / not owner).
    /// PF-1002, 403.
    #[error("forbidden")]
    Forbidden,
    /// Kit: refresh-token reuse detected; all sessions revoked. PF-1007, 401.
    #[error("refresh token reuse detected; all sessions revoked")]
    RefreshReuse,
    /// Kit: email verification required before job creation. PF-1009, 403.
    #[error("email not verified")]
    EmailNotVerified,
    /// Kit: JWT is malformed or its signature does not verify. PF-1010, 401.
    #[error("invalid token")]
    InvalidToken,
    /// Kit: JWT is well-formed but past its expiry. PF-1011, 401.
    #[error("expired token")]
    ExpiredToken,

    // -- PF-2xxx upload / media (+ request validation) --------------------
    /// Kit: magic bytes / kind not accepted (api-spec example).
    /// PF-2001, 415.
    #[error("unsupported media type")]
    UnsupportedMediaType,
    /// Taxonomy: request body / params fail validation. PF-2002, 400.
    #[error("validation failed")]
    ValidationFailed,
    /// Kit: body / upload exceeds the route or plan byte cap.
    /// PF-2003, 413.
    #[error("payload too large")]
    PayloadTooLarge,
    /// Kit: part sha256 mismatch on multipart complete. PF-2004, 400.
    #[error("part checksum mismatch")]
    PartChecksumMismatch,
    /// Kit: image dimensions exceed the intake cap (8000x8000 / pixel
    /// budget). PF-2005, 413.
    #[error("dimensions exceeded")]
    DimensionsExceeded,
    /// Kit: zip bomb inside a batch ZIP. PF-2006, 400.
    #[error("archive rejected")]
    ArchiveRejected,
    /// Kit: decode budget breach before full decode (huge image).
    /// PF-2007, 413.
    #[error("decode budget exceeded")]
    DecodeBudgetExceeded,
    /// Kit: mask hints invalid (bounds, max 50). PF-2008, 400.
    #[error("invalid hints")]
    InvalidHints,
    /// Kit: result URL past expires_at. PF-2009, 410.
    #[error("result expired")]
    ResultExpired,
    /// Taxonomy: the addressed resource does not exist. PF-2010, 404.
    #[error("not found")]
    NotFound,
    /// Taxonomy: conflicts with current state (e.g. duplicate email,
    /// idempotency-key payload mismatch). PF-2011, 409.
    #[error("conflict")]
    Conflict,

    // -- PF-3xxx quota / billing (+ rate limiting) ------------------------
    /// Kit: image quota exhausted (pre-check). PF-3001, 429.
    #[error("image quota exceeded")]
    QuotaImagesExceeded,
    /// Kit: video-seconds quota exhausted (pre-check). PF-3002, 429.
    #[error("video seconds quota exceeded")]
    QuotaVideoSecondsExceeded,
    /// Kit: plan does not entitle the requested feature. PF-3003, 403.
    #[error("feature not entitled")]
    FeatureNotEntitled,
    /// Kit: job retry attempts capped at 3. PF-3004, 409.
    #[error("retry attempts exceeded")]
    RetryAttemptsExceeded,
    /// Kit: queue depth over threshold on job create. PF-3005, 429.
    #[error("queue full")]
    QueueFull,
    /// Kit: user storage headroom exhausted. PF-3006, 429.
    #[error("storage headroom exceeded")]
    StorageHeadroomExceeded,
    /// Taxonomy: rate-limit overage (429 + problem+json). PF-3007, 429.
    #[error("rate limited")]
    RateLimited,

    // -- PF-4xxx engine / quality / compliance ----------------------------
    /// Kit: job created without the ownership attestation. PF-4001, 403.
    #[error("attestation missing")]
    AttestationMissing,
    /// Kit: output failed the quality gates (SSIM/PSNR/VMAF). PF-4002, 422.
    #[error("quality gate failed")]
    QualityGateFailed,
    /// Kit: per-job RSS budget breached; fails fast, never OOM-kills.
    /// PF-4005, 500.
    #[error("resource budget exceeded")]
    ResourceBudgetExceeded,
    /// Kit: unmasked pixels not bit-exact after roundtrip, or post-encode
    /// decode-back mismatch — a pipeline bug, never user degradation.
    /// PF-4006, 500.
    #[error("roundtrip mismatch")]
    RoundtripMismatch,
    /// Kit: post-encode ffprobe mismatch (resolution/fps/duration).
    /// PF-4007, 500.
    #[error("output mismatch")]
    OutputMismatch,
    /// Kit: provenance strip denied (flag / legal gate / attestation /
    /// second consent missing). PF-4101, 403.
    #[error("provenance strip denied")]
    ProvenanceStripDenied,

    // -- PF-5xxx system ----------------------------------------------------
    /// Taxonomy: unexpected internal failure. PF-5000, 500.
    #[error("internal error")]
    Internal,
    /// Kit: database unavailable; in-flight work fails fast. PF-5001, 503.
    #[error("database unavailable")]
    DatabaseUnavailable,
    /// Kit: state-machine transition not allowed from the current state.
    /// PF-5002, 409.
    #[error("invalid state transition")]
    InvalidStateTransition,
}

impl AppError {
    /// Stable `PF-` code.
    pub fn code(&self) -> &'static str {
        use AppError::*;
        match self {
            Unauthenticated => "PF-1001",
            Forbidden => "PF-1002",
            RefreshReuse => "PF-1007",
            EmailNotVerified => "PF-1009",
            InvalidToken => "PF-1010",
            ExpiredToken => "PF-1011",
            UnsupportedMediaType => "PF-2001",
            ValidationFailed => "PF-2002",
            PayloadTooLarge => "PF-2003",
            PartChecksumMismatch => "PF-2004",
            DimensionsExceeded => "PF-2005",
            ArchiveRejected => "PF-2006",
            DecodeBudgetExceeded => "PF-2007",
            InvalidHints => "PF-2008",
            ResultExpired => "PF-2009",
            NotFound => "PF-2010",
            Conflict => "PF-2011",
            QuotaImagesExceeded => "PF-3001",
            QuotaVideoSecondsExceeded => "PF-3002",
            FeatureNotEntitled => "PF-3003",
            RetryAttemptsExceeded => "PF-3004",
            QueueFull => "PF-3005",
            StorageHeadroomExceeded => "PF-3006",
            RateLimited => "PF-3007",
            AttestationMissing => "PF-4001",
            QualityGateFailed => "PF-4002",
            ResourceBudgetExceeded => "PF-4005",
            RoundtripMismatch => "PF-4006",
            OutputMismatch => "PF-4007",
            ProvenanceStripDenied => "PF-4101",
            Internal => "PF-5000",
            DatabaseUnavailable => "PF-5001",
            InvalidStateTransition => "PF-5002",
        }
    }

    /// HTTP status for the response carrying this error.
    pub fn status(&self) -> u16 {
        use AppError::*;
        match self {
            Unauthenticated => 401,
            Forbidden => 403,
            RefreshReuse => 401,
            EmailNotVerified => 403,
            InvalidToken => 401,
            ExpiredToken => 401,
            UnsupportedMediaType => 415,
            ValidationFailed => 400,
            PayloadTooLarge => 413,
            PartChecksumMismatch => 400,
            DimensionsExceeded => 413,
            ArchiveRejected => 400,
            DecodeBudgetExceeded => 413,
            InvalidHints => 400,
            ResultExpired => 410,
            NotFound => 404,
            Conflict => 409,
            QuotaImagesExceeded => 429,
            QuotaVideoSecondsExceeded => 429,
            FeatureNotEntitled => 403,
            RetryAttemptsExceeded => 409,
            QueueFull => 429,
            StorageHeadroomExceeded => 429,
            RateLimited => 429,
            AttestationMissing => 403,
            QualityGateFailed => 422,
            ResourceBudgetExceeded => 500,
            RoundtripMismatch => 500,
            OutputMismatch => 500,
            ProvenanceStripDenied => 403,
            Internal => 500,
            DatabaseUnavailable => 503,
            InvalidStateTransition => 409,
        }
    }

    /// Snake-case machine title for the problem body.
    pub fn title(&self) -> &'static str {
        use AppError::*;
        match self {
            Unauthenticated => "unauthenticated",
            Forbidden => "forbidden",
            RefreshReuse => "refresh_token_reused",
            EmailNotVerified => "email_not_verified",
            InvalidToken => "invalid_token",
            ExpiredToken => "expired_token",
            UnsupportedMediaType => "unsupported_media_type",
            ValidationFailed => "validation_failed",
            PayloadTooLarge => "payload_too_large",
            PartChecksumMismatch => "part_checksum_mismatch",
            DimensionsExceeded => "dimensions_exceeded",
            ArchiveRejected => "archive_rejected",
            DecodeBudgetExceeded => "decode_budget_exceeded",
            InvalidHints => "invalid_hints",
            ResultExpired => "result_expired",
            NotFound => "not_found",
            Conflict => "conflict",
            QuotaImagesExceeded => "quota_images_exceeded",
            QuotaVideoSecondsExceeded => "quota_video_seconds_exceeded",
            FeatureNotEntitled => "feature_not_entitled",
            RetryAttemptsExceeded => "retry_attempts_exceeded",
            QueueFull => "queue_full",
            StorageHeadroomExceeded => "storage_headroom_exceeded",
            RateLimited => "rate_limited",
            AttestationMissing => "attestation_missing",
            QualityGateFailed => "quality_gate_failed",
            ResourceBudgetExceeded => "resource_budget_exceeded",
            RoundtripMismatch => "roundtrip_mismatch",
            OutputMismatch => "output_mismatch",
            ProvenanceStripDenied => "provenance_strip_denied",
            Internal => "internal_error",
            DatabaseUnavailable => "database_unavailable",
            InvalidStateTransition => "invalid_state_transition",
        }
    }

    /// Static, safe detail for the body. This is what production shows:
    /// useful to the caller, free of internals (no paths, ids, SQL,
    /// provider messages). Caller-supplied diagnostics are dev-only.
    pub fn default_detail(&self) -> &'static str {
        use AppError::*;
        match self {
            Unauthenticated => "authentication is required for this resource",
            Forbidden => "you do not have permission for this resource",
            RefreshReuse => "refresh token reuse detected; all sessions have been revoked",
            EmailNotVerified => "email verification is required before creating jobs",
            InvalidToken => "the access token is invalid",
            ExpiredToken => "the access token has expired",
            UnsupportedMediaType => "this media type is not accepted",
            ValidationFailed => "the request failed validation",
            PayloadTooLarge => "the request exceeds the size limit",
            PartChecksumMismatch => "an upload part failed its checksum",
            DimensionsExceeded => "the image dimensions exceed the limit",
            ArchiveRejected => "the archive was rejected",
            DecodeBudgetExceeded => "the image is too large to decode",
            InvalidHints => "the mask hints are invalid",
            ResultExpired => "the result has expired",
            NotFound => "the requested resource was not found",
            Conflict => "the request conflicts with the current state",
            QuotaImagesExceeded => "image quota exceeded; upgrade your plan for more",
            QuotaVideoSecondsExceeded => "video-seconds quota exceeded; upgrade your plan for more",
            FeatureNotEntitled => "your plan does not include this feature; upgrade to use it",
            RetryAttemptsExceeded => "the retry limit for this job was reached",
            QueueFull => "the system is busy; retry shortly",
            StorageHeadroomExceeded => "storage headroom exceeded; free space or upgrade your plan",
            RateLimited => "too many requests; retry after the reset window",
            AttestationMissing => "an ownership attestation is required to create this job",
            QualityGateFailed => "the output failed the quality gates; see the job report",
            ResourceBudgetExceeded => "the job exceeded its resource budget and was stopped",
            RoundtripMismatch => "the pipeline produced an inconsistent result; the job failed",
            OutputMismatch => "the encoded output did not match expectations",
            ProvenanceStripDenied => "provenance stripping is not enabled for this request",
            Internal => "an internal error occurred",
            DatabaseUnavailable => "the database is temporarily unavailable",
            InvalidStateTransition => "this transition is not allowed",
        }
    }

    /// Render the RFC 7807 body. `detail` is caller-supplied diagnostic
    /// text: shown in development, replaced by `default_detail` (and sent
    /// to tracing) in production. `request_id` is echoed when known.
    pub fn to_problem(
        &self,
        profile: Profile,
        request_id: Option<&str>,
        detail: Option<&str>,
    ) -> Problem {
        let detail = match profile {
            Profile::Development => detail.unwrap_or_else(|| self.default_detail()).to_string(),
            Profile::Production => {
                if let Some(d) = detail {
                    // Trace-only: internals never reach the body.
                    tracing::error!(
                        code = self.code(),
                        detail = d,
                        "error detail withheld from production response"
                    );
                }
                self.default_detail().to_string()
            }
        };
        Problem {
            problem_type: format!("https://docs.pureframe.dev/errors/{}", self.code()),
            title: self.title().to_string(),
            status: self.status(),
            code: self.code().to_string(),
            detail,
            request_id: request_id.map(str::to_string),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every variant, for the mapping table test.
    fn all_variants() -> Vec<AppError> {
        use AppError::*;
        vec![
            Unauthenticated,
            Forbidden,
            RefreshReuse,
            EmailNotVerified,
            InvalidToken,
            ExpiredToken,
            UnsupportedMediaType,
            ValidationFailed,
            PayloadTooLarge,
            PartChecksumMismatch,
            DimensionsExceeded,
            ArchiveRejected,
            DecodeBudgetExceeded,
            InvalidHints,
            ResultExpired,
            NotFound,
            Conflict,
            QuotaImagesExceeded,
            QuotaVideoSecondsExceeded,
            FeatureNotEntitled,
            RetryAttemptsExceeded,
            QueueFull,
            StorageHeadroomExceeded,
            RateLimited,
            AttestationMissing,
            QualityGateFailed,
            ResourceBudgetExceeded,
            RoundtripMismatch,
            OutputMismatch,
            ProvenanceStripDenied,
            Internal,
            DatabaseUnavailable,
            InvalidStateTransition,
        ]
    }

    /// AC: each variant maps to a status + code + title. The full
    /// expected-value table lives in P0-018; here every variant must at
    /// least produce a well-formed mapping in its documented range.
    #[test]
    fn every_variant_maps_to_status_code_title() {
        let variants = all_variants();
        assert_eq!(variants.len(), 33, "a variant was added without a test");
        for err in &variants {
            let code = err.code();
            let status = err.status();
            let title = err.title();
            assert!(code.starts_with("PF-"), "{code}");
            assert!((100..600).contains(&status), "{code}: {status}");
            assert!(
                !title.is_empty()
                    && title
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit()),
                "{code}: bad title {title}"
            );
            // Code range matches the variant's area.
            let n: u32 = code[3..].parse().expect("numeric code");
            match err {
                AppError::Unauthenticated
                | AppError::Forbidden
                | AppError::RefreshReuse
                | AppError::EmailNotVerified
                | AppError::InvalidToken
                | AppError::ExpiredToken => assert!((1000..2000).contains(&n)),
                AppError::UnsupportedMediaType
                | AppError::ValidationFailed
                | AppError::PayloadTooLarge
                | AppError::PartChecksumMismatch
                | AppError::DimensionsExceeded
                | AppError::ArchiveRejected
                | AppError::DecodeBudgetExceeded
                | AppError::InvalidHints
                | AppError::ResultExpired
                | AppError::NotFound
                | AppError::Conflict => assert!((2000..3000).contains(&n)),
                AppError::QuotaImagesExceeded
                | AppError::QuotaVideoSecondsExceeded
                | AppError::FeatureNotEntitled
                | AppError::RetryAttemptsExceeded
                | AppError::QueueFull
                | AppError::StorageHeadroomExceeded
                | AppError::RateLimited => assert!((3000..4000).contains(&n)),
                AppError::AttestationMissing
                | AppError::QualityGateFailed
                | AppError::ResourceBudgetExceeded
                | AppError::RoundtripMismatch
                | AppError::OutputMismatch
                | AppError::ProvenanceStripDenied => {
                    assert!((4000..5000).contains(&n))
                }
                AppError::Internal
                | AppError::DatabaseUnavailable
                | AppError::InvalidStateTransition => {
                    assert!((5000..6000).contains(&n))
                }
            }
            // The kit-defined codes are exactly the ones the kit names.
            assert!(!err.default_detail().is_empty());
        }
    }

    /// Spot-check the kit-pinned mappings the api spec names explicitly.
    #[test]
    fn kit_pinned_mappings() {
        assert_eq!(AppError::UnsupportedMediaType.code(), "PF-2001");
        assert_eq!(AppError::UnsupportedMediaType.status(), 415);
        assert_eq!(
            AppError::UnsupportedMediaType.title(),
            "unsupported_media_type"
        );
        assert_eq!(AppError::RefreshReuse.code(), "PF-1007");
        assert_eq!(AppError::QualityGateFailed.code(), "PF-4002");
        assert_eq!(AppError::QualityGateFailed.status(), 422);
        assert_eq!(AppError::DatabaseUnavailable.code(), "PF-5001");
        assert_eq!(AppError::InvalidStateTransition.code(), "PF-5002");
    }

    /// AC: prod body shape — RFC 7807 fields present, caller detail
    /// withheld, static safe detail shown, no internals anywhere.
    #[test]
    fn prod_body_never_leaks_internals() {
        let secret = "postgres://pf:SECRET@localhost/db internal trace";
        let problem = AppError::DatabaseUnavailable.to_problem(
            Profile::Production,
            Some("01JREQUESTID"),
            Some(secret),
        );
        assert_eq!(problem.status, 503);
        assert_eq!(problem.code, "PF-5001");
        assert_eq!(problem.title, "database_unavailable");
        assert_eq!(
            problem.problem_type,
            "https://docs.pureframe.dev/errors/PF-5001"
        );
        assert_eq!(problem.detail, "the database is temporarily unavailable");
        assert!(
            !problem.detail.contains("SECRET"),
            "leaked: {}",
            problem.detail
        );
        assert_eq!(problem.request_id.as_deref(), Some("01JREQUESTID"));
        // And the JSON shape carries exactly the RFC fields.
        let json = problem.to_json();
        assert_eq!(json["type"], "https://docs.pureframe.dev/errors/PF-5001");
        assert_eq!(json["status"], 503);
        assert_eq!(json["code"], "PF-5001");
        assert_eq!(json["detail"], "the database is temporarily unavailable");
        assert_eq!(json["request_id"], "01JREQUESTID");
    }

    /// Dev shows the caller detail; request_id is omitted when unknown.
    #[test]
    fn dev_body_shows_caller_detail() {
        let problem = AppError::ValidationFailed.to_problem(
            Profile::Development,
            None,
            Some("field 'email': not an email address"),
        );
        assert_eq!(problem.status, 400);
        assert_eq!(problem.detail, "field 'email': not an email address");
        assert!(problem.request_id.is_none());
        assert!(problem.to_json().get("request_id").is_none());
    }

    /// No caller detail: both profiles fall back to the safe default.
    #[test]
    fn no_detail_falls_back_to_default() {
        for profile in [Profile::Development, Profile::Production] {
            let problem = AppError::NotFound.to_problem(profile, None, None);
            assert_eq!(problem.detail, "the requested resource was not found");
        }
    }

    // -- P0-018: variant-to-response table test ---------------------------
    //
    // This table is the independent oracle for the mapping: every row is
    // typed by hand from the api spec + the P0-017 design decisions. If a
    // match arm in code()/status()/title()/default_detail() drifts, this
    // test fails on the exact row. Add a row here for every new variant
    // (the count assertion enforces it).

    /// (variant, code, status, title, default detail) for all variants.
    fn response_table() -> Vec<(AppError, &'static str, u16, &'static str, &'static str)> {
        use AppError::*;
        vec![
            (
                Unauthenticated,
                "PF-1001",
                401,
                "unauthenticated",
                "authentication is required for this resource",
            ),
            (
                Forbidden,
                "PF-1002",
                403,
                "forbidden",
                "you do not have permission for this resource",
            ),
            (
                RefreshReuse,
                "PF-1007",
                401,
                "refresh_token_reused",
                "refresh token reuse detected; all sessions have been revoked",
            ),
            (
                EmailNotVerified,
                "PF-1009",
                403,
                "email_not_verified",
                "email verification is required before creating jobs",
            ),
            (
                InvalidToken,
                "PF-1010",
                401,
                "invalid_token",
                "the access token is invalid",
            ),
            (
                ExpiredToken,
                "PF-1011",
                401,
                "expired_token",
                "the access token has expired",
            ),
            (
                UnsupportedMediaType,
                "PF-2001",
                415,
                "unsupported_media_type",
                "this media type is not accepted",
            ),
            (
                ValidationFailed,
                "PF-2002",
                400,
                "validation_failed",
                "the request failed validation",
            ),
            (
                PayloadTooLarge,
                "PF-2003",
                413,
                "payload_too_large",
                "the request exceeds the size limit",
            ),
            (
                PartChecksumMismatch,
                "PF-2004",
                400,
                "part_checksum_mismatch",
                "an upload part failed its checksum",
            ),
            (
                DimensionsExceeded,
                "PF-2005",
                413,
                "dimensions_exceeded",
                "the image dimensions exceed the limit",
            ),
            (
                ArchiveRejected,
                "PF-2006",
                400,
                "archive_rejected",
                "the archive was rejected",
            ),
            (
                DecodeBudgetExceeded,
                "PF-2007",
                413,
                "decode_budget_exceeded",
                "the image is too large to decode",
            ),
            (
                InvalidHints,
                "PF-2008",
                400,
                "invalid_hints",
                "the mask hints are invalid",
            ),
            (
                ResultExpired,
                "PF-2009",
                410,
                "result_expired",
                "the result has expired",
            ),
            (
                NotFound,
                "PF-2010",
                404,
                "not_found",
                "the requested resource was not found",
            ),
            (
                Conflict,
                "PF-2011",
                409,
                "conflict",
                "the request conflicts with the current state",
            ),
            (
                QuotaImagesExceeded,
                "PF-3001",
                429,
                "quota_images_exceeded",
                "image quota exceeded; upgrade your plan for more",
            ),
            (
                QuotaVideoSecondsExceeded,
                "PF-3002",
                429,
                "quota_video_seconds_exceeded",
                "video-seconds quota exceeded; upgrade your plan for more",
            ),
            (
                FeatureNotEntitled,
                "PF-3003",
                403,
                "feature_not_entitled",
                "your plan does not include this feature; upgrade to use it",
            ),
            (
                RetryAttemptsExceeded,
                "PF-3004",
                409,
                "retry_attempts_exceeded",
                "the retry limit for this job was reached",
            ),
            (
                QueueFull,
                "PF-3005",
                429,
                "queue_full",
                "the system is busy; retry shortly",
            ),
            (
                StorageHeadroomExceeded,
                "PF-3006",
                429,
                "storage_headroom_exceeded",
                "storage headroom exceeded; free space or upgrade your plan",
            ),
            (
                RateLimited,
                "PF-3007",
                429,
                "rate_limited",
                "too many requests; retry after the reset window",
            ),
            (
                AttestationMissing,
                "PF-4001",
                403,
                "attestation_missing",
                "an ownership attestation is required to create this job",
            ),
            (
                QualityGateFailed,
                "PF-4002",
                422,
                "quality_gate_failed",
                "the output failed the quality gates; see the job report",
            ),
            (
                ResourceBudgetExceeded,
                "PF-4005",
                500,
                "resource_budget_exceeded",
                "the job exceeded its resource budget and was stopped",
            ),
            (
                RoundtripMismatch,
                "PF-4006",
                500,
                "roundtrip_mismatch",
                "the pipeline produced an inconsistent result; the job failed",
            ),
            (
                OutputMismatch,
                "PF-4007",
                500,
                "output_mismatch",
                "the encoded output did not match expectations",
            ),
            (
                ProvenanceStripDenied,
                "PF-4101",
                403,
                "provenance_strip_denied",
                "provenance stripping is not enabled for this request",
            ),
            (
                Internal,
                "PF-5000",
                500,
                "internal_error",
                "an internal error occurred",
            ),
            (
                DatabaseUnavailable,
                "PF-5001",
                503,
                "database_unavailable",
                "the database is temporarily unavailable",
            ),
            (
                InvalidStateTransition,
                "PF-5002",
                409,
                "invalid_state_transition",
                "this transition is not allowed",
            ),
        ]
    }

    /// AC: the variant-to-response table test. Exact code, status, title
    /// and default detail for every variant, plus the full JSON body
    /// (type URL, status, code, title, detail) in both profiles.
    #[test]
    fn variant_to_response_table() {
        let table = response_table();
        assert_eq!(table.len(), 33, "a variant is missing from the table");
        assert_eq!(
            table.len(),
            all_variants().len(),
            "table and variant list disagree"
        );
        let mut codes: Vec<&str> = table.iter().map(|(_, c, _, _, _)| *c).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), 33, "two variants share a code");

        for (err, code, status, title, detail) in &table {
            assert_eq!(err.code(), *code, "{code}");
            assert_eq!(err.status(), *status, "{code}");
            assert_eq!(err.title(), *title, "{code}");
            assert_eq!(err.default_detail(), *detail, "{code}");
            for profile in [Profile::Development, Profile::Production] {
                let problem = err.to_problem(profile, Some("01JTABLE"), None);
                let json = problem.to_json();
                assert_eq!(
                    json["type"],
                    format!("https://docs.pureframe.dev/errors/{code}"),
                    "{code}"
                );
                assert_eq!(json["title"], *title, "{code}");
                assert_eq!(json["status"], *status, "{code}");
                assert_eq!(json["code"], *code, "{code}");
                assert_eq!(json["detail"], *detail, "{code}");
                assert_eq!(json["request_id"], "01JTABLE", "{code}");
            }
        }
    }

    /// Debug vs prod body shape, same error + same caller detail: the
    /// two bodies agree on everything except `detail` — dev shows the
    /// caller text, prod shows the safe default and no trace of it.
    #[test]
    fn debug_vs_prod_body_shape() {
        let caller = "sql: connection refused at 10.0.0.4:5432 (password=SECRET)";
        let dev = AppError::DatabaseUnavailable.to_problem(
            Profile::Development,
            Some("01JDEBUGVSPROD"),
            Some(caller),
        );
        let prod = AppError::DatabaseUnavailable.to_problem(
            Profile::Production,
            Some("01JDEBUGVSPROD"),
            Some(caller),
        );
        assert_eq!(dev.code, prod.code);
        assert_eq!(dev.status, prod.status);
        assert_eq!(dev.title, prod.title);
        assert_eq!(dev.problem_type, prod.problem_type);
        assert_eq!(dev.request_id, prod.request_id);
        assert_eq!(dev.detail, caller);
        assert_eq!(prod.detail, "the database is temporarily unavailable");
        assert!(!prod.detail.contains("10.0.0.4"), "leaked: {}", prod.detail);
        assert!(!prod.detail.contains("SECRET"), "leaked: {}", prod.detail);
        assert_ne!(dev.detail, prod.detail);
        // JSON level: same keys, different detail value.
        let dev_json = dev.to_json();
        let prod_json = prod.to_json();
        assert_eq!(dev_json["code"], prod_json["code"]);
        assert_eq!(dev_json["detail"], caller);
        assert_eq!(
            prod_json["detail"],
            "the database is temporarily unavailable"
        );
    }

    /// Request-id propagation into bodies: present in dev and prod when
    /// known, omitted from the JSON entirely when unknown.
    #[test]
    fn request_id_propagates_into_bodies() {
        for profile in [Profile::Development, Profile::Production] {
            let with_id = AppError::NotFound.to_problem(profile, Some("01JREQID123"), None);
            assert_eq!(with_id.request_id.as_deref(), Some("01JREQID123"));
            assert_eq!(with_id.to_json()["request_id"], "01JREQID123");

            let without_id = AppError::NotFound.to_problem(profile, None, None);
            assert!(without_id.request_id.is_none());
            assert!(without_id.to_json().get("request_id").is_none());
        }
    }
}
