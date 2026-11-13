-- password_reset_token_purpose (P1-025, E1.3). The token table
-- carries a third purpose now: a forgot-password token is the
-- same shape as a verification token (single-use, hash-only)
-- but with a 30-minute TTL (not 24h) and it must never be
-- interchangeable — presenting a registration token at the
-- reset endpoint (or a reset token at verify-email) is an
-- invalid token, not a verify/reset. The purpose is checked on
-- the fetched row (same as P1-006).
--
-- No backfill question: no `password_reset` rows exist yet
-- (the purpose is new with this ticket); existing rows keep
-- their purpose.
-- No purpose index: lookup is by `token_hash` (unique).

ALTER TABLE email_verification_tokens DROP CONSTRAINT email_verification_tokens_purpose_check;
ALTER TABLE email_verification_tokens ADD CONSTRAINT email_verification_tokens_purpose_check
    CHECK (purpose IN ('verification', 'email_change', 'password_reset'));
