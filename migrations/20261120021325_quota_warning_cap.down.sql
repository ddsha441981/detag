-- Revert P1-085's quota-warning cap index. No rows are deleted:
-- the cap is enforced by the index, and without it the service's
-- ON CONFLICT DO NOTHING insert degrades to a plain insert (the
-- once/day guarantee is lost, so a revert must come with a code
-- revert of the quota-warning service).
DROP INDEX IF EXISTS email_outbox_quota_warn_once_idx;
