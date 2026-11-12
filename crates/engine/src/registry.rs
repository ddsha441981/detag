//! detag-engine::registry — model registry; implemented in Phase 2
//! (removal engine). In Phase 0 the registry is empty by design:
//! no model is loaded, and no endpoint serves inference, so the
//! models component of `/readyz` reports that state honestly
//! instead of faking a model check.
//!
//! Phase 2 replaces [`models_health`] with a real check: every
//! model the registry requires must be present and verified
//! (sha256, P0-084) before the component reports ok.

use detag_core::db::ComponentHealth;

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
}
