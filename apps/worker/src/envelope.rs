//! Versioned job envelope (P0-075, E0.8).
//!
//! Every job the workers run travels as a [`JobEnvelope`]: the
//! version first, then the identity of the work. The version is
//! the contract: this build writes [`CURRENT_VERSION`] and
//! refuses to run anything else ([`JobEnvelope::check_version`]),
//! so a newer producer can never silently run as v1 — the job
//! fails visibly instead (and P0-077's retry policy decides what
//! happens to it).
//!
//! The envelope is deliberately thin in P0-075: `job_id` (the
//! Detag job, for the audit trail), `kind` (a string — the
//! routing key), and an opaque JSON `payload`. P0-076 types it:
//! `kind` becomes an enum, `payload` becomes per-kind params
//! with a `params_hash`, and `attempt` / `enqueued_at` join the
//! envelope, with a registry mapping kind to handler. Until
//! then the stub handler in `workers.rs` accepts any kind —
//! nothing enqueues jobs in Phase 0.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The envelope version this build writes and accepts.
pub const CURRENT_VERSION: u32 = 1;

/// One unit of work for a worker. Serialized as JSON by the
/// apalis Redis storage (the storage's `JsonCodec`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobEnvelope {
    /// Envelope version — checked before the job runs.
    pub version: u32,
    /// The Detag job this envelope is for.
    pub job_id: Uuid,
    /// Routing key for the handler (typed into an enum in P0-076).
    pub kind: String,
    /// Per-kind parameters (typed in P0-076).
    pub payload: serde_json::Value,
}

/// A version this build cannot run.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EnvelopeError {
    /// The producer wrote a newer (or unknown) envelope version.
    #[error("unsupported envelope version {0} (this build runs version {CURRENT_VERSION})")]
    UnsupportedVersion(u32),
}

impl JobEnvelope {
    // NOTE (P0-075): `new` is the only way producers may build an
    // envelope (it pins the version), but no producer exists yet —
    // the first enqueue path lands in P0-076, and the unit tests
    // below are its only callers today. The `allow` is scoped to
    // this constructor and removed by the ticket that first
    // enqueues; `check_version` (called by the handler) is live.
    #[allow(dead_code)]
    /// A v1 envelope for `kind` with a JSON payload.
    pub fn new(job_id: Uuid, kind: impl Into<String>, payload: serde_json::Value) -> Self {
        Self {
            version: CURRENT_VERSION,
            job_id,
            kind: kind.into(),
            payload,
        }
    }

    /// Refuse to run an envelope this build does not understand.
    /// A newer version must fail visibly, never silently run as v1.
    pub fn check_version(&self) -> Result<(), EnvelopeError> {
        if self.version == CURRENT_VERSION {
            Ok(())
        } else {
            Err(EnvelopeError::UnsupportedVersion(self.version))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope() -> JobEnvelope {
        JobEnvelope::new(
            Uuid::now_v7(),
            "pipeline",
            serde_json::json!({"image": "s3://detag-dev/in.png"}),
        )
    }

    #[test]
    fn version_field_present_and_one() {
        let env = envelope();
        assert_eq!(env.version, CURRENT_VERSION);
        assert_eq!(CURRENT_VERSION, 1);
    }

    #[test]
    fn serde_round_trip_keeps_the_version() {
        let env = envelope();
        let json = serde_json::to_value(&env).expect("envelope serializes");
        assert_eq!(
            json["version"], 1,
            "the version field must be in the JSON: {json}"
        );
        let back: JobEnvelope = serde_json::from_value(json).expect("envelope deserializes");
        assert_eq!(back, env);
    }

    #[test]
    fn newer_version_is_rejected() {
        let mut env = envelope();
        env.version = 2;
        let err = env.check_version().expect_err("v2 must be rejected");
        assert_eq!(err, EnvelopeError::UnsupportedVersion(2));
        assert!(err.to_string().contains('2'));
        assert!(err.to_string().contains('1'));
    }

    #[test]
    fn older_version_is_rejected() {
        let mut env = envelope();
        env.version = 0;
        assert_eq!(
            env.check_version().expect_err("v0 must be rejected"),
            EnvelopeError::UnsupportedVersion(0)
        );
    }
}
