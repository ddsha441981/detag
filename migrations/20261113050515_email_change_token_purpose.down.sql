-- email_change_token_purpose (P1-006, E1.1). Down migration:
-- exactly reverses the up migration.

ALTER TABLE email_verification_tokens DROP COLUMN purpose;
