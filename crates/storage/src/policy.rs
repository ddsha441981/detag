//! Presigned-URL policy (P0-055): the rules over the raw presign
//! calls in [`crate::provider::StorageProvider`].
//!
//! Two kinds of URL, two expiry regimes:
//!
//! - **PUT part URLs** are minted during an authenticated upload
//!   session, so they must never outlive the access token that
//!   authorized the upload: `part_url_ttl < token_ttl` (FR-A-02
//!   fixes the access token at 15 min). The lifetime is
//!   configurable (`PF_UPLOAD_PART_URL_TTL_SECS`, default 600 s),
//!   but the policy refuses any configured value at or above the
//!   token TTL — fail fast at construction, not per request.
//! - **GET result URLs** are minted fresh per authenticated
//!   download request and live the plan's result TTL (FR-J-09:
//!   Free 6 h / Pro 24 h / Business 72 h). They intentionally
//!   outlive any single access token — the user downloads hours
//!   later, after re-authenticating — so the token-TTL guard does
//!   NOT apply to them. What bounds them is the plan TTL itself
//!   (a Free URL can never live 7 h) and the 7-day SigV4 cap
//!   (enforced by the trait's `validate_expiry`).
//!
//! **Single-use:** S3/MinIO presigned URLs cannot be single-use —
//! there is no such mechanism in SigV4, and this policy does not
//! pretend otherwise (`SINGLE_USE_SUPPORTED` is `false`). The
//! compensating controls are: short TTLs, one key per URL (a part
//! URL signs exactly one part of one upload; a result URL signs
//! exactly one object), and upload ids that die after complete or
//! abort (a replayed part URL targets a dead upload).

use std::time::Duration;

use crate::provider::{validate_expiry, StorageError, MAX_PRESIGN_SECS};

/// The access-token TTL from FR-A-02 (JWT access token 15 min).
/// The policy defaults to this; auth (Phase 1) passes its real
/// value when it constructs the policy, so the two can never drift
/// silently.
pub const ACCESS_TOKEN_TTL: Duration = Duration::from_secs(15 * 60);

/// Default PUT-part URL lifetime: 10 min — long enough to upload
/// one part, short enough to stay under the 15-min token TTL.
pub const DEFAULT_PART_URL_TTL: Duration = Duration::from_secs(600);

/// Presigned URLs are NOT single-use on S3/MinIO: SigV4 has no
/// such mechanism. See the module docs for the compensating
/// controls. Any caller that needs one-shot semantics must enforce
/// it server-side (e.g. an upload id that dies after complete).
pub const SINGLE_USE_SUPPORTED: bool = false;

/// A plan tier, by the `plans.code` values in the database
/// (`free` | `pro` | `business` — CHECK constraint, P0-039).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlanTier {
    /// Free plan.
    Free,
    /// Pro plan.
    Pro,
    /// Business plan.
    Business,
}

impl PlanTier {
    /// Parse a `plans.code` value. Unknown codes are a caller
    /// error, not a default — silently mapping an unknown plan to
    /// Free would hand out the wrong TTL.
    pub fn from_code(code: &str) -> Result<Self, StorageError> {
        match code {
            "free" => Ok(Self::Free),
            "pro" => Ok(Self::Pro),
            "business" => Ok(Self::Business),
            other => Err(StorageError::Invalid(format!("unknown plan code: {other}"))),
        }
    }

    /// The `plans.code` value for this tier.
    pub fn code(self) -> &'static str {
        match self {
            Self::Free => "free",
            Self::Pro => "pro",
            Self::Business => "business",
        }
    }
}

/// Which kind of presigned URL a guard applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlKind {
    /// PUT part URL: session-bound, token-TTL guard applies.
    Part,
    /// GET result URL: plan-TTL bound, token-TTL guard does not apply.
    Result,
}

/// The presigned-URL policy (P0-055).
///
/// Construct once from settings (the API builds it from
/// `Settings`: `upload_part_url_ttl_secs` + the three
/// `results_ttl_*_h` values; `token_ttl` comes from auth when it
/// lands and defaults to [`ACCESS_TOKEN_TTL`] until then), then
/// ask it for every expiry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresignPolicy {
    /// PUT-part URL lifetime (must be < `token_ttl`).
    pub part_url_ttl: Duration,
    /// The access-token TTL the part URLs must stay under.
    pub token_ttl: Duration,
    /// GET result URL lifetime for the Free plan.
    pub result_ttl_free: Duration,
    /// GET result URL lifetime for the Pro plan.
    pub result_ttl_pro: Duration,
    /// GET result URL lifetime for the Business plan.
    pub result_ttl_business: Duration,
}

