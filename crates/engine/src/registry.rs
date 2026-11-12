//! detag-engine::registry — model registry; implemented in Phase 2
//! (removal engine). In Phase 0 the registry is empty by design:
//! no model is loaded, and no endpoint serves inference, so the
//! models component of `/readyz` reports that state honestly
//! instead of faking a model check.
//!
//! P0-084 adds the startup verification: [`verify_models`] checks
//! every model the registry requires — file present under the models
//! directory with the pinned sha256 — before any worker starts
//! consuming jobs. Phase 0 requires no model, so the check attests
//! an empty registry; the sha256 comparison itself is real and runs
//! on the first Phase-2 entry. Downloading a missing model is
//! Phase 2's job (first real registry entry + license audit); this
//! step only verifies what the registry requires.
//!
//! Phase 2 replaces [`models_health`] with a real check: every
//! model the registry requires must be present and verified
//! (sha256, P0-084) before the component reports ok.

use detag_core::checksum::sha256_reader;
use detag_core::db::ComponentHealth;
use std::path::Path;

/// A model the engine requires: a file under the models directory
/// plus the sha256 the file must hash to. Owned strings: Phase 2
/// parses entries from the registry file.
pub struct ModelEntry {
    /// Human name, used in logs and error messages.
    pub name: String,
    /// File name relative to the models directory.
    pub file: String,
    /// Lowercase hex sha256 of the file.
    pub sha256: String,
}

/// Models the engine requires (P0-084). Phase 0: empty by design —
/// no model is loaded until Phase 2 (license audit + registry file).
pub fn required_models() -> Vec<ModelEntry> {
    Vec::new()
}

/// Verify every required model: the file exists under `models_dir`
/// and hashes to the pinned sha256 (streamed, never loaded whole —
/// model files are large). Returns the verified count. A missing
/// file or a hash mismatch is an error naming the model. The models
/// directory itself is never created or required: with an empty
/// registry this returns `Ok(0)` without touching the filesystem,
/// so a worker boots fine with no `./models` directory at all.
pub async fn verify_models(models_dir: &Path, entries: &[ModelEntry]) -> Result<usize, String> {
    let mut verified = 0;
    for entry in entries {
        let path = models_dir.join(&entry.file);
        let file = tokio::fs::File::open(&path).await.map_err(|_| {
            format!(
                "model {} missing: {} not found (downloading a missing model lands in Phase 2)",
                entry.name,
                path.display()
            )
        })?;
        let actual = sha256_reader(file)
            .await
            .map_err(|err| format!("model {} unreadable: {err}", entry.name))?;
        if actual != entry.sha256 {
            return Err(format!(
                "model {} sha256 mismatch: {} hashes to {actual}, registry pins {}",
                entry.name,
                path.display(),
                entry.sha256
            ));
        }
        verified += 1;
    }
    Ok(verified)
}

/// Health probe for the `/readyz` models component (P0-066,
/// FR-X-02): the registry state. Phase 0 requires no model, so an
/// empty registry is ok — the detail says so plainly, and the
/// latency is the (near-zero) cost of reading the registry, not a
/// faked number.
pub fn models_health() -> ComponentHealth {
    let started = std::time::Instant::now();
    ComponentHealth::timed(
        "models",
        true,
        "no models registered — none required until Phase 2".to_string(),
        started,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use detag_core::checksum::sha256_hex;

    /// A scratch models dir under the temp dir, unique per test —
    /// the tests run in parallel in one process, so a shared dir
    /// would let one test's file leak into another's assertions.
    fn scratch_dir(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("detag-registry-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch models dir");
        dir
    }

    #[test]
    fn empty_registry_is_ok_but_honest() {
        let health = models_health();
        assert_eq!(health.name, "models");
        assert!(health.ok, "no model is required in Phase 0: {health:?}");
        assert!(
            health.detail.contains("Phase 2"),
            "detail must say this is a Phase 0 state: {}",
            health.detail
        );
        // P0-066: every component reports its probe latency.
        assert!(
            health.latency_ms < 2000,
            "registry read must be fast: {health:?}"
        );
    }

    #[tokio::test]
    async fn empty_registry_verifies_without_touching_the_filesystem() {
        // The models dir does not even exist — with no required
        // models that must still be Ok(0), so a worker boots fine
        // with no ./models directory at all.
        let missing_dir =
            std::env::temp_dir().join(format!("detag-registry-absent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&missing_dir);
        assert_eq!(verify_models(&missing_dir, &required_models()).await, Ok(0));
    }

    #[tokio::test]
    async fn matching_sha256_verifies() {
        let dir = scratch_dir("matching");
        let content = b"fake model bytes";
        std::fs::write(dir.join("lama.onnx"), content).expect("write model file");
        let entry = ModelEntry {
            name: "lama".to_string(),
            file: "lama.onnx".to_string(),
            sha256: sha256_hex(content),
        };
        assert_eq!(verify_models(&dir, &[entry]).await, Ok(1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn wrong_sha256_fails_naming_the_model() {
        let dir = scratch_dir("wrong-sha");
        std::fs::write(dir.join("lama.onnx"), b"tampered").expect("write model file");
        let entry = ModelEntry {
            name: "lama".to_string(),
            file: "lama.onnx".to_string(),
            sha256: "0000000000000000000000000000000000000000000000000000000000000000".to_string(),
        };
        let err = verify_models(&dir, &[entry])
            .await
            .expect_err("tampered model must fail");
        assert!(err.contains("lama"), "error must name the model: {err}");
        assert!(err.contains("mismatch"), "error must say mismatch: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn missing_file_fails_naming_the_model() {
        let dir = scratch_dir("missing");
        let entry = ModelEntry {
            name: "lama".to_string(),
            file: "lama.onnx".to_string(),
            sha256: "0000000000000000000000000000000000000000000000000000000000000000".to_string(),
        };
        let err = verify_models(&dir, &[entry])
            .await
            .expect_err("missing model must fail");
        assert!(err.contains("lama"), "error must name the model: {err}");
        assert!(err.contains("missing"), "error must say missing: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
