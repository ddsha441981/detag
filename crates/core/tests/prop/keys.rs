//! Key-scheme properties (P0-096): every Redis key the typed
//! helpers build keeps the `pf:{ns}:{key}` shape with no whitespace
//! or control characters — a typo can never silently address the
//! wrong key (see `crates/core/src/redis/keys.rs`).

use detag_core::redis::keys::{self, Namespace};
use proptest::prelude::*;
use uuid::Uuid;

/// Key-part alphabet: what typed helpers accept (letters, digits,
/// `_`, `-`, `.`, `:` for multi-part keys). Shrink-friendly: a
/// failing case shrinks to the shortest offending part.
fn key_part() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[a-zA-Z0-9_.:-]{1,32}").expect("key regex compiles")
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    /// The generic builder keeps the shape for any namespace and any
    /// valid key part: `pf:{ns}:{key}`, no whitespace.
    #[test]
    fn key_shape_holds(ns in prop::sample::select(vec![
        Namespace::Flags, Namespace::Usage, Namespace::RateLimit,
        Namespace::Cache, Namespace::PubSub, Namespace::Idempotency,
        Namespace::Jobs,
    ]), part in key_part()) {
        let built = keys::key(ns, &part);
        prop_assert!(built.starts_with(&format!("pf:{}:", ns.as_str())), "key: {}", built);
        prop_assert!(!built.chars().any(|c| c.is_whitespace() || c.is_control()), "key: {}", built);
        prop_assert!(built.ends_with(&part), "key: {}", built);
    }

    /// Every namespace spells itself distinctly — no two namespaces
    /// share a prefix, so a key can never belong to two uses.
    #[test]
    fn namespaces_are_distinct(
        a in prop::sample::select(vec![
            Namespace::Flags, Namespace::Usage, Namespace::RateLimit,
            Namespace::Cache, Namespace::PubSub, Namespace::Idempotency,
            Namespace::Jobs,
        ]),
        b in prop::sample::select(vec![
            Namespace::Flags, Namespace::Usage, Namespace::RateLimit,
            Namespace::Cache, Namespace::PubSub, Namespace::Idempotency,
            Namespace::Jobs,
        ]),
    ) {
        prop_assume!(a != b);
        prop_assert_ne!(a.as_str(), b.as_str());
    }

    /// Typed helpers keep their documented prefixes for arbitrary ids.
    #[test]
    fn typed_helpers_keep_prefixes(
        scope in key_part(), id in key_part(), kind in key_part(),
        job_id in any::<u128>(), flag in key_part(),
    ) {
        let uuid = Uuid::from_u128(job_id);
        let want_ratelimit = format!("pf:ratelimit:{scope}:{id}");
        let want_cache = format!("pf:cache:{kind}:{id}");
        let want_flags = format!("pf:flags:{flag}");
        let want_job = format!("pf:jobs:{uuid}");
        let want_channel = format!("pf:pubsub:job:{uuid}");
        let want_idem = format!("pf:idempotency:{id}");
        prop_assert!(keys::rate_limit_key(&scope, &id).starts_with(&want_ratelimit));
        prop_assert!(keys::cache_key(&kind, &id).starts_with(&want_cache));
        prop_assert!(keys::flags_key(&flag).starts_with(&want_flags));
        prop_assert!(keys::job_key(&uuid).starts_with(&want_job));
        prop_assert!(keys::job_events_channel(&uuid).starts_with(&want_channel));
        prop_assert!(keys::idempotency_key(&id).starts_with(&want_idem));
    }
}
