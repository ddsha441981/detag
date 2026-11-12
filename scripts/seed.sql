-- Seed data (P0-043). Run with `just seed` (detag_core::seed).
--
-- Idempotent by construction: every INSERT is ON CONFLICT DO
-- NOTHING, so a second run is a no-op and an operator/admin edit is
-- never clobbered. The plans rows are verbatim the migration 003
-- seed (migrations/20261112065140_billing.up.sql) — migrations are
-- immutable history, this file is the live idempotent operation;
-- the two must stay in sync (values per FR-P-01, amounts in paise).
--
-- What is seeded:
-- - plans: free / pro / business per FR-P-01 (see the migration
--   header for the entitlement rationale).
-- - feature_flags: the three known flags (crates/core/src/flags.rs,
--   config.rs KNOWN_FEATURE_FLAGS + derived `gpu`), all OFF — the
--   static defaults. `provenance_strip` stays OFF (legal gate,
--   DECISIONS.md #5); `razorpay` stays OFF until the billing
--   adapter is enabled (FR-P-05); `gpu` stays OFF in dev.
--   updated_by 'seed' marks these rows as seed-owned; an admin
--   edit (different updated_by / enabled value) survives re-seed.
--
-- NOT seeded here: the admin invite. No invites table and no
-- invite flow exist in the kit, and the runbook's admin mechanism
-- is the `pf-admin-create` CLI, which has no owning ticket yet
-- (see DECISIONS.md #8, owner-input-needed). This seed invents
-- neither an admin user nor a credential — a dev seed must never
-- contain a hardcoded password.

-- Plans (FR-P-01; verbatim migration 003 seed).
INSERT INTO plans (code, currency, amount, entitlements) VALUES
    ('free', 'INR', 0, '{
        "daily_images": 10,
        "daily_video_seconds": 0,
        "max_bytes": 26214400,
        "result_ttl_hours": 6,
        "concurrency": 1,
        "lane": "standard",
        "features": {}
    }'::jsonb),
    ('pro', 'INR', 49900, '{
        "daily_images": 500,
        "daily_video_seconds": 3600,
        "max_bytes": 1073741824,
        "result_ttl_hours": 24,
        "concurrency": 3,
        "lane": "standard",
        "features": {"api_keys": true, "webhooks": true}
    }'::jsonb),
    ('business', 'INR', 199900, '{
        "daily_images": 5000,
        "daily_video_seconds": 18000,
        "max_bytes": 4294967296,
        "result_ttl_hours": 72,
        "concurrency": 10,
        "lane": "priority",
        "features": {"api_keys": true, "webhooks": true, "batch": true}
    }'::jsonb)
ON CONFLICT (code) DO NOTHING;

-- Feature flags (all OFF = static defaults).
INSERT INTO feature_flags (key, enabled, rollout, updated_by) VALUES
    ('razorpay', FALSE, '{}'::jsonb, 'seed'),
    ('provenance_strip', FALSE, '{}'::jsonb, 'seed'),
    ('gpu', FALSE, '{}'::jsonb, 'seed')
ON CONFLICT (key) DO NOTHING;
