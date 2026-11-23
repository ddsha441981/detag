-- Weekly digest dedup (P1-101): one digest per user + week —
-- the partial unique index on (template, payload->>'user_id',
-- payload->>'week_start') for template 'weekly-digest' makes a
-- second enqueue for the same user/week a no-op
-- (try_enqueue_weekly_digest, ON CONFLICT DO NOTHING), the
-- P1-085 quota-warning cap precedent. The week is the Monday
-- (UTC) the digest covers; a digest for the next week is a
-- new week_start, so the cap resets with the week.
CREATE UNIQUE INDEX email_outbox_weekly_digest_once_idx
    ON email_outbox (template, (payload->>'user_id'), (payload->>'week_start'))
    WHERE template = 'weekly-digest';
