-- Revert P1-090's resolution columns: the queue loses its
-- resolution timestamps (resolved_at / resolved_by). The
-- audit rows survive the revert (append-only), so the
-- resolutions stay provable from the trail.
ALTER TABLE abuse_reports DROP COLUMN IF EXISTS resolved_at;
ALTER TABLE abuse_reports DROP COLUMN IF EXISTS resolved_by;
