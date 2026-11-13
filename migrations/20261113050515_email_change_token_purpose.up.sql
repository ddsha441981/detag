-- email_change_token_purpose (P1-006, E1.1). The token table
-- carries a purpose now: a registration-verification token and
-- an email-change token are the same shape (single-use, 24h,
-- hash-only) but must never be interchangeable — presenting a
-- registration token at the confirm-email-change endpoint (or
-- vice versa) is an invalid token, not a verify.
--
-- Choices recorded here:
-- - `purpose` is `verification` for every row minted before
--   this migration (the DEFAULT backfills them) and for every
--   token `issue_verification` mints after it; `email_change`
--   only for tokens `issue_email_change` mints.
-- - One live token per user still holds (minting deletes the
--   user's previous UNUSED tokens, whatever their purpose): a
--   resend-verification mints a `verification` token and kills a
--   live `email_change` token, and an email-change PATCH mints
--   an `email_change` token and kills a live `verification`
--   token. The pending intent survives either way (it lives on
--   the user row, not in the token table) and is re-triggered by
--   clearing + re-setting it.
-- - No purpose index: lookup is by `token_hash` (unique);
--   purpose is checked on the fetched row.

ALTER TABLE email_verification_tokens
    ADD COLUMN purpose TEXT NOT NULL DEFAULT 'verification'
    CHECK (purpose IN ('verification', 'email_change'));
