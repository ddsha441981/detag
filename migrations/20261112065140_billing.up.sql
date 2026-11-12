-- plans + subscriptions + usage_counters (P0-039, E0.4).
-- Per docs/03-ARCHITECTURE/04-data-model.md, seed per FR-P-01.
--
-- Choices the doc leaves open, recorded here:
-- - ids are UUID PRIMARY KEY DEFAULT gen_random_uuid().
-- - plans.code is UNIQUE with a CHECK (free|pro|business).
-- - plans.amount is INTEGER in the smallest currency unit (paise
--   for INR — Stripe's convention, the primary provider): Free 0,
--   Pro 49900, Business 199900. currency is 'INR'.
-- - plans.stripe_price_id is NULL until a live price is configured
--   (no live payment keys in Phase 0 — see DECISIONS.md).
-- - entitlements jsonb keys are exactly the doc's list
--   (daily_images, daily_video_seconds, max_bytes,
--   result_ttl_hours, concurrency, lane, features), values from
--   FR-P-01: Free 10 img/day, no video, 25 MB, 6h TTL, 1 concurrent;
--   Pro 500 img/day, 60 video-min/day, 1 GB, 24h TTL, 3 concurrent,
--   API keys + webhooks; Business 5000 img/day, 300 video-min/day,
--   4 GB, 72h TTL, 10 concurrent, priority lane, batch. max_bytes
--   is bytes (25 MB = 26214400, 1 GB = 1073741824, 4 GB =
--   4294967296); daily_video_seconds 0 means "no video".
-- - lane 'standard' for Free/Pro is the seed default (the doc
--   names only Business's 'priority lane'); plans are
--   owner-changeable per FR-P-01.
-- - The seed INSERTs use ON CONFLICT (code) DO NOTHING: a fresh DB
--   gets the rows, and re-running never clobbers an admin's edits.
--   The `just seed` task (P0-043) upserts the same rows.
-- - subscriptions.plan_id is ON DELETE RESTRICT: a plan with
--   subscribers cannot be deleted. status has no default — the app
--   sets trialing/active at creation.
-- - usage_counters PK is (user_id, day) per the doc; Redis mirrors
--   the hot path (Phase 1), the DB row is the durable counter.

CREATE TABLE plans (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    code TEXT NOT NULL UNIQUE CHECK (code IN ('free', 'pro', 'business')),
    stripe_price_id TEXT,
    currency TEXT NOT NULL DEFAULT 'INR',
    amount INT NOT NULL CHECK (amount >= 0),
    entitlements JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE subscriptions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    plan_id UUID NOT NULL REFERENCES plans (id) ON DELETE RESTRICT,
    provider TEXT NOT NULL CHECK (provider IN ('stripe', 'razorpay')),
    provider_customer_id TEXT,
    provider_subscription_id TEXT,
    status TEXT NOT NULL
        CHECK (status IN ('trialing', 'active', 'past_due', 'canceled')),
    current_period_end TIMESTAMPTZ,
    cancel_at_period_end BOOLEAN NOT NULL DEFAULT false,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE usage_counters (
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    day DATE NOT NULL,
    images_used INT NOT NULL DEFAULT 0 CHECK (images_used >= 0),
    video_seconds_used INT NOT NULL DEFAULT 0 CHECK (video_seconds_used >= 0),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, day)
);

CREATE INDEX subscriptions_user_id_idx ON subscriptions (user_id);

-- Seed plans (FR-P-01). Idempotent: ON CONFLICT DO NOTHING.
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
