//! Logging hygiene, integration (P0-032, NFR-42).
//!
//! Drives the real error paths end to end through the real JSON
//! subscriber and asserts no captured log line contains a raw email,
//! a password/token value, or a raw IP address:
//!
//! * auth-failure path — `AppError::EmailNotVerified` rendered with
//!   `Profile::Production` and caller detail full of PII: the detail
//!   is withheld from the body and must reach `tracing` redacted.
//! * job-error path — a pipeline failure (`DecodeBudgetExceeded`)
//!   rendered the same way inside a `job_span`, plus the worker's
//!   convention of logging job failures through `redact`.
//!
//! The subscriber writes to a shared buffer (same pattern as the
//! unit tests in `telemetry.rs`); the assertions run on the raw
//! captured lines, so a leak anywhere in the pipeline fails here.

use detag_core::config::Settings;
use detag_core::error::{AppError, Profile};
use detag_core::telemetry::{build_subscriber, job_span, redact, LogFormat};
use std::sync::{Arc, Mutex};
use tracing_subscriber::fmt;

/// A writer that collects into a shared buffer.
#[derive(Clone, Default)]
struct BufWriter {
    buf: Arc<Mutex<Vec<u8>>>,
}

impl BufWriter {
    fn take_string(&self) -> String {
        let bytes = self.buf.lock().unwrap().clone();
        String::from_utf8(bytes).expect("logs are UTF-8")
    }
}

impl std::io::Write for BufWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.buf.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> fmt::writer::MakeWriter<'a> for BufWriter {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Caller detail stuffed with every PII class at once. If any of it
/// survives into a log line, the test fails.
const PII_DETAIL: &str = "login failed for alice@example.com \
    password=hunter2 token=example-token-value from 10.0.0.9 \
    (also 2001:db8::1)";

/// Every captured line must parse as JSON and contain no raw PII.
fn assert_hygiene(output: &str) {
    assert!(!output.is_empty(), "expected log lines, got none");
    for line in output.lines() {
        let value: serde_json::Value =
            serde_json::from_str(line).expect("every log line must parse as JSON");
        let flat = value.to_string();
        for raw in [
            "alice@example.com",
            "hunter2",
            "example-token-value",
            "10.0.0.9",
            "2001:db8::1",
        ] {
            assert!(
                !flat.contains(raw),
                "raw PII {raw} leaked into log line: {line}"
            );
        }
    }
}

fn settings() -> Settings {
    Settings {
        log_level: "info".to_string(),
        ..Settings::default()
    }
}

#[test]
fn auth_failure_path_logs_no_pii() {
    let writer = BufWriter::default();
    let (subscriber, _) =
        build_subscriber(&settings(), LogFormat::Json, "detag-test", writer.clone())
            .expect("subscriber builds");
    tracing::subscriber::with_default(subscriber, || {
        let problem = AppError::EmailNotVerified.to_problem(
            Profile::Production,
            Some("req-hygiene-1"),
            Some(PII_DETAIL),
        );
        // The body must carry the static safe detail, never the PII.
        assert_eq!(problem.code, "PF-1009");
        assert!(!problem.detail.contains("alice@example.com"));
        assert!(!problem.detail.contains("hunter2"));
        // The production log line must carry the redacted detail.
        let output = writer.take_string();
        assert_hygiene(&output);
        assert!(
            output.contains("PF-1009"),
            "error code must still reach the logs: {output}"
        );
        assert!(
            output.contains("[email]"),
            "email must be redacted, not dropped: {output}"
        );
        assert!(
            output.contains("[ip:"),
            "IP must be hashed, not dropped: {output}"
        );
    });
}

#[test]
fn job_error_path_logs_no_pii() {
    let writer = BufWriter::default();
    let (subscriber, _) =
        build_subscriber(&settings(), LogFormat::Json, "detag-test", writer.clone())
            .expect("subscriber builds");
    tracing::subscriber::with_default(subscriber, || {
        let span = job_span("job-hygiene-1");
        let _enter = span.enter();
        let problem = AppError::DecodeBudgetExceeded.to_problem(
            Profile::Production,
            Some("req-hygiene-2"),
            Some(PII_DETAIL),
        );
        assert_eq!(problem.code, "PF-2007");
        // The worker convention: job failures are logged through
        // `redact` so free-text context can never leak PII.
        tracing::error!(
            error = %redact("decode failed for alice@example.com from 10.0.0.9"),
            "job failed"
        );
        let output = writer.take_string();
        assert_hygiene(&output);
        assert!(
            output.contains("job-hygiene-1"),
            "job_id must still reach the logs: {output}"
        );
    });
}

#[test]
fn secret_values_are_redacted_not_just_emails_and_ips() {
    // `redact` must also mask credential values; an email/IP-only
    // pass would leave "hunter2" in the logs.
    let out = redact("password=hunter2 token=example-token-value api_key=key-1");
    for raw in ["hunter2", "example-token-value", "key-1"] {
        assert!(!out.contains(raw), "secret value leaked: {out}");
    }
    assert!(out.contains("[redacted]"), "values masked: {out}");
    // A bare word without a value is not a credential.
    assert_eq!(redact("the password field"), "the password field");
}
