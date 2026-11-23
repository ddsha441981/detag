-- Revert P1-101's weekly-digest dedup index. A revert must
-- come with a code revert of the weekly-digest service, which
-- relies on the index for its once-per-week cap.
DROP INDEX IF EXISTS email_outbox_weekly_digest_once_idx;
