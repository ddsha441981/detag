-- Manual quota grants (P1-107): a temporary, expiring bump to a
-- user's daily quota limits, granted by an admin with a reason
-- and audited as `admin.users.quota_granted`. Grants STACK: the
-- snapshot adds the SUM of the user's unexpired grants to the
-- plan's daily limits, so two grants for the same day add up.
-- An expired grant stays as history (never deleted) and stops
-- adding to the limits the moment `expires_at` passes — the
-- snapshot compares against the app clock, not the DB clock
-- (the frozen-clock tests advance past expiry).
CREATE TABLE quota_grants (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    images INT,
    video_seconds INT,
    expires_at TIMESTAMPTZ NOT NULL,
    reason TEXT NOT NULL,
    granted_by UUID NOT NULL REFERENCES users (id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (images IS NOT NULL OR video_seconds IS NOT NULL),
    CHECK (images IS NULL OR images > 0),
    CHECK (video_seconds IS NULL OR video_seconds > 0)
);
CREATE INDEX quota_grants_user_expires_idx
    ON quota_grants (user_id, expires_at);
