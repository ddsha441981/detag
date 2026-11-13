-- password_reset_token_purpose (P1-025, E1.3). Down migration:
-- exactly reverses the up migration. Fails if a
-- `password_reset` row exists (the CHECK would reject it) —
-- delete those rows first in that case; the down migration is
-- a dev-only escape hatch, never run against real data.

ALTER TABLE email_verification_tokens DROP CONSTRAINT email_verification_tokens_purpose_check;
ALTER TABLE email_verification_tokens ADD CONSTRAINT email_verification_tokens_purpose_check
    CHECK (purpose IN ('verification', 'email_change'));
