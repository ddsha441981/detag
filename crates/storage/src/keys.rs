//! Storage key scheme (P0-056, ADR-0011): every object key is
//! `{env}/{yyyy-mm}/{job_id}/{artifact}.{ext}`.
//!
//! The scheme is the PII guarantee: keys are built only from
//! (a) the environment, (b) the year-month, (c) the job UUID, and
//! (d) a fixed artifact stem + a validated extension. No emails,
//! usernames, original filenames, or any other user-controlled
//! string ever appears in a key — the original filename is stored
//! in the database row (`uploads.filename`), never in the key.
//!
//! Traversal is rejected at construction: `env` must be one of
//! `dev`/`staging`/`prod`, the date is a `NaiveDate` (it cannot
//! be malformed), the job id is a `Uuid`, the stem comes from the
//! [`ArtifactKind`] enum, and the extension must be short
//! alphanumeric text. The built key is passed through
//! [`validate_key`] as a final check, and [`parse_storage_key`]
//! verifies the same shape when a stored key is read back.

use chrono::NaiveDate;
use uuid::Uuid;

use crate::provider::{validate_key, StorageError};

/// Environments a key may name. Anything else — including a
/// username or an email — is rejected, so an environment can
/// never smuggle PII into a key.
pub const ENVIRONMENTS: [&str; 3] = ["dev", "staging", "prod"];

/// The fixed artifact stems. `Input` is the uploaded original
/// (`uploads.storage_key`); the rest are the `artifacts.type`
/// values (`output|mask|preview_before|preview_after|report|
/// `batch_zip`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArtifactKind {
    /// The uploaded original.
    Input,
    /// The cleaned result.
    Output,
    /// The detection mask.
    Mask,
    /// Before preview.
    PreviewBefore,
    /// After preview.
    PreviewAfter,
    /// The quality report (always `.json`).
    Report,
    /// A batch job's ZIP output (always `.zip`).
    BatchZip,
}

impl ArtifactKind {
    /// The file stem for this kind.
    pub fn stem(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Output => "output",
            Self::Mask => "mask",
            Self::PreviewBefore => "preview_before",
            Self::PreviewAfter => "preview_after",
            Self::Report => "report",
            Self::BatchZip => "batch",
        }
    }

    /// Parse a stem back to its kind (`None` for anything else).
    pub fn from_stem(stem: &str) -> Option<Self> {
        match stem {
            "input" => Some(Self::Input),
            "output" => Some(Self::Output),
            "mask" => Some(Self::Mask),
            "preview_before" => Some(Self::PreviewBefore),
            "preview_after" => Some(Self::PreviewAfter),
            "report" => Some(Self::Report),
            "batch" => Some(Self::BatchZip),
            _ => None,
        }
    }

    /// The extension this kind always uses, when it has one.
    /// `None` means the caller supplies the extension (the input
    /// keeps the uploaded file's extension; output/mask/previews
    /// take the encoded format's extension). The content allowlist
    /// itself lands in P0-059 — this is only the fixed-extension
    /// half of it.
    pub fn fixed_ext(self) -> Option<&'static str> {
        match self {
            Self::Report => Some("json"),
            Self::BatchZip => Some("zip"),
            _ => None,
        }
    }
}

/// A parsed storage key: the inverse of [`storage_key`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedKey {
    /// Environment segment.
    pub env: String,
    /// Year-month segment.
    pub year_month: String,
    /// Job id segment.
    pub job_id: Uuid,
    /// Artifact kind segment.
    pub kind: ArtifactKind,
    /// Extension segment (lowercased).
    pub ext: String,
}

/// Validate an environment segment.
pub fn validate_env(env: &str) -> Result<(), StorageError> {
    if ENVIRONMENTS.contains(&env) {
        Ok(())
    } else {
        Err(StorageError::Invalid(format!(
            "env must be one of dev/staging/prod, got {env:?}"
        )))
    }
}

/// Validate an extension segment: 1–10 lowercase-alphanumeric
/// characters after lowercasing. Rejects dots, slashes, spaces —
/// anything that could smuggle a path or a second extension in.
/// The content allowlist (png/jpeg/webp/mp4/...) lands in P0-059;
/// this is the shape check only.
pub fn validate_ext(ext: &str) -> Result<String, StorageError> {
    let lower = ext.to_ascii_lowercase();
    if lower.is_empty() || lower.len() > 10 {
        return Err(StorageError::Invalid(
            "extension must be 1..=10 characters".to_string(),
        ));
    }
    if !lower.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(StorageError::Invalid(format!(
            "extension must be alphanumeric, got {ext:?}"
        )));
    }
    Ok(lower)
}

