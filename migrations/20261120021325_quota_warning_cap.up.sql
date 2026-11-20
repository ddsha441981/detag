-- Quota-warning once/day cap (P1-085, ADR-0049): one outbox row per
-- user, UTC day and warning level. The partial unique index IS the
-- guard — the service inserts with ON CONFLICT DO NOTHING, so two
-- concurrent checks racing the same threshold enqueue exactly one
-- mail. The payload carries `user_id` (uuid string) + `day`
-- (YYYY-MM-DD) for every quota-warning row; the index keys on those
-- two expressions and only on the two quota-warning templates, so
-- no other template's enqueue can ever trip it.
CREATE UNIQUE INDEX email_outbox_quota_warn_once_idx
    ON email_outbox ((payload->>'user_id'), (payload->>'day'))
    WHERE template IN ('quota-warning-80', 'quota-warning-100');
