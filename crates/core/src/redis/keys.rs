//! Redis key helpers (P0-047): one naming convention for every
//! Redis key in the system, plus the TTL constants.
//!
//! Every key is `pf:{namespace}:{key}` — the `pf:` prefix keeps
//! Detag keys separate from anything else sharing the server, and
//! the namespace keeps the uses separate from each other. Callers
//! use the typed helpers below, never a hand-built string, so a
//! typo cannot silently address the wrong key.
//!
//! Namespaces (one per Redis use in the kit):
//!
//! | Namespace | Used by | TTL |
//! | --- | --- | --- |
//! | `flags` | feature-flag cache (P0-022, E0.8) | `TTL_FLAG_CACHE` (30 s) |
//! | `usage` | daily usage-counter mirrors (Phase 1) | `TTL_USAGE_COUNTER` (48 h) |
//! | `ratelimit` | rate-limit buckets (P0-048) | set by the limiter per window — no const here |
//! | `cache` | get_or_set cache entries (P0-049) | set per call — no const here |
//! | `pubsub` | job event channels (P0-050) | none — channels are ephemeral |
//! | `idempotency` | idempotency response cache | the row's `expires_at` — no const here |
//! | `jobs` | job status/progress cache | capped by `TTL_RESULT_MAX` (72 h) |
//!
//! TTLs that are not constants here are deliberately absent: a
//! rate-limit bucket lives exactly its window, a cache entry lives
//! its caller's TTL, an idempotency entry lives until its row
//! expires — a shared constant for those would be wrong the first
//! time a caller needed a different value.

use chrono::NaiveDate;
use std::time::Duration;
use uuid::Uuid;

/// Feature-flag cache TTL: the DB-backed flag cache TTL
/// (`FLAG_CACHE_TTL`, `crates/core/src/flags.rs`, tech-stack doc).
pub const TTL_FLAG_CACHE: Duration = Duration::from_secs(30);

/// Daily usage-counter mirror TTL: the counter is for one day, so
/// the key must outlive the day it counts plus a grace period for
/// the rollover. The durable row lives in Postgres (400 d
/// retention); the Redis copy only needs to survive the day + 1.
/// 48 h.
pub const TTL_USAGE_COUNTER: Duration = Duration::from_secs(48 * 3600);

/// Longest result TTL (Business plan, 72 h — FR-P-01): no
/// result/status cache entry may outlive the result itself. A
/// shorter plan TTL caps its own entries; this is the ceiling.
pub const TTL_RESULT_MAX: Duration = Duration::from_secs(72 * 3600);

/// One Redis namespace (the `{ns}` in `pf:{ns}:{key}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Namespace {
    /// Feature-flag cache entries.
    Flags,
    /// Daily usage-counter mirrors.
    Usage,
    /// Rate-limit buckets.
    RateLimit,
    /// get_or_set cache entries.
    Cache,
    /// Job event pubsub channels.
    PubSub,
    /// Idempotency response cache.
    Idempotency,
    /// Job status/progress cache.
    Jobs,
}

impl Namespace {
    /// The namespace as it appears in the key.
    pub fn as_str(self) -> &'static str {
        match self {
            Namespace::Flags => "flags",
            Namespace::Usage => "usage",
            Namespace::RateLimit => "ratelimit",
            Namespace::Cache => "cache",
            Namespace::PubSub => "pubsub",
            Namespace::Idempotency => "idempotency",
            Namespace::Jobs => "jobs",
        }
    }
}

/// Build a `pf:{ns}:{key}` key. Panics on an empty key or a key
/// containing whitespace or control characters. A `:` inside the
/// key is allowed — it is how multi-part keys are built
/// (`pf:usage:{uuid}:{date}`); the invariant is the `pf:{ns}:`
/// prefix, not a single segment after it. This is a programming
/// error, not a runtime one: keys come from typed helpers or
/// validated input, never from raw user text.
pub fn key(namespace: Namespace, key: &str) -> String {
    assert!(!key.is_empty(), "redis key must not be empty");
    assert!(
        !key.chars().any(|c| c.is_whitespace() || c.is_control()),
        "redis key must not contain whitespace: {key:?}"
    );
    format!("pf:{}:{key}", namespace.as_str())
}