impl Default for PresignPolicy {
    fn default() -> Self {
        Self {
            part_url_ttl: DEFAULT_PART_URL_TTL,
            token_ttl: ACCESS_TOKEN_TTL,
            result_ttl_free: Duration::from_secs(6 * 3600),
            result_ttl_pro: Duration::from_secs(24 * 3600),
            result_ttl_business: Duration::from_secs(72 * 3600),
        }
    }
}

impl PresignPolicy {
    /// Build a policy, refusing an unusable one. `result_ttl_*`
    /// are `Duration`s (the caller converts the `*_h` hour settings).
    pub fn new(
        part_url_ttl: Duration,
        token_ttl: Duration,
        result_ttl_free: Duration,
        result_ttl_pro: Duration,
        result_ttl_business: Duration,
    ) -> Result<Self, StorageError> {
        let policy = Self {
            part_url_ttl,
            token_ttl,
            result_ttl_free,
            result_ttl_pro,
            result_ttl_business,
        };
        policy.validate()?;
        Ok(policy)
    }

    /// The same checks as `new`, for a policy built literally.
    pub fn validate(&self) -> Result<(), StorageError> {
        if self.token_ttl.is_zero() {
            return Err(StorageError::InvalidConfig(
                "token_ttl must be positive".to_string(),
            ));
        }
        if self.part_url_ttl.is_zero() {
            return Err(StorageError::InvalidConfig(
                "part_url_ttl must be positive".to_string(),
            ));
        }
        // The guard: a part URL must die before the token that
        // authorized it does. Strictly less-than — equal is not
        // good enough, the URL must be dead while the token is
        // still alive.
        if self.part_url_ttl >= self.token_ttl {
            return Err(StorageError::InvalidConfig(format!(
                "part_url_ttl ({}s) must be less than token_ttl ({}s)",
                self.part_url_ttl.as_secs(),
                self.token_ttl.as_secs()
            )));
        }
        for (name, ttl) in [
            ("result_ttl_free", self.result_ttl_free),
            ("result_ttl_pro", self.result_ttl_pro),
            ("result_ttl_business", self.result_ttl_business),
        ] {
            if ttl.is_zero() {
                return Err(StorageError::InvalidConfig(format!(
                    "{name} must be positive"
                )));
            }
            if ttl.as_secs() > MAX_PRESIGN_SECS {
                return Err(StorageError::InvalidConfig(format!(
                    "{name} ({}s) exceeds the 7-day SigV4 presign cap",
                    ttl.as_secs()
                )));
            }
        }
        Ok(())
    }

    /// The expiry to mint PUT part URLs with.
    pub fn part_url_expiry(&self) -> Duration {
        self.part_url_ttl
    }

    /// The expiry to mint GET result URLs with for `plan`.
    pub fn result_url_expiry(&self, plan: PlanTier) -> Duration {
        match plan {
            PlanTier::Free => self.result_ttl_free,
            PlanTier::Pro => self.result_ttl_pro,
            PlanTier::Business => self.result_ttl_business,
        }
    }

