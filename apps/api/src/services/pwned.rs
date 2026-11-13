//! Have I Been Pwned k-anonymity check (P1-026, FR-A-01).
//!
//! The check runs on register + reset, behind
//! `PF_PASSWORD_PWNED_CHECK` (default off — see the Settings docs
//! for why it is opt-in). When on, the api asks the HIBP range API
//! for the 5-char SHA-1 prefix of the candidate password and
//! rejects it when its suffix appears in the returned range.
//!
//! Privacy: only the 5-char prefix leaves the machine (the full
//! hash and the password never do). Fail-open: any transport error
//! or non-2xx answer logs a warn and lets the registration/reset
//! continue — a down HIBP must never block signups. The check is
//! deliberately NOT part of the sync `Validate` step (it is async
//! network I/O); the services call [`check_pwned_policy`] after the
//! rate limit + policy checks and before the Argon2 hash, so a
//! rejected password costs no hashing.
//!
//! The base URL is a setting (`PF_PASSWORD_PWNED_URL`, default the
//! public HIBP API) — an operator mirror, and the test hook: tests
//! point it at a local stub server (see `detag_testkit::
//! serve_one_http_response`), never at the real API.

use std::sync::LazyLock;
use std::time::Duration;

use detag_core::config::Settings;
use detag_core::error::FieldError;
use detag_validator::{hibp_range_contains, hibp_sha1_prefix_suffix, ValidationErrors};
use thiserror::Error;

/// HIBP request timeout: 5 s, one attempt, no retry — the check is
/// advisory (fail-open), so a slow answer is worse than no answer.
pub const HIBP_TIMEOUT: Duration = Duration::from_secs(5);

/// One shared client: connection reuse across registrations, and no
/// per-request client construction (which would re-resolve + re-TLS
/// every time).
static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .timeout(HIBP_TIMEOUT)
        .user_agent("detag-api/1.0 (haveibeenpwned range check)")
        .build()
        .expect("pwned: reqwest client builds")
});

/// What the HIBP lookup can fail with. Carries no password and no
/// hash — the prefix is never logged either (20 bits of the hash is
/// still information about the password).
#[derive(Debug, Error)]
pub enum PwnedError {
    #[error("pwned check transport failed")]
    Transport(#[from] reqwest::Error),
    #[error("pwned check answered {0}")]
    BadStatus(u16),
    #[error("pwned check url invalid")]
    BadUrl,
}

/// True when the password's SHA-1 suffix appears in the HIBP range
/// for its prefix. `Ok(false)` = not seen in any breach.
pub async fn is_password_pwned(settings: &Settings, password: &str) -> Result<bool, PwnedError> {
    let (prefix, suffix) = hibp_sha1_prefix_suffix(password);
    let base = settings.password_pwned_url.trim_end_matches('/');
    let url =
        reqwest::Url::parse(&format!("{base}/range/{prefix}")).map_err(|_| PwnedError::BadUrl)?;
    let resp = CLIENT
        .get(url)
        .send()
        .await
        .map_err(PwnedError::Transport)?;
    let status = resp.status();
    if !status.is_success() {
        return Err(PwnedError::BadStatus(status.as_u16()));
    }
    let body = resp.text().await.map_err(PwnedError::Transport)?;
    Ok(hibp_range_contains(&body, &suffix))
}

/// Enforce the pwned policy for one candidate password: no-op when
/// the flag is off; `Err(ValidationErrors)` with code
/// `pwned_password` when the password is breached; `Ok(())` when
/// it is clean — or when the check itself failed (fail-open, with a
/// warn in the log).
pub async fn check_pwned_policy(
    settings: &Settings,
    password: &str,
) -> Result<(), ValidationErrors> {
    if !settings.password_pwned_check {
        return Ok(());
    }
    match is_password_pwned(settings, password).await {
        Ok(true) => {
            let mut errors = ValidationErrors::default();
            errors.push(FieldError::new(
                "password",
                "pwned_password",
                "has appeared in a data breach — choose a different password",
            ));
            Err(errors)
        }
        Ok(false) => Ok(()),
        Err(err) => {
            tracing::warn!(error = %err, "pwned check failing open: HIBP unreachable");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use detag_testkit::serve_one_http_response;

    fn settings_with(url: &str) -> Settings {
        Settings {
            password_pwned_check: true,
            password_pwned_url: url.to_string(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn pwned_password_is_rejected() {
        // The stub returns a range body containing "password"'s
        // suffix (the canonical HIBP example).
        let (url, server) = serve_one_http_response(
            200,
            "1E4C9B93F3F0682250B6CF8331B7EE68FD8:9543776\n0018A45C4D1DEF81644B54AB7F969B88D65:1"
                .to_string(),
        )
        .await;
        let settings = settings_with(&url);
        let err = check_pwned_policy(&settings, "password")
            .await
            .expect_err("pwned");
        assert_eq!(err.0.len(), 1);
        assert_eq!(err.0[0].code, "pwned_password");
        assert_eq!(err.0[0].field, "password");
        server.abort();
    }

    #[tokio::test]
    async fn clean_password_passes() {
        let (url, server) = serve_one_http_response(
            200,
            "0018A45C4D1DEF81644B54AB7F969B88D65:1\n01330C689E5DC05884B7E4379AF1E0A7C:2"
                .to_string(),
        )
        .await;
        let settings = settings_with(&url);
        check_pwned_policy(&settings, "password")
            .await
            .expect("clean");
        server.abort();
    }

    #[tokio::test]
    async fn flag_off_never_calls_the_api() {
        // No server is started at all: if the check ran, the
        // request would fail — but the flag is off, so it must not
        // run (and must not fail open either — it must not call).
        let mut settings = Settings::default();
        assert!(!settings.password_pwned_check);
        settings.password_pwned_url = "http://127.0.0.1:1".to_string();
        check_pwned_policy(&settings, "password")
            .await
            .expect("flag off");
    }

    #[tokio::test]
    async fn transport_error_fails_open() {
        // The URL points at a closed port: the lookup errors, and
        // the policy lets the password through (warn logged).
        let settings = Settings {
            password_pwned_check: true,
            password_pwned_url: "http://127.0.0.1:1".to_string(),
            ..Default::default()
        };
        check_pwned_policy(&settings, "password")
            .await
            .expect("fail open");
    }

    #[tokio::test]
    async fn non_200_fails_open() {
        let (url, server) = serve_one_http_response(503, "try again later".to_string()).await;
        let settings = settings_with(&url);
        check_pwned_policy(&settings, "password")
            .await
            .expect("fail open");
        server.abort();
    }
}
