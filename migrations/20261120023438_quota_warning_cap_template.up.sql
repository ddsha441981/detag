-- Quota-warning cap fix (P1-085, found by the ticket's own live
-- test `full_at_100_enqueues_100_template`): the first index
-- keyed on (user_id, day) only, so an 80% mail sent blocked the
-- 100% mail for the same user/day — the two levels were one cap,
-- not two. The key must include the template: one mail per user
-- + UTC day + LEVEL.
DROP INDEX IF EXISTS email_outbox_quota_warn_once_idx;
CREATE UNIQUE INDEX email_outbox_quota_warn_once_idx
    ON email_outbox (template, (payload->>'user_id'), (payload->>'day'))
    WHERE template IN ('quota-warning-80', 'quota-warning-100');