    /// Guard a requested expiry before minting. `plan` is needed
    /// only for result URLs (the plan TTL is their ceiling).
    ///
    /// - Part: `requested` must be positive, at most the
    ///   configured part TTL, and strictly under the token TTL.
    /// - Result: `requested` must be positive and at most the
    ///   plan's TTL (a Free URL can never live 7 h). The 7-day
    ///   SigV4 cap is enforced again by the trait itself.
    pub fn check_expiry(
        &self,
        kind: UrlKind,
        plan: PlanTier,
        requested: Duration,
    ) -> Result<Duration, StorageError> {
        validate_expiry(requested)?;
        match kind {
            UrlKind::Part => {
                if requested > self.part_url_ttl {
                    return Err(StorageError::Invalid(format!(
                        "part URL expiry ({}s) exceeds the configured part TTL ({}s)",
                        requested.as_secs(),
                        self.part_url_ttl.as_secs()
                    )));
                }
                if requested >= self.token_ttl {
                    return Err(StorageError::Invalid(format!(
                        "part URL expiry ({}s) must be less than the token TTL ({}s)",
                        requested.as_secs(),
                        self.token_ttl.as_secs()
                    )));
                }
            }
            UrlKind::Result => {
                let ceiling = self.result_url_expiry(plan);
                if requested > ceiling {
                    return Err(StorageError::Invalid(format!(
                        "result URL expiry ({}s) exceeds the {} plan TTL ({}s)",
                        requested.as_secs(),
                        plan.code(),
                        ceiling.as_secs()
                    )));
                }
            }
        }
        Ok(requested)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::StorageProvider;
    use crate::s3::{S3Config, S3Storage};

    fn hours(h: u64) -> Duration {
        Duration::from_secs(h * 3600)
    }

    #[test]
    fn defaults_match_the_kit() {
        let policy = PresignPolicy::default();
        assert_eq!(policy.part_url_ttl, Duration::from_secs(600));
        assert_eq!(policy.token_ttl, ACCESS_TOKEN_TTL);
        assert_eq!(policy.result_url_expiry(PlanTier::Free), hours(6));
        assert_eq!(policy.result_url_expiry(PlanTier::Pro), hours(24));
        assert_eq!(policy.result_url_expiry(PlanTier::Business), hours(72));
        // S3 presigned URLs are not single-use (SigV4 has no such
        // mechanism) — asserted at compile time so the claim can
        // never silently flip.
        const { assert!(!SINGLE_USE_SUPPORTED) };
    }

    #[test]
    fn plan_codes_round_trip() {
        for (code, tier) in [
            ("free", PlanTier::Free),
            ("pro", PlanTier::Pro),
            ("business", PlanTier::Business),
        ] {
            assert_eq!(PlanTier::from_code(code).expect(code), tier);
            assert_eq!(tier.code(), code);
        }
        assert!(matches!(
            PlanTier::from_code("enterprise"),
            Err(StorageError::Invalid(_))
        ));
    }

    #[test]
    fn part_ttl_must_stay_under_the_token_ttl() {
        // Equal is not good enough: strictly less-than.
        assert!(matches!(
            PresignPolicy::new(
                Duration::from_secs(900),
                ACCESS_TOKEN_TTL,
                hours(6),
                hours(24),
                hours(72),
            ),
            Err(StorageError::InvalidConfig(_))
        ));
        assert!(matches!(
            PresignPolicy::new(
                Duration::from_secs(901),
                ACCESS_TOKEN_TTL,
                hours(6),
                hours(24),
                hours(72),
            ),
            Err(StorageError::InvalidConfig(_))
        ));
        assert!(PresignPolicy::new(
            Duration::from_secs(899),
            ACCESS_TOKEN_TTL,
            hours(6),
            hours(24),
            hours(72),
        )
        .is_ok());
    }

    #[test]
    fn result_ttls_must_fit_the_sigv4_cap() {
        assert!(matches!(
            PresignPolicy::new(
                Duration::from_secs(600),
                ACCESS_TOKEN_TTL,
                hours(6),
                hours(24),
                Duration::from_secs(8 * 24 * 3600),
            ),
            Err(StorageError::InvalidConfig(_))
        ));
    }

    #[test]
    fn check_expiry_part_guards() {
        let policy = PresignPolicy::default();
        assert_eq!(
            policy
                .check_expiry(UrlKind::Part, PlanTier::Free, Duration::from_secs(600))
                .expect("600s part URL is the configured TTL"),
            Duration::from_secs(600)
        );
        assert!(matches!(
            policy.check_expiry(UrlKind::Part, PlanTier::Free, Duration::from_secs(601)),
            Err(StorageError::Invalid(_))
        ));
        assert!(matches!(
            policy.check_expiry(UrlKind::Part, PlanTier::Free, Duration::ZERO),
            Err(StorageError::Invalid(_))
        ));
    }

    #[test]
    fn check_expiry_result_is_bound_by_plan() {
        let policy = PresignPolicy::default();
        assert!(policy
            .check_expiry(UrlKind::Result, PlanTier::Free, hours(6))
            .is_ok());
        assert!(matches!(
            policy.check_expiry(UrlKind::Result, PlanTier::Free, hours(7)),
            Err(StorageError::Invalid(_))
        ));
        assert!(policy
            .check_expiry(UrlKind::Result, PlanTier::Business, hours(72))
            .is_ok());
        // A shorter-than-plan expiry is fine (e.g. a one-off link).
        assert!(policy
            .check_expiry(UrlKind::Result, PlanTier::Pro, hours(1))
            .is_ok());
    }

    #[tokio::test]
    async fn policy_expiries_reach_the_presigned_urls() {
        // The policy's numbers are what the S3 impl actually signs:
        // offline (presigning is local), dummy credentials.
        let policy = PresignPolicy::default();
        let store = S3Storage::new(S3Config {
            endpoint: "http://localhost:9000".to_string(),
            bucket: "detag-dev".to_string(),
            region: "us-east-1".to_string(),
            access_key: "minioadmin".to_string(),
            secret_key: "minioadmin".to_string(),
        })
        .expect("test config builds");

        let part = store
            .presign_put_part(
                "dev/2026-11/job-1/input.png",
                "upload-1",
                1,
                policy.part_url_expiry(),
            )
            .await
            .expect("part presign");
        assert!(part.url.as_str().contains("X-Amz-Expires=600"));

        let result = store
            .presign_get(
                "dev/2026-11/job-1/result.png",
                policy.result_url_expiry(PlanTier::Free),
            )
            .await
            .expect("result presign");
        assert!(result.url.as_str().contains("X-Amz-Expires=21600"));
    }
}