/// Build a storage key: `{env}/{yyyy-mm}/{job_id}/{stem}.{ext}`.
///
/// - `date` supplies the year-month (`%Y-%m`); a `NaiveDate`
///   cannot be malformed, so no date validation is needed.
/// - `ext` is lowercased and shape-validated; kinds with a
///   [`ArtifactKind::fixed_ext`] must use it (passing another
///   extension for `Report`/`BatchZip` is a caller error).
/// - The result always passes [`validate_key`].
pub fn storage_key(
    env: &str,
    date: NaiveDate,
    job_id: &Uuid,
    kind: ArtifactKind,
    ext: &str,
) -> Result<String, StorageError> {
    validate_env(env)?;
    let ext = validate_ext(ext)?;
    if let Some(fixed) = kind.fixed_ext() {
        if ext != fixed {
            return Err(StorageError::Invalid(format!(
                "{} files always use .{fixed}, got .{ext}",
                kind.stem()
            )));
        }
    }
    let key = format!(
        "{env}/{}/{job_id}/{}.{ext}",
        date.format("%Y-%m"),
        kind.stem()
    );
    validate_key(&key)?;
    Ok(key)
}

/// Parse a storage key back into its segments, rejecting anything
/// that does not match the ADR-0011 layout — including traversal
/// (`..` segments), wrong environments, malformed dates, non-UUID
/// job ids, unknown stems, and bad extensions.
pub fn parse_storage_key(key: &str) -> Result<ParsedKey, StorageError> {
    validate_key(key)?;
    let mut segments = key.split('/');
    let env = segments.next().unwrap_or("");
    let year_month = segments.next().unwrap_or("");
    let job_id = segments.next().unwrap_or("");
    let file = segments.next().unwrap_or("");
    if segments.next().is_some() {
        return Err(StorageError::Invalid(format!(
            "storage key must have 4 segments, got {key:?}"
        )));
    }
    validate_env(env)?;
    let mut ym = year_month.split('-');
    let year = ym.next().unwrap_or("");
    let month = ym.next().unwrap_or("");
    if ym.next().is_some()
        || year.len() != 4
        || !year.chars().all(|c| c.is_ascii_digit())
        || month.len() != 2
        || !month.chars().all(|c| c.is_ascii_digit())
        || !matches!(
            month,
            "01" | "02" | "03" | "04" | "05" | "06" | "07" | "08" | "09" | "10" | "11" | "12"
        )
    {
        return Err(StorageError::Invalid(format!(
            "storage key year-month must be yyyy-mm, got {year_month:?}"
        )));
    }
    let job_id = Uuid::parse_str(job_id).map_err(|_| {
        StorageError::Invalid(format!("storage key job_id must be a UUID, got {job_id:?}"))
    })?;
    let (stem, ext) = file.rsplit_once('.').ok_or_else(|| {
        StorageError::Invalid(format!(
            "storage key file must have an extension, got {file:?}"
        ))
    })?;
    let kind = ArtifactKind::from_stem(stem)
        .ok_or_else(|| StorageError::Invalid(format!("unknown artifact stem: {stem:?}")))?;
    let ext = validate_ext(ext)?;
    if let Some(fixed) = kind.fixed_ext() {
        if ext != fixed {
            return Err(StorageError::Invalid(format!(
                "{} files always use .{fixed}, got .{ext}",
                kind.stem()
            )));
        }
    }
    Ok(ParsedKey {
        env: env.to_string(),
        year_month: year_month.to_string(),
        job_id,
        kind,
        ext,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job_id() -> Uuid {
        Uuid::parse_str("019aa1b2-c3d4-7e5f-8000-000000000001").unwrap()
    }

    fn date() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, 15).unwrap()
    }

    #[test]
    fn layout_matches_adr_0011() {
        assert_eq!(
            storage_key("dev", date(), &job_id(), ArtifactKind::Output, "png").expect("valid key"),
            "dev/2026-11/019aa1b2-c3d4-7e5f-8000-000000000001/output.png"
        );
        assert_eq!(
            storage_key("prod", date(), &job_id(), ArtifactKind::Input, "jpg").expect("valid key"),
            "prod/2026-11/019aa1b2-c3d4-7e5f-8000-000000000001/input.jpg"
        );
    }

    #[test]
    fn every_kind_builds_a_distinct_key() {
        let mut keys = Vec::new();
        for (kind, ext) in [
            (ArtifactKind::Input, "png"),
            (ArtifactKind::Output, "png"),
            (ArtifactKind::Mask, "png"),
            (ArtifactKind::PreviewBefore, "png"),
            (ArtifactKind::PreviewAfter, "png"),
            (ArtifactKind::Report, "json"),
            (ArtifactKind::BatchZip, "zip"),
        ] {
            keys.push(storage_key("dev", date(), &job_id(), kind, ext).expect("valid"));
        }
        let mut sorted = keys.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), keys.len(), "stems must be distinct: {keys:?}");
    }

    #[test]
    fn extension_is_lowercased() {
        assert!(
            storage_key("dev", date(), &job_id(), ArtifactKind::Output, "PNG")
                .expect("valid")
                .ends_with("output.png")
        );
    }

    #[test]
    fn fixed_extensions_are_enforced() {
        assert!(matches!(
            storage_key("dev", date(), &job_id(), ArtifactKind::Report, "txt"),
            Err(StorageError::Invalid(_))
        ));
        assert!(matches!(
            storage_key("dev", date(), &job_id(), ArtifactKind::BatchZip, "tar"),
            Err(StorageError::Invalid(_))
        ));
    }

    #[test]
    fn traversal_and_pii_are_rejected() {
        // Traversal in any free-form segment.
        for (env, ext) in [
            ("../prod", "png"),
            ("dev/../prod", "png"),
            ("dev", "png/../../etc"),
            ("dev", "p/n"),
            ("dev", "png "),
            ("dev", ""),
            ("dev", "p.n.g"),
            ("dev", "png\x00"),
        ] {
            assert!(
                storage_key(env, date(), &job_id(), ArtifactKind::Output, ext).is_err(),
                "env={env:?} ext={ext:?} must be rejected"
            );
        }
        // PII as the environment: emails and usernames are not envs.
        for env in ["user@example.com", "deendayal", "prod\\x", "PROD"] {
            assert!(
                storage_key(env, date(), &job_id(), ArtifactKind::Output, "png").is_err(),
                "env={env:?} must be rejected"
            );
        }
    }

    #[test]
    fn built_keys_contain_no_pii_characters() {
        // Property: a built key is only [a-z0-9-/._] plus the UUID
        // dashes — no '@', no uppercase, no user string.
        let key =
            storage_key("staging", date(), &job_id(), ArtifactKind::Mask, "PNG").expect("valid");
        assert!(
            key.chars().all(|c| c.is_ascii_lowercase()
                || c.is_ascii_digit()
                || c == '/'
                || c == '.'
                || c == '-'),
            "{key}"
        );
        assert!(!key.contains('@'));
    }

    #[test]
    fn parse_round_trip() {
        let key = storage_key("dev", date(), &job_id(), ArtifactKind::PreviewAfter, "webp")
            .expect("valid");
        let parsed = parse_storage_key(&key).expect("parses");
        assert_eq!(parsed.env, "dev");
        assert_eq!(parsed.year_month, "2026-11");
        assert_eq!(parsed.job_id, job_id());
        assert_eq!(parsed.kind, ArtifactKind::PreviewAfter);
        assert_eq!(parsed.ext, "webp");
    }

    #[test]
    fn parse_rejects_non_layout_keys() {
        for bad in [
            "",
            "dev/2026-11/job/output.png", // not a UUID
            "dev/2026-13/019aa1b2-c3d4-7e5f-8000-000000000001/output.png", // month 13
            "dev/26-11/019aa1b2-c3d4-7e5f-8000-000000000001/output.png", // short year
            "qa/2026-11/019aa1b2-c3d4-7e5f-8000-000000000001/output.png", // bad env
            "dev/2026-11/019aa1b2-c3d4-7e5f-8000-000000000001/unknown.png", // bad stem
            "dev/2026-11/019aa1b2-c3d4-7e5f-8000-000000000001/output", // no ext
            "dev/2026-11/019aa1b2-c3d4-7e5f-8000-000000000001/output.pn/g", // 5 segments
            "dev/2026-11/019aa1b2-c3d4-7e5f-8000-000000000001/../output.png", // traversal
            "dev/2026-11/019aa1b2-c3d4-7e5f-8000-000000000001/report.txt", // wrong fixed ext
        ] {
            assert!(parse_storage_key(bad).is_err(), "{bad:?} must be rejected");
        }
    }
}
