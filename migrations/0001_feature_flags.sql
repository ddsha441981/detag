-- Feature flags (P0-022). Additive; applied by the migration runner
-- (P0-036). Table per docs/03-ARCHITECTURE/04-data-model.md:
-- key PK, enabled bool, rollout jsonb, updated_by, updated_at.
-- No foreign key on updated_by: the users table lands in a later
-- migration, and flags must be seedable before it exists.
CREATE TABLE IF NOT EXISTS feature_flags (
    key TEXT PRIMARY KEY,
    enabled BOOLEAN NOT NULL DEFAULT FALSE,
    rollout JSONB NOT NULL DEFAULT '{}'::jsonb,
    updated_by TEXT,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