/// Feature-flag cache entry for `flag` (`pf:flags:{flag}`).
pub fn flags_key(flag: &str) -> String {
    key(Namespace::Flags, flag)
}

/// Daily usage counter for `user_id` on `day` (`pf:usage:{user}:{YYYY-MM-DD}`).
pub fn usage_key(user_id: &Uuid, day: NaiveDate) -> String {
    key(
        Namespace::Usage,
        &format!("{}:{}", user_id, day.format("%Y-%m-%d")),
    )
}

/// Rate-limit bucket for `scope` (e.g. `jobs_create`) and `id`
/// (user id, api-key prefix, or IP hash).
/// (`pf:ratelimit:{scope}:{id}`.)
pub fn rate_limit_key(scope: &str, id: &str) -> String {
    key(Namespace::RateLimit, &format!("{scope}:{id}"))
}

/// Cache entry for `kind` (e.g. `plan`) and `id`.
/// (`pf:cache:{kind}:{id}`.)
pub fn cache_key(kind: &str, id: &str) -> String {
    key(Namespace::Cache, &format!("{kind}:{id}"))
}

/// Job event pubsub channel for `job_id` (`pf:pubsub:job:{id}`).
/// Channels share the key shape but are not keys — nothing is
/// ever SET on one, and there is no TTL.
pub fn job_events_channel(job_id: &Uuid) -> String {
    key(Namespace::PubSub, &format!("job:{job_id}"))
}

/// Idempotency response cache for `key` (the idempotency key the
/// client sent). (`pf:idempotency:{key}`.)
pub fn idempotency_key(key_text: &str) -> String {
    key(Namespace::Idempotency, key_text)
}

/// Job status/progress cache for `job_id` (`pf:jobs:{id}`).
pub fn job_key(job_id: &Uuid) -> String {
    key(Namespace::Jobs, &job_id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn uuid() -> Uuid {
        Uuid::parse_str("0198c8d2-0000-7000-8000-000000000001").expect("test uuid parses")
    }

    #[test]
    fn key_shape_is_pf_namespace_key() {
        assert_eq!(key(Namespace::Flags, "gpu"), "pf:flags:gpu");
        assert_eq!(key(Namespace::Usage, "u1"), "pf:usage:u1");
    }

    #[test]
    fn typed_helpers_produce_the_documented_keys() {
        let day = NaiveDate::from_ymd_opt(2026, 11, 12).expect("test date");
        assert_eq!(flags_key("razorpay"), "pf:flags:razorpay");
        assert_eq!(
            usage_key(&uuid(), day),
            "pf:usage:0198c8d2-0000-7000-8000-000000000001:2026-11-12"
        );
        assert_eq!(
            rate_limit_key("jobs_create", "user-1"),
            "pf:ratelimit:jobs_create:user-1"
        );
        assert_eq!(cache_key("plan", "pro"), "pf:cache:plan:pro");
        assert_eq!(
            job_events_channel(&uuid()),
            "pf:pubsub:job:0198c8d2-0000-7000-8000-000000000001"
        );
        assert_eq!(idempotency_key("abc"), "pf:idempotency:abc");
        assert_eq!(
            job_key(&uuid()),
            "pf:jobs:0198c8d2-0000-7000-8000-000000000001"
        );
    }

    #[test]
    fn ttl_constants_match_their_sources() {
        // Flag cache TTL is the flags module's cache TTL (30 s).
        assert_eq!(TTL_FLAG_CACHE, crate::flags::FLAG_CACHE_TTL);
        assert_eq!(TTL_USAGE_COUNTER, Duration::from_secs(48 * 3600));
        // Longest plan result TTL: Business, 72 h (FR-P-01).
        assert_eq!(TTL_RESULT_MAX, Duration::from_secs(72 * 3600));
    }

    #[test]
    #[should_panic(expected = "must not be empty")]
    fn empty_key_panics() {
        key(Namespace::Flags, "");
    }

    #[test]
    #[should_panic(expected = "must not contain whitespace")]
    fn whitespace_in_key_panics() {
        key(Namespace::Flags, "a b");
    }
}
