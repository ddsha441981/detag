//! Versioned job envelope (P0-075, E0.8; typed in P0-076).
//!
//! Every job the workers run travels as a [`JobEnvelope`]: the
//! version first, then the identity of the work. The version is
//! the contract: this build writes [`CURRENT_VERSION`] and
//! refuses to run anything else ([`JobEnvelope::check_version`]),
//! so a newer producer can never silently run as v1 — the job
//! fails visibly instead (and P0-077's retry policy decides what
//! happens to it).
//!
//! The envelope is typed (P0-076): `job_id` (the Detag job, for
//! the audit trail), `kind` (a string on the wire — the routing
//! key, parsed into [`JobKind`](crate::registry::JobKind) by the
//! registry; it stays a string here so an unknown kind survives
//! serde and reaches the registry, where it is quarantined
//! instead of failing at the codec), `attempt` (the attempt
//! number at enqueue time — 0 for a new job; the live attempt
//! count is apalis-internal and P0-077 wires it into the retry
//! policy), `enqueued_at`, an opaque JSON `payload`, and
//! `params_hash` (sha256 of the canonical payload JSON, so a
//! consumer can prove the params were not tampered with in
//! transit). P0-076 types `kind` into an enum at dispatch and
//! `payload` becomes per-kind params with a registry mapping
//! kind to handler; the per-kind param shapes land with the
//! handlers that consume them (Phases 2–3).

use chrono::{DateTime, Utc};
use detag_core::checksum::sha256_hex;
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
    /// Attempt number at enqueue time (0 for a new job).
    pub attempt: u32,
    /// When the producer enqueued the job.
    pub enqueued_at: DateTime<Utc>,
    /// Per-kind parameters (typed in P0-076).
    pub payload: serde_json::Value,
    /// sha256 (hex) of the canonical payload JSON — proves the
    /// params were not tampered with in transit.
    pub params_hash: String,
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
    // envelope (it pins the version and computes the params hash),
    // but no producer exists yet — the first enqueue path lands in
    // P0-082 (the ping smoke job), and the unit tests below are its
    // only callers today. The `allow` is scoped to this
    // constructor and removed by the ticket that first enqueues;
    // `check_version` (called by the handler) is live.
    #[allow(dead_code)]
    /// A v1 envelope for `kind` with a JSON payload: attempt 0,
    /// enqueued now, `params_hash` over the canonical payload.
    pub fn new(job_id: Uuid, kind: impl Into<String>, payload: serde_json::Value) -> Self {
        Self {
            version: CURRENT_VERSION,
            job_id,
            kind: kind.into(),
            attempt: 0,
            enqueued_at: Utc::now(),
            params_hash: Self::compute_params_hash(&payload),
            payload,
        }
    }

    /// sha256 (hex) of the canonical payload JSON. Canonical =
    /// `serde_json::to_string`, whose object keys are sorted
    /// (BTreeMap), so equal payloads hash equal regardless of
    /// the key order the producer wrote.
    pub fn compute_params_hash(payload: &serde_json::Value) -> String {
        let canonical = serde_json::to_string(payload).expect("payload serializes");
        sha256_hex(canonical.as_bytes())
    }

    /// True when `params_hash` matches the current payload —
    /// false when the params were tampered with in transit.
    pub fn verify_params_hash(&self) -> bool {
        Self::compute_params_hash(&self.payload) == self.params_hash
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

    #[test]
    fn new_envelope_has_attempt_zero_and_enqueued_now() {
        let before = Utc::now();
        let env = envelope();
        let after = Utc::now();
        assert_eq!(env.attempt, 0, "a new envelope is attempt 0");
        assert!(
            env.enqueued_at >= before && env.enqueued_at <= after,
            "enqueued_at must be now, got {}",
            env.enqueued_at
        );
    }

    #[test]
    fn params_hash_is_sha256_of_the_payload() {
        let env = envelope();
        assert_eq!(env.params_hash.len(), 64, "sha256 hex is 64 chars");
        assert!(env.verify_params_hash(), "a fresh envelope verifies");
        assert_eq!(
            env.params_hash,
            JobEnvelope::compute_params_hash(&env.payload),
            "hash matches the recomputation"
        );
    }

    #[test]
    fn params_hash_is_stable_under_key_order() {
        let a = serde_json::json!({"b": 2, "a": 1});
        let b = serde_json::json!({"a": 1, "b": 2});
        assert_eq!(
            JobEnvelope::compute_params_hash(&a),
            JobEnvelope::compute_params_hash(&b),
            "equal payloads hash equal regardless of key order"
        );
    }

    #[test]
    fn tampered_payload_fails_verification() {
        let mut env = envelope();
        env.payload = serde_json::json!({"image": "s3://detag-dev/other.png"});
        assert!(
            !env.verify_params_hash(),
            "a changed payload must not verify against the old hash"
        );
    }

    #[test]
    fn new_fields_round_trip_through_json() {
        let env = envelope();
        let json = serde_json::to_value(&env).expect("envelope serializes");
        assert_eq!(json["attempt"], 0);
        assert!(json["enqueued_at"].is_string(), "enqueued_at in JSON");
        assert_eq!(json["params_hash"], env.params_hash.as_str());
        let back: JobEnvelope = serde_json::from_value(json).expect("envelope deserializes");
        assert_eq!(back, env);
    }
}
