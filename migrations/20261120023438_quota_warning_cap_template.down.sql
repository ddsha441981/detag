-- Revert P1-085's cap-index fix: restore the (user_id, day)-only
-- key. This reintroduces the bug the fix corrected (an 80% mail
-- blocks the same day's 100% mail), so a revert must come with a
-- code revert of the quota-warning service.
DROP INDEX IF EXISTS email_outbox_quota_warn_once_idx;
CREATE UNIQUE INDEX email_outbox_quota_warn_once_idx
    ON email_outbox ((payload->>'user_id'), (payload->>'day'))
    WHERE template IN ('quota-warning-80', 'quota-warning-100');
